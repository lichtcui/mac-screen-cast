#![allow(non_upper_case_globals, dead_code)]

use std::ffi::c_void;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::hash::{DefaultHasher, Hasher};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use threadpool::ThreadPool;

use core_graphics::base::{kCGBitmapByteOrder32Big, kCGBitmapByteOrder32Little, kCGImageAlphaNoneSkipFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::{CGContext, CGInterpolationQuality};
use core_graphics::geometry::{CGRect, CGPoint, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window;
use foreign_types::ForeignType;

use core_foundation::base::{CFIndex, CFRelease, CFType, TCFType};
use core_foundation::data::{CFMutableDataRef, CFDataCreateMutable, CFDataGetMutableBytePtr, CFDataGetLength};
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

// ============================================================
// Core Graphics window capture + JPEG encoding
// ============================================================

fn capture_window(window_id: u32, max_w: u32, quality: u8, jpeg_out: &mut Vec<u8>) -> bool
{
    let cg = match capture_cgimage(window_id) {
        Some(c) => c, None => return false,
    };
    let (w, h) = (cg.width() as u32, cg.height() as u32);
    if w == 0 || h == 0 { return false; }

    let encode_image = if w > max_w {
        let nh = (h * max_w) / w;
        let cs = CGColorSpace::create_device_rgb();
        let ctx = CGContext::create_bitmap_context(
            None, max_w as usize, nh as usize, 8, max_w as usize * 4, &cs,
            kCGBitmapByteOrder32Big | kCGImageAlphaNoneSkipFirst,
        );
        ctx.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityHigh);
        let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(max_w as _, nh as _));
        ctx.draw_image(rect, &cg);
        match ctx.create_image() {
            Some(img) => img,
            None => return false,
        }
    } else {
        cg
    };

    encode_native_jpeg(&encode_image, quality, jpeg_out)
}

fn capture_cgimage(window_id: u32) -> Option<CGImage> {
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

// ---------- native JPEG encoding via ImageIO ----------

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    static kCGImageDestinationLossyCompressionQuality: CFStringRef;

    fn CGImageDestinationCreateWithData(
        data: CFMutableDataRef,
        type_: CFStringRef,
        count: CFIndex,
        options: *const c_void,
    ) -> *mut c_void;

    fn CGImageDestinationAddImage(
        dest: *mut c_void,
        image: *const c_void,
        properties: *const c_void,
    );

    fn CGImageDestinationFinalize(dest: *mut c_void) -> u8;
}

fn encode_native_jpeg(image: &CGImage, quality: u8, out: &mut Vec<u8>) -> bool {
    unsafe {
        let data = CFDataCreateMutable(std::ptr::null_mut(), 0);
        if data.is_null() { return false; }

        let uti = CFString::new("public.jpeg");
        let dest = CGImageDestinationCreateWithData(data, uti.as_concrete_TypeRef(), 1, std::ptr::null());
        if dest.is_null() { CFRelease(data as *const _); return false; }
        let dest = CFType::wrap_under_create_rule(dest as *const _);

        let quality_val = CFNumber::from(quality as f64 / 100.0);
        let mut props = CFMutableDictionary::<*const c_void, *const c_void>::new();
        props.add(
            &(kCGImageDestinationLossyCompressionQuality as *const c_void),
            &(quality_val.as_CFTypeRef()),
        );

        CGImageDestinationAddImage(
            dest.as_concrete_TypeRef() as *mut _,
            image.as_ptr() as *const _,
            props.as_concrete_TypeRef() as *const _,
        );

        let ok = CGImageDestinationFinalize(dest.as_concrete_TypeRef() as *mut _);
        if ok != 0 {
            let len = CFDataGetLength(data);
            if len > 0 {
                let ptr = CFDataGetMutableBytePtr(data);
                if !ptr.is_null() {
                    let bytes = std::slice::from_raw_parts(ptr, len as usize);
                    out.clear();
                    out.extend_from_slice(bytes);
                }
            }
        }
        CFRelease(data as *const _);
        ok != 0
    }
}

