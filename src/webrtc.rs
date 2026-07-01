use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use rustrtc::config::MediaCapabilities;
use rustrtc::media::{MediaKind, VideoFrame, sample_track};
use rustrtc::{
    IceCandidate, PeerConnection, RtcConfiguration, RtpCodecParameters,
    SdpType, SessionDescription,
};
use tokio::runtime::Handle;

use crate::h264::H264Frame;

const RTP_CLOCK_RATE: u32 = 90000;
const RTP_MTU: usize = 1200;

/// RTP packets for one frame — sent from caller thread to WebRTC Tokio task.
struct RtpBatch {
    ts: u32,
    packets: Vec<(Vec<u8>, bool)>,
}

pub struct WebRtcHandle {
    pub offer: String,
    pc: PeerConnection,
    rtp_timestamp: Arc<AtomicU32>,
    rtp_ts_per_frame: u32,
    rt: Handle,
    stop_notify: Arc<tokio::sync::Notify>,
    rtp_tx: tokio::sync::mpsc::Sender<RtpBatch>,
    rtp_buf: Mutex<Vec<(Vec<u8>, bool)>>,
}

impl Clone for WebRtcHandle {
    fn clone(&self) -> Self {
        WebRtcHandle {
            offer: self.offer.clone(),
            pc: self.pc.clone(),
            rtp_timestamp: self.rtp_timestamp.clone(),
            rtp_ts_per_frame: self.rtp_ts_per_frame,
            rt: self.rt.clone(),
            stop_notify: self.stop_notify.clone(),
            rtp_tx: self.rtp_tx.clone(),
            rtp_buf: Mutex::new(Vec::with_capacity(32)),
        }
    }
}

impl WebRtcHandle {
    pub fn new(rt: Handle, fps: u32, width: u32, height: u32) -> Result<Self, String> {
        let level = |pixels: u32| -> &str {
            match pixels {
                p if p <= 1280 * 720 => "1f", // Level 3.1
                p if p <= 1920 * 1080 => "29", // Level 4.1
                _ => "32",                     // Level 5.0
            }
        };
        let profile_level_id = format!("42e0{}", level(width * height));

        let stop_notify = Arc::new(tokio::sync::Notify::new());

        let (pc, sample_source, offer_sdp) = rt.block_on({
            let notify_c = stop_notify.clone();
            async move {
                let mut h264 = rustrtc::config::VideoCapability::h264();
                h264.fmtp = Some(format!(
                    "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={}",
                    profile_level_id,
                ));

                let caps = MediaCapabilities {
                    video: vec![h264],
                    ..Default::default()
                };

                let config = RtcConfiguration {
                    ice_servers: vec![],
                    media_capabilities: Some(caps),
                    ..Default::default()
                };

                let pc = PeerConnection::new(config);

                let (source, track, feedback_rx) = sample_track(MediaKind::Video, 120);

                // Drain the feedback channel to prevent backpressure on the sender side.
                tokio::spawn(async move {
                    let mut rx = feedback_rx;
                    while rx.recv().await.is_some() {}
                });

                pc.add_track(
                    track,
                    RtpCodecParameters {
                        payload_type: 96,
                        clock_rate: RTP_CLOCK_RATE,
                        channels: 0,
                    },
                )
                .map_err(|e| e.to_string())?;

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

                let pc_c = pc.clone();
                tokio::spawn(async move {
                    notify_c.notified().await;
                    pc_c.close();
                });

                Ok::<_, String>((pc, source, offer_sdp))
            }
        })?;

        // Channel for RTP batches: caller thread → Tokio WebRTC send task.
        // Capacity 8 keeps latency bounded while smoothing brief send bursts.
        let (rtp_tx, mut rtp_rx) = tokio::sync::mpsc::channel::<RtpBatch>(8);

        let source_for_task = sample_source.clone();
        rt.spawn(async move {
            while let Some(batch) = rtp_rx.recv().await {
                for (payload, is_last) in batch.packets {
                    if let Err(e) = source_for_task
                        .send_video(VideoFrame {
                            rtp_timestamp: batch.ts,
                            data: Bytes::from(payload),
                            is_last_packet: is_last,
                            ..Default::default()
                        })
                        .await
                    {
                        eprintln!("  WebRTC send error: {}", e);
                    }
                }
            }
        });

        Ok(WebRtcHandle {
            offer: offer_sdp,
            pc,
            rtp_timestamp: Arc::new(AtomicU32::new(0)),
            rtp_ts_per_frame: RTP_CLOCK_RATE / fps.max(1),
            rt,
            stop_notify,
            rtp_tx,
            rtp_buf: Mutex::new(Vec::with_capacity(32)),
        })
    }

