use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::runtime::Runtime;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::TrackLocal;
use webrtc::media::Sample;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;

use crate::h264::H264Frame;

pub struct WebRtcHandle {
    pub offer: String,
    pc: Arc<tokio::sync::Mutex<RTCPeerConnection>>,
    track: Arc<TrackLocalStaticSample>,
    _rt: Runtime,
}

/// Convert AVCC H.264 data to annex-b format for WebRTC.
fn avcc_to_annexb(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 100);
    let mut pos = 0;
    while pos + 4 <= data.len() {
        let nal_size = u32::from_be_bytes([data[pos], data[pos+1], data[pos+2], data[pos+3]]) as usize;
        pos += 4;
        if pos + nal_size > data.len() { break; }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[pos..pos + nal_size]);
        pos += nal_size;
    }
    out
}

impl WebRtcHandle {
    pub fn new(stop: Arc<AtomicBool>) -> Result<Self, String> {
        let rt = Runtime::new().map_err(|e| e.to_string())?;

        let mut m = MediaEngine::default();
        m.register_default_codecs().map_err(|e| e.to_string())?;

        let api = APIBuilder::new()
            .with_media_engine(m)
            .build();

        let codec_cap = RTCRtpCodecCapability {
            mime_type: "video/H264".to_owned(),
            clock_rate: 90000,
            channels: 0,
            sdp_fmtp_line: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f".to_owned(),
            rtcp_feedback: vec![],
        };
        let track = Arc::new(TrackLocalStaticSample::new(
            codec_cap,
            "video".to_owned(),
            "screenstream".to_owned(),
        ));

        let config = RTCConfiguration::default();

        let (pc, offer_sdp) = rt.block_on(async {
            let pc = Arc::new(tokio::sync::Mutex::new(
                api.new_peer_connection(config).await.map_err(|e| e.to_string())?
            ));

            {
                let rtp_sender = pc.lock().await
                    .add_track(Arc::clone(&track) as Arc<dyn TrackLocal + Send + Sync>)
                    .await
                    .map_err(|e| e.to_string())?;

                tokio::spawn(async move {
                    let mut buf = vec![0u8; 1500];
                    while let Ok((_, _)) = rtp_sender.read(&mut buf).await {}
                });
            }

            let offer = pc.lock().await
                .create_offer(None)
                .await
                .map_err(|e| e.to_string())?;

            pc.lock().await
                .set_local_description(offer.clone())
                .await
                .map_err(|e| e.to_string())?;

            // Wait for ICE gathering complete using a callback
            let (ice_done_tx, mut ice_done_rx) = tokio::sync::mpsc::unbounded_channel();
            {
                let pc_ref = pc.lock().await;
                pc_ref.on_ice_candidate(Box::new(move |c| {
                    let tx = ice_done_tx.clone();
                    Box::pin(async move {
                        if c.is_none() { // GatheringComplete
                            let _ = tx.send(());
                        }
                    })
                }));
            }
            // Wait for gathering complete with timeout
            tokio::time::timeout(Duration::from_secs(3), ice_done_rx.recv()).await.ok();

            let offer_sdp = pc.lock().await
                .local_description().await
                .ok_or("no local desc")?
                .sdp.clone();

            let pc_c = pc.clone();
            let stop_c = stop.clone();
            tokio::spawn(async move {
                while !stop_c.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                pc_c.lock().await.close().await.ok();
            });

            Ok::<_, String>((pc, offer_sdp))
        })?;

        Ok(WebRtcHandle { offer: offer_sdp, pc, track, _rt: rt })
    }

    pub fn set_answer(&self, answer_sdp: String) -> Result<(), String> {
        let pc = self.pc.clone();
        self._rt.block_on(async {
            use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
            let answer = RTCSessionDescription::answer(answer_sdp).map_err(|e| e.to_string())?;
            pc.lock().await
                .set_remote_description(answer)
                .await
                .map_err(|e| e.to_string())
        })
    }

    pub fn add_candidate(&self, candidate: &str) -> Result<(), String> {
        let pc = self.pc.clone();
        let c = candidate.to_string();
        self._rt.block_on(async {
            use webrtc::peer_connection::sdp::sdp_type::RTCSdpType;
            use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
            let init = RTCIceCandidateInit {
                candidate: c,
                sdp_mid: None,
                sdp_mline_index: None,
                username_fragment: None,
            };
            pc.lock().await.add_ice_candidate(init).await.map_err(|e| e.to_string())
        })
    }

    pub fn send_frame(&self, frame: &H264Frame) -> Result<(), String> {
        let mut data = Vec::new();
        if frame.is_keyframe {
            if let Some(ref sps) = frame.sps {
                data.extend_from_slice(&[0, 0, 0, 1]);
                data.extend_from_slice(sps);
            }
            if let Some(ref pps) = frame.pps {
                data.extend_from_slice(&[0, 0, 0, 1]);
                data.extend_from_slice(pps);
            }
        }
        data.extend_from_slice(&avcc_to_annexb(&frame.data));

        let sample = Sample {
            data: bytes::Bytes::from(data),
            duration: Duration::from_secs_f64(1.0 / 30.0),
            ..Default::default()
        };

        let track = self.track.clone();
        self._rt.block_on(async move {
            track.write_sample(&sample).await.map_err(|e| e.to_string())
        })
    }
}
