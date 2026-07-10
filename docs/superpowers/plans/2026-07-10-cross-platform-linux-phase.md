# 跨平台屏幕捕获 — Phase 1 架构 + Phase 2 Linux 实现计划

> **面向 AI 代理的工作者：** 必需子技能：使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans 逐任务实现此计划。步骤使用复选框（`- [ ]`）语法来跟踪进度。

**目标：** 为 mac-screen-cast 提取跨平台 ScreenCapture/HardwareEncoder traits（Phase 1），并实现 Linux PipeWire → VAAPI 零拷贝捕获管线（Phase 2），保持 macOS 功能不变。

**架构：**
- Phase 1: 将现有 `capture.rs`/`h264.rs` 包装到 trait 接口后，新增 `capture/`、`encoder/` 模块目录，通过 `#[cfg(target_os)]` 条件编译平台代码
- Phase 2: Linux 上通过 `zbus`（D-Bus）→ `pipewire` crate（DMA-BUF）→ VAAPI FFI 实现零拷贝捕获编码管线
- 所有跨平台代码（`webrtc.rs`、`server.rs`）不变；`main.rs` 管线编排抽入 `pipeline.rs`

**技术栈：** Rust 2021 edition, `screencapturekit` 8 / `videotoolbox` 0.18 (macOS), `pipewire` 0.10 / `zbus` 5 / `x11rb` 0.13 (Linux), VAAPI thin FFI

**前提条件：** 设计文档已批准并提交 (`docs/superpowers/specs/2026-07-10-cross-platform-screen-capture-design.md`)

---

## 文件结构

### 新/改文件总览

```
src/
├── capture/                          # [新建] 模块目录
│   ├── mod.rs                        # [新建] ScreenCapture trait + FrameBuffer
│   ├── macos.rs                      # [新建] macOS 实现（包装现有 capture.rs 逻辑）
│   └── linux.rs                      # [新建] Linux 实现（Portal + PipeWire + X11）
│       ├── portal.rs                 # [新建] zbus D-Bus xdg-desktop-portal 交互
│       └── pipewire.rs               # [新建] PipeWire stream 接收 + DMA-BUF
├── encoder/
│   ├── mod.rs                        # [新建] HardwareEncoder trait + EncodedFrame
│   ├── macos.rs                      # [新建] macOS 实现（包装现有 VtEncoder）
│   ├── linux_vaapi.rs                # [新建] VAAPI FFI 编码器
│   └── linux_nvenc.rs                # [新建] NVENC FFI 编码器（备用）
├── pipeline.rs                       # [新建] 从 main.rs 抽取的管线编排
├── capture.rs                        # [删除] 内容移入 capture/macos.rs
├── main.rs                           # [修改] CLI 解析 → 启动 pipeline
├── lib.rs                            # [修改] 暴露新模块供 benchmark
├── webrtc.rs                         # [不变]
├── server.rs                         # [不变]
└── update_checker.rs                 # [不变]
docs/
└── resources/
    └── player.html                   # [不变]
Cargo.toml                            # [修改] 条件编译依赖
build.rs                              # [修改] cfg(target_os) 保护
```

---

## 任务拆分

### 任务 1：Cargo.toml + build.rs 条件编译适配

**文件：**
- 修改：`Cargo.toml`
- 修改：`build.rs`

- [ ] **步骤 1：将 macOS 专有依赖移入 `[target.'cfg(target_os = "macos")']`**

当前 `Cargo.toml` 中 `screencapturekit`、`videotoolbox`、`apple-cf` 是无条件依赖。移到条件段下：

```toml
# ── macOS ──
[target.'cfg(target_os = "macos")'.dependencies]
screencapturekit = { version = "8", features = ["macos_14_0"] }
videotoolbox = "0.18"
apple-cf = { version = "0.9", default-features = false, features = ["iosurface"] }
```

- [ ] **步骤 2：添加 Linux 条件依赖**

