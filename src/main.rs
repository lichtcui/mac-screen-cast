use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::hash::{DefaultHasher, Hasher};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use threadpool::ThreadPool;

use core_graphics::base::{kCGBitmapByteOrder32Big, kCGImageAlphaNoneSkipFirst};
use core_graphics::color_space::CGColorSpace;
use core_graphics::context::{CGContext, CGInterpolationQuality};
use core_graphics::geometry::{CGRect, CGPoint, CGSize};
use core_graphics::image::CGImage;
use core_graphics::window;
use foreign_types::ForeignType;

use core_foundation::base::{CFIndex, CFRelease, CFType, TCFType};
use core_foundation::data::{CFMutableDataRef, CFDataCreateMutable, CFDataGetMutableBytePtr, CFDataGetLength};
use core_foundation::dictionary::CFMutableDictionary;
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};

// ---------- Core Graphics window capture ----------

fn capture_window(window_id: u32, max_w: u32, quality: u8, jpeg_out: &mut Vec<u8>) -> bool
{
    let cg = match capture_cgimage(window_id) {
        Some(c) => c, None => return false,
    };
    let (w, h) = (cg.width() as u32, cg.height() as u32);
    if w == 0 || h == 0 { return false; }

    // Scale via CoreGraphics if wider than limit (hardware-accelerated on Apple Silicon)
    let encode_image = if w > max_w {
        let nh = (h * max_w) / w;
        let cs = CGColorSpace::create_device_rgb();
        let ctx = CGContext::create_bitmap_context(
            None, max_w as usize, nh as usize, 8, max_w as usize * 4, &cs,
            kCGBitmapByteOrder32Big | kCGImageAlphaNoneSkipFirst,
        );
        ctx.set_interpolation_quality(CGInterpolationQuality::CGInterpolationQualityHigh);
        let rect = CGRect::new(&CGPoint::new(0.0, 0.0), &CGSize::new(max_w as _, nh as _));
        ctx.draw_image(rect, &cg);
        match ctx.create_image() {
            Some(img) => img,
            None => return false,
        }
    } else {
        cg
    };

    encode_native_jpeg(&encode_image, quality, jpeg_out)
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

// ---------- native JPEG encoding via ImageIO ----------

#[link(name = "ImageIO", kind = "framework")]
extern "C" {
    static kCGImageDestinationLossyCompressionQuality: CFStringRef;

    fn CGImageDestinationCreateWithData(
        data: CFMutableDataRef,
        type_: CFStringRef,
        count: CFIndex,
        options: *const std::ffi::c_void,
    ) -> *mut std::ffi::c_void;

    fn CGImageDestinationAddImage(
        dest: *mut std::ffi::c_void,
        image: *const std::ffi::c_void,
        properties: *const std::ffi::c_void,
    );

    fn CGImageDestinationFinalize(dest: *mut std::ffi::c_void) -> u8;
}

fn encode_native_jpeg(image: &CGImage, quality: u8, out: &mut Vec<u8>) -> bool {
    unsafe {
        let data = CFDataCreateMutable(std::ptr::null_mut(), 0);
        if data.is_null() {
            return false;
        }

        let uti = CFString::new("public.jpeg");
        let dest = CGImageDestinationCreateWithData(data, uti.as_concrete_TypeRef(), 1, std::ptr::null());
        if dest.is_null() {
            CFRelease(data as *const _);
            return false;
        }
        let dest = CFType::wrap_under_create_rule(dest as *const _);

        // Properties: { kCGImageDestinationLossyCompressionQuality = quality/100.0 }
        let quality_val = CFNumber::from(quality as f64 / 100.0);
        let mut props = CFMutableDictionary::<*const std::ffi::c_void, *const std::ffi::c_void>::new();
        props.add(
            &(kCGImageDestinationLossyCompressionQuality as *const std::ffi::c_void),
            &(quality_val.as_CFTypeRef()),
        );

        CGImageDestinationAddImage(
            dest.as_concrete_TypeRef() as *mut _,
            image.as_ptr() as *const _,
            props.as_concrete_TypeRef() as *const _,
        );

        let ok = CGImageDestinationFinalize(dest.as_concrete_TypeRef() as *mut _);

        if ok != 0 {
            let len = CFDataGetLength(data);
            if len > 0 {
                let ptr = CFDataGetMutableBytePtr(data);
                if !ptr.is_null() {
                    let bytes = std::slice::from_raw_parts(ptr, len as usize);
                    out.clear();
                    out.extend_from_slice(bytes);
                }
            }
        }

        CFRelease(data as *const _);
        ok != 0
    }
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
    format!(r#"<!DOCTYPE html><html><meta charset="utf-8"><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><title>ScreenStream</title><style>*{{margin:0;background:#000}}body{{display:flex;min-height:100dvh;align-items:center;justify-content:center}}img{{width:100%;max-height:100dvh;object-fit:contain}}#b{{position:fixed;bottom:0;left:0;right:0;display:flex;gap:12px;padding:3px 10px;background:rgba(0,0,0,.5);color:#aaa;font:11px/1.3 monospace;z-index:99}}.g{{color:#4a4}}.r{{color:#c44}}</style><body><img id=s src="http://{ip}:8081/stream"><div id=b><span id=st class=g>streaming</span></div><script>document.getElementById('s').onerror=function(){{var s=document.getElementById('st');s.textContent='reconnecting';s.className='r';var i=this;setTimeout(function(){{i.src='http://{ip}:8081/stream?'+Date.now()}},2000)}}</script>"#)
}

fn get_ip() -> String {
    Command::new("sh").arg("-c").arg("ipconfig getifaddr en0 2>/dev/null || echo 127.0.0.1")
        .output().map(|o| String::from_utf8_lossy(&o.stdout).trim().into()).unwrap_or_default()
}

// ---------- MJPEG stream ----------

fn handle_mjpeg_client(mut stream: TcpStream, frame: Arc<ArcSwap<Vec<u8>>>, version: Arc<AtomicU64>, signal: Arc<(Mutex<()>, Condvar)>, stop: Arc<AtomicBool>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let mut buf = [0u8; 4096];
    if stream.read(&mut buf).is_err() { return; }
    let req = String::from_utf8_lossy(&buf);
    if !req.starts_with("GET /stream") { return; }

    let _ = stream.set_nodelay(true);
    let header = "HTTP/1.1 200 OK\r\n\
                  Content-Type: multipart/x-mixed-replace; boundary=frame\r\n\
                  Cache-Control: no-cache\r\n\
                  Connection: close\r\n\r\n";
    if stream.write_all(header.as_bytes()).is_err() { return; }

    let mut last_version = 0u64;
    loop {
        if stop.load(Ordering::Relaxed) { break; }
        let ver = version.load(Ordering::Acquire);
        if ver != last_version {
            let jpeg = frame.load_full();
            if !jpeg.is_empty() {
                let part = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len());
                if stream.write_all(part.as_bytes()).is_err() { break; }
                if stream.write_all(&jpeg).is_err() { break; }
                if stream.write_all(b"\r\n").is_err() { break; }
                let _ = stream.flush();
                last_version = ver;
            }
        } else {
            let (mtx, cv) = &*signal;
            let guard = mtx.lock().unwrap();
            if version.load(Ordering::Acquire) != last_version {
                continue;
            }
            let _ = cv.wait_timeout(guard, Duration::from_millis(500)).unwrap();
        }
    }
}

// ---------- main ----------

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut wid: u32 = 0;
    let mut max_w: u32 = 1280;
    let mut quality: u8 = 70;
    let mut fps: u32 = 30;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-w" | "--window-id" => { i += 1; wid = args[i].parse().unwrap_or(0); }
            "--width" => { i += 1; max_w = args[i].parse().unwrap_or(1280); }
            "-q" | "--quality" => { i += 1; quality = args[i].parse().unwrap_or(70); }
            "--fps" => { i += 1; fps = args[i].parse().unwrap_or(30).clamp(1, 60); }
            "-l" | "--list" => {
                for (id, app, title) in list_windows() {
                    println!("{:>5} | {} | {}", id, app, title);
                }
                return;
            }
            "-h" | "--help" => {
                println!("screenstream [-l] [-w <id>] [--width <px>] [-q|--quality <1-100>] [--fps <1-60>]");
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

    let frame: Arc<ArcSwap<Vec<u8>>> = Arc::new(ArcSwap::from_pointee(Vec::new()));
    let frame_version = Arc::new(AtomicU64::new(0));
    let signal: Arc<(Mutex<()>, Condvar)> = Arc::new((Mutex::new(()), Condvar::new()));
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
                    let j = svr_f.load_full();
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
    let mjpeg_sig = signal.clone();
    let mjpeg_stop = stop.clone();
    let pool = ThreadPool::new(16);

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
                    let f = mjpeg_frame.clone();
                    let v = mjpeg_fv.clone();
                    let sig = mjpeg_sig.clone();
                    let s = mjpeg_stop.clone();
                    pool.execute(move || handle_mjpeg_client(stream, f, v, sig, s));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break,
            }
        }
    });

    let c_stop = stop.clone();
    let c_sig = signal.clone();
    static CTRLC_COUNT: AtomicBool = AtomicBool::new(false);
    ctrlc::set_handler(move || {
        if CTRLC_COUNT.swap(true, Ordering::Relaxed) {
            eprintln!("\n强制退出");
            std::process::exit(1);
        }
        eprintln!("\n⏳ 正在停止 (再按一次 Ctrl+C 强制退出)...");
        c_stop.store(true, Ordering::Relaxed);
        c_sig.1.notify_all();
    }).ok();

    let mut jpeg_buf = Vec::new();
    let mut fc: u64 = 0;
    let mut last = Instant::now();
    let mut fpc: u32 = 0;
    let frame_dur_active = Duration::from_secs_f64(1.0 / fps as f64);
    let mut frame_dur = frame_dur_active;
    let mut next_capture = Instant::now();
    let mut content_hash = 0u64;
    let mut idle_count = 0u32;

    while !stop.load(Ordering::Relaxed) {
        if capture_window(wid, max_w, quality, &mut jpeg_buf) {
            let h = {
                let mut s = DefaultHasher::new();
                s.write(&jpeg_buf);
                s.finish()
            };
            if h == content_hash {
                idle_count += 1;
                if idle_count > 5 {
                    frame_dur = Duration::from_secs_f64(1.0);
                }
            } else {
                content_hash = h;
                idle_count = 0;
                frame_dur = frame_dur_active;
                let jpeg = std::mem::take(&mut jpeg_buf);
                frame.store(Arc::new(jpeg));
                frame_version.fetch_add(1, Ordering::Release);
                signal.1.notify_all();
                fc += 1; fpc += 1;
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
        if last.elapsed() >= Duration::from_secs(5) {
            let fps = if fpc > 0 { fpc as f64 / last.elapsed().as_secs_f64() } else { 0.0 };
            eprintln!("  {:4.0} fps | {} frames", fps, fc);
            fpc = 0; last = Instant::now();
        }
    }

    srv.join().ok();
    mjpeg.join().ok();
}
