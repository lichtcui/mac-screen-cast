use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use core_graphics::base::{kCGBitmapByteOrder32Big, kCGBitmapByteOrder32Little, kCGImageAlphaNoneSkipFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::CGContext;
use core_graphics::geometry::{CGRect, CGPoint, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window;
use foreign_types::ForeignType;

// ---------- pixel-format introspection ----------
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGImageGetBitmapInfo(image: *mut std::ffi::c_void) -> u32;
}

// ---------- Core Graphics window capture ----------

fn capture_window(window_id: u32) -> Option<Vec<u8>> {
    const MAX_W: u32 = 1280;
    const Q: u8 = 70;

    let cg = capture_cgimage(window_id)?;
    let (w, h) = (cg.width() as u32, cg.height() as u32);
    if w == 0 || h == 0 { return None; }

    // Fast path: read CGImage pixels directly
    // Fallback: render through a known-format bitmap context
    let rgb = read_cgimage_rgb(&cg).or_else(|| render_cgimage_to_rgb(&cg))?;

    // Scale if wider than limit — Nearest filter for speed
    let (fw, fh, data) = if w > MAX_W {
        let nh = (h * MAX_W) / w;
        let img = image::RgbImage::from_raw(w, h, rgb)?;
        let scaled = image::imageops::resize(&img, MAX_W, nh, image::imageops::FilterType::Nearest);
        (MAX_W, nh, scaled.into_raw())
    } else {
        (w, h, rgb)
    };

    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, Q);
    enc.encode(&data, fw, fh, image::ExtendedColorType::Rgb8).ok()?;

    if buf.len() > 500 { Some(buf) } else { None }
}

fn capture_cgimage(window_id: u32) -> Option<CGImage> {
    let null_rect = CGRect::new(
        &CGPoint::new(f64::INFINITY, f64::INFINITY),
        &CGSize::new(0.0, 0.0),
    );
    window::create_image(
        null_rect,
        window::kCGWindowListOptionIncludingWindow,
        window_id,
        window::kCGWindowImageDefault | window::kCGWindowImageNominalResolution,
    )
}

/// Read CGImage pixels directly and convert to packed RGB.
fn read_cgimage_rgb(image: &CGImage) -> Option<Vec<u8>> {
    let w = image.width();
    let h = image.height();
    if w == 0 || h == 0 { return None; }
    if image.bits_per_pixel() != 32 { return None; }

    let bpr = image.bytes_per_row();
    let raw_ptr = image.as_ptr() as *mut std::ffi::c_void;
    let bitmap_info = unsafe { CGImageGetBitmapInfo(raw_ptr) };
    let byte_order = bitmap_info & 0x7000;

    let cf_data = image.data();
    let raw: &[u8] = &cf_data;
    if raw.len() < h.saturating_mul(bpr) { return None; }

    let mut rgb = Vec::with_capacity(w * h * 3);

    match byte_order {
        // BGRA: bytes = [B, G, R, A] (most macOS systems)
        a if a == kCGBitmapByteOrder32Little => {
            for row in 0..h {
                let r = &raw[row * bpr..][..w * 4];
                for px in r.chunks_exact(4) {
                    rgb.push(px[2]);
                    rgb.push(px[1]);
                    rgb.push(px[0]);
                }
            }
        }
        // XRGB big-endian: bytes = [X, R, G, B]
        a if a == kCGBitmapByteOrder32Big => {
            for row in 0..h {
                let r = &raw[row * bpr..][..w * 4];
                for px in r.chunks_exact(4) {
                    rgb.push(px[1]);
                    rgb.push(px[2]);
                    rgb.push(px[3]);
                }
            }
        }
        _ => return None,
    }

    Some(rgb)
}

/// Fallback: render through a known-format bitmap context.
fn render_cgimage_to_rgb(image: &CGImage) -> Option<Vec<u8>> {
    let w = image.width();
    let h = image.height();
    if w == 0 || h == 0 { return None; }

    let cs = CGColorSpace::create_device_rgb();
    let bpr = w * 4;
    let mut ctx = CGContext::create_bitmap_context(
        None, w, h, 8, bpr, &cs,
        kCGBitmapByteOrder32Big | kCGImageAlphaNoneSkipFirst,
    );

    let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(w as _, h as _));
    ctx.draw_image(rect, image);

    let raw = ctx.data();
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in raw.chunks_exact(4) {
        rgb.push(px[1]);
        rgb.push(px[2]);
        rgb.push(px[3]);
    }
    Some(rgb)
}