```toml
# ── Linux ──
[target.'cfg(target_os = "linux")'.dependencies]
pipewire = "0.10"
zbus = { version = "5", default-features = false, features = ["tokio"] }
drm-fourcc = "2"
x11rb = { version = "0.13", features = ["shm", "composite"] }
libc = "0.2"
```

- [ ] **步骤 3：为 workspace-level 依赖保留跨平台公共依赖**

确认 `rustrtc`、`rustls`、`tokio`、`clap`、`tiny_http` 等在 `[dependencies]`（无条件）下。`spin_sleep` 仅 macOS 用，可移入 macOS 条件段。

- [ ] **步骤 4：build.rs 添加 cfg 保护**

```rust
fn main() {
    let target = std::env::var("TARGET").expect("Cargo always sets TARGET in build.rs");
    if target.contains("-apple-") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
```

Swift rpath 只在 macOS 需要，Linux/Windows 不应添加。

- [ ] **步骤 5：编译验证**

运行：`cargo build`
预期：macOS 上正常编译通过，macOS 依赖正常引入。

运行：`cargo check --target x86_64-unknown-linux-gnu 2>&1 || true`
预期：Linux 编译错误应为缺少平台实现函数（正常，还未实现），而不是"unsupported platform"编译错误。

---

### 任务 2：ScreenCapture trait + FrameBuffer 定义

**文件：**
- 创建：`src/capture/mod.rs`

- [ ] **步骤 1：编写 FrameBuffer 和 FrameBufferHandle 的定义**

```rust
use std::time::Instant;

/// 平台帧缓冲区句柄
#[cfg(target_os = "macos")]
pub type PlatformFrameHandle = crate::ffi::IOSurfaceRef;

/// 不透明的平台帧缓冲区
pub struct FrameBuffer {
    pub handle: FrameBufferHandle,
    pub width: u32,
    pub height: u32,
    pub pts: u64,
    pub timescale: u32,
    pub cap_time: Instant,
}

pub enum FrameBufferHandle {
    #[cfg(target_os = "macos")]
    IOSurface(crate::ffi::IOSurfaceRef),
    #[cfg(target_os = "linux")]
    DmaBuf { fd: std::os::fd::OwnedFd, modifier: u64, drm_format: u32 },
    #[cfg(target_os = "windows")]
    D3D11Texture(*mut std::ffi::c_void),
}

// SAFETY: 每个平台的资源类型都是线程安全的
// - IOSurfaceRef: CF 对象，线程安全
// - OwnedFd: Rust 标准库的 Send 类型
#[cfg(target_os = "macos")]
unsafe impl Send for FrameBufferHandle {}
#[cfg(target_os = "linux")]
unsafe impl Send for FrameBufferHandle {}
unsafe impl Send for FrameBuffer {}
```

- [ ] **步骤 2：编写 ScreenCapture trait**

```rust
/// 每个平台需要提供的屏幕捕获能力
pub trait ScreenCapture: Send {
    type Options;

    /// 创建捕获会话（获取窗口/显示器选择权限）
    fn create(options: Self::Options) -> Result<Self>
    where
        Self: Sized;

    /// 开始捕获，通过回调投递帧
    fn start<F>(&mut self, on_frame: F) -> Result<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static;

    /// 停止捕获
    fn stop(&mut self) -> Result<()>;
}
```

设计选择：`stop()` 是同步的（non-async），因为 macOS 和 Linux 的停止都是同步操作（设置 atomic、join 线程）。

- [ ] **步骤 3：添加模块级 re-export**

```rust
// capture/mod.rs
mod macos;
mod linux;

pub use macos::MacosCapture;
pub use linux::LinuxCapture;
```

- [ ] **步骤 4：运行 `cargo build` 验证模块结构**

此时 `pub mod capture;` 在 `main.rs` 会报缺少 `macos.rs` 和 `linux.rs` — 预期行为。创建两个空文件占位先通过编译。

---

### 任务 3：macOS ScreenCapture 封装

**文件：**
- 创建：`src/capture/macos.rs`
- 删除：`src/capture.rs`

- [ ] **步骤 1：创建 macos.rs 包装现有 CaptureSession**

