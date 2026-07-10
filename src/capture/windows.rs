//! Windows screen capture via DXGI Desktop Duplication API.
//!
//! Uses IDXGIOutputDuplication to acquire frames, then creates a shared
//! D3D11 texture that can be passed to the NVENC encoder.
//!
//! Frame lifecycle (OBS standard pattern):
//!   1. AcquireNextFrame → IDXGIResource
//!   2. OpenSharedResource → ID3D11Texture2D (independent COM ref)
//!   3. ReleaseFrame()     ← release DDA tracking immediately
//!   4. Pass ID3D11Texture2D to encoder (ref-counted, stays alive)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{CaptureResult, FrameBuffer, FrameBufferHandle, ScreenCapture};

/// Windows screen capture via DXGI Desktop Duplication.
pub struct WindowsCapture {
    width: u32,
    height: u32,
    fps: u32,
    window_id: Option<u32>,
    worker_thread: Option<thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

impl ScreenCapture for WindowsCapture {
    type Options = (u32, u32, u32, Option<u32>); // (width, height, fps, window_id)

    fn create(options: Self::Options) -> CaptureResult<Self> {
        Ok(WindowsCapture {
            width: options.0,
            height: options.1,
            fps: options.2,
            window_id: options.3,
            worker_thread: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    fn start<F>(&mut self, _on_frame: F) -> CaptureResult<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static,
    {
        // Full implementation requires:
        //   1. CreateDXGIFactory1 → EnumAdapters → EnumOutputs
        //   2. DuplicateOutput → IDXGIOutputDuplication
        //   3. Loop: AcquireNextFrame → IDXGIResource
        //   4. OpenSharedResource → ID3D11Texture2D
        //   5. ReleaseFrame()
        //   6. Create FrameBuffer with handle → encode
        //   7. If window_id set: clip to window bounds via DwmGetWindowBounds
        //
        // This requires a Windows environment with D3D11 capable GPU.
        Err("Windows DXGI capture not yet implemented".into())
    }

    fn stop(&mut self) -> CaptureResult<()> {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker_thread.take() {
            let _ = h.join();
        }
        Ok(())
    }
}
