//! Linux screen capture — Wayland (PipeWire) primary, X11 fallback.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use super::{CaptureResult, FrameBuffer, ScreenCapture};

mod portal;
mod pipewire;

/// Linux screen capture implementation.
///
/// Auto-detects the display server:
/// - `WAYLAND_DISPLAY` set → Wayland (PipeWire via xdg-desktop-portal)
/// - Only `DISPLAY` set → X11 fallback (via XComposite + XShm, future)
pub struct LinuxCapture {
    width: u32,
    height: u32,
    fps: u32,
    worker_thread: Option<thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

impl ScreenCapture for LinuxCapture {
    type Options = (u32, u32, u32); // (width, height, fps)

    fn create(options: Self::Options) -> CaptureResult<Self> {
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
        if !is_wayland {
            let has_display = std::env::var("DISPLAY").is_ok();
            if !has_display {
                return Err("Neither WAYLAND_DISPLAY nor DISPLAY set — no display server detected".into());
            }
            return Err("X11 capture not yet implemented; run under Wayland".into());
        }
        Ok(LinuxCapture {
            width: options.0,
            height: options.1,
            fps: options.2,
            worker_thread: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    fn start<F>(&mut self, _on_frame: F) -> CaptureResult<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static,
    {
        // Full implementation requires:
        //   1. Tokio runtime → zbus Portal D-Bus session
        //   2. PipeWire main-loop thread → DMA-BUF fd extraction
        //   3. Channel bridge between Portal and PipeWire threads
        //
        // This requires a running Wayland + PipeWire environment to test.
        // Placeholder returns a clear error until the full integration
        // is wired on an actual Linux machine.
        Err("Linux capture start not yet implemented — needs PipeWire + Portal runtime".into())
    }

    fn stop(&mut self) -> CaptureResult<()> {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.worker_thread.take() {
            let _ = h.join();
        }
        Ok(())
    }
}