将现有 `src/capture.rs` 的 `CaptureSession` 完整移入 `src/capture/macos.rs`，实现 `ScreenCapture` trait：

```rust
use super::{FrameBuffer, FrameBufferHandle, ScreenCapture};
use screencapturekit::prelude::*;

pub struct MacosCapture {
    session: Option<CaptureSession>,
    // 保存 capture 相关配置
    filter: SCContentFilter,
    out_w: u32,
    out_h: u32,
    fps: u32,
}

impl ScreenCapture for MacosCapture {
    type Options = (SCContentFilter, u32, u32, u32); // (filter, w, h, fps)

    fn create(options: Self::Options) -> Result<Self> {
        Ok(MacosCapture {
            session: None,
            filter: options.0,
            out_w: options.1,
            out_h: options.2,
            fps: options.3,
        })
    }

    fn start<F>(&mut self, on_frame: F) -> Result<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static,
    {
        // 将 on_frame 包装为 CMSampleBuffer 回调
        let session = CaptureSession::new(
            self.filter.clone(),
            self.out_w,
            self.out_h,
            self.fps,
            move |sample: CMSampleBuffer, _| {
                if let Some(pb) = sample.image_buffer() {
                    if let Some(surface) = pb.io_surface() {
                        // 构造 FrameBuffer
                        let fb = FrameBuffer {
                            handle: FrameBufferHandle::IOSurface(surface),
                            width: self.out_w,
                            height: self.out_h,
                            pts: 0, // 由调用者填充
                            timescale: 1000,
                            cap_time: Instant::now(),
                        };
                        on_frame(fb);
                    }
                }
            },
        );
        self.session = Some(session);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        if let Some(mut s) = self.session.take() {
            s.stop();
        }
        Ok(())
    }
}
```

注意：`CaptureSession::new` 的闭包需要 `self` 的所有权。实际实现时需让闭包捕获 `out_w`/`out_h` 值的拷贝而非 `&self` 引用。

- [ ] **步骤 2：删除旧的 `src/capture.rs`**

删除后更新 `main.rs` 中的 `mod capture;` — 改为 `mod capture;` 指向目录（新增 `capture/mod.rs`），这是 Rust 2021 支持的模块结构。

- [ ] **步骤 3：编译验证**

运行：`cargo build`
预期：macOS 编译通过，capture 模块正常链接。

- [ ] **步骤 4：功能验证**

运行：`cargo test`
预期：现有单元测试通过。

---

### 任务 4：HardwareEncoder trait + 封装

**文件：**
- 创建：`src/encoder/mod.rs`
- 创建：`src/encoder/macos.rs`

- [ ] **步骤 1：定义 HardwareEncoder trait + EncodedFrame**

```rust
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
    pub pts: u64,
}

pub trait HardwareEncoder: Send {
    type Options;

    fn new(width: u32, height: u32, fps: u32, options: Self::Options) -> Result<Self>
    where
        Self: Sized;

    /// 编码一帧
    fn encode(&self, frame: &super::capture::FrameBuffer) -> Result<EncodedFrame>;
}
```

- [ ] **步骤 2：封装 VtEncoder 到 `encoder/macos.rs`**

```rust
pub struct MacosEncoder {
    inner: VtEncoder,
    out_w: u32,
    out_h: u32,
}

impl HardwareEncoder for MacosEncoder {
    type Options = ();

    fn new(width: u32, height: u32, fps: u32, _options: Self::Options) -> Result<Self> {
        Ok(MacosEncoder {
            inner: VtEncoder::new(width, height, fps)?,
            out_w: width,
            out_h: height,
        })
    }

    fn encode(&self, frame: &super::capture::FrameBuffer) -> Result<EncodedFrame> {
        match &frame.handle {
            FrameBufferHandle::IOSurface(surface) => {
                let h264 = self.inner.encode(surface, frame.pts, fps)?;
                Ok(EncodedFrame {
                    data: h264.data,
                    is_keyframe: h264.is_keyframe,
                    sps: h264.sps,
                    pps: h264.pps,
                    pts: frame.pts,
                })
            }
            _ => Err(anyhow::anyhow!("unexpected frame handle type")),
        }
    }
}
```

