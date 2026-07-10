//! Windows NVENC hardware encoder — thin FFI bindings.
//!
//! Uses NVIDIA's NVENC API for hardware H.264 encoding on Windows.
//! AMD GPUs can use AMF as a fallback (future).

use std::ffi::c_void;

use super::EncoderResult;

/// NVENC H.264 hardware encoder for Windows.
pub struct NvencEncoder;

impl NvencEncoder {
    /// Create NVENC encoder session
    ///
    /// # Arguments
    /// * `device` - D3D11 device pointer for resource registration
    /// * `width`, `height` - encoding resolution
    /// * `fps` - target frame rate
    pub fn new(
        _device: *mut c_void,
        _width: u32,
        _height: u32,
        _fps: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // TODO: 
        // 1. Load nvEncodeAPI.dll
        // 2. NvEncodeAPICreateInstance → INvenc
        // 3. NvEncOpenEncodeSession
        // 4. NvEncInitializeEncoder (with H.264 config)
        // 5. NvEncRegisterResource (D3D11 texture shared handle)
        Err("NVENC encoder requires NVIDIA GPU + nvEncodeAPI — not yet implemented".into())
    }

    /// Encode a D3D11 texture to H.264
    pub fn encode(
        &self,
        _texture: *mut c_void,
        _width: u32,
        _height: u32,
    ) -> EncoderResult<super::EncodedFrame> {
        // TODO:
        // 1. NvEncMapInputResource
        // 2. NvEncEncodePicture
        // 3. NvEncUnmapInputResource
        Err("NVENC encode not yet implemented".into())
    }
}
