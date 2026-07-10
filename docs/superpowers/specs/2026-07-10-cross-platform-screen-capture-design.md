# 跨平台屏幕捕获设计

## 概述

将 `mac-screen-cast` 从纯 macOS 工具扩展为支持 **macOS + Linux + Windows** 三平台的屏幕捕获 + H.264 硬件编码 + WebRTC 流媒体工具。

### 当前状态（macOS 仅）

```
ScreenCaptureKit ──zero-copy IOSurface──→ VideoToolbox ──H.264──→ WebRTC → Browser
```

### 目标状态（三平台）

```
macOS: ScreenCaptureKit ──IOSurface──→ VideoToolbox ──H.264──→ WebRTC
Linux:  PipeWire/DMA-BUF  ──fd──→ VAAPI/NVENC ──H.264──→ WebRTC
Windows: DXGI DDA ──shared handle──→ NVENC/AMF ──H.264──→ WebRTC
```

编码后的 H.264 数据流格式统一（AVCC NALs），WebRTC 层完全跨平台复用。

---

## 架构

### ScreenCapture trait（跨平台抽象）

```rust
/// 不透明的平台帧缓冲区句柄，零拷贝传给编码器
pub struct FrameBuffer {
    /// 平台特定的 GPU/CPU 资源句柄
    pub handle: FrameBufferHandle,
    pub width: u32,
    pub height: u32,
    pub pts: u64,        // 递增的时间戳
    pub timescale: u32,  // pts 的时间基准（通常 1000 或 fps）
    pub cap_time: std::time::Instant, // 捕获时间戳，用于延迟追踪
}

pub enum FrameBufferHandle {
    /// macOS: IOSurface 引用（已 retain），Drop 时 CFRelease
    #[cfg(target_os = "macos")]
    IOSurface(IoSurfaceRef),
    /// Linux (Wayland): DMA-BUF fd，OwnedFd 自动 close
    #[cfg(target_os = "linux")]
    DmaBuf { fd: std::os::fd::OwnedFd, modifier: u64, drm_format: u32 },
    /// Linux (X11 fallback): CPU 端共享内存，需上传到 GPU
    #[cfg(target_os = "linux")]
    CpuBuffer { ptr: *mut u8, size: usize, fd: std::os::fd::OwnedFd },
    /// Windows: D3D11 Texture2D COM 接口，Drop 时自动 Release
    #[cfg(target_os = "windows")]
    D3D11Texture(windows::Graphics::DirectX::Direct3D11::ID3D11Texture2D),
}

// 注：FrameBuffer 通过 channel 跨线程发送，需要 unsafe impl Send
// 每平台的 Drop 实现负责释放 GPU/CPU 资源
//
// 安全理由（unsafe impl Send）：
// - IOSurfaceRef: CF 对象，线程安全
// - DmaBuf fd: OwnedFd 是 Send
// - CpuBuffer ptr: 只在编码线程使用，不跨线程
// - ID3D11Texture2D: COM 接口是线程安全的
//
// 每个平台需要分别 #[cfg] 包裹 unsafe impl Send for FrameBuffer

/// 每个平台需要提供的能力
#[async_trait]
pub trait ScreenCapture: Send {
    type Options;

    /// 创建捕获会话（获取窗口/显示器选择权限）
    async fn create(options: Self::Options) -> Result<Self>
    where
        Self: Sized;

    /// 开始捕获，通过回调投递帧
    async fn start<F>(&mut self, on_frame: F) -> Result<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static;

    /// 停止捕获
    async fn stop(&mut self) -> Result<()>;
}
```

### HardwareEncoder trait（跨平台抽象）

```rust
/// 编码器输出
pub struct EncodedFrame {
    pub data: Vec<u8>,        // AVCC 格式 H.264
    pub is_keyframe: bool,
    pub pts: u64,
    // SPS/PPS 可选内联在 data 中，或通过额外字段提供
    pub sps_pps: Option<(Vec<u8>, Vec<u8>)>,
}

#[async_trait]
pub trait HardwareEncoder: Send {
    type Options;

    fn new(width: u32, height: u32, fps: u32, options: Self::Options) -> Result<Self>
    where
        Self: Sized;

    /// 编码一帧
    async fn encode(&self, frame: &FrameBuffer) -> Result<EncodedFrame>;
}
```

### 管线编排（main.rs，跨平台）

