mod capture;
mod h264;
mod server;
mod update_checker;
mod webrtc;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use screencapturekit::prelude::*;
use tiny_http::Header;

// ── Content-Type header helpers (hardcoded strings, parse cannot fail) ──

fn header_html() -> Header {
    "Content-Type: text/html; charset=utf-8"
        .parse()
        .expect("valid Content-Type: text/html")
}

fn header_sdp() -> Header {
    "Content-Type: application/sdp"
        .parse()
        .expect("valid Content-Type: application/sdp")
}

fn header_json() -> Header {
    "Content-Type: application/json"
        .parse()
        .expect("valid Content-Type: application/json")
}

fn header_text() -> Header {
    "Content-Type: text/plain"
        .parse()
        .expect("valid Content-Type: text/plain")
}

/// Helper: acquire RwLock read guard, recovering from a poisoned lock.
fn rw_read<'a, T>(rw: &'a RwLock<T>) -> std::sync::RwLockReadGuard<'a, T> {
    rw.read().unwrap_or_else(|e| e.into_inner())
}

/// Helper: acquire RwLock write guard, recovering from a poisoned lock.
fn rw_write<'a, T>(rw: &'a RwLock<T>) -> std::sync::RwLockWriteGuard<'a, T> {
    rw.write().unwrap_or_else(|e| e.into_inner())
}

// ── CLI argument parsing via clap derive ──

#[derive(Parser)]
#[command(
    name = "mac-screen-cast",
    version,
    about = "Stream macOS screen to browser over LAN",
    after_help = "\
EXAMPLES:
  mac-screen-cast -l --json              List windows as JSON
  mac-screen-cast -w 12345 --width 720   Stream at 720p
  mac-screen-cast -w 12345 --fps 60      Stream at 60 fps

HTTP API (at runtime):
  GET  /         HTML player page
  GET  /offer    WebRTC SDP offer
  POST /signal   WebRTC answer + ICE candidates (JSON body)
  GET  /latency  Current capture-to-send latency (ms)
"
)]
struct Cli {
    /// Window ID to capture (omit for interactive picker)
    #[arg(short = 'w', long = "window-id")]
    window_id: Option<u32>,

    /// Max output width in pixels
    #[arg(long, default_value_t = 1280)]
    width: u32,

    /// Target frame rate (1-60)
    #[arg(long, default_value_t = 30)]
    fps: u32,

    /// HTTP server port
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// List visible windows
    #[arg(short, long)]
    list: bool,

    /// JSON output for --list
    #[arg(long)]
    json: bool,
}

// ── Shared state between threads ──

struct SharedState {
    stop: Arc<AtomicBool>,
    ctrlc_count: Arc<AtomicBool>,
    frame_count: Arc<AtomicU64>,
    latest_latency: Arc<AtomicU64>,
    wr_handle: Arc<RwLock<Option<webrtc::WebRtcHandle>>>,
    wr_version: Arc<AtomicU64>,
    wr_connected: Arc<AtomicBool>,
    webrtc_rt: Arc<tokio::runtime::Runtime>,
}

