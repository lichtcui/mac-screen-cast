use std::error::Error;
use std::time::Instant;

/// Convenience alias for fallible capture operations.
pub type CaptureResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

// ── Re-export from legacy module (will be absorbed into macos.rs in Task 3) ──
pub mod old;
pub use old::CaptureSession;

// ── Platform frame buffer ──

/// 不透明的平台帧缓冲区，零拷贝传给编码器
pub struct FrameBuffer {
    pub handle: FrameBufferHandle,
    pub width: u32,
    pub height: u32,
    pub pts: u64,
    pub timescale: u32,
    pub cap_time: Instant,
}

/// 平台特定的帧缓冲区句柄
pub enum FrameBufferHandle {
    /// macOS: IOSurface 引用（Clone 时 CFRetain，Drop 时 CFRelease）
    #[cfg(target_os = "macos")]
    IOSurface(apple_cf::iosurface::IOSurface),
    /// Linux (Wayland): DMA-BUF fd
    #[cfg(target_os = "linux")]
    DmaBuf {
        fd: std::os::fd::OwnedFd,
        modifier: u64,
        drm_format: u32,
    },
    /// Linux (X11): CPU 端共享内存
    #[cfg(target_os = "linux")]
    CpuBuffer {
        ptr: *mut u8,
        size: usize,
        fd: std::os::fd::OwnedFd,
    },
    /// Windows: D3D11 Texture2D COM 接口
    #[cfg(target_os = "windows")]
    D3D11Texture(*mut std::ffi::c_void),
}

// SAFETY: 各平台资源类型都是线程安全的
#[cfg(target_os = "macos")]
unsafe impl Send for FrameBufferHandle {}
#[cfg(target_os = "linux")]
unsafe impl Send for FrameBufferHandle {}
#[cfg(target_os = "windows")]
unsafe impl Send for FrameBufferHandle {}
unsafe impl Send for FrameBuffer {}

/// 每个平台需要提供的屏幕捕获能力
pub trait ScreenCapture: Send {
    type Options;

    /// 创建捕获会话（获取窗口/显示器选择权限）
    fn create(options: Self::Options) -> CaptureResult<Self>
    where
        Self: Sized;

    /// 开始捕获，通过回调投递帧
    fn start<F>(&mut self, on_frame: F) -> CaptureResult<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static;

    /// 停止捕获
    fn stop(&mut self) -> CaptureResult<()>;
}

// ── Platform module stubs (will be filled in subsequent tasks) ──

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod linux;
