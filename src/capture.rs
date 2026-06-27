use core_graphics::geometry::{CGRect, CGPoint, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window;

/// Capture a window screenshot as CGImage.
pub fn capture_cgimage(window_id: u32) -> Option<CGImage> {
    let null_rect = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0.0, 0.0),
    );
    window::create_image(
        null_rect,
        window::kCGWindowListOptionIncludingWindow,
        window_id,
        window::kCGWindowImageDefault | window::kCGWindowImageNominalResolution,
    )
}
