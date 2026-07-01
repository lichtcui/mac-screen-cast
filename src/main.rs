mod capture;
mod h264;
mod server;
mod update_checker;
mod webrtc;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::prelude::*;
use screencapturekit::stream::content_filter::SCContentFilter;

fn lock_mutex<'a, T>(m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn main() {
    update_checker::check();

    let _ = rustls::crypto::CryptoProvider::install_default(
        rustls::crypto::ring::default_provider(),
    );
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u32 = 0;
    let mut window_name = String::from("mac-screen-cast");
    let mut max_w: u32 = 1280;
    let mut fps: u32 = 30;
    let mut port: u16 = 8080;
    let mut json_output = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--window-id" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --window-id");
                    break;
                }
                wid = args[i].parse().unwrap_or(0);
            }
            "--width" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --width");
                    break;
                }
                max_w = args[i].parse().unwrap_or(1280);
            }
            "--fps" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --fps");
                    break;
                }
                fps = args[i].parse().unwrap_or(30).clamp(1, 60);
            }
            "--port" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Missing value for --port");
                    break;
                }
                port = args[i].parse().unwrap_or(8080);
            }
            "--json" => json_output = true,
            "-l" | "--list" => {
                if json_output {
                    println!("{}", server::list_windows_json());
                } else {
                    for w in server::list_windows() {
                        println!("{:>5} | {} | {}", w.id, w.app, w.title);
                    }
                }
                return;
            }
            "-h" | "--help" => {
                eprintln!("mac-screen-cast — stream macOS screen to browser over LAN");
                eprintln!();
                eprintln!("USAGE");
                eprintln!("  mac-screen-cast [FLAGS] [OPTIONS]");
                eprintln!("  mac-screen-cast -l [--json]");
                eprintln!("  mac-screen-cast -w <id> [--width px] [--fps N] [--port N]");
                eprintln!();
                eprintln!("FLAGS");
                eprintln!("  -l, --list       List visible windows (human-readable)");
                eprintln!("       --json      JSON output for --list (machine-parseable)");
                eprintln!("  -h, --help       Show this help");
                eprintln!();
                eprintln!("OPTIONS");
                eprintln!("  -w, --window-id  Window ID to capture                    [env: none]");
                eprintln!("      --width      Max output width in pixels              [default: 1280]");
                eprintln!("      --fps        Target frame rate (1-60)                [default: 30]");
                eprintln!("      --port       HTTP server port                        [default: 8080]");
                eprintln!();
                eprintln!("EXAMPLES");
                eprintln!("  mac-screen-cast -l --json              List windows as JSON");
                eprintln!("  mac-screen-cast -w 12345 --width 720   Stream at 720p");
                eprintln!("  mac-screen-cast -w 12345 --fps 60      Stream at 60 fps");
                eprintln!();
                eprintln!("HTTP API");
                eprintln!("  GET  /         HTML player page");
                eprintln!("  GET  /offer    WebRTC SDP offer");
                eprintln!("  POST /signal   WebRTC answer + ICE candidates (JSON body)");
                eprintln!("  GET  /latency  Current capture-to-send latency (ms)");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    if wid == 0 {
        let wins = server::list_windows();
        if wins.is_empty() {
            eprintln!("No windows found");
            return;
        }
        let mut seen = std::collections::HashSet::new();
        let mut uq = Vec::new();
        for w in &wins {
            if seen.insert((&w.app, &w.title)) {
                uq.push(w);
            }
        }
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
                wid = uq[n - 1].id;
                window_name = uq[n - 1].app.clone();
            }
        }
        if wid == 0 {
            return;
        }
    }

    // ── Initialize CoreGraphics (required for SCKit before macOS 26+?) ──
    // The screencapturekit crate provides this FFI function to prevent
    // CGS_REQUIRE_INIT assertion failures in command-line tools.
    // SAFETY: sc_initialize_core_graphics() is an FFI function exported by
    // the screencapturekit crate. It initializes CoreGraphics internals and
    // must be called once before any SCStream usage in command-line tools.
    unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }

    // ── Compute output dimensions + build capture filter via SCShareableContent ──
    let (out_w, out_h, capture_filter) = {
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

        (ow & !1, oh & !1, filter)
    };

    eprintln!(
        "  Capture {}x{} @ {}fps, output to :{}",
        out_w, out_h, fps, port
    );

    // ── Init H.264 encoder ──
    let encoder = match h264::VtEncoder::new(out_w, out_h, fps) {
        Ok(e) => Arc::new(e),
        Err(e) => {
            eprintln!("Encoder init failed: {e}");
            return;
        }
    };

    // ── Channel: encoded frames (+ capture timestamp) → WebRTC sender ──
    let (frame_tx, frame_rx) = mpsc::sync_channel::<(h264::H264Frame, Instant)>(60);

    // ── Stop flag ──
    let stop = Arc::new(AtomicBool::new(false));
    let ctrlc_count = Arc::new(AtomicBool::new(false));
    {
        let c_stop = stop.clone();
        let c_count = ctrlc_count.clone();
        ctrlc::set_handler(move || {
            if c_count.swap(true, Ordering::Relaxed) {
                eprintln!("\nForce exit");
                std::process::exit(1);
            }
            eprintln!("\nStopping (Ctrl+C again to force)...");
            c_stop.store(true, Ordering::Relaxed);
        })
        .ok();
    }

    // ── Latency measurement ──
    let latest_latency = Arc::new(AtomicU64::new(0));

    // ── WebRTC ──
    let webrtc_rt = Arc::new(
        tokio::runtime::Runtime::new().expect("create Tokio runtime for WebRTC"),
    );
    let wr_handle: Arc<Mutex<Option<webrtc::WebRtcHandle>>> = Arc::new(Mutex::new(None));
    let srv_wr_conn = Arc::new(AtomicBool::new(false));
    let srv_wr = wr_handle.clone();
    let srv_rt = webrtc_rt.clone();
    let svr_s = stop.clone();
    let srv_lat = latest_latency.clone();
    let ip = server::get_ip();
    eprintln!("  Initializing WebRTC...");

    let srv = thread::spawn(move || {
        use tiny_http::{Header, Response};
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
                        (true,  true)  => '█',
                        (true,  false) => '▀',
                        (false, true)  => '▄',
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
            if svr_s.load(Ordering::Relaxed) {
                break;
            }
            let mut req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r,
                _ => continue,
            };
            let url = req.url();
            let path = url.split('?').next().unwrap_or("/");

            let resp = match path {
                "/" => Response::from_data(server::html(fps, &window_name).into_bytes())
                    .with_header(
                        "Content-Type: text/html; charset=utf-8"
                            .parse::<Header>()
                            .unwrap(),
                    ),
                "/offer" => {
                    // Refresh: close old PeerConnection, create new one
                    let was_connected = srv_wr_conn.swap(false, Ordering::Relaxed);
                    if was_connected {
                        // Close old PC inside the shared runtime
                        {
                            let guard = lock_mutex(&srv_wr);
                            if let Some(ref wr) = *guard {
                                wr.close();
                            }
                        }
                        // Create replacement PeerConnection on the same runtime
                        if let Ok(new_handle) =
                            webrtc::WebRtcHandle::new(srv_rt.handle().clone(), fps, out_w, out_h)
                        {
                            *lock_mutex(&srv_wr) = Some(new_handle);
                            eprintln!("  WebRTC offer recreated for refresh");
                        } else {
                            eprintln!("  WebRTC recreate failed");
                        }
                    }
                    match lock_mutex(&srv_wr).as_ref() {
                        Some(wr) => Response::from_data(wr.offer.clone().into_bytes())
                            .with_header(
                                "Content-Type: application/sdp"
                                    .parse::<Header>()
                                    .unwrap(),
                            ),
                        None => Response::from_data(Vec::from("not ready")).with_status_code(503),
                    }
                }
                "/signal" => {
                    let mut ok = false;
                    let mut body = String::new();
                    if req.as_reader().read_to_string(&mut body).is_ok() {
                        if let Some(ref wr) = *lock_mutex(&srv_wr) {
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
                                        srv_wr_conn.store(true, Ordering::Relaxed);
                                        eprintln!("  Signal exchange complete");
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
                        .with_header("Content-Type: application/json".parse::<Header>().unwrap())
                }
                "/latency" => {
                    let ms = srv_lat.load(Ordering::Relaxed);
                    Response::from_data(format!("{}", ms).into_bytes())
                }
                _ => Response::from_data(Vec::from("{\"error\":\"not found\"}"))
                    .with_status_code(404)
                    .with_header("Content-Type: application/json".parse::<Header>().unwrap()),
            };
            req.respond(resp).ok();
        }
    });

    // ── Init WebRTC (blocks ~3s for ICE gathering) ──
    {
        match webrtc::WebRtcHandle::new(webrtc_rt.handle().clone(), fps, out_w, out_h) {
            Ok(handle) => {
                eprintln!("  WebRTC offer ready");
                *lock_mutex(&wr_handle) = Some(handle);
            }
            Err(e) => eprintln!("  WebRTC init failed: {}", e),
        }
    }

    // ── Start ScreenCaptureKit capture ──
    let encoder_cb = encoder.clone();
    let frame_tx_cb = frame_tx.clone();
    let frame_count = Arc::new(AtomicU64::new(0));
    let frame_count_cb = frame_count.clone();

    let mut capture_session = match capture::CaptureSession::new(
        capture_filter,
        out_w,
        out_h,
        fps,
        move |sample: CMSampleBuffer, _type: SCStreamOutputType| {
            let n = frame_count_cb.fetch_add(1, Ordering::Relaxed);
            if n < 10 || n.is_multiple_of(150) {
                eprint!("\r  SCFrame #{}", n);
                std::io::stderr().flush().ok();
            }
            if let Some(pb) = sample.image_buffer() {
                if let Some(surface) = pb.io_surface() {
                    let cap_time = Instant::now();
                    match encoder_cb.encode(&surface, n, fps as i32) {
                        Ok(frame) => {
                            let _ = frame_tx_cb.send((frame, cap_time));
                        }
                        Err(e) => {
                            eprintln!("\n  Encode error: {}", e);
                        }
                    }
                } else {
                    eprint!("\r  No IOSurface for frame #{}", n);
                    std::io::stderr().flush().ok();
                }
            }
        },
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return;
        }
    };

    // ── Main loop: receive encoded frames → WebRTC ──
    let mut h264_frame_count: u64 = 0;
    let mut last_report = Instant::now();
    let mut last_frame = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        match frame_rx.recv_timeout(Duration::from_millis(100)) {
            Ok((frame, cap_time)) => {
                h264_frame_count += 1;
                last_frame = Instant::now();

                if let Some(ref wr) = *lock_mutex(&wr_handle) {
                    if let Err(e) = wr.send_frame(&frame) {
                        eprintln!("  WebRTC send error: {}", e);
                    }
                }

                latest_latency.store(cap_time.elapsed().as_millis() as u64, Ordering::Relaxed);

                if h264_frame_count.is_multiple_of(150) && last_report.elapsed() >= Duration::from_secs(3) {
                    print!("\r  {} frames", h264_frame_count);
                    std::io::stdout().flush().ok();
                    last_report = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_frame.elapsed() > Duration::from_secs(3) && h264_frame_count > 0 {
                    eprintln!("\n  No frames for 3s — encoder may have failed");
                    last_frame = Instant::now();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("  Encoder channel disconnected");
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
    }

    // Cleanup
    capture_session.stop();
    srv.join().ok();
}
