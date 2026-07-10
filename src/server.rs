use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::Duration;
use tiny_http::{Header, Response};

/// A visible window.
#[derive(Debug, serde::Serialize)]
pub struct Window {
    pub id: u32,
    pub app: String,
    pub title: String,
}

/// List visible windows via Swift.
pub fn list_windows() -> Vec<Window> {
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
        Ok(o) => {
            if !o.status.success() {
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!("  Window listing failed: swift exited with error:\n{}", stderr.trim());
                return Vec::new();
            }
            o
        }
        Err(e) => {
            eprintln!("  Window listing failed: could not run swift -e: {}", e);
            return Vec::new();
        }
    };
    String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
        let p: Vec<&str> = l.trim().split(" ||| ").collect();
        if p.len() >= 3 { Some(Window { id: p[0].parse().ok()?, app: p[1].into(), title: p[2].into() }) } else { None }
    }).collect()
}

/// List windows as JSON array.
pub fn list_windows_json() -> String {
    serde_json::to_string(&list_windows()).unwrap_or_else(|_| "[]".into())
}

/// WebRTC video page with real-time latency display.
///
/// Template uses `{{TITLE}}` and `{{FPS}}` placeholders,
/// defined in `docs/resources/player.html`.
pub fn html(fps: u32, title: &str) -> String {
    include_str!("../docs/resources/player.html")
        .replace("{{TITLE}}", title)
        .replace("{{FPS}}", &fps.to_string())
}

/// Get local IP address of the default route interface.
///
/// Uses `route -n get default` (parsed in Rust) to find the primary
/// interface, then queries its IP with `ipconfig getifaddr`.
/// Falls back to probing en0/en1/en2 directly, then 127.0.0.1.
pub fn get_ip() -> String {
    // Find the default route interface by parsing `route -n get default` output in Rust
    // (no shell, no grep/awk — pure Rust string parsing)
    let default_iface = Command::new("route")
        .args(["-n", "get", "default"])
        .output()
        .ok()
        .and_then(|o| {
            if !o.status.success() {
                return None;
            }
            let stdout = String::from_utf8_lossy(&o.stdout);
            for line in stdout.lines() {
                let line = line.trim();
                if let Some(iface) = line.strip_prefix("interface:") {
                    let iface = iface.trim();
                    if !iface.is_empty() {
                        return Some(iface.to_string());
                    }
                }
            }
            None
        });

    if let Some(ref iface) = default_iface {
        if let Ok(out) = Command::new("ipconfig")
            .arg("getifaddr")
            .arg(iface)
            .output()
        {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ip.is_empty() && ip.contains('.') {
                return ip;
            }
        }
    }

    // Fallback: probe common interfaces directly (no shell wrapper needed)
    for iface in &["en0", "en1", "en2"] {
        if let Ok(out) = Command::new("ipconfig").arg("getifaddr").arg(iface).output() {
            let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !ip.is_empty() && ip.contains('.') {
                return ip;
            }
        }
    }

    String::from("127.0.0.1")
}

// ── RwLock helpers (recover from poisoned locks) ──

pub(crate) fn rw_read<'a, T>(rw: &'a RwLock<T>) -> std::sync::RwLockReadGuard<'a, T> {
    rw.read().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn rw_write<'a, T>(rw: &'a RwLock<T>) -> std::sync::RwLockWriteGuard<'a, T> {
    rw.write().unwrap_or_else(|e| e.into_inner())
}

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

// ── HTTP server thread ──

#[allow(clippy::too_many_arguments)]
pub fn spawn_http_server(
    port: u16,
    fps: u32,
    out_w: u32,
    out_h: u32,
    window_name: &str,
    wr_handle: Arc<RwLock<Option<crate::webrtc::WebRtcHandle>>>,
    wr_version: Arc<AtomicU64>,
    wr_connected: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    latest_latency: Arc<AtomicU64>,
    webrtc_rt: Arc<tokio::runtime::Runtime>,
) -> thread::JoinHandle<()> {
    let ip = get_ip();
    let wn = window_name.to_string();

    eprintln!("  Initializing WebRTC...");

    thread::spawn(move || {
        let server = match tiny_http::Server::http(format!("0.0.0.0:{}", port)) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Port {} in use: {}", port, e);
                return;
            }
        };
        eprintln!("\n  WebRTC stream \u{2192}  http://{}:{}", ip, port);

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
                        (true, true) => '\u{2588}',
                        (true, false) => '\u{2580}',
                        (false, true) => '\u{2584}',
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
                "/" => Response::from_data(html(fps, &wn).into_bytes())
                    .with_header(header_html()),
                "/offer" => {
                    let was_connected = wr_connected.swap(false, Ordering::Relaxed);
                    if was_connected {
                        let old = rw_read(&wr_handle).clone();
                        if let Some(ref wr) = old {
                            wr.close();
                        }
                        if let Ok(new_handle) =
                            crate::webrtc::WebRtcHandle::new(webrtc_rt.handle().clone(), fps, out_w, out_h)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_contains_video_tag() {
        let page = html(30, "test");
        assert!(page.contains("<video"));
        assert!(page.contains("/offer"));
        assert!(page.contains("/latency"));
        assert!(page.contains("/signal"));
    }

    #[test]
    fn html_works_without_stun() {
        let page = html(30, "test");
        assert!(page.contains("<video"));
        assert!(page.contains("/offer"));
        assert!(page.contains("/latency"));
        assert!(page.contains("/signal"));
        assert!(!page.contains("stun.l.google.com"));
    }

    #[test]
    fn html_uses_title() {
        let page = html(30, "Ghostty - 👻");
        assert!(page.contains("<title>Ghostty - 👻</title>"));
    }

    #[test]
    fn html_shows_fps() {
        let page = html(60, "test");
        assert!(page.contains("60fps"));
        assert!(!page.contains("{{FPS}}"));
    }

    #[test]
    fn get_ip_returns_string() {
        let ip = get_ip();
        assert!(!ip.is_empty());
        assert!(ip.contains('.'));
    }
}
