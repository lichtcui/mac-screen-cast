use crate::capture::{FrameBuffer, FrameBufferHandle};
use crate::h264::VtEncoder;

use super::{EncodedFrame, EncoderResult, HardwareEncoder};

/// macOS VideoToolbox encoder wrapping `VtEncoder` in the `HardwareEncoder` trait.
pub struct MacosEncoder {
    inner: VtEncoder,
    fps: u32,
}

impl HardwareEncoder for MacosEncoder {
    type Options = ();

    fn new(width: u32, height: u32, fps: u32, _options: Self::Options) -> EncoderResult<Self> {
        Ok(MacosEncoder {
            inner: VtEncoder::new(width, height, fps)
                .map_err(|e| format!("VtEncoder init failed: {e}"))?,
            fps,
        })
    }

    fn encode(&self, frame: &FrameBuffer) -> EncoderResult<EncodedFrame> {
        let surface_ref = match &frame.handle {
            FrameBufferHandle::IOSurface(surface) => surface,
            _ => return Err("unexpected frame handle type for macOS encoder".into()),
        };

        let h264 = self
            .inner
            .encode(surface_ref, frame.pts, self.fps as i32)
            .map_err(|e| format!("VtEncoder encode failed: {e}"))?;

        Ok(EncodedFrame {
            data: h264.data,
            is_keyframe: h264.is_keyframe,
            sps: h264.sps,
            pps: h264.pps,
            pts: frame.pts,
        })
    }
}