注意：当前 `VtEncoder::encode` 签名是 `encode(&self, surface: &IOSurfaceRef, pts: u64, fps: i32)`，需要确认实际参数匹配。如有差异则调整包装代码。

- [ ] **步骤 3：编译验证**

运行：`cargo build`
预期：通过。

---

### 任务 5：抽取 pipeline.rs（从 main.rs）

**文件：**
- 创建：`src/pipeline.rs`
- 修改：`src/main.rs`

- [ ] **步骤 1：从 main.rs 抽取 run_pipeline 函数到 pipeline.rs**

将 `run_pipeline()`、`spawn_encoder_thread()`、`CaptureSetup`、`RawFrame` 从 `main.rs` 移入 `pipeline.rs`。

```rust
pub struct PipelineConfig {
    pub out_w: u32,
    pub out_h: u32,
    pub fps: u32,
    pub port: u16,
    pub window_name: String,
}

pub fn run_pipeline<C: ScreenCapture, E: HardwareEncoder>(
    capture: &mut C,
    encoder: Arc<E>,
    config: PipelineConfig,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    // ... 通道设置、线程启动、帧循环 ...
}
```

- [ ] **步骤 2：简化 main.rs**

```rust
fn main() {
    // CLI 解析、窗口选择、CaptureSetup 计算...
    // → 调用 pipeline::run_pipeline()
}
```

`main.rs` 只负责解析 CLI、构建平台特定的 `Capture` 和 `Encoder`、创建 `SharedState`、启动 `pipeline::run_pipeline()`。

- [ ] **步骤 3：编译+运行验证**

运行：`cargo build` → 通过
运行：`cargo test` → 现有测试通过
运行：`cargo run -- --list` → 窗口列表正常
运行：`cargo run -- -w <id>` → 流媒体功能正常（全回归）

---

### 任务 6：Linux Portal 交互（zbus D-Bus）

**文件：**
- 创建：`src/capture/linux/`（目录）
- 创建：`src/capture/linux/mod.rs` — 入口，检测 Wayland/X11 并分发
- 创建：`src/capture/linux/portal.rs` — xdg-desktop-portal D-Bus 交互
- 创建：`src/capture/linux/pipewire.rs` — PipeWire stream + DMA-BUF

先调整模块结构：`capture/linux.rs` → `capture/linux/mod.rs` + `capture/linux/portal.rs` + `capture/linux/pipewire.rs`

- [ ] **步骤 1：创建 Linux capture 模块入口**

`src/capture/linux/mod.rs`：
```rust
#[cfg(target_os = "linux")]
pub mod portal;
#[cfg(target_os = "linux")]
pub mod pipewire;

use std::sync::mpsc;

use super::{FrameBuffer, ScreenCapture};
use portal::PortalCapturer;
use pipewire::PipeWireStream;

pub struct LinuxCapture {
    portal: Option<PortalCapturer>,
    stream: Option<PipeWireStream>,
    width: u32,
    height: u32,
    fps: u32,
}

impl ScreenCapture for LinuxCapture {
    type Options = (u32, u32, u32, u32); // width, height, fps, window_id

    fn create(options: Self::Options) -> Result<Self> {
        // 检测运行环境：WAYLAND_DISPLAY → Wayland, DISPLAY → X11
        Ok(LinuxCapture { ... })
    }

    fn start<F>(&mut self, on_frame: F) -> Result<()>
    where F: FnMut(FrameBuffer) + Send + 'static {
        // 1. Portal D-Bus 交互（获取 PipeWire node）
        // 2. 启动 PipeWire 线程 → on_process → on_frame
        // 3. 回调中投递 FrameBuffer::DmaBuf
    }

    fn stop(&mut self) -> Result<()> {
        // 停止 PipeWire 线程 + 关闭 Portal session
    }
}
```

- [ ] **步骤 2：实现 Portal D-Bus 交互（portal.rs）**