// ============================================================
// VideoToolbox H.264 encoder
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
        width: i32, height: i32,
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
        width: i32, height: i32,
        codec_type: FourCharCode,
        encoder_spec: *const c_void,
        source_image_attrs: *const c_void,
        compressed_data_allocator: *const c_void,
        callback: Option<unsafe extern "C" fn(
            *mut c_void, *mut c_void, OSStatus, u32, *mut c_void
        )>,
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
    fn VTSessionSetProperty(session: *mut c_void, key: CFStringRef, value: *const c_void) -> OSStatus;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetFormatDescription(sbuf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetDataBuffer(sbuf: *mut c_void) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(sbuf: *mut c_void) -> CMTime;
    fn CMBlockBufferGetDataLength(buf: *mut c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(
        buf: *mut c_void, offset: usize, length: usize, dest: *mut c_void,
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

// H.264 encoded frame from the encoder callback
#[derive(Clone)]
struct H264Frame {
    data: Vec<u8>,
    is_keyframe: bool,
    pts_timescale: i32,
    pts_value: i64,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

// Encoder session wrapper
struct VtEncoder {
    session: *mut c_void,
    rx: mpsc::Receiver<H264Frame>,
}

impl VtEncoder {
    fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let tx = Arc::new(Mutex::new(tx));
        let tx_cb = Arc::into_raw(tx.clone()) as *mut c_void;

        let mut session: *mut c_void = std::ptr::null_mut();
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null_mut(),
                width as i32, height as i32,
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

        // Configure properties
        enc.set_prop_bool(b"RealTime\0", true)?;
        enc.set_prop_bool(b"AllowFrameReordering\0", false)?;
        enc.set_prop_i32(b"ExpectedFrameRate\0", fps as i32)?;
        enc.set_prop_i32(b"MaxKeyFrameInterval\0", (fps * 2) as i32)?;
        enc.set_prop_i32(b"AverageBitRate\0", 4_000_000)?;

        Ok(enc)
    }

    fn set_prop_bool(&self, raw_key: &[u8], val: bool) -> Result<(), String> {
        unsafe {
            let key_name = std::str::from_utf8(&raw_key[..raw_key.len()-1]).unwrap();
            let key = CFString::new(key_name);
            let cfval = if val { kCFBooleanTrue } else { kCFBooleanFalse };
            let s = VTSessionSetProperty(self.session, key.as_concrete_TypeRef(), cfval as *const _);
            if s != 0 { return Err(format!("set_prop failed: {}", s)); }
        }
        Ok(())
    }

    fn set_prop_i32(&self, raw_key: &[u8], val: i32) -> Result<(), String> {
        unsafe {
            let key_name = std::str::from_utf8(&raw_key[..raw_key.len()-1]).unwrap();
            let key = CFString::new(key_name);
            let num = CFNumber::from(val);
            let s = VTSessionSetProperty(self.session, key.as_concrete_TypeRef(), num.as_CFTypeRef());
            if s != 0 { return Err(format!("set_prop failed: {}", s)); }
        }
        Ok(())
    }
    // Returns the encoded H264Frame (blocking until VT callback fires).
    fn encode_frame(&self, cg: &CGImage, frame_num: u64, frame_dur_tscale: i32) -> Result<H264Frame, String> {
        let w = cg.width();
        let h = cg.height();

        // Create CVPixelBuffer
        let mut pb: *mut c_void = std::ptr::null_mut();
        let attrs = std::ptr::null(); // no special attrs needed
        let status = unsafe {
            CVPixelBufferCreate(
                std::ptr::null_mut(),
                w as i32, h as i32,
                kCVPixelFormatType_32BGRA,
                attrs,
                &mut pb,
            )
        };
        if status != 0 || pb.is_null() {
            return Err(format!("CVPixelBufferCreate failed: {}", status));
        }

        // Lock and render
        unsafe {
            CVPixelBufferLockBaseAddress(pb, 0);
            let data = CVPixelBufferGetBaseAddress(pb);
            let bpr = CVPixelBufferGetBytesPerRow(pb);

            let cs = CGColorSpace::create_device_rgb();
            let ctx = CGContext::create_bitmap_context(
                Some(data), w, h as usize, 8, bpr, &cs,
                kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst,
            );
            let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(w as _, h as _));
            ctx.draw_image(rect, cg);
            drop(ctx); drop(cs);

            CVPixelBufferUnlockBaseAddress(pb, 0);
        }

        // Encode
        let pts = CMTime {
            value: (frame_num * frame_dur_tscale as u64) as i64,
            timescale: frame_dur_tscale,
            flags: 0, epoch: 0,
        };
        let dur = CMTime {
            value: frame_dur_tscale as i64,
            timescale: frame_dur_tscale,
            flags: 0, epoch: 0,
        };
        let encode_status = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session, pb, pts, dur,
                std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut(),
            )
        };

        // Release our ref — VT retains it internally
        unsafe { CFRelease(pb as *const _); }

        if encode_status != 0 {
            unsafe { VTCompressionSessionCompleteFrames(self.session, kCMTimeInvalid); }
            return Err(format!("VTCompressionSessionEncodeFrame failed: {}", encode_status));
        }

        // Sync: flush + wait for callback (with timeout to prevent freezing)
        unsafe {
            VTCompressionSessionCompleteFrames(self.session, kCMTimeInvalid);
        }
        self.rx.recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("encode timeout/error: {:?}", e))
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

    // Check if this is a keyframe via format description
    let fmt_desc = CMSampleBufferGetFormatDescription(sample_buffer);
    let mut sps_ptr: *const u8 = std::ptr::null();
    let mut sps_size: usize = 0;
    let mut param_count: usize = 0;
    let mut _nal_len: i32 = 0;

    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;

    if !fmt_desc.is_null() {
        let s = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            fmt_desc, 0, &mut sps_ptr, &mut sps_size, &mut param_count, &mut _nal_len,
        );
        if s == 0 && !sps_ptr.is_null() {
            sps = Some(std::slice::from_raw_parts(sps_ptr, sps_size).to_vec());
        }
        if param_count > 1 {
            let mut pps_ptr: *const u8 = std::ptr::null();
            let mut pps_size: usize = 0;
            let _ = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                fmt_desc, 1, &mut pps_ptr, &mut pps_size, &mut param_count, &mut _nal_len,
            );
            if !pps_ptr.is_null() {
                pps = Some(std::slice::from_raw_parts(pps_ptr, pps_size).to_vec());
            }
        }
    }

    // Extract encoded data
    let block_buf = CMSampleBufferGetDataBuffer(sample_buffer);
    if block_buf.is_null() { return; }

    let len = CMBlockBufferGetDataLength(block_buf);
    if len == 0 { return; }

    let mut data = vec![0u8; len];
    let copy_status = CMBlockBufferCopyDataBytes(block_buf, 0, len, data.as_mut_ptr() as *mut _);
    if copy_status != 0 { return; }

    let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);

    // Determine if this is a keyframe by scanning NAL units for IDR (type 5)
    let is_keyframe = {
        let mut pos = 0usize;
        let mut kf = false;
        while pos + 5 <= len {
            let nal_sz = u32::from_be_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]) as usize;
            if nal_sz == 0 || pos + 4 + nal_sz > len { break; }
            if (data[pos+4] & 0x1f) == 5 { kf = true; break; }
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

