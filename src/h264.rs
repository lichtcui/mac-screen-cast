#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;


use core_foundation::base::{CFRelease, TCFType};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::base::{kCGBitmapByteOrder32Little, kCGImageAlphaNoneSkipFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGRect, CGPoint, CGSize};
use core_graphics::image::CGImage;

// ============================================================
// FFI types and constants
// ============================================================

type OSStatus = i32;
type FourCharCode = u32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

const kCMTimeInvalid: CMTime = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };

const kCMVideoCodecType_H264: FourCharCode = 0x61766331; // 'avc1'
const kCVPixelFormatType_32BGRA: FourCharCode = 0x42475241; // 'BGRA'

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        pixel_format: FourCharCode,
        attrs: *const c_void,
        buf_out: *mut *mut c_void,
    ) -> i32;

    fn CVPixelBufferLockBaseAddress(buf: *mut c_void, flags: u32) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buf: *mut c_void, flags: u32) -> i32;
    fn CVPixelBufferGetBaseAddress(buf: *mut c_void) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(buf: *mut c_void) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: FourCharCode,
        encoder_spec: *const c_void,
        source_image_attrs: *const c_void,
        compressed_data_allocator: *const c_void,
        callback: Option<
            unsafe extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, *mut c_void),
        >,
        refcon: *mut c_void,
        session_out: *mut *mut c_void,
    ) -> OSStatus;

    fn VTCompressionSessionEncodeFrame(
        session: *mut c_void,
        pixel_buffer: *mut c_void,
        pts: CMTime,
        duration: CMTime,
        frame_properties: *const c_void,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;

    fn VTCompressionSessionCompleteFrames(session: *mut c_void, until_time: CMTime) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: *mut c_void);
    fn VTSessionSetProperty(
        session: *mut c_void,
        key: CFStringRef,
        value: *const c_void,
    ) -> OSStatus;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetFormatDescription(sbuf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetDataBuffer(sbuf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(sbuf: *mut c_void) -> CMTime;
    fn CMBlockBufferGetDataLength(buf: *mut c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(
        buf: *mut c_void,
        offset: usize,
        length: usize,
        dest: *mut c_void,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: *mut c_void,
        idx: usize,
        ptr_out: *mut *const u8,
        size_out: *mut usize,
        count_out: *mut usize,
        nal_len_out: *mut i32,
    ) -> OSStatus;
}

extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
}

// ============================================================
// Types
// ============================================================

#[derive(Clone)]
pub struct H264Frame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts_timescale: i32,
    pub pts_value: i64,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

pub struct VtEncoder {
    session: *mut c_void,
    rx: mpsc::Receiver<H264Frame>,
}

impl VtEncoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        let tx_cb = Arc::into_raw(tx.clone()) as *mut c_void;

        let mut session: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null_mut(),
                width as i32,
                height as i32,
                kCMVideoCodecType_H264,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                Some(encode_callback),
                tx_cb,
                &mut session,
            )
        };
        if status != 0 || session.is_null() {
            let _ = unsafe { Arc::from_raw(tx_cb as *const Mutex<mpsc::Sender<H264Frame>>) };
            return Err(format!("VTCompressionSessionCreate failed: {}", status));
        }

        let _ = height; // suppress unused
        let enc = VtEncoder { session, rx };

        enc.set_prop_bool(b"RealTime\0", true)?;
        enc.set_prop_bool(b"AllowFrameReordering\0", false)?;
        enc.set_prop_i32(b"ExpectedFrameRate\0", fps as i32)?;
        enc.set_prop_i32(b"MaxKeyFrameInterval\0", (fps * 2) as i32)?;
        enc.set_prop_i32(b"AverageBitRate\0", 4_000_000)?;

        Ok(enc)
    }

    /// Encode a CGImage into an H.264 frame (blocks until VT callback fires).
    pub fn encode_frame(
        &self,
        cg: &CGImage,
        frame_num: u64,
        frame_dur_tscale: i32,
    ) -> Result<H264Frame, String> {
        let w = cg.width();
        let h = cg.height();

        // Create CVPixelBuffer
        let mut pb: *mut c_void = std::ptr::null_mut();
        let attrs = std::ptr::null();
        let status = unsafe {
            CVPixelBufferCreate(
                std::ptr::null_mut(),
                w as i32,
                h as i32,
                kCVPixelFormatType_32BGRA,
                attrs,
                &mut pb,
            )
        };
        if status != 0 || pb.is_null() {
            return Err(format!("CVPixelBufferCreate failed: {}", status));
        }

        // Lock and render CGImage into pixel buffer
        unsafe {
            CVPixelBufferLockBaseAddress(pb, 0);
            let data = CVPixelBufferGetBaseAddress(pb);
            let bpr = CVPixelBufferGetBytesPerRow(pb);

            let cs = CGColorSpace::create_device_rgb();
            let ctx = CGContext::create_bitmap_context(
                Some(data),
                w,
                h as usize,
                8,
                bpr,
                &cs,
                kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst,
            );
            let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(w as _, h as _));
            ctx.draw_image(rect, cg);
            drop(ctx);
            drop(cs);

            CVPixelBufferUnlockBaseAddress(pb, 0);
        }

        // Encode
        let pts = CMTime {
            value: (frame_num * frame_dur_tscale as u64) as i64,
            timescale: frame_dur_tscale,
            flags: 0,
            epoch: 0,
        };
        let dur = CMTime {
            value: frame_dur_tscale as i64,
            timescale: frame_dur_tscale,
            flags: 0,
            epoch: 0,
        };
        let encode_status = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pb,
                pts,
                dur,
                std::ptr::null(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };

        // Release our ref — VT retains it internally
        unsafe { CFRelease(pb as *const _) };

        if encode_status != 0 {
            unsafe { VTCompressionSessionCompleteFrames(self.session, kCMTimeInvalid) };
            return Err(format!("VTCompressionSessionEncodeFrame failed: {}", encode_status));
        }

        // Sync: flush + wait for callback (with 5s timeout)
        unsafe { VTCompressionSessionCompleteFrames(self.session, kCMTimeInvalid) };
        self.rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("encode timeout/error: {:?}", e))
    }

    fn set_prop_bool(&self, raw_key: &[u8], val: bool) -> Result<(), String> {
        unsafe {
            let key_name = std::str::from_utf8(&raw_key[..raw_key.len() - 1]).unwrap();
            let key = CFString::new(key_name);
            let cfval = if val { kCFBooleanTrue } else { kCFBooleanFalse };
            let s = VTSessionSetProperty(self.session, key.as_concrete_TypeRef(), cfval as *const _);
            if s != 0 {
                return Err(format!("set_prop failed: {}", s));
            }
        }
        Ok(())
    }

    fn set_prop_i32(&self, raw_key: &[u8], val: i32) -> Result<(), String> {
        unsafe {
            let key_name = std::str::from_utf8(&raw_key[..raw_key.len() - 1]).unwrap();
            let key = CFString::new(key_name);
            let num = CFNumber::from(val);
            let s = VTSessionSetProperty(self.session, key.as_concrete_TypeRef(), num.as_CFTypeRef());
            if s != 0 {
                return Err(format!("set_prop failed: {}", s));
            }
        }
        Ok(())
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        if !self.session.is_null() {
            unsafe {
                VTCompressionSessionInvalidate(self.session);
                CFRelease(self.session as *const _);
            }
        }
    }
}