```
[CaptureThread]            [EncoderThread]          [SendThread]
     │                          │                        │
     │───FrameBuffer───→        │                        │
     │                 VtEncoder::encode(&iosurface)     │
     │                          │───EncodedFrame──→      │
     │                          │               wr.send_frame()
```

现有的 `main.rs` 管线结构保持不变——只需要将 macOS 专用的 `screencapturekit` / `videotoolbox` 替换为 trait 调用。

---

## Linux 方案

### 分层架构

```
┌───────────────────────────────────────────────┐
│  ScreenCapture trait impl (linux.rs)           │
│  ┌─────────────────────────────────────────┐   │
│  │  PortalCapturer                         │   │
│  │  - zbus D-Bus xdg-desktop-portal 交互   │   │
│  │  - 处理 Session/Request/Start 信号流    │   │
│  │  - 返回 PipeWire node ID + FD          │   │
│  └────────────┬────────────────────────────┘   │
│               ▼                                 │
│  ┌─────────────────────────────────────────┐   │
│  │  PipeWireStream                         │   │
│  │  - pw_stream 连接 + 回调                │   │
│  │  - 从 spa_buffer 提取 DMA-BUF fd        │   │
│  └────────────┬────────────────────────────┘   │
│               ▼                                 │
│  ┌─────────────────────────────────────────┐   │
│  │  DmaBufFrameBuffer (FrameBuffer enum)    │   │
│  │  - 持有 fd + modifier + drm_format      │   │
│  └─────────────────────────────────────────┘   │
└───────────────────────────────────────────────┘

┌───────────────────────────────────────────────┐
│  HardwareEncoder trait impl (linux.rs)         │
│  ┌─────────────────────────────────────────┐   │
│  │  VaapiEncoder                           │   │
│  │  - libva Display/Config/Context/Surface │   │
│  │  - DMA-BUF fd → VA surface 导入（零拷贝）│   │
│  │  - vaSyncSurface + vaGetEncodedBits     │   │
│  └─────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────┐   │
│  │  NvencEncoder（备用）                    │   │
│  │  - 通过 nvEncodeAPI FFI                 │   │
│  └─────────────────────────────────────────┘   │
└───────────────────────────────────────────────┘
```

### Portal D-Bus 交互细节

```
Client                                      xdg-desktop-portal
  │                                                │
  │  CreateSession()                               │
  │───────────────────────────────────────────────→│
  │  ← Response: session_handle + request_handle   │
  │                                                │
  │  org.freedesktop.portal.ScreenCast.CreateSession│
  │    options: { handle_token, session_handle_token } │
  │                                                │
  │  ← Signal: Response (0 = success, session path)│
  │                                                │
  │  org.freedesktop.portal.ScreenCast.SelectSources│
  │    options: { types: (MONITOR | WINDOW) }      │
  │                                                │
  │  (系统弹出选择器，用户选择窗口/显示器)            │
  │                                                │
  │  Start()                                       │
  │───────────────────────────────────────────────→│
  │  ← Signal: PipeWire stream node info (FD + ID) │
```

**Rust D-Bus 包选择**：`zbus` 5.x（纯 Rust，异步，tokio 兼容，62M+ 下载量，716 GitHub stars，周更）。

**重要：需启用 `tokio` feature**：
```toml
zbus = { version = "5", default-features = false, features = ["tokio"] }
```

### D-Bus / PipeWire 异步桥接

`zbus` 使用 tokio 异步运行时，而 PipeWire 使用自己的主循环回调模型。

**方案：专用线程 + mpsc 通道（推荐）**

```
[D-Bus thread (zbus/tokio)]              [PipeWire thread]
     │                                          │
     │  Portal Start()                          │ pw_main_loop_run()
     │  → 获取 PipeWire node ID + FD            │
     │  → 通过 Arc<Mutex<>> 传给 PipeWire 线程   │
     │─────────────────────────────────────────→│
     │                                          │ on_process() →
     │                                    mpsc  │ FrameBuffer(DmaBuf fd)
     │                                          ▼
     │                              [Encoder thread]
     │                              VAAPI::encode(&frame)
```

流程：
1. D-Bus 线程通过 `zbus` 调用 Portal API，获取 PipeWire node ID 和 FD
2. 通过 `Arc<Mutex<Option<...>>>` 共享给 PipeWire 线程
3. PipeWire 线程运行 `pw_main_loop_run()`，`on_process` 回调提取 DMA-BUF fd
4. 回调中构造 `FrameBuffer::DmaBuf` 并通过 `mpsc::Sender` 发送给编码器线程
5. 这完全匹配现有 pipeline 的 capture_thread → mpsc → encoder_thread 模式

