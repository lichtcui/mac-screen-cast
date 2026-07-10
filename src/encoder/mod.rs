use std::error::Error;

use crate::capture::FrameBuffer;

/// Convenience alias for fallible encoder operations.
pub type EncoderResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// An encoded H.264 frame in AVCC format.
///
/// AVCC format uses 4-byte big-endian length prefixes before each NAL unit
/// (as opposed to Annex B's start code `0x00000001`). This is the native
/// output format of VideoToolbox on macOS, and also what VAAPI/NVENC produce
/// natively.
pub struct EncodedFrame {
    /// Compressed frame data: AVCC-format bytes
    pub data: Vec<u8>,
    /// `true` if this is an IDR (key) frame
    pub is_keyframe: bool,
    /// Sequence parameter set (present on keyframes only)
    pub sps: Option<Vec<u8>>,
    /// Picture parameter set (present on keyframes only)
    pub pps: Option<Vec<u8>>,
    /// Presentation timestamp
    pub pts: u64,
}

/// Each platform provides a hardware-accelerated H.264 encoder.
pub trait HardwareEncoder: Send {
    type Options;

    /// Create a new encoder session for the given resolution and frame rate.
    fn new(width: u32, height: u32, fps: u32, options: Self::Options) -> EncoderResult<Self>
    where
        Self: Sized;

    /// Encode a single frame (blocking call).
    ///
    /// The encoder borrows the GPU resource from `frame` without consuming it,
    /// so the caller may reuse or discard the `FrameBuffer` after encoding.
    fn encode(&self, frame: &FrameBuffer) -> EncoderResult<EncodedFrame>;
}

// ── Platform module stubs ──

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "linux")]
mod linux_vaapi;
#[cfg(target_os = "linux")]
mod linux_nvenc;
