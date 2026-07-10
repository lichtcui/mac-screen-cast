use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use screencapturekit::prelude::*;

use crate::capture::{FrameBuffer, FrameBufferHandle, ScreenCapture};
#[cfg(target_os = "macos")]
use crate::capture::macos::MacosCapture;
use crate::h264;
use crate::server;
use crate::webrtc;

/// Shared mutable state accessible across threads.
pub struct SharedState {
    pub stop: Arc<AtomicBool>,
    pub ctrlc_count: Arc<AtomicBool>,
    pub frame_count: Arc<AtomicU64>,
    pub latest_latency: Arc<AtomicU64>,
    pub wr_handle: Arc<RwLock<Option<webrtc::WebRtcHandle>>>,
    pub wr_version: Arc<AtomicU64>,
    pub wr_connected: Arc<AtomicBool>,
    pub webrtc_rt: Arc<tokio::runtime::Runtime>,
}

impl SharedState {
    pub fn new() -> Self {
        let webrtc_rt = Arc::new(
            tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime for WebRTC"),
        );
        SharedState {
            stop: Arc::new(AtomicBool::new(false)),
            ctrlc_count: Arc::new(AtomicBool::new(false)),
            frame_count: Arc::new(AtomicU64::new(0)),
            latest_latency: Arc::new(AtomicU64::new(0)),
            wr_handle: Arc::new(RwLock::new(None)),
            wr_version: Arc::new(AtomicU64::new(0)),
            wr_connected: Arc::new(AtomicBool::new(false)),
            webrtc_rt,
        }
    }
}

/// Configuration for the full capture pipeline.
pub struct PipelineConfig {
    pub out_w: u32,
    pub out_h: u32,
    pub fps: u32,
    pub port: u16,
    pub window_name: String,
}

