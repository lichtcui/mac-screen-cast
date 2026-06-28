use screencapturekit::cm::CMTime;
use screencapturekit::prelude::*;

/// Thin wrapper around an SCStream for capturing a window.
pub struct CaptureSession {
    stream: Option<SCStream>,
}

impl CaptureSession {
    pub fn new<H>(
        window_id: u32,
        output_width: u32,
        output_height: u32,
        fps: u32,
        handler: H,
    ) -> Result<Self, String>
    where
        H: SCStreamOutputTrait + 'static,
    {
        let content = SCShareableContent::get().map_err(|e| {
            format!(
                "SCShareableContent error: {}. \
                 Grant Screen Recording permission in \
                 System Settings > Privacy & Security > Screen Recording.",
                e
            )
        })?;

        let windows = content.windows();
        let window = windows
            .iter()
            .find(|w| w.window_id() == window_id)
            .ok_or_else(|| format!("Window {} not found", window_id))?;

        let filter = SCContentFilter::create().with_window(window).build();

        let mut config = SCStreamConfiguration::default();
        config
            .set_width(output_width)
            .set_height(output_height)
            .set_pixel_format(PixelFormat::BGRA)
            .set_shows_cursor(false);
        config.set_minimum_frame_interval(&CMTime::new(1, fps as i32));

        let mut stream = SCStream::new(&filter, &config);

        stream.add_output_handler(handler, SCStreamOutputType::Screen);
        stream
            .start_capture()
            .map_err(|e| format!("SCStream start_capture failed: {}", e))?;

        Ok(CaptureSession {
            stream: Some(stream),
        })
    }

    pub fn stop(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.stop_capture();
        }
    }
}

impl Drop for CaptureSession {
    fn drop(&mut self) {
        self.stop();
    }
}
