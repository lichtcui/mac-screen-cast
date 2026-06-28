mod capture;
mod fmp4;
mod h264;
mod server;
mod webrtc;

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::base::{kCGBitmapByteOrder32Big, kCGImageAlphaNoneSkipFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::{CGContext, CGInterpolationQuality};
use core_graphics::geometry::{CGRect, CGPoint, CGSize};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u32 = 0;
    let mut max_w: u32 = 1280;
    let mut fps: u32 = 30;
    let mut port: u16 = 8080;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--window-id" => { i += 1; wid = args[i].parse().unwrap_or(0); }
            "--width" => { i += 1; max_w = args[i].parse().unwrap_or(1280); }
            "--fps" => { i += 1; fps = args[i].parse().unwrap_or(30).clamp(1, 60); }
            "--port" => { i += 1; port = args[i].parse().unwrap_or(8080); }
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
        if wins.is_empty() { eprintln!("无窗口"); return; }
        let mut seen = std::collections::HashSet::new();
        let mut uq = Vec::new();
        for w in &wins { if seen.insert((&w.1, &w.2)) { uq.push(w); } }
        for (j, (_, a, t)) in uq.iter().enumerate() {
            println!("  [{:2}] {} - {}", j + 1, a, if t.len()>55{&t[..55]}else{t});
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

    let stop = Arc::new(AtomicBool::new(false));

    let wr_handle: Arc<Mutex<Option<webrtc::WebRtcHandle>>> = Arc::new(Mutex::new(None));
    let webrtc_connected = Arc::new(AtomicBool::new(false));
    let svr_s = stop.clone();
    let svr_port = port;
    let ip = server::get_ip();

    let srv_wr = wr_handle.clone();
    let srv_wr_conn = webrtc_connected.clone();

    let srv = thread::spawn(move || {
        use tiny_http::{Header, Response};
        let server = match tiny_http::Server::http(format!("0.0.0.0:{}", svr_port)) {
            Ok(s) => s,
            Err(e) => { eprintln!("端口 {} 被占用: {}", svr_port, e); return; }
        };
        eprintln!("\n  WebRTC stream →  http://{}:{}", ip, svr_port);

        loop {
            if svr_s.load(Ordering::Relaxed) { break; }
            let mut req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r, _ => continue,
            };
            let url = req.url();
            let path = url.split('?').next().unwrap_or("/");

            let resp = match path {
                "/" => Response::from_data(server::html(fps).into_bytes())
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<Header>().unwrap()),
                "/offer" => {
                    match srv_wr.lock().unwrap().as_ref() {
                        Some(wr) => Response::from_data(wr.offer.clone().into_bytes())
                            .with_header("Content-Type: application/sdp".parse::<Header>().unwrap()),
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
                                            if let Some(cs) = c.as_str() {
                                                let _ = wr.add_candidate(cs);
                                            }
                                        }
                                    }
                                    srv_wr_conn.store(true, Ordering::Relaxed);
                                    eprintln!("  WebRTC connected");
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

    let c_stop = stop.clone();
    static CTRLC_COUNT: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        if CTRLC_COUNT.swap(true, Ordering::Relaxed) {
            eprintln!("\n强制退出");
            std::process::exit(1);
        }
        eprintln!("\n⏳ 正在停止 (再按一次 Ctrl+C 强制退出)...");
        c_stop.store(true, Ordering::Relaxed);
    }).ok();

    let frame_dur = Duration::from_secs_f64(1.0 / fps as f64);
    let mut next_capture = Instant::now();

    // Determine capture dimensions from a test capture
    let (enc_w, enc_h) = match capture::capture_cgimage(wid) {
        Some(cg) => {
            let w = cg.width() as u32;
            let h = cg.height() as u32;
            if w > 0 && h > 0 {
                if w > max_w {
                    let nh = (h * max_w) / w;
                    // ensure even dimensions (VideoToolbox requirement)
                    (max_w | 1, nh | 1)
                } else {
                    (w | 1, h | 1)
                }
            } else {
                (max_w, max_w * 9 / 16)
            }
        }
        None => (max_w, max_w * 9 / 16),
    };
    let (enc_w, enc_h) = (enc_w & !1, enc_h & !1); // round down to even for encoder

    let mut h264_encoder: Option<h264::VtEncoder> = h264::VtEncoder::new(enc_w, enc_h, fps).ok();
    let mut h264_frame_count: u64 = 0;
    let mut scaler_ctx: Option<(CGContext, u32, u32)> = None;

    let mut last_report = Instant::now();


    // Initialize WebRTC (blocks until offer is generated)
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

    while !stop.load(Ordering::Relaxed) {
        // Recover dead encoder
        if h264_encoder.is_none() {
            h264_encoder = h264::VtEncoder::new(enc_w, enc_h, fps).ok();
        }

        // Capture and optionally scale using cached context
        let shared_cg = match capture::capture_cgimage(wid) {
            None => None,
            Some(cg) => {
                let (w, h) = (cg.width() as u32, cg.height() as u32);
                if w == 0 || h == 0 { None }
                else if w > max_w {
                    let nh = (h * max_w) / w;
                    if !matches!(scaler_ctx, Some((_, tw, th)) if tw == max_w && th == nh) {
                        let cs = CGColorSpace::create_device_rgb();
                        let ctx = CGContext::create_bitmap_context(
                            None, max_w as usize, nh as usize, 8, max_w as usize * 4, &cs,
                            kCGBitmapByteOrder32Big | kCGImageAlphaNoneSkipFirst,
                        );
                        ctx.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityHigh);
                        scaler_ctx = Some((ctx, max_w, nh));
                    }
                    let (ref ctx, _, _) = scaler_ctx.as_ref().unwrap();
                    let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(max_w as _, nh as _));
                    ctx.draw_image(rect, &cg);
                    ctx.create_image()
                } else {
                    Some(cg)
                }
            }
        };

        if let Some(ref cg) = shared_cg {
            let result = h264_encoder.as_ref()
                .map(|e| e.encode_frame(cg, h264_frame_count, 30));
            match result {
                Some(Ok(frame)) => {
                    h264_frame_count += 1;

                    {
                        let wr = wr_handle.lock().unwrap();
                        if let Some(ref wr) = *wr {
                            if let Err(e) = wr.send_frame(&frame) {
                                eprintln!("  WebRTC send error: {}", e);
                            }
                        }
                    }

                    if h264_frame_count % 150 == 0 && last_report.elapsed() >= Duration::from_secs(3) {
                        print!("\r  {} frames", h264_frame_count);
                        std::io::stdout().flush().ok();
                        last_report = Instant::now();
                    }
                }
                Some(Err(e)) => {
                    eprintln!("  H.264 encode error: {}, restarting encoder", e);
                    h264_encoder = h264::VtEncoder::new(enc_w, enc_h, fps).ok();
                    h264_frame_count = 0;
                }
                None => {
                    h264_encoder = h264::VtEncoder::new(enc_w, enc_h, fps).ok();
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
    }

    srv.join().ok();
}