/// Spawn a background thread that pulls `FrameBuffer`s from `frame_rx`,
/// encodes them with the given hardware encoder, and forwards the resulting
/// `H264Frame` through `frame_tx`.
fn spawn_encoder_thread(
    encoder: Arc<h264::VtEncoder>,
    frame_tx: mpsc::SyncSender<(h264::H264Frame, Instant)>,
    frame_rx: mpsc::Receiver<FrameBuffer>,
    fps: u32,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    const ENCODE_ERROR_THROTTLE: Duration = Duration::from_secs(5);
    let encoder_timeout = Duration::from_millis((1000 / fps).max(16) as u64);
    thread::spawn(move || {
        let mut last_error_log = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            match frame_rx.recv_timeout(encoder_timeout) {
                Ok(fb) => {
                    let cap_time = fb.cap_time;
                    if let FrameBufferHandle::IOSurface(ref surface) = fb.handle {
                        match encoder.encode(surface, fb.pts, fps as i32) {
                            Ok(frame) => {
                                let _ = frame_tx.send((frame, cap_time));
                            }
                            Err(e) => {
                                let now = Instant::now();
                                if now - last_error_log >= ENCODE_ERROR_THROTTLE {
                                    eprintln!("\n  Encode error: {}", e);
                                    last_error_log = now;
                                }
                            }
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    })
}

/// Main pipeline loop: receives encoded frames from the encoder thread and
/// sends them over WebRTC. Stops when `stop` is set.
fn run_pipeline(
    frame_rx: mpsc::Receiver<(h264::H264Frame, Instant)>,
    stop: Arc<AtomicBool>,
    frame_count: Arc<AtomicU64>,
    latest_latency: Arc<AtomicU64>,
    wr_handle: Arc<RwLock<Option<webrtc::WebRtcHandle>>>,
    wr_version: Arc<AtomicU64>,
    webrtc_rt: Arc<tokio::runtime::Runtime>,
    capture_session: &mut impl ScreenCapture,
    encoder_thread: thread::JoinHandle<()>,
    http_thread: thread::JoinHandle<()>,
) {
    let mut send_count: u64 = 0;
    let mut last_stats = Instant::now();
    let mut last_send = Instant::now();
    let mut prev_cap: u64 = 0;
    let mut prev_send: u64 = 0;

    let mut wr_cached: Option<webrtc::WebRtcHandle> = server::rw_read(&wr_handle).clone();
    let mut wr_ver = wr_version.load(Ordering::Acquire);

    while !stop.load(Ordering::Relaxed) {
        match frame_rx.recv_timeout(Duration::from_millis(100)) {
            Ok((frame, cap_time)) => {
                let v = wr_version.load(Ordering::Acquire);
                if v != wr_ver {
                    wr_cached = server::rw_read(&wr_handle).clone();
                    wr_ver = v;
                }

                send_count += 1;
                last_send = Instant::now();

                if let Some(ref wr) = wr_cached {
                    if let Err(e) = wr.send_frame(&frame) {
                        eprintln!("\n  WebRTC send error: {}", e);
                    }
                }

                latest_latency.store(cap_time.elapsed().as_millis() as u64, Ordering::Relaxed);

                let elapsed = last_stats.elapsed();
                if elapsed >= Duration::from_secs(1) {
                    let cap_total = frame_count.load(Ordering::Relaxed);
                    let cap_fps = (cap_total - prev_cap) as f64 / elapsed.as_secs_f64();
                    let snd_fps = (send_count - prev_send) as f64 / elapsed.as_secs_f64();
                    let lat = latest_latency.load(Ordering::Relaxed);
                    eprint!(
                        "\r  cap: {:.0}fps  send: {:.0}fps  lat: {}ms  [total: {}]  ",
                        cap_fps, snd_fps, lat, send_count,
                    );
                    std::io::stderr().flush().ok();
                    prev_cap = cap_total;
                    prev_send = send_count;
                    last_stats = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_send.elapsed() > Duration::from_secs(3) && send_count > 0 {
                    eprintln!("\n  No frames for 3s — encoder may have failed");
                    last_send = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("\n  Encoder channel disconnected");
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    let _ = capture_session.stop();
    encoder_thread.join().ok();
    http_thread.join().ok();

    let _ = webrtc_rt;
}

/// Setup Ctrl+C handler that force-exits on double-tap.
fn setup_ctrlc_handler(stop: Arc<AtomicBool>, ctrlc_count: Arc<AtomicBool>) {
    ctrlc::set_handler(move || {
        if ctrlc_count.swap(true, Ordering::Relaxed) {
            eprintln!("\nForce exit");
            std::process::exit(1);
        }
        eprintln!("\nStopping (Ctrl+C again to force)...");
        stop.store(true, Ordering::Relaxed);
    })
    .ok();
}

/// Run the full capture → encode → WebRTC pipeline on macOS.
///
/// Called from `main()` after CLI argument parsing, window selection,
/// and capture frame computation. Takes ownership of the `SCContentFilter`
/// and all pipeline configuration.
pub fn run_macos_pipeline(filter: SCContentFilter, config: PipelineConfig) {
    let PipelineConfig {
        out_w,
        out_h,
        fps,
        port,
        window_name,
    } = config;

    // ── Init H.264 encoder ──
    let encoder = Arc::new(
        h264::VtEncoder::new(out_w, out_h, fps)
            .unwrap_or_else(|e| { eprintln!("Encoder init failed: {e}"); std::process::exit(1); }),
    );

    // ── Channels ──
    let (frame_tx, frame_rx) = mpsc::sync_channel::<(h264::H264Frame, Instant)>(15);
    let (cap_tx, cap_rx) = mpsc::sync_channel::<FrameBuffer>(4);

    // ── Shared state ──
    let state = SharedState::new();
    let stop = state.stop.clone();
    let ctrlc_count = state.ctrlc_count.clone();
    let frame_count = state.frame_count.clone();
    let latest_latency = state.latest_latency.clone();
    let wr_handle = state.wr_handle.clone();
    let wr_version = state.wr_version.clone();
    let wr_connected = state.wr_connected.clone();
    let webrtc_rt = state.webrtc_rt.clone();

    // ── Ctrl+C handler ──
    setup_ctrlc_handler(stop.clone(), ctrlc_count);

    // ── Encoder thread ──
    let encoder_thread = spawn_encoder_thread(
        encoder,
        frame_tx,
        cap_rx,
        fps,
        state.stop.clone(),
    );

    // ── HTTP server ──
    let http_thread = server::spawn_http_server(
        port,
        fps,
        out_w,
        out_h,
        &window_name,
        wr_handle.clone(),
        wr_version.clone(),
        wr_connected.clone(),
        stop.clone(),
        latest_latency.clone(),
        webrtc_rt.clone(),
    );

    // ── Init WebRTC (blocks ~3s for ICE gathering) ──
    {
        let rt_handle = webrtc_rt.handle().clone();
        match webrtc::WebRtcHandle::new(rt_handle, fps, out_w, out_h) {
            Ok(handle) => {
                eprintln!("  WebRTC offer ready");
                *server::rw_write(&wr_handle) = Some(handle);
                wr_version.fetch_add(1, Ordering::Release);
            }
            Err(e) => eprintln!("  WebRTC init failed: {}", e),
        }
    }

    // ── Start capture ──
    let cap_tx_cb = cap_tx.clone();
    let stop_for_cap = stop.clone();

    #[cfg(target_os = "macos")]
    let mut capture_session = MacosCapture::create((filter, out_w, out_h, fps))
        .unwrap_or_else(|e| {
            eprintln!("Capture init failed: {e}");
            std::process::exit(1);
        });

    #[cfg(target_os = "macos")]
    let _ = capture_session.start(move |fb: FrameBuffer| {
        if cap_tx_cb.send(fb).is_err() {
            eprintln!("\n  Encoder thread disconnected \u{2014} stopping capture");
            stop_for_cap.store(true, Ordering::Relaxed);
        }
    });

    // ── Main pipeline loop ──
    #[cfg(target_os = "macos")]
    run_pipeline(
        frame_rx,
        stop,
        frame_count,
        latest_latency,
        wr_handle,
        wr_version,
        webrtc_rt,
        &mut capture_session,
        encoder_thread,
        http_thread,
    );
}
