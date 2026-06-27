use std::io::Write;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// ---------- CGWindowListCreateImage via dlsym ----------

fn capture_window(window_id: u32) -> Option<Vec<u8>> {
    let tmp = format!("/tmp/sc{}.jpg", window_id);
    Command::new("screencapture")
        .args(["-l", &window_id.to_string(), "-x", &tmp])
        .output().ok()?;
    // Scale to max 1280px wide (phone-friendly), sips is macOS built-in
    Command::new("sips")
        .args(["-Z", "1280", &tmp, "--out", &tmp])
        .output().ok()?;
    let data = std::fs::read(&tmp).ok()?;
    let _ = std::fs::remove_file(&tmp);
    if data.len() > 500 { Some(data) } else { None }
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

const HTML: &str = r#"<!DOCTYPE html><html><meta charset="utf-8"><meta name=viewport content="width=device-width,initial-scale=1,maximum-scale=1,user-scalable=no"><title>ScreenStream</title><style>*{margin:0;background:#000}body{display:flex;min-height:100dvh;align-items:center;justify-content:center}img{width:100%;max-height:100dvh;object-fit:contain}#b{position:fixed;bottom:0;left:0;right:0;display:flex;gap:12px;padding:3px 10px;background:rgba(0,0,0,.5);color:#aaa;font:11px/1.3 monospace;z-index:99}.g{color:#4a4}.r{color:#c44}</style><body><img id=s><div id=b><span id=st class=r>init</span><span id=fs>--</span><span id=sz>--</span><span id=ms>--</span></div><script>(function(){var i=document.getElementById('s'),st=document.getElementById('st'),fs=document.getElementById('fs'),sz=document.getElementById('sz'),ms=document.getElementById('ms'),fc=0,t0=Date.now();function u(){var t1=performance.now();i.src='/frame?'+Math.random();i.onload=function(){fc++;ms.textContent=(performance.now()-t1).toFixed(0)+'ms';st.textContent='live';st.className='g';sz.textContent=i.naturalWidth+'x'+i.naturalHeight;fs.textContent=(fc/((Date.now()-t0)/1000)).toFixed(1)+'fps';setTimeout(u,50)};i.onerror=function(){st.textContent='err';st.className='r';setTimeout(u,200)}};setTimeout(u,200)})()</script>"#;

fn get_ip() -> String {
    Command::new("sh").arg("-c").arg("ipconfig getifaddr en0 2>/dev/null || echo 127.0.0.1")
        .output().map(|o| String::from_utf8_lossy(&o.stdout).trim().into()).unwrap_or_default()
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

    // Shared state
    let frame: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));

    // HTTP server thread
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
        eprintln!("{:->50}\n", "");

        loop {
            if svr_s.load(Ordering::Relaxed) { break; }
            let req = match server.recv_timeout(Duration::from_millis(500)) {
                Ok(Some(r)) => r, _ => continue,
            };
            let path = req.url().split('?').next().unwrap_or("/").to_string();
            let resp = match path.as_str() {
                "/" => Response::from_data(HTML.as_bytes().to_vec())
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<Header>().unwrap()),
                "/frame" => {
                    let j = svr_f.lock().unwrap().clone();
                    if j.is_empty() { Response::from_data(Vec::new()).with_status_code(503) }
                    else {
                        Response::from_data(j)
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

    // Ctrl+C — first press sets stop flag, second press force-exits
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

    // Capture loop (main thread)
    let mut fc: u64 = 0;
    let mut last = Instant::now();
    let mut fpc: u32 = 0;

    while !stop.load(Ordering::Relaxed) {
        if let Some(jpeg) = capture_window(wid) {
            *frame.lock().unwrap() = jpeg;
            fc += 1; fpc += 1;
        }
        // No artificial sleep — let CGWindowListCreateImage be the rate limiter
        if last.elapsed() >= Duration::from_secs(5) {
            let fps = fpc as f64 / last.elapsed().as_secs_f64();
            eprintln!("  {:4.0} fps | {} frames", fps, fc);
            fpc = 0; last = Instant::now();
        }
    }

    srv.join().ok();
}