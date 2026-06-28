use std::ffi::c_void;
use std::sync::Mutex;

use videotoolbox::compression::{CompressionSession, EncodedFrame};
use videotoolbox::session::Codec;

#[derive(Clone)]
pub struct H264Frame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts_timescale: i32,
    pub pts_value: i64,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

struct SpsPpsCache {
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

pub struct VtEncoder {
    session: CompressionSession,
    cache: Mutex<SpsPpsCache>,
}

unsafe impl Send for VtEncoder {}
unsafe impl Sync for VtEncoder {}

extern "C" {
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: *mut c_void,
        idx: usize,
        ptr_out: *mut *const u8,
        size_out: *mut usize,
        count_out: *mut usize,
        nal_len_out: *mut i32,
    ) -> i32;
}

impl VtEncoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let session = CompressionSession::builder(width as i32, height as i32, Codec::H264)
            .with_real_time(true)
            .with_allow_frame_reordering(false)
            .with_expected_frame_rate(fps as f64)
            .with_max_keyframe_interval((fps * 2) as i32)
            .with_average_bit_rate(4_000_000)
            .build()
            .map_err(|e| format!("CompressionSession init failed: {}", e))?;
        Ok(VtEncoder {
            session,
            cache: Mutex::new(SpsPpsCache { sps: None, pps: None }),
        })
    }

    pub fn encode(
        &self,
        surface: &apple_cf::iosurface::IOSurface,
        frame_num: u64,
        fps: i32,
    ) -> Result<H264Frame, String> {
        let encoded = self
            .session
            .encode(surface, (frame_num as i64, fps))
            .map_err(|e| format!("VT encode failed: {}", e))?;

        let is_keyframe = scan_for_idr(&encoded.data);

        if is_keyframe {
            let (sps, pps) = extract_sps_pps(&encoded);
            if let Ok(mut cache) = self.cache.lock() {
                cache.sps = sps;
                cache.pps = pps;
            }
        }

        let (sps, pps) = if is_keyframe {
            let cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            (cache.sps.clone(), cache.pps.clone())
        } else {
            (None, None)
        };

        Ok(H264Frame {
            data: encoded.data,
            is_keyframe,
            pts_timescale: encoded.presentation_time.1,
            pts_value: encoded.presentation_time.0,
            sps,
            pps,
        })
    }
}

fn extract_sps_pps(encoded: &EncodedFrame) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    let sb = match encoded.cm_sample_buffer() {
        Some(sb) => sb,
        None => return (None, None),
    };

    let fmt_desc: *mut c_void = unsafe {
        videotoolbox::ffi::CMSampleBufferGetFormatDescription(sb.as_ptr().cast())
    } as *mut c_void;
    if fmt_desc.is_null() {
        return (None, None);
    }

    unsafe {
        let mut sps_ptr: *const u8 = std::ptr::null();
        let mut sps_size: usize = 0;
        let mut param_count: usize = 0;
        let mut _nal_len: i32 = 0;

        let mut sps = None;
        let mut pps = None;

        if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
            fmt_desc,
            0,
            &mut sps_ptr,
            &mut sps_size,
            &mut param_count,
            &mut _nal_len,
        ) == 0
            && !sps_ptr.is_null()
        {
            sps = Some(std::slice::from_raw_parts(sps_ptr, sps_size).to_vec());
        }

        if param_count > 1 {
            let mut pps_ptr: *const u8 = std::ptr::null();
            let mut pps_size: usize = 0;
            let mut _pc: usize = 0;
            if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                fmt_desc,
                1,
                &mut pps_ptr,
                &mut pps_size,
                &mut _pc,
                &mut _nal_len,
            ) == 0
                && !pps_ptr.is_null()
            {
                pps = Some(std::slice::from_raw_parts(pps_ptr, pps_size).to_vec());
            }
        }

        (sps, pps)
    }
}

fn scan_for_idr(data: &[u8]) -> bool {
    let mut pos = 0;
    while pos + 5 <= data.len() {
        let nal_sz = u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]) as usize;
        if nal_sz == 0 || pos + 4 + nal_sz > data.len() {
            break;
        }
        if (data[pos + 4] & 0x1f) == 5 {
            return true;
        }
        pos += 4 + nal_sz;
    }
    false
}

/// Parse AVCC-format data into NAL units (4-byte length prefix per NAL).
/// Returns `Vec` of `(nal_unit_data, is_last)` pairs.
pub fn avcc_nal_units(data: &[u8]) -> Vec<(Vec<u8>, bool)> {
    let mut units = Vec::new();
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let nal_size = u32::from_be_bytes([
            data[pos],
            data[pos + 1],
            data[pos + 2],
            data[pos + 3],
        ]) as usize;
        pos += 4;
        if pos + nal_size > data.len() {
            break;
        }
        let is_last = pos + nal_size >= data.len();
        units.push((data[pos..pos + nal_size].to_vec(), is_last));
        pos += nal_size;
    }
    units
}