```rust
use zbus::Connection;

/// 通过 xdg-desktop-portal 获取 PipeWire 屏幕捕获资源
pub struct PortalCapturer {
    conn: Connection,
    session_handle: String,
    pw_node_id: u32,
    pw_fd: std::os::fd::OwnedFd,
}

impl PortalCapturer {
    /// 异步执行 Portal Session 创建 + SelectSources + Start 流程
    pub async fn new(window_id: Option<u32>) -> Result<Self> {
        let conn = Connection::session().await?;

        // 1. CreateSession
        // 2. SelectSources (types: MONITOR | WINDOW)
        // 3. Start → 获取 PipeWire node ID + FD

        // zbus 调用示例：
        // let proxy = conn.call_method(
        //     Some("org.freedesktop.portal.Desktop"),
        //     "/org/freedesktop/portal/desktop",
        //     Some("org.freedesktop.portal.ScreenCast"),
        //     "CreateSession",
        //     &(options),
        // ).await?;

        // ... 处理信号流获取 session handle ...
        // ... 处理 Response 信号 ...
        // ... Start 后获取 PipeWire 参数 ...

        unimplemented!("Portal D-Bus interaction")
    }

    pub fn pw_node_id(&self) -> u32 { self.pw_node_id }
    pub fn pw_fd(&self) -> &std::os::fd::OwnedFd { &self.pw_fd }
    pub fn take_fd(self) -> std::os::fd::OwnedFd { self.pw_fd }
}
```

关键实现细节：
- D-Bus 的 handle_token / session_handle_token 配对机制
- 通过 `Request::signal` 接收用户选择结果
- Start 成功后获得的 PipeWire node ID 和 FD

- [ ] **步骤 3：实现 PipeWire 流接收（pipewire.rs）**

```rust
use pipewire as pw;

/// PipeWire stream 接收 → DMA-BUF fd 提取
pub struct PipeWireStream {
    main_loop: pw::main_loop::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PipeWireStream {
    pub fn new<F>(
        pw_node_id: u32,
        width: u32,
        height: u32,
        fps: u32,
        on_frame: F,
    ) -> Result<Self>
    where
        F: FnMut(FrameBuffer) + Send + 'static,
    {
        pw::init();

        let main_loop = pw::main_loop::MainLoop::new(None)?;
        let context = pw::context::Context::new(&main_loop)?;
        let core = context.connect(None)?;

        let stream = pw::stream::Stream::new(&core, "screen-capture", pw::properties::Properties::new())?;

        // 配置 stream
        let mut params = Vec::new();
        let param = pw::spa::pod::Pod::from(...); // 视频格式参数
        stream.connect(
            pw::stream::StreamDirection::Input,
            Some(pw_node_id),
            pw::stream::StreamFlags::AUTOCONNECT,
            &params,
        )?;

        // 注册 on_process 回调 → 提取 DMA-BUF fd → on_frame(FrameBuffer)
        // ...

        Ok(PipeWireStream { ... })
    }
}
```

- [ ] **步骤 4：编译验证**

运行：`cargo build`
预期：macOS 上 Linux 条件编译代码不编译，macOS 功能正常。

---

### 任务 7：Linux VAAPI 编码器 FFI

**文件：**
- 创建：`src/encoder/linux_vaapi.rs`
- 修改：`src/encoder/linux.rs`（引入 VAAPI 和 NVENC 模块）

- [ ] **步骤 1：VAAPI FFI bindings**

手写 `libva.so` / `libva-drm.so` 的 FFI 绑定：

