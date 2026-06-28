mod capture;
mod h264;
mod server;
mod webrtc;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use screencapturekit::cm::CMSampleBuffer;
use screencapturekit::prelude::*;

fn main() {
    let _ = rustls::crypto::CryptoProvider::install_default(
        rustls::crypto::ring::default_provider(),
    );
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u32 = 0;
    let mut max_w: u32 = 1280;
    let mut fps: u32 = 30;
    let mut port: u16 = 8080;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--window-id" => {
                i += 1;
                wid = args[i].parse().unwrap_or(0);
            }
            "--width" => {
                i += 1;
                max_w = args[i].parse().unwrap_or(1280);
            }
            "--fps" => {
                i += 1;
                fps = args[i].parse().unwrap_or(30).clamp(1, 60);
            }
            "--port" => {
                i += 1;
                port = args[i].parse().unwrap_or(8080);
            }
            "-l" | "--list" => {
                for (id, app, title) in server::list_windows() {
                    println!("{:>5} | {} | {}", id, app, title);
                }
                return;
            }
            "-h" | "--help" => {
                println!("screenstream [-l] [-w <id>] [--width <px>] [--fps <1-60>] [--port <n>]");
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
            if seen.insert((&w.1, &w.2)) {
                uq.push(w);
            }
        }
        for (j, (_, a, t)) in uq.iter().enumerate() {
            println!(
                "  [{:2}] {} - {}",
                j + 1,
                a,
                if t.len() > 55 { &t[..55] } else { t }
            );
        }
        print!("Select window (1-{}): ", uq.len());
        std::io::stdout().flush().ok();
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).ok();
        if let Ok(n) = s.trim().parse::<usize>() {
            if n >= 1 && n <= uq.len() {
                wid = uq[n - 1].0;
            }
        }
        if wid == 0 {
            return;
        }
    }

    // ── Initialize CoreGraphics (required for SCKit before macOS 26+?) ──
    // The screencapturekit crate provides this FFI function to prevent
    // CGS_REQUIRE_INIT assertion failures in command-line tools.
    unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }

    // ── Compute output dimensions via SCShareableContent ──
    let (out_w, out_h) = {
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
        (ow & !1, oh & !1)
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

    // ── Channel: encoded frames → WebRTC sender ──
    let (frame_tx, frame_rx) = mpsc::channel::<h264::H264Frame>();

    // ── Stop flag ──
    let stop = Arc::new(AtomicBool::new(false));
    static CTRLC_COUNT: AtomicBool = AtomicBool::new(false);
    {
        let c_stop = stop.clone();
        ctrlc::set_handler(move || {
            if CTRLC_COUNT.swap(true, Ordering::Relaxed) {
                eprintln!("\nForce exit");
                std::process::exit(1);
            }
            eprintln!("\nStopping (Ctrl+C again to force)...");
            c_stop.store(true, Ordering::Relaxed);
        })
        .ok();
    }

    // ── WebRTC ──
    let wr_handle: Arc<Mutex<Option<webrtc::WebRtcHandle>>> = Arc::new(Mutex::new(None));
    let webrtc_connected = Arc::new(AtomicBool::new(false));
    let srv_wr = wr_handle.clone();
    let srv_wr_conn = webrtc_connected.clone();
    let svr_s = stop.clone();
    let ip = server::get_ip();

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
                "/" => Response::from_data(server::html(fps).into_bytes())
                    .with_header(
                        "Content-Type: text/html; charset=utf-8"
                            .parse::<Header>()
                            .unwrap(),
                    ),
                "/offer" => {
                    if srv_wr_conn.load(Ordering::Relaxed) {
                        if let Some(ref wr) = *srv_wr.lock().unwrap() {
                            wr.close();
                        }
                        srv_wr_conn.store(false, Ordering::Relaxed);
                        if let Ok(new_handle) = webrtc::WebRtcHandle::new(svr_s.clone()) {
                            eprintln!("  WebRTC offer recreated");
                            *srv_wr.lock().unwrap() = Some(new_handle);
                        }
                    }
                    match srv_wr.lock().unwrap().as_ref() {
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
                    let mut body = String::new();
                    if req.as_reader().read_to_string(&mut body).is_ok() {
                        if let Some(ref wr) = *srv_wr.lock().unwrap() {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                                if let Some(sdp) = v["sdp"].as_str() {
                                    let _ = wr.set_answer(sdp.to_string());
                                    if let Some(cands) = v["candidates"].as_array() {
                                        for c in cands {
                                            let cs = c["candidate"].as_str().unwrap_or("");
                                            if !cs.is_empty() {
                                                let sdp_mid =
                                                    c["sdpMid"].as_str().map(|s| s.to_string());
                                                let sdp_mline_index =
                                                    c["sdpMLineIndex"].as_u64().map(|n| n as u16);
                                                let _ = wr.add_candidate(
                                                    cs,
                                                    sdp_mid,
                                                    sdp_mline_index,
                                                );
                                            }
                                        }
                                    }
                                    srv_wr_conn.store(true, Ordering::Relaxed);
                                    eprintln!("  Signal exchange complete");
                                }
                            }
                        }
                    }
                    Response::from_data(Vec::from("ok"))
                }
                _ => Response::from_data(Vec::new()).with_status_code(404),
            };
            req.respond(resp).ok();
        }
    });

    // ── Init WebRTC (blocks ~3s for ICE gathering) ──
    {
        eprintln!("  Initializing WebRTC...");
        let wstop = stop.clone();
        match webrtc::WebRtcHandle::new(wstop) {
            Ok(handle) => {
                eprintln!("  WebRTC offer ready");
                *wr_handle.lock().unwrap() = Some(handle);
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
        wid,
        out_w,
        out_h,
        fps,
        move |sample: CMSampleBuffer, _type: SCStreamOutputType| {
            let n = frame_count_cb.fetch_add(1, Ordering::Relaxed);
            if let Some(pb) = sample.image_buffer() {
                match encoder_cb.encode_pixelbuffer(
                    pb.as_ptr(),
                    pb.width() as u32,
                    pb.height() as u32,
                    n,
                    fps as i32,
                ) {
                    Ok(frame) => {
                        let _ = frame_tx_cb.send(frame);
                    }
                    Err(e) => {
                        eprintln!("  Encode error: {}", e);
                    }
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
            Ok(frame) => {
                h264_frame_count += 1;
                last_frame = Instant::now();

                if let Some(ref wr) = *wr_handle.lock().unwrap() {
                    if let Err(e) = wr.send_frame(&frame) {
                        eprintln!("  WebRTC send error: {}", e);
                    }
                }

                if h264_frame_count % 150 == 0 && last_report.elapsed() >= Duration::from_secs(3) {
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
                break;
            }
        }
    }

    // Cleanup
    capture_session.stop();
    srv.join().ok();
}
