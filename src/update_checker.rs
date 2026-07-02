use std::time::{Duration, SystemTime};

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "lichtcui/mac-screen-cast";
const CACHE_TTL: Duration = Duration::from_secs(86400); // 24h
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Check for newer releases on GitHub in a background thread.
///
/// Caches the latest tag in `/tmp/msc-version-cache` (24h TTL).
/// Prints to stderr if a newer version is found.
pub fn check() {
    std::thread::spawn(|| {
        // Respect cache TTL
        let cache = std::env::temp_dir().join("msc-version-cache");
        if let Ok(meta) = std::fs::metadata(&cache) {
            if let Ok(modified) = meta.modified() {
                if let Ok(elapsed) = SystemTime::now().duration_since(modified) {
                    if elapsed < CACHE_TTL {
                        return;
                    }
                }
            }
        }

        // Fetch latest release with a 10s timeout so a hang doesn't block shutdown
        let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
        let result = ureq::get(&url)
            .set("User-Agent", "mac-screen-cast")
            .timeout(HTTP_TIMEOUT)
            .call();

        let tag = match result {
            Ok(resp) => {
                let body: String = resp.into_string().unwrap_or_default();
                let body: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                match body["tag_name"].as_str() {
                    Some(t) => t.to_string(),
                    None => return, // unexpected response shape
                }
            }
            Err(ureq::Error::Status(code, _)) if code == 404 || code == 403 => {
                return; // repo not found or rate limited — not actionable
            }
            Err(_) => {
                return; // network error or timeout — best-effort
            }
        };

        // Update cache regardless (avoids retrying on next launch)
        let _ = std::fs::write(&cache, &tag);

        // Compare and notify
        if let (Some(latest), Some(current)) = (parse_version(&tag), parse_version(CURRENT_VERSION)) {
            if latest > current {
                eprintln!(
                    "  Update available: {} → {} (cargo install mac-screen-cast --force)",
                    CURRENT_VERSION, tag
                );
                eprintln!("  https://github.com/{}/releases/tag/{}\n", REPO, tag);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_v_prefix() {
        assert_eq!(parse_version("v0.2.4"), Some((0, 2, 4)));
    }

    #[test]
    fn parse_version_no_prefix() {
        assert_eq!(parse_version("1.15.0"), Some((1, 15, 0)));
    }

    #[test]
    fn parse_version_only_two_parts() {
        assert_eq!(parse_version("1.0"), None);
    }

    #[test]
    fn parse_version_empty() {
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn parse_version_non_numeric() {
        assert_eq!(parse_version("v0.2.4-beta"), None);
    }

    #[test]
    fn parse_version_four_parts() {
        // splitn(3, '.') only splits into 3 parts, the last contains the rest
        assert_eq!(parse_version("1.2.3.4"), None);
    }

    #[test]
    fn parse_version_just_v() {
        assert_eq!(parse_version("v"), None);
    }

    #[test]
    fn parse_version_leading_zero() {
        assert_eq!(parse_version("0.0.0"), Some((0, 0, 0)));
    }

    #[test]
    fn parse_version_large_numbers() {
        assert_eq!(parse_version("999.888.777"), Some((999, 888, 777)));
    }
}