此方案零改动现有 pipeline，纯新增一个线程 + 通道。

---

### PipeWire 流接收

```
pw_init() → pw_main_loop → pw_context → pw_core (连接)
  → pw_stream_connect(PW_DIRECTION_INPUT, PW_ID_ANY, ...)
  → 回调 on_process():
      pw_buffer → spa_buffer → spa_meta (header) + spa_data[]
      spa_data[0].type == SPA_DATA_DmaBuf → fd, modifier, stride
```

**包选择**：`pipewire` crate（官方 PipeWire Rust 绑定，v0.10，1M+ 下载量，活跃维护）。

注意 crates.io 上的名字是 `pipewire` 而非 `pipewire-rs`。仓库位于 https://gitlab.freedesktop.org/pipewire/pipewire-rs 。

### VAAPI 编码

**DMA-BUF 导入方向**：PipeWire 产出的 DMA-BUF fd 需要 **导入** 为 VA surface，而非导出。

```
vaDisplay = vaGetDisplayDRM(drm_fd)
  → vaInitialize()
  → vaCreateConfig(VAProfileH264High, VAEntrypointEncSlice, ...)
  → vaCreateContext(num_surfaces=4)   // ping-pong buffer pool

对于每帧（PipeWire on_process 回调中的 DMA-BUF fd）：
  → vaCreateSurfaces + VASurfaceAttribExternalBuffers
     将 DMA-BUF fd 直接导入为 VA surface（零拷贝）
  → vaBeginPicture() → vaRenderPicture() → vaEndPicture()
  → vaSyncSurface() → vaGetEncodedBits() → 输出 H.264 NALs
```

`VASurfaceAttribExternalBuffers` 使用示例（概念代码）：
```c
VASurfaceAttribExternalBuffers attr = {0};
attr.pixel_format = VA_FOURCC_NV12;
attr.width = width;
attr.height = height;
attr.data_size = stride * height;
attr.num_planes = 2;
attr.buffers = &dma_buf_fd;
attr.num_buffers = 1;
attr.flags = VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME;

VASurfaceAttrib attrib;
attrib.type = VASurfaceAttribExternalBuffers;
attrib.value.type = VAGenericValueTypePointer;
attrib.value.value.p = &attr;

vaCreateSurfaces(va_dpy, VA_RT_FORMAT_YUV420, width, height,
                 &surface, 1, &attrib, 1);
```

**版本要求**：
- libva ≥ 2.6 — `VASurfaceAttribExternalBuffers` 可用
- Mesa VAAPI driver ≥ 21.0 — AMD 稳定 DMA-BUF 导入
- intel-media-driver ≥ 20.0 — Intel 稳定
- Kernel Linux ≥ 5.4

**Intel vs AMD 差异**：
| 方面 | Intel (iHD driver) | AMD (mesa) |
|------|--------------------|------------|
| VASurfaceAttribExternalBuffers | ✅ VADRTYPE_ALLOCED | ✅ VADRTYPE_DISPLAY |
| 首选格式 | NV12 | NV12 |
| 已知问题 | 无 | mesa < 21.0 有 DMA-BUF bug |

**NVIDIA VAAPI**：`nvidia-vaapi-driver` 支持 DMA-BUF 导入，但优选 NVENC 原生路径。

**FFI 方案**：
- 手写薄 FFI 层，只封装 `libva.so` + `libva-drm.so` 的编码用子集
  - ~20 个函数，struct 布局参考 `va/va.h`
  - `vaGetDisplayDRM`, `vaInitialize`, `vaCreateConfig`, `vaCreateSurfaces`, `vaBeginPicture`, `vaRenderPicture`, `vaEndPicture`, `vaSyncSurface`, `vaGetEncodedBits`, `vaDestroySurfaces`, `vaTerminate`

### X11 兜底

```
检测 DISPLAY 环境变量 + 无 WAYLAND_DISPLAY → X11 模式

XCompositeNameWindowPixmap(dpy, window) → Pixmap
  → XShmGetImage(dpy, pixmap, image, ...) → CPU 像素
  → 通过 vaPutSurface/vaDeriveImage 上传到 VA surface
  → VASurfaceAttribExternalBuffers 导入 → VAAPI 编码
```

**FrameBufferHandle**：X11 路径走 CPU 像素，使用 `CpuBuffer` variant。

**延迟预算**：

