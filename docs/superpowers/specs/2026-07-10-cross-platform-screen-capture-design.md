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
    /// 平台特定的 GPU 资源句柄
    pub handle: FrameBufferHandle,
    pub width: u32,
    pub height: u32,
    pub pts: u64,        // 递增的时间戳
    pub timescale: u32,  // pts 的时间基准（通常 1000 或 fps）
}

pub enum FrameBufferHandle {
    #[cfg(target_os = "macos")]
    IOSurface(crate::ffi::IOSurfaceRef),
    #[cfg(target_os = "linux")]
    DmaBuf { fd: std::os::fd::OwnedFd, modifier: u64, drm_format: u32 },
    #[cfg(target_os = "windows")]
    D3D11Texture { texture: *mut c_void, keyed_mutex: *mut c_void },
}

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

**Rust D-Bus 包选择**：`zbus` 3.x（纯 Rust，异步，tokio 兼容）。

### PipeWire 流接收

```
pw_init() → pw_main_loop → pw_context → pw_core (连接)
  → pw_stream_connect(PW_DIRECTION_INPUT, PW_ID_ANY, ...)
  → 回调 on_process():
      pw_buffer → spa_buffer → spa_meta (header) + spa_data[]
      spa_data[0].type == SPA_DATA_DmaBuf → fd, modifier, stride
```

**包选择**：`pipewire-rs`（社区维护，覆盖 sys 绑定 + 高层 API）。

### VAAPI 编码

```
vaDisplay = vaGetDisplayDRM(drm_fd)  // 或 vaGetDisplay (X11)
  → vaInitialize()
  → vaCreateConfig(VAProfileH264High, VAEntrypointEncSlice, ...)
  → vaCreateContext()

对于每帧：
  → vaCreateSurfaces(VA_RT_FORMAT_YUV420, w, h)
  → vaPutSurface (走 DMA-BUF 导入：VASurfaceAttribExternalBuffers)
     或 vaExportSurfaceHandle(DMA_BUF) — 依赖 libva 版本
  → vaBeginPicture() → vaRenderPicture() → vaEndPicture()
  → vaSyncSurface() → vaGetEncodedBits() → 输出 H.264 NALs
```

**FFI 方案**：
- 优先用社区 `vaapi-rs`（如果成熟度够）
- 否则手写薄 FFI 层，只封装 `libva.so` + `libva-drm.so` 的编码用子集
  - ~20 个函数，struct 布局参考 `va/va.h`
  - `vaGetDisplayDRM`, `vaInitialize`, `vaCreateConfig`, `vaCreateSurfaces`, `vaBeginPicture`, `vaRenderPicture`, `vaEndPicture`, `vaSyncSurface`, `vaGetEncodedBits`, `vaDestroySurfaces`, `vaTerminate`

### X11 兜底

```
检测 DISPLAY 环境变量 + 无 WAYLAND_DISPLAY → X11 模式

XCompositeNameWindowPixmap(dpy, window) → Pixmap
  → XShmGetImage(dpy, pixmap, image, ...) → CPU 像素
  → 通过 libva 的 vaPutSurface/vaDeriveImage 上传到 GPU surface
```

X11 没有 DMA-BUF 零拷贝通道，但功能完整。延迟增加 ~1-2ms（CPU→GPU 上传）。

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
├── main.rs                   // 跨平台 CLI + 管线编排
├── lib.rs                    // 暴露 benchmark 接口
├── capture.rs                // (保留) 未来收拢跨平台实现
├── capture/
│   ├── mod.rs                // ScreenCapture trait + FrameBuffer
│   ├── linux.rs              // cfg(target_os = "linux")
│   │   ├── portal.rs         // D-Bus xdg-desktop-portal 交互
│   │   ├── pipewire.rs       // PipeWire stream 接收
│   │   └── x11.rs            // X11 fallback
│   ├── windows.rs            // cfg(target_os = "windows")
│   │   └── dxgi.rs           // DXGI Desktop Duplication
│   └── macos.rs              // cfg(target_os = "macos")
│       └── sck.rs            // 现有 ScreenCaptureKit 代码
├── encoder/
│   ├── mod.rs                // HardwareEncoder trait
│   ├── linux.rs              // VAAPIEncoder + NvencEncoder
│   ├── windows.rs            // NvencEncoder + AmfEncoder
│   └── macos.rs              // 现有 VtEncoder
├── webrtc.rs                 // 跨平台，不变
├── server.rs                 // 跨平台，不变
└── update_checker.rs         // 跨平台，不变
```

### Cargo.toml 条件依赖

```toml
[target.'cfg(target_os = "linux")'.dependencies]
pipewire = "0.9"
zbus = "4"
vaapi = "0.2"        # 或手写 FFI
xcb = "1"
x11rb = "0.13"

[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
    "Graphics_DirectX_Direct3D11",
    "Graphics_Dxgi",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Direct3D11",
    "Win32_UI_WindowsAndMessaging",
] }

[target.'cfg(target_os = "macos")'.dependencies]
screencapturekit = { version = "8", features = ["macos_14_0"] }
videotoolbox = "0.18"
```

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
6. 添加 X11 fallback（`xcb` + XShm）
7. Linux 编译 + 全流程测试

### 阶段 3 — Windows DXGI + NVENC

1. 实现 DXGI DDA 捕获（`windows` crate）
2. 实现窗口裁剪（Dw mGetWindowBounds）
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
