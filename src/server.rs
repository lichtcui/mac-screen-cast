use std::process::Command;

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
/// Uses `route get default` to find the primary interface, then queries
/// its IP with `ipconfig getifaddr`. Falls back to probing en0/en1 directly,
/// then 127.0.0.1. Typically completes in 2 process spawns instead of 10.
pub fn get_ip() -> String {
    // Find the default route interface name with a single `route get` call
    let default_iface = Command::new("sh")
        .arg("-c")
        .arg("route get default 2>/dev/null | grep 'interface:' | awk '{print $2}'")
        .output()
        .ok()
        .and_then(|o| {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() { Some(s) } else { None }
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
