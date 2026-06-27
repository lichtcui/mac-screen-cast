mod capture;
mod fmp4;
mod h264;
mod server;

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use core_graphics::base::{kCGBitmapByteOrder32Big, kCGImageAlphaNoneSkipFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::{CGContext, CGInterpolationQuality};
use core_graphics::geometry::{CGRect, CGPoint, CGSize};

fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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

    // Shared video state for fMP4
    let video_init: Arc<ArcSwap<Vec<u8>>> = Arc::new(ArcSwap::from_pointee(Vec::new()));
    let video_segments: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let video_seg_count: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let video_seg_base: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
    let last_h264_req: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));
    let svr_s = stop.clone();
    let svr_port = port;
    let ip = server::get_ip();

    let srv_video_init = video_init.clone();
    let srv_video_segs = video_segments.clone();
    let srv_video_base = video_seg_base.clone();
    let srv_last_h264 = last_h264_req.clone();

    let srv = thread::spawn(move || {
        use tiny_http::{Header, Response};
        let server = match tiny_http::Server::http(format!("0.0.0.0:{}", svr_port)) {
            Ok(s) => s,
            Err(e) => { eprintln!("端口 {} 被占用: {}", svr_port, e); return; }
        };
        eprintln!("\n  H.264 stream  →  http://{}:{}", ip, svr_port);

        let hdr_ct_video = || "Content-Type: video/mp4".parse::<Header>().unwrap();
        let hdr_cache = || "Cache-Control: no-cache, no-store, must-revalidate".parse::<Header>().unwrap();
        let hdr_pragma = || "Pragma: no-cache".parse::<Header>().unwrap();
        let hdr_expires = || "Expires: 0".parse::<Header>().unwrap();
        let hdr_cors = || "Access-Control-Allow-Origin: *".parse::<Header>().unwrap();
        let hdr_seg = |n: u32| format!("X-Seg: {}", n).parse::<Header>().unwrap();

        loop {
            if svr_s.load(Ordering::Relaxed) { break; }
            let req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r, _ => continue,
            };
            let url = req.url();
            let path = url.split('?').next().unwrap_or("/");
            let resp = match path {
                "/" => Response::from_data(server::html(fps).into_bytes())
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<Header>().unwrap()),
                "/init.mp4" => {
                    srv_last_h264.store(epoch_millis(), Ordering::Relaxed);
                    let data = srv_video_init.load_full();
                    if data.is_empty() {
                        let mut result = Response::from_data(Vec::new()).with_status_code(503);
                        for _ in 0..50 {
                            thread::sleep(Duration::from_millis(100));
                            let d = srv_video_init.load_full();
                            if !d.is_empty() {
                                result = Response::from_data(d.to_vec())
                                    .with_header(hdr_ct_video());
                                break;
                            }
                        }
                        result
                    } else {
                        Response::from_data(data.to_vec()).with_header(hdr_ct_video())
                    }
                }
                "/seg" => {
                    srv_last_h264.store(epoch_millis(), Ordering::Relaxed);
                    let after: u32 = url.split('?').nth(1).and_then(|q| {
                        q.split('&').next().and_then(|s| s.parse().ok())
                    }).unwrap_or(0);
                    let mut result = Response::from_data(Vec::new()).with_status_code(503);
                    for _ in 0..100 {
                        if svr_s.load(Ordering::Relaxed) { break; }
                        let base = srv_video_base.load(Ordering::Acquire);
                        let segs = srv_video_segs.lock().unwrap();
                        if segs.is_empty() { drop(segs); thread::sleep(Duration::from_millis(50)); continue; }
                        let seg_num = if after >= base {
                            let idx = (after - base) as usize;
                            if idx < segs.len() { after }
                            else { drop(segs); thread::sleep(Duration::from_millis(50)); continue; }
                        } else {
                            base
                        };
                        let idx = (seg_num - base) as usize;
                        if idx < segs.len() {
                            result = Response::from_data(segs[idx].clone())
                                .with_header(hdr_ct_video())
                                .with_header(hdr_cache())
                                .with_header(hdr_pragma())
                                .with_header(hdr_expires())
                                .with_header(hdr_cors())
                                .with_header(hdr_seg(seg_num));
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                    result
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
    let mut h264_fmp4: Option<fmp4::Fmp4State> = None;
    let mut h264_frame_count: u64 = 0;
    let mut h264_ready = false;

    let vt_init = video_init.clone();
    let vt_segs = video_segments.clone();
    let vt_count = video_seg_count.clone();
    let vt_base = video_seg_base.clone();
    let main_last_h264 = last_h264_req.clone();
    let mut scaler_ctx: Option<(CGContext, u32, u32)> = None;

    let mut last_report = Instant::now();

    let mut idle_since: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed) {
        // Go idle when there have been no H.264 requests for 10 seconds
        let clients_h264 = epoch_millis() - main_last_h264.load(Ordering::Relaxed) <= 10000;
        if !clients_h264 && h264_ready {
            match idle_since {
                None => idle_since = Some(Instant::now()),
                Some(t) if t.elapsed() > Duration::from_secs(10) => {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                _ => {}
            }
        } else {
            idle_since = None;
        }

        // Recover dead encoder
        if h264_encoder.is_none() {
            h264_encoder = h264::VtEncoder::new(enc_w, enc_h, fps).ok();
            if h264_encoder.is_some() && h264_ready {
                eprintln!("  H.264 encoder re-created");
            }
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

                    if h264_frame_count % 150 == 0 && last_report.elapsed() >= Duration::from_secs(3) {
                        print!("\r  {} frames @ {}fps", h264_frame_count, fps);
                        std::io::stdout().flush().ok();
                        last_report = Instant::now();
                    }

                    if h264_fmp4.is_none() && frame.sps.is_some() && frame.pps.is_some() {
                        let sps = frame.sps.clone().unwrap();
                        let pps = frame.pps.clone().unwrap();
                        let fmp4 = fmp4::Fmp4State::new(sps, pps, enc_w, enc_h, 90000);
                        let init_seg = fmp4.build_init_segment();
                        vt_init.store(Arc::new(init_seg));
                        h264_fmp4 = Some(fmp4);
                    }

                    if let Some(ref mut fmp4) = h264_fmp4 {
                        let seg = fmp4.build_media_segment(
                            &frame.data, frame.is_keyframe,
                            frame.pts_timescale, frame.pts_value,
                        );
                        let mut segs = vt_segs.lock().unwrap();
                        segs.push(seg);
                        if segs.len() > 300 {
                            segs.remove(0);
                            vt_base.fetch_add(1, Ordering::Release);
                        }
                        vt_count.fetch_add(1, Ordering::Release);
                        if !h264_ready {
                            h264_ready = true;
                            eprintln!("  H.264 stream ready");
                        }
                    }
                }
                Some(Err(e)) => {
                    eprintln!("  H.264 encode error: {}, restarting encoder", e);
                    h264_encoder = h264::VtEncoder::new(enc_w, enc_h, fps).ok();
                    h264_fmp4 = None;
                    h264_ready = false;
                    h264_frame_count = 0;
                    vt_init.store(Arc::new(Vec::new()));
                    vt_segs.lock().unwrap().clear();
                    vt_base.store(0, Ordering::Release);
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
