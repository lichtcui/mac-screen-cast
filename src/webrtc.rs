use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use rustrtc::config::MediaCapabilities;
use rustrtc::media::{MediaKind, VideoFrame, sample_track};
use rustrtc::{
    IceCandidate, PeerConnection, RtcConfiguration, IceServer, RtpCodecParameters,
    SdpType, SessionDescription,
};
use tokio::runtime::Runtime;

use crate::h264::H264Frame;

const RTP_CLOCK_RATE: u32 = 90000;
const RTP_TS_PER_FRAME: u32 = RTP_CLOCK_RATE / 30; // 3000 for 30fps
const RTP_MTU: usize = 1200;

pub struct WebRtcHandle {
    pub offer: String,
    pc: PeerConnection,
    sample_source: rustrtc::media::track::SampleStreamSource,
    rtp_timestamp: AtomicU32,
    _rt: Runtime,
}

impl WebRtcHandle {
    pub fn new(stop: Arc<AtomicBool>) -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;

        let (pc, sample_source, offer_sdp) = rt.block_on(async {
            // H.264 codec capability matching current SDP fmtp
            let mut h264 = rustrtc::config::VideoCapability::h264();
            h264.fmtp = Some(
                "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                    .to_string(),
            );

            let mut caps = MediaCapabilities::default();
            caps.video = vec![h264];

            let config = RtcConfiguration {
                ice_servers: vec![IceServer::new(vec![
                    "stun:stun.l.google.com:19302".to_owned(),
                ])],
                media_capabilities: Some(caps),
                ..Default::default()
            };

            let pc = PeerConnection::new(config);

            // Create video track
            let (source, track, _feedback_rx) = sample_track(MediaKind::Video, 120);

            // Add track with H.264 RTP parameters
            pc.add_track(
                track,
                RtpCodecParameters {
                    payload_type: 96,
                    clock_rate: RTP_CLOCK_RATE,
                    channels: 0,
                },
            )
            .map_err(|e| e.to_string())?;

            // Create offer, set local description, wait for ICE gathering
            let offer = pc.create_offer().await.map_err(|e| e.to_string())?;
            pc.set_local_description(offer)
                .map_err(|e| e.to_string())?;

            tokio::time::timeout(Duration::from_secs(3), pc.wait_for_gathering_complete())
                .await
                .ok();

            let offer_sdp = pc
                .local_description()
                .ok_or("no local desc")?
                .to_sdp_string();

            // Spawn stop monitor: close PC when stop flag is set
            let pc_c = pc.clone();
            let stop_c = stop.clone();
            tokio::spawn(async move {
                while !stop_c.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                pc_c.close();
            });

            Ok::<_, String>((pc, source, offer_sdp))
        })?;

        Ok(WebRtcHandle {
            offer: offer_sdp,
            pc,
            sample_source,
            rtp_timestamp: AtomicU32::new(0),
            _rt: rt,
        })
    }

    pub fn set_answer(&self, answer_sdp: String) -> Result<(), String> {
        let pc = self.pc.clone();
        self._rt.block_on(async {
            let answer =
                SessionDescription::parse(SdpType::Answer, &answer_sdp).map_err(|e| e.to_string())?;
            pc.set_remote_description(answer)
                .await
                .map_err(|e| e.to_string())
        })
    }

    pub fn add_candidate(
        &self,
        candidate: &str,
        _sdp_mid: Option<String>,
        _sdp_mline_index: Option<u16>,
    ) -> Result<(), String> {
        let ice_candidate =
            IceCandidate::from_sdp(candidate).map_err(|e| format!("ICE parse error: {}", e))?;
        self.pc
            .add_ice_candidate(ice_candidate)
            .map_err(|e| e.to_string())
    }

    pub fn send_frame(&self, frame: &H264Frame) -> Result<(), String> {
        let mut nal_units: Vec<(Vec<u8>, bool)> = Vec::new();

        // SPS/PPS as separate NAL units before keyframes
        if frame.is_keyframe {
            if let Some(ref sps) = frame.sps {
                nal_units.push((sps.clone(), false));
            }
            if let Some(ref pps) = frame.pps {
                nal_units.push((pps.clone(), false));
            }
        }

        // Parse AVCC NAL units (4-byte length prefix per NAL unit)
        let data = &frame.data;
        let mut pos = 0;
        while pos + 4 <= data.len() {
            let nal_size =
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize;
            pos += 4;
            if pos + nal_size > data.len() {
                break;
            }
            let is_last = pos + nal_size >= data.len();
            nal_units.push((data[pos..pos + nal_size].to_vec(), is_last));
            pos += nal_size;
        }

        let ts = self
            .rtp_timestamp
            .fetch_add(RTP_TS_PER_FRAME, Ordering::Relaxed);

        let mut frames: Vec<VideoFrame> = Vec::new();

        for (nal_data, is_last_nal) in &nal_units {
            if nal_data.is_empty() {
                continue;
            }
            let nal_header = nal_data[0];
            let nal_payload = &nal_data[1..];
            let total_size = 1 + nal_payload.len();

            if total_size <= RTP_MTU {
                // Single NAL unit packet
                frames.push(VideoFrame {
                    rtp_timestamp: ts,
                    data: Bytes::from(nal_data.clone()),
                    is_last_packet: *is_last_nal,
                    ..Default::default()
                });
            } else {
                // FU-A fragmentation (RFC 6184)
                let fu_indicator = 0x1c | (nal_header & 0x60); // FU-A type + NRI
                let nal_type = nal_header & 0x1f;
                let max_chunk = RTP_MTU - 2; // FU indicator + FU header
                let mut offset = 0;

                while offset < nal_payload.len() {
                    let chunk_end = (offset + max_chunk).min(nal_payload.len());
                    let chunk = &nal_payload[offset..chunk_end];
                    let is_first = offset == 0;
                    let is_last = chunk_end >= nal_payload.len();

                    let fu_header = (if is_first { 0x80 } else { 0 })
                        | (if is_last { 0x40 } else { 0 })
                        | nal_type;

                    let mut fu_payload = Vec::with_capacity(2 + chunk.len());
                    fu_payload.push(fu_indicator);
                    fu_payload.push(fu_header);
                    fu_payload.extend_from_slice(chunk);

                    frames.push(VideoFrame {
                        rtp_timestamp: ts,
                        data: Bytes::from(fu_payload),
                        is_last_packet: is_last && *is_last_nal,
                        ..Default::default()
                    });

                    offset = chunk_end;
                }
            }
        }

        let source = self.sample_source.clone();
        self._rt.block_on(async move {
            for vf in frames {
                source.send_video(vf).await.map_err(|e| e.to_string())?;
            }
            Ok(())
        })
    }

    pub fn close(&self) {
        self.pc.close();
    }
}