```rust
#[cfg(target_os = "linux")]
mod ffi {
    use libc::{c_char, c_int, c_uint, c_void};

    pub type VADisplay = *mut c_void;
    pub type VAConfigID = c_uint;
    pub type VAContextID = c_uint;
    pub type VASurfaceID = c_uint;

    #[link(name = "va")]
    extern "C" {
        pub fn vaGetDisplayDRM(fd: c_int) -> VADisplay;
        pub fn vaInitialize(
            dpy: VADisplay,
            major: *mut c_uint,
            minor: *mut c_uint,
        ) -> c_int;
        pub fn vaTerminate(dpy: VADisplay) -> c_int;
        pub fn vaCreateConfig(
            dpy: VADisplay,
            profile: c_int,
            entrypoint: c_int,
            attribs: *mut c_void,
            num_attribs: c_int,
            config_id: *mut VAConfigID,
        ) -> c_int;
        pub fn vaCreateSurfaces(
            dpy: VADisplay,
            format: c_uint,
            width: c_uint,
            height: c_uint,
            surfaces: *mut VASurfaceID,
            num_surfaces: c_int,
            attribs: *mut c_void,
            num_attribs: c_int,
        ) -> c_int;
        pub fn vaBeginPicture(
            dpy: VADisplay,
            context: VAContextID,
            surface: VASurfaceID,
        ) -> c_int;
        pub fn vaRenderPicture(
            dpy: VADisplay,
            context: VAContextID,
            buffers: *mut c_void,
            num_buffers: c_int,
        ) -> c_int;
        pub fn vaEndPicture(dpy: VADisplay, context: VAContextID) -> c_int;
        pub fn vaSyncSurface(dpy: VADisplay, surface: VASurfaceID) -> c_int;
        pub fn vaGetEncodedBits(dpy: VADisplay, size: *mut c_uint) -> *mut c_void;
        pub fn vaCreateContext(
            dpy: VADisplay,
            config: VAConfigID,
            picture_w: c_int,
            picture_h: c_int,
            flag: c_int,
            render_targets: *mut VASurfaceID,
            num_render_targets: c_int,
            context: *mut VAContextID,
        ) -> c_int;
    }
}
```

- [ ] **步骤 2：实现 VaapiEncoder struct**

```rust
pub struct VaapiEncoder {
    display: ffi::VADisplay,
    config: ffi::VAConfigID,
    context: ffi::VAContextID,
    width: u32,
    height: u32,
    fps: i32,
    // 编码 surface pool
    surfaces: Vec<ffi::VASurfaceID>,
}

impl HardwareEncoder for VaapiEncoder {
    type Options = i32; // DRM fd

    fn new(width: u32, height: u32, fps: u32, drm_fd: Self::Options) -> Result<Self> {
        // 1. vaGetDisplayDRM(drm_fd)
        // 2. vaInitialize()
        // 3. vaCreateConfig(VAProfileH264High, VAEntrypointEncSlice)
        // 4. vaCreateContext()
        unimplemented!("VAAPI initialization")
    }

    fn encode(&self, frame: &super::capture::FrameBuffer) -> Result<EncodedFrame> {
        match &frame.handle {
            FrameBufferHandle::DmaBuf { fd, modifier, drm_format } => {
                // 1. vaCreateSurfaces + VASurfaceAttribExternalBuffers 导入 DMA-BUF fd
                // 2. vaBeginPicture → vaRenderPicture → vaEndPicture
                // 3. vaSyncSurface → vaGetEncodedBits
                unimplemented!("VAAPI encode frame")
            }
            _ => Err(anyhow::anyhow!("unexpected frame handle type for VAAPI")),
        }
    }
}
```

VASurfaceAttribExternalBuffers 结构体布局：

```rust
#[repr(C)]
struct VASurfaceAttribExternalBuffers {
    pixel_format: u32,           // VA_FOURCC_NV12
    width: u32,
    height: u32,
    data_size: u32,
    num_planes: u32,
    pitches: [u32; 4],
    offsets: [u32; 4],
    buffers: *const i32,         // DMA-BUF fd 数组
    num_buffers: u32,
    flags: u32,
    private_data: *mut c_void,
}

const VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME: u32 = 1 << 3;
const VASurfaceAttribExternalBuffers: i32 = 52; // enum 值
```

- [ ] **步骤 3：NVENC 编码器占位**

```rust
pub struct NvencEncoder;

impl HardwareEncoder for NvencEncoder {
    type Options = i32; // NV 设备索引
    // ... 仅声明，实现留待后续
}
```

