use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::SCScreenshotManager;

use super::{CaptureResult, FrameBuffer, FrameBufferHandle, ScreenCapture};

/// Polling-based window capture using `SCScreenshotManager`.
///
/// Wraps the original `CaptureSession` polling logic into the `ScreenCapture`
/// trait. Uses macOS's `SCScreenshotManager` (not `SCStream`) to avoid blank
/// buffers on certain macOS 26 native-app windows (e.g. Ghostty, Clash Verge).
pub struct MacosCapture {
    stop: Arc<AtomicBool>,
    seq: Arc<AtomicU64>,
    join_handle: Option<thread::JoinHandle<()>>,
    filter: SCContentFilter,
    out_w: u32,
    out_h: u32,
    fps: u32,
}

impl ScreenCapture for MacosCapture {
    type Options = (SCContentFilter, u32, u32, u32);

    fn create(options: Self::Options) -> CaptureResult<Self> {
        Ok(MacosCapture {
            stop: Arc::new(AtomicBool::new(false)),
            seq: Arc::new(AtomicU64::new(0)),
            join_handle: None,
            filter: options.0,
            out_w: options.1,
            out_h: options.2,
            fps: options.3,
        })
    }

    fn start<F>(&mut self, mut on_frame: F) -> CaptureResult<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static,
    {
        let mut config = SCStreamConfiguration::default();
        config
            .set_width(self.out_w)
            .set_height(self.out_h)
            .set_pixel_format(PixelFormat::BGRA)
            .set_shows_cursor(false);

        let interval = Duration::from_secs_f64(1.0 / f64::from(self.fps));
        let stop = self.stop.clone();
        let seq = self.seq.clone();
        let filter = self.filter.clone();
        let out_w = self.out_w;
        let out_h = self.out_h;

        const CAPTURE_ERROR_THROTTLE: Duration = Duration::from_secs(5);

        let handle = thread::spawn(move || {
            let mut next_frame = Instant::now();
            let mut last_error_log = Instant::now();

            while !stop.load(Ordering::Relaxed) {
                match SCScreenshotManager::capture_sample_buffer(&filter, &config) {
                    Ok(sample) => {
                        if let Some(pb) = sample.image_buffer() {
                            if let Some(surface) = pb.io_surface() {
                                let pts = seq.fetch_add(1, Ordering::Relaxed);
                                let fb = FrameBuffer {
                                    handle: FrameBufferHandle::IOSurface(surface),
                                    width: out_w,
                                    height: out_h,
                                    pts,
                                    timescale: 1000,
                                    cap_time: Instant::now(),
                                };
                                on_frame(fb);
                            }
                        }
                    }
                    Err(e) => {
                        let now = Instant::now();
                        if now - last_error_log >= CAPTURE_ERROR_THROTTLE {
                            eprintln!("  Screenshot capture error: {}", e);
                            last_error_log = now;
                        }
                    }
                }

                next_frame += interval;
                let now = Instant::now();
                if next_frame > now {
                    spin_sleep::sleep(next_frame - now);
                } else {
                    next_frame = now;
                }
            }
        });

        self.join_handle = Some(handle);
        Ok(())
    }

    fn stop(&mut self) -> CaptureResult<()> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
        Ok(())
    }
}

impl Drop for MacosCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