    pub fn set_answer(&self, answer_sdp: String) -> Result<(), String> {
        let pc = self.pc.clone();
        self.rt.block_on(async {
            let answer =
                SessionDescription::parse(SdpType::Answer, &answer_sdp).map_err(|e| e.to_string())?;
            pc.set_remote_description(answer)
                .await
                .map_err(|e| e.to_string())
        })
    }

    pub fn add_candidate(&self, candidate: &str) -> Result<(), String> {
        let ice_candidate =
            IceCandidate::from_sdp(candidate).map_err(|e| format!("ICE parse error: {}", e))?;
        self.pc
            .add_ice_candidate(ice_candidate)
            .map_err(|e| e.to_string())
    }

    /// Close the PeerConnection inside the Tokio runtime context.
    /// Safe to call from any thread.
    pub fn close(&self) {
        self.stop_notify.notify_one();
        let pc = self.pc.clone();
        self.rt.block_on(async move {
            pc.close();
        });
    }

    pub fn send_frame(&self, frame: &H264Frame) -> Result<(), String> {
        let mut rtp_packets = self
            .rtp_buf
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        rtp_packets.clear();

        if frame.is_keyframe {
            if let Some(ref sps) = frame.sps {
                rtp_packets.extend(packetize_nal(sps.clone(), false, RTP_MTU));
            }
            if let Some(ref pps) = frame.pps {
                rtp_packets.extend(packetize_nal(pps.clone(), false, RTP_MTU));
            }
        }

        for (nal, is_last) in crate::h264::avcc_nal_units(&frame.data) {
            if frame.is_keyframe {
                match nal.first().map(|b| b & 0x1f) {
                    Some(7) | Some(8) => continue,
                    _ => {}
                }
            }
            rtp_packets.extend(packetize_nal(nal, is_last, RTP_MTU));
        }

        let ts = self
            .rtp_timestamp
            .fetch_add(self.rtp_ts_per_frame, Ordering::Relaxed);

        // Move the packets out of the reused buffer into the channel.
        // The Tokio task takes ownership; next frame gets a fresh clear.
        let batch = RtpBatch {
            ts,
            packets: std::mem::take(&mut *rtp_packets),
        };
        drop(rtp_packets);

        self.rtp_tx
            .blocking_send(batch)
            .map_err(|e| format!("RTP channel closed: {}", e))
    }
}

impl Drop for WebRtcHandle {
    fn drop(&mut self) {
        self.stop_notify.notify_one();
    }
}

