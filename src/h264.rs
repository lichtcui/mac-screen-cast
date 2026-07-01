use std::ffi::c_void;

use videotoolbox::compression::{CompressionSession, EncodedFrame, ProfileLevel};
use videotoolbox::session::Codec;

#[derive(Clone)]
pub struct H264Frame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

pub struct VtEncoder {
    session: CompressionSession,
}

// SAFETY: VideoToolbox CompressionSession handles (`VTCompressionSessionRef`) are
// documented as thread-safe by Apple. All internal state is protected by the
// framework. This is safe even though `CompressionSession` doesn't implement
// `Send`/`Sync` itself.
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
        // Heuristic: 0.07 bits per pixel per frame, clamped to [500 Kbps, 10 Mbps]
        let bitrate = ((width * height * fps * 7) / 100)
            .clamp(500_000, 10_000_000);
        let session = CompressionSession::builder(width as i32, height as i32, Codec::H264)
            .with_profile_level(ProfileLevel::H264ConstrainedBaselineAutoLevel)
            .with_real_time(true)
            .with_allow_frame_reordering(false)
            .with_expected_frame_rate(fps as f64)
            .with_max_keyframe_interval((fps * 2) as i32)
            .with_average_bit_rate(bitrate as i32)
            .build()
            .map_err(|e| format!("CompressionSession init failed: {}", e))?;
        Ok(VtEncoder { session })
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

        let is_keyframe = (encoded.info_flags & 1) != 0;

        let (sps, pps) = if is_keyframe {
            extract_sps_pps(&encoded)
        } else {
            (None, None)
        };

        Ok(H264Frame {
            data: encoded.data,
            is_keyframe,
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

    // SAFETY: CMSampleBufferGetFormatDescription is a CoreMedia C API.
    // `sb` is a valid CMSampleBuffer from the encoder output (checked above).
    let fmt_desc: *mut c_void = unsafe {
        videotoolbox::ffi::CMSampleBufferGetFormatDescription(sb.as_ptr().cast())
    } as *mut c_void;
    if fmt_desc.is_null() {
        return (None, None);
    }

    // SAFETY: This FFI reads SPS/PPS parameter sets from a CoreMedia format
    // description under the same object lifetime that the buffer is valid.
    // The returned pointers point into the format description's stable backing
    // memory and are safe to dereference within this scope.
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

#[cfg(test)]
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
        if nal_size == 0 || pos + nal_size > data.len() {
            break;
        }
        let is_last = pos + nal_size >= data.len();
        units.push((data[pos..pos + nal_size].to_vec(), is_last));
        pos += nal_size;
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avcc(nal_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut nal: Vec<u8> = Vec::with_capacity(1 + payload.len());
        nal.push(nal_type);
        nal.extend_from_slice(payload);
        let len = (nal.len() as u32).to_be_bytes();
        [len.as_slice(), &nal].concat()
    }

    #[test]
    fn avcc_nal_units_empty() {
        assert!(avcc_nal_units(&[]).is_empty());
    }

    #[test]
    fn avcc_nal_units_single() {
        let data = avcc(0x41, &[1, 2, 3, 4]);
        let units = avcc_nal_units(&data);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].0, &[0x41, 1, 2, 3, 4]);
        assert!(units[0].1);
    }

    #[test]
    fn avcc_nal_units_multiple() {
        let n1 = avcc(0x41, &[1, 2]);
        let n2 = avcc(0x41, &[3, 4, 5]);
        let data = [n1.as_slice(), n2.as_slice()].concat();
        let units = avcc_nal_units(&data);
        assert_eq!(units.len(), 2);
        assert!(!units[0].1);
        assert_eq!(units[0].0, &[0x41, 1, 2]);
        assert!(units[1].1);
        assert_eq!(units[1].0, &[0x41, 3, 4, 5]);
    }

    #[test]
    fn avcc_nal_units_truncated() {
        let data = [0x00, 0x00, 0x00, 0x0A, 0x41, 0x01, 0x02];
        assert!(avcc_nal_units(&data).is_empty());
    }

    #[test]
    fn avcc_nal_units_zero_length() {
        // Zero-length NAL at the end should stop parsing
        let data = [0x00, 0x00, 0x00, 0x05, 0x41, 0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00];
        let units = avcc_nal_units(&data);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].0, &[0x41, 0x01, 0x02, 0x03, 0x04]);
    }

    #[test]
    fn avcc_nal_units_zero_length_mid() {
        // Zero-length NAL in the middle should stop parsing
        let n1 = avcc(0x41, &[1, 2, 3]);
        let zero = [0x00, 0x00, 0x00, 0x00];
        let n2 = avcc(0x41, &[4, 5, 6]);
        let data = [n1.as_slice(), zero.as_slice(), n2.as_slice()].concat();
        let units = avcc_nal_units(&data);
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].0, &[0x41, 1, 2, 3]);
    }

    #[test]
    fn scan_for_idr_true() {
        let data = avcc(0x65, &[0x88, 0x84]);
        assert!(scan_for_idr(&data));
    }

    #[test]
    fn scan_for_idr_false() {
        let data = avcc(0x41, &[1, 2, 3, 4]);
        assert!(!scan_for_idr(&data));
    }

    #[test]
    fn scan_for_idr_among_multiple() {
        let n1 = avcc(0x41, &[1, 2]);
        let n2 = avcc(0x65, &[0x88]);
        let data = [n1.as_slice(), n2.as_slice()].concat();
        assert!(scan_for_idr(&data));
    }

    #[test]
    fn scan_for_idr_early_exit() {
        let n1 = avcc(0x65, &[0x88]);
        let n2 = avcc(0x41, &[1, 2, 3]);
        let data = [n1.as_slice(), n2.as_slice()].concat();
        assert!(scan_for_idr(&data));
    }

    #[test]
    fn scan_for_idr_empty() {
        assert!(!scan_for_idr(&[]));
    }
}