// Booleans for VTSessionSetProperty
extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFBooleanFalse: *const c_void;
}

// ============================================================
// Minimal fMP4 muxer (H.264 only)
// ============================================================

struct Fmp4State {
    sps: Vec<u8>,
    pps: Vec<u8>,
    width: u32,
    height: u32,
    timescale: u32,
    sequence_number: u32,
    dts_counter: u64, // local DTS starting from 0
}

impl Fmp4State {
    fn new(sps: Vec<u8>, pps: Vec<u8>, width: u32, height: u32, timescale: u32) -> Self {
        Fmp4State { sps, pps, width, height, timescale, sequence_number: 1, dts_counter: 0 }
    }

    fn codecs_string(&self) -> String {
        let profile = self.sps.get(1).copied().unwrap_or(0x42);
        let compat = self.sps.get(2).copied().unwrap_or(0x00);
        let level = self.sps.get(3).copied().unwrap_or(0x1e);
        format!("avc1.{:02x}{:02x}{:02x}", profile, compat, level)
    }

    fn build_init_segment(&self) -> Vec<u8> {
        let ftyp = self.build_ftyp();
        let moov = self.build_moov();
        let combined = [&ftyp[..], &moov[..]].concat();
        let mut out = Vec::with_capacity(combined.len());
        out.extend_from_slice(&combined);
        out
    }

    fn build_ftyp(&self) -> Vec<u8> {
        // major brand + minor + compatible brands
        let payload = {
            let mut p = Vec::new();
            p.extend_from_slice(b"iso5");
            p.extend_from_slice(&0u32.to_be_bytes()); // minor version
            p.extend_from_slice(b"iso5");
            p.extend_from_slice(b"iso6");
            p.extend_from_slice(b"mp41");
            p
        };
        Self::box_full(b"ftyp", &payload)
    }