// ============================================================
// VT compression output callback
// ============================================================

unsafe extern "C" fn encode_callback(
    refcon: *mut c_void,
    _source_ref: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: *mut c_void,
) {
    if status != 0 || sample_buffer.is_null() {
        return;
    }

    let tx = &*(refcon as *const Mutex<mpsc::Sender<H264Frame>>);

    // Extract SPS/PPS from format description
    let fmt_desc = CMSampleBufferGetFormatDescription(sample_buffer);
    let mut sps_ptr: *const u8 = std::ptr::null();
    let mut sps_size: usize = 0;
    let mut param_count: usize = 0;
    let mut _nal_len: i32 = 0;

    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;

    if !fmt_desc.is_null() {
        let s = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            fmt_desc,
            0,
            &mut sps_ptr,
            &mut sps_size,
            &mut param_count,
            &mut _nal_len,
        );
        if s == 0 && !sps_ptr.is_null() {
            sps = Some(std::slice::from_raw_parts(sps_ptr, sps_size).to_vec());
        }
        if param_count > 1 {
            let mut pps_ptr: *const u8 = std::ptr::null();
            let mut pps_size: usize = 0;
            let _ = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                fmt_desc,
                1,
                &mut pps_ptr,
                &mut pps_size,
                &mut param_count,
                &mut _nal_len,
            );
            if !pps_ptr.is_null() {
                pps = Some(std::slice::from_raw_parts(pps_ptr, pps_size).to_vec());
            }
        }
    }

    // Extract encoded data
    let block_buf = CMSampleBufferGetDataBuffer(sample_buffer);
    if block_buf.is_null() {
        return;
    }

    let len = CMBlockBufferGetDataLength(block_buf);
    if len == 0 {
        return;
    }

    let mut data = vec![0u8; len];
    let copy_status = CMBlockBufferCopyDataBytes(block_buf, 0, len, data.as_mut_ptr() as *mut _);
    if copy_status != 0 {
        return;
    }

    let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);

    // Determine if this is a keyframe by scanning NAL units for IDR (type 5)
    let is_keyframe = {
        let mut pos = 0usize;
        let mut kf = false;
        while pos + 5 <= len {
            let nal_sz =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
            if nal_sz == 0 || pos + 4 + nal_sz > len {
                break;
            }
            if (data[pos + 4] & 0x1f) == 5 {
                kf = true;
                break;
            }
            pos += 4 + nal_sz;
        }
        kf
    };

    let frame = H264Frame {
        data,
        is_keyframe,
        pts_timescale: pts.timescale,
        pts_value: pts.value,
        sps,
        pps,
    };

    if let Ok(tx) = tx.lock() {
        let _ = tx.send(frame);
    }
}