| 步骤 | 时间 (1080p) |
|------|-------------|
| XShmGetImage (GPU → SHM CPU readback) | 1-3 ms |
| CPU → GPU 上传 | 0.5-2 ms |
| X11 round-trip 开销 | 0.2-0.5 ms |
| **总计** | **~2-6 ms** |

对于 fallback 路径是可接受的。

**注意**：
- XComposite 需要启用 compositor（无 compositor 时降级到 `XGetImage` 直接捕获）
- 窗口移动/缩放时需重新调用 `XCompositeNameWindowPixmap`
- Rust 包用 `x11rb`（选 `x11rb` 而非 `xcb`，前者更 Rust-idiomatic）

### 系统依赖

| 发行版 | 依赖包 |
|--------|--------|
| Fedora | `pipewire-devel`, `libva-devel`, `libva-utils`, `mesa-libva`, `dbus-devel` |
| Ubuntu/Debian | `libpipewire-0.3-dev`, `libva-dev`, `libva-drm2`, `libdbus-1-dev` |
| Arch | `pipewire`, `libva`, `libva-mesa-driver` |

运行时可选依赖：
- `xdg-desktop-portal` + 后端（`xdg-desktop-portal-gtk` / `xdg-desktop-portal-kde`）
- NVIDIA 卡：`nvidia-vaapi-driver` 或 NVENC

---

## Windows 方案（概述）

### DXGI Desktop Duplication API

```
CreateDXGIFactory1 → EnumAdapters → EnumOutputs → DuplicateOutput
  → AcquireNextFrame → IDXGIResource → shared handle
  → OpenSharedHandle → D3D11 Texture2D
  → 传给 NVENC/AMF 编码器
```

**窗口裁剪**：DDA 捕获的是整个显示器。需要：
1. `DwmGetWindowBounds` 或 `GetWindowRect` 获取目标窗口位置
2. 从 full-screen frame 中裁剪对应区域
3. 传给编码器时更新裁剪区域（窗口移动时）

**Rust 包**：`windows` crate（Microsoft 官方绑定，含 `Graphics.DirectX.Direct3D11` + `Graphics.Dxgi` 命名空间）。

**帧生命周期**：OBS/Chrome 等使用的标准模式——
```
AcquireNextFrame() → IDXGIResource
  → OpenSharedResource(&mut ID3D11Texture2D)  // 创建 texture 的独立 COM 引用
  → ReleaseFrame()                             // 立即释放 DDA 帧队列跟踪
  → 将 ID3D11Texture2D 传给 encoder（COM 引用计数独立存在）
```
`ReleaseFrame()` 后 texture 仍然有效。FrameBuffer Drop 时 COM 接口自动
Release，不需要特殊的 release 回调。

### NVENC 编码

```
NvEncodeAPIFFI → 初始化编码器会话
  → NvEncRegisterResource(NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX) 以共享 texture handle
  → NvEncEncodePicture() → 输出 H.264
```

AMF（AMD）作为备选。

---

## 模块目录结构

```
src/
├── main.rs                   // 跨平台 CLI 解析 + 启动 pipeline
├── pipeline.rs               // Capture → Encode → WebRTC 管线编排
├── lib.rs                    // 暴露 benchmark 接口
├── capture/
│   ├── mod.rs                // ScreenCapture trait + FrameBuffer 定义
│   ├── linux.rs              // cfg(target_os = "linux")
│   │   ├── portal.rs         // D-Bus xdg-desktop-portal 交互
│   │   ├── pipewire.rs       // PipeWire stream 接收
│   │   └── x11.rs            // X11 fallback
│   ├── windows.rs            // cfg(target_os = "windows")
│   │   └── dxgi.rs           // DXGI Desktop Duplication
│   └── macos.rs              // cfg(target_os = "macos")
│       └── sck.rs            // 现有 ScreenCaptureKit 代码（从原 capture.rs 移入）
├── encoder/
│   ├── mod.rs                // HardwareEncoder trait + #[cfg] 平台模块
│   ├── linux.rs              // VAAPIEncoder + NvencEncoder
│   ├── windows.rs            // NvencEncoder + AmfEncoder
│   └── macos.rs              // 现有 VtEncoder
├── webrtc.rs                 // 跨平台，不变
├── server.rs                 // 跨平台，不变
└── update_checker.rs         // 跨平台，不变
```

**注意**：`capture.rs` 不再在根目录存在。原有的 macOS `CaptureSession` 移入 `capture/sck.rs`。根目录 `capture/` 作为模块目录，`capture/mod.rs` 定义 trait 并 `pub mod macos;` 等。