impl SharedState {
    fn new() -> Self {
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

// ── Capture setup: dimensions + SCContentFilter ──

struct CaptureSetup {
    out_w: u32,
    out_h: u32,
    filter: SCContentFilter,
}

fn init_capture(wid: u32, max_w: u32) -> CaptureSetup {
    let content = SCShareableContent::get().unwrap_or_else(|e| {
        eprintln!("ScreenCaptureKit error: {e}");
        eprintln!(
            "Grant Screen Recording permission in \
             System Settings > Privacy & Security > Screen Recording."
        );
        std::process::exit(1);
    });
    let windows = content.windows();
    let window = windows
        .iter()
        .find(|w| w.window_id() == wid)
        .unwrap_or_else(|| {
            eprintln!("Window {wid} not found — try --list to see available windows.");
            std::process::exit(1);
        });
    let frame = window.frame();
    let nw = frame.size().width as u32;
    let nh = frame.size().height as u32;

    let (ow, oh) = if nw > max_w {
        let rnh = (nh * max_w) / nw;
        (max_w, rnh)
    } else {
        (nw, nh)
    };

    let filter = SCContentFilter::create().with_window(window).build();

    CaptureSetup {
        out_w: ow & !1,
        out_h: oh & !1,
        filter,
    }
}

// ── Interactive window picker ──

fn select_window_interactively() -> (u32, String) {
    let wins = server::list_windows();
    if wins.is_empty() {
        eprintln!("No windows found");
        std::process::exit(1);
    }
    let mut seen = std::collections::HashSet::new();
    let uq: Vec<&server::Window> = wins
        .iter()
        .filter(|w| seen.insert((&w.app, &w.title)))
        .collect();

    for (j, w) in uq.iter().enumerate() {
        println!(
            "  [{:2}] {} - {}",
            j + 1,
            w.app,
            if w.title.len() > 55 { &w.title[..55] } else { &w.title }
        );
    }
    print!("Select window (1-{}): ", uq.len());
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s).ok();

    if let Ok(n) = s.trim().parse::<usize>() {
        if n >= 1 && n <= uq.len() {
            return (uq[n - 1].id, uq[n - 1].app.clone());
        }
    }
    std::process::exit(0);
}

// ── Print window list and exit ──

fn print_windows_and_exit(json: bool) {
    if json {
        println!("{}", server::list_windows_json());
    } else {
        for w in server::list_windows() {
            println!("{:>5} | {} | {}", w.id, w.app, w.title);
        }
    }
    std::process::exit(0);
}

// ── Encoder thread: decoupled from capture for pipeline parallelism ──

struct RawFrame {
    sample: CMSampleBuffer,
    seq: u64,
    cap_time: Instant,
}

fn spawn_encoder_thread(
    encoder: Arc<h264::VtEncoder>,
    frame_tx: mpsc::SyncSender<(h264::H264Frame, Instant)>,
    raw_rx: mpsc::Receiver<RawFrame>,
    fps: u32,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    const ENCODE_ERROR_THROTTLE: Duration = Duration::from_secs(5);
    let encoder_timeout = Duration::from_millis((1000 / fps).max(16) as u64);
    thread::spawn(move || {
        let mut last_error_log = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            match raw_rx.recv_timeout(encoder_timeout) {
                Ok(raw) => {
                    if let Some(pb) = raw.sample.image_buffer() {
                        if let Some(surface) = pb.io_surface() {
                            match encoder.encode(&surface, raw.seq, fps as i32) {
                                Ok(frame) => {
                                    let _ = frame_tx.send((frame, raw.cap_time));
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
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    })
}

// ── Setup Ctrl+C handler ──

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

// ── HTTP server thread ──

#[allow(clippy::too_many_arguments)]
fn spawn_http_server(
    port: u16,
    fps: u32,
    out_w: u32,
    out_h: u32,
    window_name: &str,
    wr_handle: Arc<RwLock<Option<webrtc::WebRtcHandle>>>,
    wr_version: Arc<AtomicU64>,
    wr_connected: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    latest_latency: Arc<AtomicU64>,
    webrtc_rt: Arc<tokio::runtime::Runtime>,
) -> thread::JoinHandle<()> {
    let ip = server::get_ip();
    let wn = window_name.to_string();

    eprintln!("  Initializing WebRTC...");

    thread::spawn(move || {
        use tiny_http::Response;
        let server = match tiny_http::Server::http(format!("0.0.0.0:{}", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Port {} in use: {}", port, e);
                return;
            }
        };
        eprintln!("\n  WebRTC stream →  http://{}:{}", ip, port);

        if let Ok(qr) = qrcode::QrCode::new(format!("http://{}:{}", ip, port)) {
            let w = qr.width();
            let mut out = String::new();
            let mut y = 0usize;
            out.push_str("  \n");
            while y < w {
                out.push_str("  ");
                for x in 0..w {
                    let top = qr[(x, y)] != qrcode::Color::Light;
                    let bot = y + 1 < w && qr[(x, y + 1)] != qrcode::Color::Light;
                    out.push(match (top, bot) {
                        (true, true) => '█',
                        (true, false) => '▀',
                        (false, true) => '▄',
                        (false, false) => ' ',
                    });
                }
                out.push_str("  \n");
                y += 2;
            }
            out.push_str("  \n");
            eprintln!("{}", out);
        }



        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let mut req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r,
                _ => continue,
            };
            let url = req.url();
            let path = url.split('?').next().unwrap_or("/");

            let resp = match path {
                "/" => Response::from_data(server::html(fps, &wn).into_bytes())
                    .with_header(header_html()),
                "/offer" => {
                    let was_connected = wr_connected.swap(false, Ordering::Relaxed);
                    if was_connected {
                        let old = rw_read(&wr_handle).clone();
                        if let Some(ref wr) = old {
                            wr.close();
                        }
                        if let Ok(new_handle) =
                            webrtc::WebRtcHandle::new(webrtc_rt.handle().clone(), fps, out_w, out_h)
                        {
                            *rw_write(&wr_handle) = Some(new_handle);
                            wr_version.fetch_add(1, Ordering::Release);
                            eprintln!("\n  WebRTC offer recreated for refresh");
                        } else {
                            eprintln!("\n  WebRTC recreate failed");
                        }
                    }
                    match rw_read(&wr_handle).as_ref() {
                        Some(wr) => Response::from_data(wr.offer.clone().into_bytes())
                            .with_header(header_sdp()),
                        None => Response::from_data(Vec::from("not ready")).with_status_code(503),
                    }
                }
                "/signal" => {
                    let mut ok = false;
                    let mut body = String::new();
                    if req.as_reader().read_to_string(&mut body).is_ok() {
                        if let Some(ref wr) = *rw_read(&wr_handle) {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                                if let Some(sdp) = v["sdp"].as_str() {
                                    if wr.set_answer(sdp.to_string()).is_ok() {
                                        if let Some(cands) = v["candidates"].as_array() {
                                            for c in cands {
                                                let cs = c["candidate"].as_str().unwrap_or("");
                                                if !cs.is_empty() {
                                                    let _ = wr.add_candidate(cs);
                                                }
                                            }
                                        }
                                        wr_connected.store(true, Ordering::Relaxed);
                                        eprintln!("\n  Signal exchange complete");
                                        ok = true;
                                    }
                                }
                            }
                        }
                    }
                    let (status, body) = if ok {
                        (200, "{\"status\":\"ok\"}")
                    } else {
                        (500, "{\"error\":\"signal failed\"}")
                    };
                    Response::from_data(Vec::from(body))
                        .with_status_code(status)
                        .with_header(header_json())
                }
                "/latency" => {
                    let ms = latest_latency.load(Ordering::Relaxed);
                    Response::from_data(format!("{}", ms).into_bytes())
                        .with_header(header_text())
                }
                _ => Response::from_data(Vec::from("{\"error\":\"not found\"}"))
                    .with_status_code(404)
                    .with_header(header_json()),
            };
            req.respond(resp).ok();
        }
    })
}

// ── Main pipeline: receive encoded frames → WebRTC ──

#[allow(clippy::too_many_arguments)]
fn run_pipeline(
    frame_rx: mpsc::Receiver<(h264::H264Frame, Instant)>,
    stop: Arc<AtomicBool>,
    frame_count: Arc<AtomicU64>,
    latest_latency: Arc<AtomicU64>,
    wr_handle: Arc<RwLock<Option<webrtc::WebRtcHandle>>>,
    wr_version: Arc<AtomicU64>,
    webrtc_rt: Arc<tokio::runtime::Runtime>,
    capture_session: &mut capture::CaptureSession,
    encoder_thread: thread::JoinHandle<()>,
    http_thread: thread::JoinHandle<()>,
) {

    let mut send_count: u64 = 0;
    let mut last_stats = Instant::now();
    let mut last_send = Instant::now();
    let mut prev_cap: u64 = 0;
    let mut prev_send: u64 = 0;

    let mut wr_cached: Option<webrtc::WebRtcHandle> = rw_read(&wr_handle).clone();
    let mut wr_ver = wr_version.load(Ordering::Acquire);

    while !stop.load(Ordering::Relaxed) {
        match frame_rx.recv_timeout(Duration::from_millis(100)) {
            Ok((frame, cap_time)) => {
                let v = wr_version.load(Ordering::Acquire);
                if v != wr_ver {
                    wr_cached = rw_read(&wr_handle).clone();
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

    capture_session.stop();
    encoder_thread.join().ok();
    http_thread.join().ok();

    let _ = webrtc_rt;
}

fn main() {
    update_checker::check();

    let _ = rustls::crypto::CryptoProvider::install_default(
        rustls::crypto::ring::default_provider(),
    );

    let cli = Cli::parse();

    // --list: print windows and exit
    if cli.list {
        print_windows_and_exit(cli.json);
    }

    // Resolve window ID: CLI arg or interactive picker
    let (wid, window_name) = match cli.window_id {
        Some(id) if id > 0 => (id, String::from("mac-screen-cast")),
        _ => select_window_interactively(),
    };

    // ── Initialize CoreGraphics ──
    // SAFETY: sc_initialize_core_graphics() is an FFI function exported by
    // the screencapturekit crate. It initializes CoreGraphics internals and
    // must be called once before any SCStream usage in command-line tools.
    unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }

    // ── Compute output dimensions + build capture filter ──
    let CaptureSetup {
        out_w,
        out_h,
        filter,
    } = init_capture(wid, cli.width);

    eprintln!(
        "  Capture {}x{} @ {}fps, output to :{}",
        out_w, out_h, cli.fps, cli.port
    );

    // ── Init H.264 encoder ──
    let encoder = Arc::new(
        h264::VtEncoder::new(out_w, out_h, cli.fps)
            .unwrap_or_else(|e| { eprintln!("Encoder init failed: {e}"); std::process::exit(1); }),
    );

    // ── Channels ──
    let (frame_tx, frame_rx) = mpsc::sync_channel::<(h264::H264Frame, Instant)>(15);
    let (raw_tx, raw_rx) = mpsc::sync_channel::<RawFrame>(4);

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
        raw_rx,
        cli.fps,
        state.stop.clone(),
    );

    // ── HTTP server ──
    let http_thread = spawn_http_server(
        cli.port,
        cli.fps,
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
        match webrtc::WebRtcHandle::new(rt_handle, cli.fps, out_w, out_h) {
            Ok(handle) => {
                eprintln!("  WebRTC offer ready");
                *rw_write(&wr_handle) = Some(handle);
                wr_version.fetch_add(1, Ordering::Release);
            }
            Err(e) => eprintln!("  WebRTC init failed: {}", e),
        }
    }

    // ── Start ScreenCaptureKit capture ──
    let raw_tx_cb = raw_tx.clone();
    let stop_for_cap = stop.clone();
    let frame_count_cb = frame_count.clone();

    let mut capture_session = capture::CaptureSession::new(
        filter,
        out_w,
        out_h,
        cli.fps,
        move |sample: CMSampleBuffer, _type: SCStreamOutputType| {
            let seq = frame_count_cb.fetch_add(1, Ordering::Relaxed);
            if raw_tx_cb
                .send(RawFrame {
                    sample,
                    seq,
                    cap_time: Instant::now(),
                })
                .is_err()
            {
                eprintln!("\n  Encoder thread disconnected — stopping capture");
                stop_for_cap.store(true, Ordering::Relaxed);
            }
        },
    );

    // ── Main pipeline ──
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
