//! Linux VAAPI hardware encoder — thin FFI bindings to libva.

use std::ffi::c_void;

use super::EncoderResult;

/// VAAPI H.264 hardware encoder
pub struct VaapiEncoder {
    display: VADisplay,
    config_id: VAConfigID,
    context_id: VAContextID,
    width: u32,
    height: u32,
}

// SAFETY: libva handles are thread-safe pointers
unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    /// Create a VAAPI encoder session
    pub fn new(drm_fd: i32, width: u32, height: u32, fps: u32) -> Result<Self, Box<dyn std::error::Error>> {
        let _ = (drm_fd, fps);
        // TODO: vaGetDisplayDRM, vaInitialize, vaCreateConfig, vaCreateContext
        Err("VAAPI encoder requires libva at runtime — not yet wired".into())
    }

    /// Encode a frame from a DMA-BUF fd
    pub fn encode(&self, _dma_buf_fd: i32, _width: u32, _height: u32) -> EncoderResult<super::EncodedFrame> {
        // TODO: VASurfaceAttribExternalBuffers, vaBegin/Render/EndPicture, vaGetEncodedBits
        Err("VAAPI encode not yet implemented".into())
    }
}

impl Drop for VaapiEncoder {
    fn drop(&mut self) {
        // TODO: vaDestroySurfaces, vaTerminate
    }
}

// ── FFI type aliases ──

type VADisplay = *mut c_void;
type VAConfigID = u32;
type VAContextID = u32;
type VASurfaceID = u32;

#[link(name = "va-drm")]
extern "C" {
    fn vaGetDisplayDRM(fd: i32) -> VADisplay;
}

#[link(name = "va")]
extern "C" {
    fn vaInitialize(dpy: VADisplay, major: *mut u32, minor: *mut u32) -> i32;
    fn vaTerminate(dpy: VADisplay) -> i32;
    fn vaCreateConfig(
        dpy: VADisplay,
        profile: i32,
        entrypoint: i32,
        attrib_list: *const c_void,
        num_attribs: i32,
        config_id: *mut VAConfigID,
    ) -> i32;
    fn vaCreateSurfaces(
        dpy: VADisplay,
        format: u32,
        width: u32,
        height: u32,
        surfaces: *mut VASurfaceID,
        num_surfaces: i32,
        attrib_list: *const c_void,
        num_attribs: i32,
    ) -> i32;
    fn vaDestroySurfaces(dpy: VADisplay, surfaces: *mut VASurfaceID, num_surfaces: i32) -> i32;
    fn vaCreateContext(
        dpy: VADisplay,
        config_id: VAConfigID,
        picture_width: i32,
        picture_height: i32,
        flag: i32,
        render_targets: *mut VASurfaceID,
        num_render_targets: i32,
        context: *mut VAContextID,
    ) -> i32;
    fn vaBeginPicture(dpy: VADisplay, context_id: VAContextID, surface_id: VASurfaceID) -> i32;
    fn vaRenderPicture(dpy: VADisplay, context_id: VAContextID, buffers: *const *mut c_void, num_buffers: i32) -> i32;
    fn vaEndPicture(dpy: VADisplay, context_id: VAContextID) -> i32;
    fn vaSyncSurface(dpy: VADisplay, surface_id: VASurfaceID) -> i32;
    fn vaGetEncodedBits(dpy: VADisplay, size: *mut u32) -> *mut c_void;
}