### Cargo.toml 条件依赖

所有 macOS 专用依赖（`screencapturekit`、`videotoolbox`、`apple-cf`）**必须**放在
`[target.'cfg(target_os = "macos")']` 下，否则 Linux/Windows 会因 unsupported platform 报错。

```toml
# ── 跨平台通用依赖 ──
[dependencies]
clap = { version = "4", features = ["derive"] }
tiny_http = "0.12"
ctrlc = "3.4"
bytes = "1"
serde_json = "1"
serde = { version = "1", features = ["derive"] }
tokio = { version = "1", features = ["rt", "sync", "macros", "time"] }
rustrtc = "0.3"
rustls = { version = "0.23", features = ["ring"] }
qrcode = "0.14"
ureq = "2"
spin_sleep = "1"
static_assertions = "1"

# ── macOS ──
[target.'cfg(target_os = "macos")'.dependencies]
screencapturekit = { version = "8", features = ["macos_14_0"] }
videotoolbox = "0.18"
apple-cf = { version = "0.9", default-features = false, features = ["iosurface"] }

# ── Linux ──
[target.'cfg(target_os = "linux")'.dependencies]
pipewire = "0.10"
zbus = { version = "5", default-features = false, features = ["tokio"] }
drm-fourcc = "2"
x11rb = { version = "0.13", features = ["shm", "composite"] }
libc = "0.2"
# VAAPI/NVENC：手写薄 FFI 层，不依赖外部 crate

# ── Windows ──
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.62", features = [
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Direct3D11",
    "Win32_UI_WindowsAndMessaging",
    "Win32_Foundation",
] }
```

**关键变更**：
- macOS 依赖已全部 `cfg(target_os)` 包裹
- `zbus` 明确启用 `tokio` feature
- 去除了 `xcb` crate，只使用 `x11rb`
- 新增 `drm-fourcc`（DMA-BUF 格式常量）和 `libc`（fd 操作）
- VAAPI/NVENC 通过手写 FFI 实现，不依赖社区封装

---

## 实施计划

### 阶段 1 — 基础架构（不涉及具体平台）

1. 提取 `ScreenCapture` trait + `FrameBuffer` 枚举（lib.rs）
2. 提取 `HardwareEncoder` trait（新文件）
3. 调整 `main.rs` 管线调用 trait 接口（此时 macOS 实现包装现有代码）
4. 保持 `cargo build` 通过（macOS 仍是唯一目标）
5. 添加条件编译 `cfg_attr` 模块框架

### 阶段 2 — Linux PipeWire + VAAPI

1. 实现 Portal D-Bus 交互（`zbus`）
2. 实现 PipeWire stream 接收（`pipewire-rs`）
3. 实现 DMA-BUF → FrameBuffer
4. 实现 VAAPI 编码器（FFI 封装）
5. 集成到主管线
6. 添加 X11 fallback（`x11rb` + XShm）
7. Linux 编译 + 全流程测试

### 阶段 3 — Windows DXGI + NVENC

1. 实现 DXGI DDA 捕获（`windows` crate）
2. 实现窗口裁剪（DwmGetWindowBounds）
3. 实现 NVENC 编码（FFI）
4. 集成到主管线
5. Windows 编译 + 全流程测试

---

## 错误处理策略

| 场景 | 处理方式 |
|------|----------|
| Linux 上无 PipeWire | 降级到 X11（老桌面）或报错退出 |
| Linux 上无 VAAPI | 检查 NVENC 备用，均不可用时报错 |
| Windows 上无 DXGI（Win < 8） | 报错，要求 Windows 8+ |
| Windows 上无 NVIDIA GPU | 检查 AMF（AMD GPU），均不可用时报错 |
| 用户未授权屏幕捕获 | 提系统级权限提示，同现有 macOS 模式 |
| Portal 选择器被取消 | 退出并提示用户重试 |

---

## 跨平台复用

| 模块 | 复用方式 |
|------|----------|
| H.264 RTP 包化（FU-A） | 完全复用（已有 `webrtc.rs`） |
| WebRTC PeerConnection | 完全复用（`rustrtc` 跨平台） |
| HTTP 服务器 | 完全复用（`tiny_http` 跨平台） |
| CLI 参数解析 | 完全复用（`clap` 跨平台） |
| Update checker | 完全复用 |
| SPS/PPS 提取 | 内核复用（NAL 格式标准） |