/// Packetize a NAL unit into one or more RTP payloads.
///
/// NAL units smaller than `mtu` are sent as a single packet.
/// Larger units are fragmented using FU-A (RFC 6184).
pub(crate) fn packetize_nal(
    data: Vec<u8>,
    is_last_nal: bool,
    mtu: usize,
) -> Vec<(Vec<u8>, bool)> {
    if data.is_empty() {
        return vec![];
    }
    if data.len() <= mtu {
        return vec![(data, is_last_nal)];
    }
    let nal_header = data[0];
    let fu_indicator = 0x1c | (nal_header & 0x60);
    let nal_type = nal_header & 0x1f;
    let max_chunk = mtu - 2;
    let num_fragments = (data.len() - 1).div_ceil(max_chunk);
    let mut packets = Vec::with_capacity(num_fragments);
    let mut offset = 1;
    while offset < data.len() {
        let chunk_end = (offset + max_chunk).min(data.len());
        let chunk = &data[offset..chunk_end];
        let is_first = offset == 1;
        let is_last_fragment = chunk_end >= data.len();
        let fu_header = (if is_first { 0x80 } else { 0 })
            | (if is_last_fragment { 0x40 } else { 0 })
            | nal_type;
        let mut payload = Vec::with_capacity(2 + chunk.len());
        payload.push(fu_indicator);
        payload.push(fu_header);
        payload.extend_from_slice(chunk);
        packets.push((payload, is_last_fragment && is_last_nal));
        offset = chunk_end;
    }
    packets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packetize_empty() {
        assert!(packetize_nal(vec![], false, 1200).is_empty());
    }

    #[test]
    fn packetize_single_small() {
        let nal = vec![0x41, 0x01, 0x02, 0x03];
        let packets = packetize_nal(nal, true, 1200);
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].0, &[0x41, 0x01, 0x02, 0x03]);
        assert!(packets[0].1);
    }

    #[test]
    fn packetize_exact_mtu() {
        let mut nal = vec![0x41];
        nal.extend(std::iter::repeat_n(0xFF, 1199));
        let packets = packetize_nal(nal, true, 1200);
        assert_eq!(packets.len(), 1);
    }

    #[test]
    fn packetize_is_last_propagated() {
        let nal = vec![0x41, 0x01, 0x02, 0x03];
        let packets = packetize_nal(nal, false, 1200);
        assert_eq!(packets.len(), 1);
        assert!(!packets[0].1);
    }

    #[test]
    fn packetize_fua_basic() {
        let nal_type = 0x65;
        let mut nal = vec![nal_type];
        nal.extend(std::iter::repeat_n(0xFF, 2400));
        let packets = packetize_nal(nal, true, 1200);
        assert!(packets.len() >= 2, "should be fragmented into {} packets", packets.len());
        // All packets ≤ MTU
        for p in &packets {
            assert!(p.0.len() <= 1200, "packet too large: {}", p.0.len());
        }
        // First: FU-A start bit
        assert_ne!(packets[0].0[1] & 0x80, 0);
        assert!(!packets[0].1);
        // Last: FU-A end bit
        assert_ne!(packets.last().unwrap().0[1] & 0x40, 0);
        assert!(packets.last().unwrap().1);
    }

    #[test]
    fn packetize_fua_nal_type_preserved() {
        let mut nal = vec![0x21]; // NRI=1, type=1
        nal.extend(std::iter::repeat_n(0xFF, 2000));
        let packets = packetize_nal(nal, true, 1200);
        for p in &packets {
            assert_eq!(p.0[1] & 0x1f, 1, "NAL type should be 1");
        }
    }

    #[test]
    fn packetize_fua_is_last_nal_false() {
        let mut nal = vec![0x65];
        nal.extend(std::iter::repeat_n(0xFF, 2400));
        let packets = packetize_nal(nal, false, 1200);
        assert!(!packets.last().unwrap().1);
    }

    #[test]
    fn packetize_fua_three_fragments() {
        // 2401 byte NAL → 3 FU-A fragments (max_chunk=1198)
        let mut nal = vec![0x41];
        nal.extend(std::iter::repeat_n(0xAA, 2400));
        let packets = packetize_nal(nal, true, 1200);
        assert_eq!(packets.len(), 3);
        assert_ne!(packets[0].0[1] & 0x80, 0); // start
        assert_eq!(packets[0].0[1] & 0x40, 0);
        assert_eq!(packets[1].0[1] & 0xC0, 0); // continuation (no S/E)
        assert_eq!(packets[2].0[1] & 0x80, 0);
        assert_ne!(packets[2].0[1] & 0x40, 0); // end
    }

    #[test]
    fn packetize_fua_mtu_plus_one() {
        // NAL exactly 1 byte larger than MTU → two FU-A fragments
        let mut nal = vec![0x65];
        nal.extend(std::iter::repeat_n(0xFF, 1200)); // total 1201 bytes
        let packets = packetize_nal(nal, true, 1200);
        assert_eq!(packets.len(), 2, "MTU+1 should produce 2 fragments");
        // First fragment: FU-A start bit set, max_chunk=1198 bytes + 2 header = 1200
        assert_eq!(packets[0].0.len(), 1200);
        assert_ne!(packets[0].0[1] & 0x80, 0); // start
        assert_eq!(packets[0].0[1] & 0x40, 0);
        // Second fragment: FU-A end bit set, remaining 2 bytes
        assert_eq!(packets[1].0.len(), 2 + 2); // 2 remaining bytes + 2 header
        assert_eq!(packets[1].0[1] & 0x80, 0);
        assert_ne!(packets[1].0[1] & 0x40, 0); // end
        assert!(packets[1].1); // is_last propagated
    }
}