// ---------- window listing ----------

fn list_windows() -> Vec<(u32, String, String)> {
    let script = r#"
import Foundation
import CoreGraphics
let w = CGWindowListCopyWindowInfo(.optionAll, kCGNullWindowID) as! [[String:Any]]
for x in w { if let n = x[kCGWindowName as String] as? String, !n.isEmpty,
                let o = x[kCGWindowOwnerName as String] as? String,
                let l = x[kCGWindowLayer as String] as? NSNumber, l.intValue == 0 {
                  print("\(x[kCGWindowNumber as String] as! NSNumber) ||| \(o) ||| \(n)") } }
"#;
    let out = match Command::new("swift").arg("-e").arg(script).output() {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };
    if !out.status.success() { return Vec::new(); }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
        let p: Vec<&str> = l.trim().split(" ||| ").collect();
        if p.len() >= 3 { Some((p[0].parse().ok()?, p[1].into(), p[2].into())) } else { None }
    }).collect()
}

// ---------- HTTP ----------

fn html(ip: &str) -> String {
    format!(r#"<!DOCTYPE html><html><meta charset="utf-8"><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><title>ScreenStream</title><style>*{{margin:0;background:#000}}body{{display:flex;min-height:100dvh;align-items:center;justify-content:center}}img{{width:100%;max-height:100dvh;object-fit:contain}}#b{{position:fixed;bottom:0;left:0;right:0;display:flex;gap:12px;padding:3px 10px;background:rgba(0,0,0,.5);color:#aaa;font:11px/1.3 monospace;z-index:99}}.g{{color:#4a4}}.r{{color:#c44}}</style><body><img id=s src="http://{ip}:8081/stream"><div id=b><span id=st class=g>streaming</span></div>"#)
}

fn get_ip() -> String {
    Command::new("sh").arg("-c").arg("ipconfig getifaddr en0 2>/dev/null || echo 127.0.0.1")
        .output().map(|o| String::from_utf8_lossy(&o.stdout).trim().into()).unwrap_or_default()
}

// ---------- MJPEG stream ----------

fn handle_mjpeg_client(mut stream: TcpStream, frame: Arc<Mutex<Arc<Vec<u8>>>>, version: Arc<AtomicU64>, signal: Arc<Condvar>, stop: Arc<AtomicBool>) {
    let mut buf = [0u8; 4096];
    if stream.read(&mut buf).is_err() { return; }
    let req = String::from_utf8_lossy(&buf);
    if !req.starts_with("GET /stream") { return; }

    let _ = stream.set_nodelay(true);
    let header = "HTTP/1.1 200 OK\r\n\
                  Content-Type: multipart/x-mixed-replace; boundary=--frame\r\n\
                  Cache-Control: no-cache\r\n\
                  Connection: close\r\n\r\n";
    if stream.write_all(header.as_bytes()).is_err() { return; }

    let mut last_version = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) { break; }
        let ver = version.load(Ordering::Acquire);
        if ver != last_version {
            let jpeg = frame.lock().unwrap().clone();
            if !jpeg.is_empty() {
                let part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
                if stream.write_all(part.as_bytes()).is_err() { break; }
                if stream.write_all(&jpeg).is_err() { break; }
                if stream.write_all(b"\r\n").is_err() { break; }
                let _ = stream.flush();
                last_version = ver;
            }
        } else {
            let lock = frame.lock().unwrap();
            if version.load(Ordering::Acquire) != last_version {
                continue;
            }
            let _ = signal.wait_timeout(lock, Duration::from_millis(500)).unwrap();
        }
    }
}