    fn build_moov(&self) -> Vec<u8> {
        let mut p = Vec::new();
        // mvhd
        let mut mvhd = Vec::new();
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // ctime
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // mtime
        mvhd.extend_from_slice(&self.timescale.to_be_bytes());
        mvhd.extend_from_slice(&0u32.to_be_bytes()); // duration (live)
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); // rate 1.0
        mvhd.extend_from_slice(&0x0100u16.to_be_bytes()); // volume 1.0
        mvhd.extend_from_slice(&[0u8; 10]); // reserved
        // unity matrix
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); mvhd.extend_from_slice(&[0u8; 12]);
        mvhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); mvhd.extend_from_slice(&[0u8; 12]);
        mvhd.extend_from_slice(&0x4000_0000u32.to_be_bytes()); mvhd.extend_from_slice(&[0u8; 24]);
        mvhd.extend_from_slice(&2u32.to_be_bytes()); // next track ID
        p.extend_from_slice(&Self::box_full(b"mvhd", &mvhd));

        // trak
        let trak = self.build_trak();
        p.extend_from_slice(&trak);

        // mvex
        let mut trex = Vec::new();
        trex.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        trex.extend_from_slice(&1u32.to_be_bytes()); // track ID
        trex.extend_from_slice(&1u32.to_be_bytes()); // default sample desc index
        trex.extend_from_slice(&0u32.to_be_bytes()); // default duration
        trex.extend_from_slice(&0u32.to_be_bytes()); // default size
        trex.extend_from_slice(&0u32.to_be_bytes()); // default flags
        p.extend_from_slice(&Self::box_full(b"mvex", &Self::box_full(b"trex", &trex)));

        Self::box_full(b"moov", &p)
    }

    fn build_trak(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // tkhd
        let mut tkhd = Vec::new();
        tkhd.extend_from_slice(&0x0000_0003u32.to_be_bytes()); // ver 0, flags: enabled+in_movie
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&0u32.to_be_bytes());
        tkhd.extend_from_slice(&1u32.to_be_bytes()); // track ID
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // reserved
        tkhd.extend_from_slice(&0u32.to_be_bytes()); // duration
        tkhd.extend_from_slice(&[0u8; 8]); // reserved
        tkhd.extend_from_slice(&0u16.to_be_bytes()); tkhd.extend_from_slice(&0u16.to_be_bytes());
        tkhd.extend_from_slice(&0u16.to_be_bytes()); // vol=0 for video
        tkhd.extend_from_slice(&0u16.to_be_bytes());
        // matrix
        tkhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); tkhd.extend_from_slice(&[0u8; 12]);
        tkhd.extend_from_slice(&0x0001_0000u32.to_be_bytes()); tkhd.extend_from_slice(&[0u8; 12]);
        tkhd.extend_from_slice(&0x4000_0000u32.to_be_bytes());
        tkhd.extend_from_slice(&(self.width << 16).to_be_bytes());
        tkhd.extend_from_slice(&(self.height << 16).to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"tkhd", &tkhd));

        // mdia
        let mdia = self.build_mdia();
        p.extend_from_slice(&mdia);

        Self::box_full(b"trak", &p)
    }

    fn build_mdia(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // mdhd
        let mut mdhd = Vec::new();
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes());
        mdhd.extend_from_slice(&self.timescale.to_be_bytes());
        mdhd.extend_from_slice(&0u32.to_be_bytes()); // duration
        mdhd.extend_from_slice(&0x55c4u16.to_be_bytes()); // "und" language
        mdhd.extend_from_slice(&0u16.to_be_bytes());
        p.extend_from_slice(&Self::box_full(b"mdhd", &mdhd));

        // hdlr
        let mut hdlr = Vec::new();
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(&0u32.to_be_bytes());
        hdlr.extend_from_slice(b"vide");
        hdlr.extend_from_slice(&[0u8; 12]);
        hdlr.extend_from_slice(b"VideoHandler\0");
        p.extend_from_slice(&Self::box_full(b"hdlr", &hdlr));

        // minf
        let minf = self.build_minf();
        p.extend_from_slice(&minf);

        Self::box_full(b"mdia", &p)
    }

    fn build_minf(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // vmhd
        let mut vmhd_fixed = Vec::new();
        vmhd_fixed.extend_from_slice(&0x0000_0001u32.to_be_bytes());
        vmhd_fixed.extend_from_slice(&[0u8; 8]);
        p.extend_from_slice(&Self::box_full(b"vmhd", &vmhd_fixed));

        // dinf
        let dref = {
            let mut d = Vec::new();
            d.extend_from_slice(&0u32.to_be_bytes());
            d.extend_from_slice(&1u32.to_be_bytes());
            d.extend_from_slice(&Self::box_full(b"url ", &[0u8, 0, 0, 1]));
            Self::box_full(b"dref", &d)
        };
        p.extend_from_slice(&Self::box_full(b"dinf", &dref));

        // stbl
        let stbl = self.build_stbl();
        p.extend_from_slice(&stbl);

        Self::box_full(b"minf", &p)
    }

    fn build_stbl(&self) -> Vec<u8> {
        let mut p = Vec::new();

        // stsd
        let stsd = self.build_stsd();
        p.extend_from_slice(&stsd);

        // empty stts, stsc, stsz, stco
        let empty4 = |tag: &[u8; 4]| -> Vec<u8> {
            Self::box_full(tag, &[0u8; 8]) // version+flags, count=0
        };
        p.extend_from_slice(&empty4(b"stts"));
        p.extend_from_slice(&empty4(b"stsc"));
        let mut stsz = Vec::new();
        stsz.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        stsz.extend_from_slice(&0u32.to_be_bytes()); // sample_size=0 (variable)
        stsz.extend_from_slice(&0u32.to_be_bytes()); // sample_count=0
        p.extend_from_slice(&Self::box_full(b"stsz", &stsz));
        p.extend_from_slice(&empty4(b"stco"));

        Self::box_full(b"stbl", &p)
    }

    fn build_stsd(&self) -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&0u32.to_be_bytes()); // version+flags
        p.extend_from_slice(&1u32.to_be_bytes()); // entry_count

        // avc1 sample entry
        let mut avc1 = Vec::new();
        avc1.extend_from_slice(&[0u8; 6]); // reserved
        avc1.extend_from_slice(&1u16.to_be_bytes()); // data ref index
        avc1.extend_from_slice(&0u16.to_be_bytes()); // pre-defined
        avc1.extend_from_slice(&0u16.to_be_bytes());
        avc1.extend_from_slice(&[0u8; 12]); // pre-defined
        avc1.extend_from_slice(&(self.width as u16).to_be_bytes());
        avc1.extend_from_slice(&(self.height as u16).to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // hres
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes()); // vres
        avc1.extend_from_slice(&0u32.to_be_bytes()); // reserved
        avc1.extend_from_slice(&1u16.to_be_bytes()); // frame count
        avc1.extend_from_slice(&[0u8; 32]); // compressor name
        avc1.extend_from_slice(&0x0018u16.to_be_bytes()); // depth 24
        avc1.extend_from_slice(&0xffffu16.to_be_bytes()); // pre-defined

        // avcC
        let avcc = self.build_avcc();
        avc1.extend_from_slice(&avcc);

        p.extend_from_slice(&Self::box_full(b"avc1", &avc1));
        Self::box_full(b"stsd", &p)
    }

    fn build_avcc(&self) -> Vec<u8> {
        let mut p = vec![
            1, // configurationVersion
            self.sps.get(1).copied().unwrap_or(0x42), // profile
            self.sps.get(2).copied().unwrap_or(0x00), // compatibility
            self.sps.get(3).copied().unwrap_or(0x1e), // level
            0xff, // 6b reserved | 2b lengthSizeMinusOne = 3 (4-byte)
            0xe1, // 3b reserved | 5b num SPS
        ];
        p.extend_from_slice(&(self.sps.len() as u16).to_be_bytes());
        p.extend_from_slice(&self.sps);
        p.push(1); // num PPS
        p.extend_from_slice(&(self.pps.len() as u16).to_be_bytes());
        p.extend_from_slice(&self.pps);
        Self::box_full(b"avcC", &p)
    }

    // Build a media segment (moof + mdat) for a single video sample
    fn build_media_segment(&mut self, data: &[u8], is_keyframe: bool, _pts_tscale: i32, _pts_val: i64) -> Vec<u8> {
        let seq = self.sequence_number;
        self.sequence_number += 1;

        // Use local DTS counter starting at 0 (not VT's absolute timestamps)
        let dts = self.dts_counter;
        let frame_ticks = self.timescale as u64 / 30; // 3000 for 30fps @ 90kHz
        self.dts_counter += frame_ticks;
        let cto = 0i32; // composition time offset = 0

        // Build moof
        let moof = self.build_moof(seq, dts, data.len(), is_keyframe, cto);
        let moof_size = moof.len();

        // Build mdat with correct offset
        // data_offset = moof size + mdat header (8 bytes)
        let data_offset = moof_size as u32 + 8;
        let moof = self.build_moof_with_offset(seq, dts, data.len(), is_keyframe, cto, data_offset);
        let mdat_size = 8 + data.len();

        let mut segment = Vec::with_capacity(moof.len() + mdat_size);
        segment.extend_from_slice(&moof);
        segment.extend_from_slice(&(mdat_size as u32).to_be_bytes());
        segment.extend_from_slice(b"mdat");
        segment.extend_from_slice(data);
        segment
    }

    fn build_moof(&self, seq: u32, dts: u64, data_len: usize, key: bool, cto: i32) -> Vec<u8> {
        self.build_moof_with_offset(seq, dts, data_len, key, cto, 0)
    }

    fn build_moof_with_offset(&self, seq: u32, dts: u64, data_len: usize, key: bool, cto: i32, data_off: u32) -> Vec<u8> {
        let mut p = Vec::new();

        // mfhd
        let mfhd = {
            let mut b = Vec::new();
            b.extend_from_slice(&0u32.to_be_bytes()); // ver+flags
            b.extend_from_slice(&seq.to_be_bytes());
            Self::box_full(b"mfhd", &b)
        };
        p.extend_from_slice(&mfhd);

        // traf
        let traf = self.build_traf(dts, data_len, key, cto, data_off);
        p.extend_from_slice(&traf);

        Self::box_full(b"moof", &p)
    }

    fn build_traf(&self, dts: u64, data_len: usize, key: bool, cto: i32, data_off: u32) -> Vec<u8> {
        let mut p = Vec::new();

        // tfhd
        let tfhd = {
            let mut b = Vec::new();
            // flags: default-base-is-moof (0x020000)
            b.extend_from_slice(&0x0002_0000u32.to_be_bytes());
            b.extend_from_slice(&1u32.to_be_bytes()); // track ID
            Self::box_full(b"tfhd", &b)
        };
        p.extend_from_slice(&tfhd);

        // tfdt (version 1 for 64-bit)
        let tfdt = {
            let mut b = Vec::new();
            b.extend_from_slice(&0x0100_0000u32.to_be_bytes()); // ver=1
            b.extend_from_slice(&dts.to_be_bytes());
            Self::box_full(b"tfdt", &b)
        };
        p.extend_from_slice(&tfdt);

        // trun (version 1 for signed CTO)
        // flags: data-offset, duration, size, flags, cto
        let trun_flags: u32 = 0x0001 | 0x0100 | 0x0200 | 0x0400 | 0x0800;
        let trun = {
            let mut b = Vec::new();
            b.extend_from_slice(&(0x0100_0000u32 | trun_flags).to_be_bytes()); // ver=1 + flags
            b.extend_from_slice(&1u32.to_be_bytes()); // sample count
            b.extend_from_slice(&data_off.to_be_bytes());

            // Sample duration (estimated from timescale/fps = 90000/30 = 3000)
            let dur = self.timescale as u32 / 30;
            b.extend_from_slice(&dur.to_be_bytes());

            // Sample size
            b.extend_from_slice(&(data_len as u32).to_be_bytes());

            // Sample flags
            let flags = if key { 0x0200_0000u32 } else { 0x0101_0000u32 };
            b.extend_from_slice(&flags.to_be_bytes());

            // CTO
            b.extend_from_slice(&cto.to_be_bytes());
            Self::box_full(b"trun", &b)
        };
        p.extend_from_slice(&trun);

        Self::box_full(b"traf", &p)
    }

    fn box_full(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let size = 8 + payload.len();
        let mut b = Vec::with_capacity(size);
        b.extend_from_slice(&(size as u32).to_be_bytes());
        b.extend_from_slice(tag);
        b.extend_from_slice(payload);
        b
    }
}

