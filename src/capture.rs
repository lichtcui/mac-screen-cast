use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::prelude::*;
use screencapturekit::screenshot_manager::SCScreenshotManager;
use screencapturekit::stream::configuration::SCStreamConfiguration;
use screencapturekit::stream::content_filter::SCContentFilter;

/// Polling-based window capture using `SCScreenshotManager`.
///
/// macOS 26's `SCStream` with `desktopIndependentWindow` returns blank pixel
/// buffers for certain native-app windows (e.g. Ghostty, Clash Verge).
/// `SCScreenshotManager` is a different code path that does not have this
/// limitation.
pub struct CaptureSession {
    stop: Arc<AtomicBool>,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl CaptureSession {
    pub fn new<H>(
        filter: SCContentFilter,
        output_width: u32,
        output_height: u32,
        fps: u32,
        mut handler: H,
    ) -> Result<Self, String>
    where
        H: FnMut(CMSampleBuffer, SCStreamOutputType) + Send + 'static,
    {
        let mut config = SCStreamConfiguration::default();
        config
            .set_width(output_width)
            .set_height(output_height)
            .set_pixel_format(PixelFormat::BGRA)
            .set_shows_cursor(false);

        let interval = Duration::from_secs_f64(1.0 / fps as f64);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_c = stop.clone();

        let join_handle = thread::spawn(move || {
            let mut next_frame = Instant::now();
            while !stop_c.load(Ordering::Relaxed) {
                match SCScreenshotManager::capture_sample_buffer(&filter, &config) {
                    Ok(sample) => {
                        handler(sample, SCStreamOutputType::Screen);
                    }
                    Err(e) => {
                        eprintln!("  Screenshot capture error: {}", e);
                    }
                }
                next_frame += interval;
                let now = Instant::now();
                if next_frame > now {
                    thread::sleep(next_frame - now);
                } else {
                    next_frame = now;
                }
            }
        });

        Ok(CaptureSession {
            stop,
            join_handle: Some(join_handle),
        })
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.join_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop();
    }
}