// ---------- main ----------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u32 = 0;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--window-id" => { i += 1; wid = args[i].parse().unwrap_or(0); }
            "-l" | "--list" => {
                for (id, app, title) in list_windows() {
                    println!("{:>5} | {} | {}", id, app, title);
                }
                return;
            }
            "-h" | "--help" => {
                println!("screenstream [-l] [-w <id>]");
                return;
            }
            _ => {}
        }
        i += 1;
    }

    if wid == 0 {
        let wins = list_windows();
        if wins.is_empty() { eprintln!("无窗口"); return; }
        let mut seen = std::collections::HashSet::new();
        let mut uq = Vec::new();
        for w in &wins { if seen.insert((&w.1, &w.2)) { uq.push(w); } }
        for (j, (_, a, t)) in uq.iter().enumerate() {
            println!("  [{:2}] {} - {}", j+1, a, if t.len()>55{&t[..55]}else{t});
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

    let frame: Arc<Mutex<Arc<Vec<u8>>>> = Arc::new(Mutex::new(Arc::new(Vec::new())));
    let frame_version = Arc::new(AtomicU64::new(0));
    let frame_signal: Arc<Condvar> = Arc::new(Condvar::new());
    let stop = Arc::new(AtomicBool::new(false));

    let svr_f = frame.clone();
    let svr_s = stop.clone();
    let svr_w = wid;
    let ip = get_ip();

    let srv = thread::spawn(move || {
        use tiny_http::{Header, Response};
        let server = match tiny_http::Server::http("0.0.0.0:8080") {
            Ok(s) => s,
            Err(e) => { eprintln!("端口 8080 被占用: {}", e); return; }
        };
        eprintln!("\n{:->50}", "");
        eprintln!("  ScreenStream  |  window {}  |  http://{}:8080", svr_w, ip);
        eprintln!("  MJPEG stream  |             |  http://{}:8081/stream", ip);
        eprintln!("{:->50}\n", "");

        loop {
            if svr_s.load(Ordering::Relaxed) { break; }
            let req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r, _ => continue,
            };
            let path = req.url().split('?').next().unwrap_or("/").to_string();
            let resp = match path.as_str() {
                "/" => Response::from_data(html(&ip).into_bytes())
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<Header>().unwrap()),
                "/frame" => {
                    let j = svr_f.lock().unwrap().clone();
                    if j.is_empty() { Response::from_data(Vec::new()).with_status_code(503) }
                    else {
                        Response::from_data(j.to_vec())
                            .with_header("Content-Type: image/jpeg".parse::<Header>().unwrap())
                            .with_header("Cache-Control: no-cache, no-store, must-revalidate".parse::<Header>().unwrap())
                            .with_header("Pragma: no-cache".parse::<Header>().unwrap())
                            .with_header("Expires: 0".parse::<Header>().unwrap())
                            .with_header("Access-Control-Allow-Origin: *".parse::<Header>().unwrap())
                    }
                }
                _ => Response::from_data(Vec::new()).with_status_code(404),
            };
            req.respond(resp).ok();
        }
    });

    let mjpeg_frame = frame.clone();
    let mjpeg_fv = frame_version.clone();
    let mjpeg_cv = frame_signal.clone();
    let mjpeg_stop = stop.clone();

    let mjpeg = thread::spawn(move || {
        let listener = match TcpListener::bind("0.0.0.0:8081") {
            Ok(l) => l,
            Err(e) => { eprintln!("端口 8081 被占用: {}", e); return; }
        };
        let _ = listener.set_nonblocking(true);
        loop {
            if mjpeg_stop.load(Ordering::Relaxed) { break; }
            match listener.accept() {
                Ok((stream, _)) => {
                    thread::spawn({
                        let f = mjpeg_frame.clone();
                        let v = mjpeg_fv.clone();
                        let c = mjpeg_cv.clone();
                        let s = mjpeg_stop.clone();
                        move || handle_mjpeg_client(stream, f, v, c, s)
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    });

    let c_stop = stop.clone();
    let c_cv = frame_signal.clone();
    static CTRLC_COUNT: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        if CTRLC_COUNT.swap(true, Ordering::Relaxed) {
            eprintln!("\n强制退出");
            std::process::exit(1);
        }
        eprintln!("\n⏳ 正在停止 (再按一次 Ctrl+C 强制退出)...");
        c_stop.store(true, Ordering::Relaxed);
        c_cv.notify_all();
    }).ok();

    let mut fc: u64 = 0;
    let mut last = Instant::now();
    let mut fpc: u32 = 0;
    let frame_dur = Duration::from_secs_f64(1.0 / 30.0);
    let mut next_capture = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        if let Some(jpeg) = capture_window(wid) {
            *frame.lock().unwrap() = Arc::new(jpeg);
            frame_version.fetch_add(1, Ordering::Release);
            frame_signal.notify_all();
            fc += 1; fpc += 1;
        }
        let now = Instant::now();
        if now < next_capture {
            thread::sleep(next_capture - now);
        }
        next_capture += frame_dur;
        if next_capture < now {
            next_capture = now;
        }
        if last.elapsed() >= Duration::from_secs(5) {
            let fps = fpc as f64 / last.elapsed().as_secs_f64();
            eprintln!("  {:4.0} fps | {} frames", fps, fc);
            fpc = 0; last = Instant::now();
        }
    }

    srv.join().ok();
    mjpeg.join().ok();
}