// ============================================================
// HTTP helpers
// ============================================================

fn list_windows() -> Vec<(u32, String, String)> {
    let script = r#"
import Foundation
import CoreGraphics
let w = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as! [[String:Any]]
for x in w { if let n = x[kCGWindowName as String] as? String, !n.isEmpty,
                let o = x[kCGWindowOwnerName as String] as? String,
                let l = x[kCGWindowLayer as String] as? NSNumber, l.intValue == 0 {
                  print("\(x[kCGWindowNumber as String] as! NSNumber) ||| \(o) ||| \(n)") } }
"#;
    let out = match Command::new("swift").arg("-e").arg(script).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !out.status.success() { return Vec::new(); }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
        let p: Vec<&str> = l.trim().split(" ||| ").collect();
        if p.len() >= 3 { Some((p[0].parse().ok()?, p[1].into(), p[2].into())) } else { None }
    }).collect()
}

fn html() -> String {
    r#"<!DOCTYPE html><html><meta charset="utf-8"><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><title>ScreenStream</title><style>*{margin:0;background:#000}body{display:flex;min-height:100vh;min-height:100dvh;align-items:center;justify-content:center}video{width:100%;max-height:100vh;max-height:100dvh}#b{position:fixed;bottom:0;left:0;right:0;display:flex;gap:12px;padding:3px 10px;background:rgba(0,0,0,.5);color:#aaa;font:11px/1.3 monospace;z-index:99;user-select:none}}.g{color:#4a4}.r{color:#c44}</style><body><video id=v autoplay muted playsinline></video><div id=b><span id=st class=g>loading</span></div><script>
let v=document.getElementById('v'),st=document.getElementById('st'),init=!1,m,ab,seg=0;
fetch('/init.mp4').then(r=>r.arrayBuffer()).then(b=>{
let d=new Uint8Array(b),p=-1;for(let i=0;i<d.length-4;i++){if(d[i]===0x61&&d[i+1]===0x76&&d[i+2]===0x63&&d[i+3]===0x43){p=i+5;break}}
let codecs='avc1.'+[d[p],d[p+1],d[p+2]].map(x=>x.toString(16).padStart(2,'0')).join('');
m=new MediaSource();m.onsourceopen=()=>{ab=m.addSourceBuffer('video/mp4;codecs="'+codecs+'"');ab.appendBuffer(b)};
v.src=URL.createObjectURL(m)
}).catch(()=>{st.textContent='no h264';st.className='r'});
(function p(){fetch('/seg?'+seg+'&'+Date.now()).then(r=>{
if(!r.ok){setTimeout(p,500);return}
seg++;return r.arrayBuffer()
}).then(b=>{if(b&&ab&&!ab.updating)try{ab.appendBuffer(b);st.textContent='h264';st.className='g'}catch(e){}setTimeout(p,30)}).catch(()=>setTimeout(p,1000))})()
</script>"#.into()
}