- [ ] **步骤 4：编译验证（Linux 交叉编译检查）**

运行：`cargo build`（macOS）
预期：macOS 编译通过，Linux 代码不进入编译。

---

### 任务 8：Linux pipeline 集成

**文件：**
- 修改：`src/pipeline.rs` — Linux 路径的 pipeline 创建
- 修改：`src/main.rs` — Linux 上使用 LinuxCapture + VaapiEncoder

- [ ] **步骤 1：pipeline.rs 添加平台特定创建函数**

```rust
#[cfg(target_os = "linux")]
fn create_linux_pipeline(
    out_w: u32, out_h: u32, fps: u32, window_id: u32,
) -> Result<(LinuxCapture, Arc<VaapiEncoder>)> {
    // 打开 DRM 设备 /dev/dri/renderD128
    // 创建 VaapiEncoder
    // 创建 LinuxCapture（Portal + PipeWire）
}

#[cfg(target_os = "macos")]
fn create_macos_pipeline(
    out_w: u32, out_h: u32, fps: u32, filter: SCContentFilter,
) -> Result<(MacosCapture, Arc<MacosEncoder>)> {
    // 现有逻辑
}
```

- [ ] **步骤 2：main.rs 使用平台分发**

```rust
fn main() {
    let cli = Cli::parse();

    // --list 处理（跨平台，macOS 用 Swift，Linux 用 x11rb/wl-roots）

    #[cfg(target_os = "macos")]
    let (mut capture, encoder) = create_macos_pipeline(out_w, out_h, cli.fps, filter);

    #[cfg(target_os = "linux")]
    let (mut capture, encoder) = create_linux_pipeline(out_w, out_h, cli.fps, wid);

    run_pipeline(&mut capture, encoder, config, stop);
}
```

- [ ] **步骤 3：编译验证**

运行：`cargo build`（macOS）→ 通过
运行：需在 Linux 环境完整编译验证

---

### 任务 9：Linux 全流程测试

- [ ] **步骤 1：编写 Portal D-Bus 交互单元测试**

```rust
#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    #[test]
    #[ignore] // 需要 D-Bus session
    fn test_portal_session_flow() {
        // 测试 Portal CreateSession → SelectSources → Start 流程
    }
}
```

- [ ] **步骤 2：编写 VAAPI 编码器单元测试（mock）**

用 DRI 设备 `/dev/dri/renderD128` 测试 VAAPI 初始化和编码。

- [ ] **步骤 3：端到端测试（Linux）**

运行：`cargo build && cargo run -- --list`
预期：窗口列表正常显示。

运行：`cargo run -- -w <window_id> --port 8081 --width 1280 --fps 30`
预期：浏览器打开 `http://localhost:8081` 看到流画面。

- [ ] **步骤 4：X11 fallback 验证**

在 X11 环境下运行（`env WAYLAND_DISPLAY= DISPLAY=:0 cargo run -- --list`）
预期：正常降级到 X11 路径，显示窗口列表。

---

## 错误处理对照

| 场景 | 处理 | 相关任务 |
|------|------|---------|
| Linux 无 PipeWire | 降级 X11（检测 DISPLAY） | 任务 6 |
| Linux 无 VAAPI | 尝试 NVENC 备用 | 任务 7 |
| Portal 选择器被取消 | 退出并提示 | 任务 6 |
| DRM 设备不可访问 | 报错提示 -- 需要 video 组权限 | 任务 8 |
| VAAPI 编码失败 | 回退到 NVENC 或报错 | 任务 7 |

---

## 自检清单

- [ ] 所有任务覆盖设计文档中的所有需求
- [ ] 没有 "TODO"、"待定"、"补充细节" 占位符
- [ ] 代码步骤包含实际可编译的代码片段
- [ ] 类型和方法签名在任务间一致（`FrameBufferHandle` 的变体名、`HardwareEncoder::encode` 签名）
- [ ] 所有文件路径精确
- [ ] 每个步骤包含测试/验证命令和预期输出
