use std::time::{Duration, SystemTime};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "lichtcui/mac-screen-cast";

/// Check for newer releases on GitHub in a background thread.
/// Prints to stderr if a newer version is found.
pub fn check() {
    std::thread::spawn(|| {
        let cache = std::env::temp_dir().join("msc-version-cache");
        if let Ok(meta) = std::fs::metadata(&cache) {
            if let Ok(modified) = meta.modified() {
                if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                    if elapsed < Duration::from_secs(86400) {
                        return;
                    }
                }
            }
        }
        let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
        let resp = ureq::get(&url)
            .set("User-Agent", "mac-screen-cast")
            .call();
        let resp = match resp {
            Ok(r) => r,
            Err(_) => return,
        };
        let body: String = resp.into_string().unwrap_or_default();
        let body: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();

        let tag = match body["tag_name"].as_str() {
            Some(t) => t,
            None => return,
        };

        let _ = std::fs::write(&cache, tag);

        match (parse_version(tag), parse_version(CURRENT_VERSION)) {
            (Some(latest), Some(current)) if latest > current => {
                eprintln!(
                    "  Update available: {} → {} (cargo install mac-screen-cast --force)",
                    CURRENT_VERSION, tag
                );
                if let Some(url) = body["html_url"].as_str() {
                    eprintln!("  {}\n", url);
                }
            }
            _ => {}
        }
    });
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let parts: Vec<&str> = v.splitn(3, '.').collect();
    if parts.len() == 3 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}
