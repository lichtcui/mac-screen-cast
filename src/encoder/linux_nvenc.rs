//! Linux NVENC hardware encoder — thin FFI bindings (placeholder).
//!
//! This module provides an NVENC-based H.264 encoder for NVIDIA GPUs.
//! Full implementation deferred to a later phase.

use super::EncoderResult;

/// NVENC H.264 hardware encoder (placeholder)
pub struct NvencEncoder;

impl NvencEncoder {
    pub fn new(_device_index: i32, _width: u32, _height: u32, _fps: u32) -> Result<Self, Box<dyn std::error::Error>> {
        Err("NVENC encoder not yet implemented".into())
    }

    pub fn encode(&self, _dma_buf_fd: i32, _width: u32, _height: u32) -> EncoderResult<super::EncodedFrame> {
        Err("NVENC encode not yet implemented".into())
    }
}