fn get_ip() -> String {
    Command::new("sh").arg("-c").arg("ipconfig getifaddr en0 2>/dev/null || echo 127.0.0.1")
        .output().map(|o| String::from_utf8_lossy(&o.stdout).trim().into()).unwrap_or_default()
}

// ---------- MJPEG stream (fallback) ----------

fn handle_mjpeg_client(mut stream: TcpStream, frame: Arc<ArcSwap<Vec<u8>>>, version: Arc<AtomicU64>, signal: Arc<(Mutex<()>, Condvar)>, stop: Arc<AtomicBool>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 4096];
    if stream.read(&mut buf).is_err() { return; }
    let req = String::from_utf8_lossy(&buf);
    if !req.starts_with("GET /stream") { return; }

    let _ = stream.set_nodelay(true);
    let header = "HTTP/1.1 200 OK\r\n\
                  Content-Type: multipart/x-mixed-replace; boundary=frame\r\n\
                  Cache-Control: no-cache\r\n\
                  Connection: close\r\n\r\n";
    if stream.write_all(header.as_bytes()).is_err() { return; }

    let mut last_version = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) { break; }
        let ver = version.load(Ordering::Acquire);
        if ver != last_version {
            let jpeg = frame.load_full();
            if !jpeg.is_empty() {
                let part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
                if stream.write_all(part.as_bytes()).is_err() { break; }
                if stream.write_all(&jpeg).is_err() { break; }
                if stream.write_all(b"\r\n").is_err() { break; }
                let _ = stream.flush();
                last_version = ver;
            }
        } else {
            let (mtx, cv) = &*signal;
            let guard = mtx.lock().unwrap();
            if version.load(Ordering::Acquire) != last_version {
                continue;
            }
            let _ = cv.wait_timeout(guard, Duration::from_millis(500)).unwrap();
        }
    }
}

