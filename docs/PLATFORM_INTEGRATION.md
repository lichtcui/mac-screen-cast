# 平台集成指南

当前代码在 macOS 上完整可用，Linux 和 Windows 需要在对就平台上填充实现体。

---

## Linux 集成

需要一台运行 Wayland + PipeWire 的 Linux 机器（推荐 Fedora 40+ 或 Ubuntu 24.04+）。

### 准备工作

```bash
# 安装系统依赖
sudo dnf install pipewire-devel libva-devel libva-utils mesa-libva dbus-devel
# 或 (Ubuntu/Debian)
sudo apt install libpipewire-0.3-dev libva-dev libva-drm2 libdbus-1-dev

# 确认分支
git checkout feat/cross-platform-linux

# 尝试编译
cargo build
```

### 需要填充的文件

按此顺序填充：

#### 1. `src/capture/linux/portal.rs` — Portal D-Bus 交互

- [ ] 实现 `create_portal_session()` 函数
- [ ] 使用 `zbus` 调用 D-Bus `org.freedesktop.portal.ScreenCast` 接口
- [ ] CreateSession → SelectSources → Start 流程
- [ ] 提取 PipeWire node ID + FD

参考：https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.ScreenCast.html

#### 2. `src/capture/linux/pipewire.rs` — PipeWire 流接收

- [ ] 实现 `PipeWireCapture::new()` 函数
- [ ] `pw_init()` → pw_main_loop → pw_context → pw_core 连接
- [ ] `pw_stream_connect(Input, pw_node_id, ...)` 注册
- [ ] `on_process` 回调中提取 DMA-BUF fd
- [ ] 构造 `FrameBufferHandle::DmaBuf`

参考：官方 PipeWire 示例（`src/examples/`）和 `pipewire` crate API

#### 3. `src/encoder/linux_vaapi.rs` — VAAPI 编码器

- [ ] 实现 `VaapiEncoder::new()` — vaGetDisplayDRM → vaInitialize → vaCreateConfig → vaCreateContext
- [ ] 实现 `VaapiEncoder::encode()` — DMA-BUF fd 导入 → vaBeginPicture → vaRenderPicture → vaEndPicture → vaSyncSurface → vaGetEncodedBits
- [ ] 验证 `#[link(name = "va")]` 和 `#[link(name = "va-drm")]` 链接正确

#### 4. `src/capture/linux/x11.rs` — X11 fallback（新建文件）

- [ ] 创建 `src/capture/linux/x11.rs`
- [ ] 在 `src/capture/linux/mod.rs` 中添加 `mod x11;` 和 `pub use x11::X11Capture;`
- [ ] 实现 XCompositeNameWindowPixmap → XShmGetImage 流程
- [ ] CPU 像素通过 vaPutSurface/vaDeriveImage 上传到 VA surface
- [ ] 构造 `FrameBufferHandle::CpuBuffer`

#### 5. 集成 + 端到端测试

- [ ] 更新 `src/capture/linux/mod.rs` 中的 `LinuxCapture::start()` 串联 Portal → PipeWire 管线
- [ ] 添加 DRM 设备打开逻辑（`/dev/dri/renderD128`）
- [ ] `cargo run -- --list` 窗口列表
- [ ] `cargo run -- -w <window_id> --width 1280 --fps 30` 流媒体测试
- [ ] X11 环境降级测试：`env WAYLAND_DISPLAY= DISPLAY=:0 cargo run -- --list`

---

## Windows 集成

需要一台 Windows 10/11 机器（建议有 NVIDIA GPU 用于 NVENC）。

### 准备工作

```powershell
# Rust 已安装
# 确认分支
git checkout feat/cross-platform-linux

# 尝试编译
cargo build
```

### 需要填充的文件

#### 1. `src/capture/windows.rs` — DXGI DDA 捕获

- [ ] 实现 `WindowsCapture::start()` 函数体
- [ ] `CreateDXGIFactory1` → `EnumAdapters` → `EnumOutputs` → `DuplicateOutput`
- [ ] 捕获循环：`AcquireNextFrame` → `IDXGIResource` → `OpenSharedResource` → `ID3D11Texture2D` → `ReleaseFrame()`
- [ ] 窗口裁剪：`DwmGetWindowBounds` 或 `GetWindowRect` 裁剪全屏帧
- [ ] 构造 `FrameBufferHandle::D3D11Texture`

参考：OBS Studio 源码或 Microsoft DXGI DDA 文档

#### 2. `src/encoder/windows.rs` — NVENC 编码器

- [ ] 实现 `NvencEncoder::new()` — 加载 nvEncodeAPI → 初始化编码会话
- [ ] 实现 `NvencEncoder::encode()` — 注册 D3D11 纹理 → NvEncEncodePicture → 输出 H.264

参考：NVIDIA Video Codec SDK 示例

#### 3. 集成 + 端到端测试

- [ ] 更新 `pipeline.rs` 添加 Windows pipeline 入口函数
- [ ] `cargo run -- --list` 窗口列表
- [ ] `cargo run -- -w <window_id> --width 1280 --fps 30` 流媒体测试

---

## 提示

- 每个文件的存根中都包含详细的 TODO 注释和步骤说明
- FFI 绑定声明（如 `linux_vaapi.rs` 中的 extern 块）已经写好，只需填充调用逻辑
- 所有代码在 `#[cfg(target_os = "...")]` 保护下，不会影响其他平台
- 填充后运行 `cargo test` 确保现有测试不回归