// ============================================================
// main
// ============================================================

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u32 = 0;
    let mut max_w: u32 = 1280;
    let mut quality: u8 = 70;
    let mut fps: u32 = 30;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--window-id" => { i += 1; wid = args[i].parse().unwrap_or(0); }
            "--width" => { i += 1; max_w = args[i].parse().unwrap_or(1280); }
            "-q" | "--quality" => { i += 1; quality = args[i].parse().unwrap_or(70); }
            "--fps" => { i += 1; fps = args[i].parse().unwrap_or(30).clamp(1, 60); }
            "-l" | "--list" => {
                for (id, app, title) in list_windows() {
                    println!("{:>5} | {} | {}", id, app, title);
                }
                return;
            }
            "-h" | "--help" => {
                println!("screenstream [-l] [-w <id>] [--width <px>] [-q|--quality <1-100>] [--fps <1-60>]");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    if wid == 0 {
        let wins = list_windows();
        if wins.is_empty() { eprintln!("无窗口"); return; }
        let mut seen = std::collections::HashSet::new();
        let mut uq = Vec::new();
        for w in &wins { if seen.insert((&w.1, &w.2)) { uq.push(w); } }
        for (j, (_, a, t)) in uq.iter().enumerate() {
            println!("  [{:2}] {} - {}", j+1, a, if t.len()>55{&t[..55]}else{t});
        }
        print!("选择窗口 (1-{}): ", uq.len());
        std::io::stdout().flush().ok();
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).ok();
        if let Ok(n) = s.trim().parse::<usize>() {
            if n >= 1 && n <= uq.len() { wid = uq[n-1].0; }
        }
        if wid == 0 { return; }
    }

    let frame: Arc<ArcSwap<Vec<u8>>> = Arc::new(ArcSwap::from_pointee(Vec::new()));
    let frame_version = Arc::new(AtomicU64::new(0));
    let signal: Arc<(Mutex<()>, Condvar)> = Arc::new((Mutex::new(()), Condvar::new()));
    let stop = Arc::new(AtomicBool::new(false));

    // Shared video state for fMP4
    let video_init: Arc<ArcSwap<Vec<u8>>> = Arc::new(ArcSwap::from_pointee(Vec::new()));
    let video_segments: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let video_seg_count: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let svr_f = frame.clone();
    let svr_s = stop.clone();
    let svr_w = wid;
    let ip = get_ip();

    let srv_video_init = video_init.clone();
    let srv_video_segs = video_segments.clone();
    let srv_video_count = video_seg_count.clone();
    let srv = thread::spawn(move || {
        use tiny_http::{Header, Response};
        let server = match tiny_http::Server::http("0.0.0.0:8080") {
            Ok(s) => s,
            Err(e) => { eprintln!("端口 8080 被占用: {}", e); return; }
        };
        eprintln!("\n{:->50}", "");
        eprintln!("  ScreenStream  |  window {}  |  http://{}:8080", svr_w, ip);
        eprintln!("  Video stream  |             |  http://{}:8080/video.html", ip);
        eprintln!("  MJPEG fallback |             |  http://{}:8081/stream", ip);
        eprintln!("{:->50}\n", "");

        let hdr_ct_video = || "Content-Type: video/mp4".parse::<Header>().unwrap();
        let hdr_cache = || "Cache-Control: no-cache, no-store, must-revalidate".parse::<Header>().unwrap();
        let hdr_pragma = || "Pragma: no-cache".parse::<Header>().unwrap();
        let hdr_expires = || "Expires: 0".parse::<Header>().unwrap();
        let hdr_cors = || "Access-Control-Allow-Origin: *".parse::<Header>().unwrap();

        loop {
            if svr_s.load(Ordering::Relaxed) { break; }
            let req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r, _ => continue,
            };
            let url = req.url();
            let path = url.split('?').next().unwrap_or("/");
            let resp = match path {
                "/" | "/video.html" => Response::from_data(html().into_bytes())
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<Header>().unwrap()),
                "/init.mp4" => {
                    let data = srv_video_init.load_full();
                    if data.is_empty() {
                        let mut result = Response::from_data(Vec::new()).with_status_code(503);
                        for _ in 0..50 {
                            thread::sleep(Duration::from_millis(100));
                            let d = srv_video_init.load_full();
                            if !d.is_empty() {
                                result = Response::from_data(d.to_vec())
                                    .with_header(hdr_ct_video());
                                break;
                            }
                        }
                        result
                    } else {
                        Response::from_data(data.to_vec()).with_header(hdr_ct_video())
                    }
                }
                "/seg" => {
                    let after: u32 = url.split('?').nth(1).and_then(|q| {
                        q.split('&').next().and_then(|s| s.parse().ok())
                    }).unwrap_or(0);
                    let mut result = Response::from_data(Vec::new()).with_status_code(503);
                    for _ in 0..100 {
                        if svr_s.load(Ordering::Relaxed) { break; }
                        let current = srv_video_count.load(Ordering::Acquire);
                        if current > after {
                            let segs = srv_video_segs.lock().unwrap();
                            let idx = after as usize;
                            if idx < segs.len() {
                                result = Response::from_data(segs[idx].clone())
                                    .with_header(hdr_ct_video())
                                    .with_header(hdr_cache())
                                    .with_header(hdr_pragma())
                                    .with_header(hdr_expires())
                                    .with_header(hdr_cors());
                                break;
                            }
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    result
                }
                "/frame" => {
                    let j = svr_f.load_full();
                    if j.is_empty() { Response::from_data(Vec::new()).with_status_code(503) }
                    else {
                        Response::from_data(j.to_vec())
                            .with_header("Content-Type: image/jpeg".parse::<Header>().unwrap())
                            .with_header(hdr_cache())
                            .with_header(hdr_pragma())
                            .with_header(hdr_expires())
                            .with_header(hdr_cors())
                    }
                }
                _ => Response::from_data(Vec::new()).with_status_code(404),
            };
            req.respond(resp).ok();
        }
    });

    // MJPEG server
    let mjpeg_frame = frame.clone();
    let mjpeg_fv = frame_version.clone();
    let mjpeg_sig = signal.clone();
    let mjpeg_stop = stop.clone();
    let pool = ThreadPool::new(16);

    let mjpeg = thread::spawn(move || {
        let listener = match TcpListener::bind("0.0.0.0:8081") {
            Ok(l) => l,
            Err(e) => { eprintln!("端口 8081 被占用: {}", e); return; }
        };
        let _ = listener.set_nonblocking(true);
        loop {
            if mjpeg_stop.load(Ordering::Relaxed) { break; }
            match listener.accept() {
                Ok((stream, _)) => {
                    let f = mjpeg_frame.clone();
                    let v = mjpeg_fv.clone();
                    let sig = mjpeg_sig.clone();
                    let s = mjpeg_stop.clone();
                    pool.execute(move || handle_mjpeg_client(stream, f, v, sig, s));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    });

    let c_stop = stop.clone();
    let c_sig = signal.clone();
    static CTRLC_COUNT: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        if CTRLC_COUNT.swap(true, Ordering::Relaxed) {
            eprintln!("\n强制退出");
            std::process::exit(1);
        }
        eprintln!("\n⏳ 正在停止 (再按一次 Ctrl+C 强制退出)...");
        c_stop.store(true, Ordering::Relaxed);
        c_sig.1.notify_all();
    }).ok();

    let mut jpeg_buf = Vec::new();
    let mut fc: u64 = 0;
    let mut last = Instant::now();
    let mut fpc: u32 = 0;
    let frame_dur_active = Duration::from_secs_f64(1.0 / fps as f64);
    let mut frame_dur = frame_dur_active;
    let mut next_capture = Instant::now();
    let mut content_hash = 0u64;
    let mut idle_count = 0u32;

    // Determine capture dimensions from a test capture
    let (enc_w, enc_h) = match capture_cgimage(wid) {
        Some(cg) => {
            let w = cg.width() as u32;
            let h = cg.height() as u32;
            if w > 0 && h > 0 {
                if w > max_w {
                    let nh = (h * max_w) / w;
                    // Ensure even dimensions (VT requirement)
                    (max_w | 1, nh | 1) // |1 rounds up to odd, then we & !1 for even
                } else {
                    (w | 1, h | 1)
                }
            } else {
                (max_w, max_w * 9 / 16)
            }
        }
        None => (max_w, max_w * 9 / 16),
    };
    let (enc_w, enc_h) = (enc_w & !1, enc_h & !1); // force even

    let h264_encoder: Option<VtEncoder> = VtEncoder::new(enc_w, enc_h, fps).ok();
    let mut h264_fmp4: Option<Fmp4State> = None;
    let mut h264_frame_count: u64 = 0;
    let mut h264_ready = false;

    let vt_init = video_init.clone();
    let vt_segs = video_segments.clone();
    let vt_count = video_seg_count.clone();

    while !stop.load(Ordering::Relaxed) {
        // Single capture shared between JPEG and H.264
        let shared_cg = capture_cgimage(wid).and_then(|cg| {
            let (w, h) = (cg.width() as u32, cg.height() as u32);
            if w == 0 || h == 0 { return None; }
            if w > max_w {
                let nh = (h * max_w) / w;
                let cs = CGColorSpace::create_device_rgb();
                let ctx = CGContext::create_bitmap_context(
                    None, max_w as usize, nh as usize, 8, max_w as usize * 4, &cs,
                    kCGBitmapByteOrder32Big | kCGImageAlphaNoneSkipFirst,
                );
                ctx.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityHigh);
                let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(max_w as _, nh as _));
                ctx.draw_image(rect, &cg);
                ctx.create_image()
            } else {
                Some(cg)
            }
        });

        if let Some(ref cg) = shared_cg {
            // JPEG encode from shared CGImage
            if encode_native_jpeg(cg, quality, &mut jpeg_buf) {
                let h = {
                    let mut s = DefaultHasher::new();
                    s.write(&jpeg_buf);
                    s.finish()
                };
                if h == content_hash {
                    idle_count += 1;
                    if idle_count > 5 {
                        frame_dur = Duration::from_secs_f64(1.0);
                    }
                } else {
                    content_hash = h;
                    idle_count = 0;
                    frame_dur = frame_dur_active;
                    let jpeg = std::mem::take(&mut jpeg_buf);
                    frame.store(Arc::new(jpeg));
                    frame_version.fetch_add(1, Ordering::Release);
                    signal.1.notify_all();
                    fc += 1; fpc += 1;
                }
            }

            // H.264 encode from same CGImage
            if let Some(ref encoder) = h264_encoder {
                match encoder.encode_frame(cg, h264_frame_count, 30) {
                    Ok(frame) => {
                        h264_frame_count += 1;

                        if h264_fmp4.is_none() && frame.sps.is_some() && frame.pps.is_some() {
                            let sps = frame.sps.clone().unwrap();
                            let pps = frame.pps.clone().unwrap();
                            let fmp4 = Fmp4State::new(sps, pps, enc_w, enc_h, 90000);
                            let init_seg = fmp4.build_init_segment();
                            vt_init.store(Arc::new(init_seg));
                            h264_fmp4 = Some(fmp4);
                        }

                        if let Some(ref mut fmp4) = h264_fmp4 {
                            let seg = fmp4.build_media_segment(
                                &frame.data, frame.is_keyframe,
                                frame.pts_timescale, frame.pts_value,
                            );
                            let mut segs = vt_segs.lock().unwrap();
                            segs.push(seg);
                            if segs.len() > 300 { segs.remove(0); }
                            vt_count.fetch_add(1, Ordering::Release);
                            if !h264_ready {
                                h264_ready = true;
                                eprintln!("  H.264 stream ready");
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  H.264 encode error: {}", e);
                    }
                }
            }
        }
        let now = Instant::now();
        if now < next_capture {
            thread::sleep(next_capture - now);
        }
        next_capture += frame_dur;
        if next_capture < now {
            next_capture = now;
        }
        if last.elapsed() >= Duration::from_secs(5) {
            let fps = if fpc > 0 { fpc as f64 / last.elapsed().as_secs_f64() } else { 0.0 };
            eprintln!("  {:4.0} fps | {} frames | h264: {}", fps, fc, if h264_ready { "OK" } else { "starting" });
            fpc = 0; last = Instant::now();
        }
    }

    srv.join().ok();
    mjpeg.join().ok();
}
