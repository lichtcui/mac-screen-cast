# mac-screen-cast

[![Crates.io](https://img.shields.io/crates/v/mac-screen-cast)](https://crates.io/crates/mac-screen-cast)
[![Downloads](https://img.shields.io/crates/d/mac-screen-cast)](https://crates.io/crates/mac-screen-cast)
[![License](https://img.shields.io/crates/l/mac-screen-cast)](LICENSE)

Stream macOS screen to browser over LAN. Uses ScreenCaptureKit for zero-copy capture, VideoToolbox for hardware H.264 encoding, and WebRTC for low-latency delivery.

## Requirements

- **macOS 12.3+** — ScreenCaptureKit availability
- **Screen Recording permission** — required by macOS for screen capture

## Installation

### From source (cargo)

```bash
cargo install mac-screen-cast
```

### Pre-built binary (macOS)

Download the latest binary for your architecture from [GitHub Releases](https://github.com/lichtcui/mac-screen-cast/releases):

```bash
# Download (auto-detect architecture)
ARCH=$(uname -m | sed 's/arm64/aarch64/')
curl -LO "https://github.com/lichtcui/mac-screen-cast/releases/latest/download/mac-screen-cast-${ARCH}-apple-darwin"

# Remove quarantine attribute (Gatekeeper bypass)
xattr -d com.apple.quarantine mac-screen-cast-*

# Ad-hoc sign (bypass "Apple could not verify" warning)
codesign --force -s - mac-screen-cast-*

# Make executable
chmod +x mac-screen-cast-*
./mac-screen-cast-*
```

> **Why these steps?** Files downloaded via a browser get a `com.apple.quarantine` flag, triggering Gatekeeper to block unsigned binaries — `xattr -d` removes that flag. On newer macOS versions, even without quarantine, macOS may still refuse to run unsigned binaries with "Apple could not verify" — `codesign --force -s -` creates an ad-hoc signature to satisfy the check. These steps are standard practice for unsigned open-source macOS tools.

## Usage

### Screen Recording permission

The first run will automatically prompt for permission. If denied, reset and re-trigger:

```bash
tccutil reset ScreenCapture
mac-screen-cast -l
```

Alternatively, grant manually in **System Settings > Privacy & Security > Screen Recording**.

### Basic usage

Just run it — a terminal picker will show all visible windows. Type a number and press Enter to start streaming:

```bash
mac-screen-cast
```

Press **Ctrl+C** to stop streaming. A second Ctrl+C forces an immediate exit if the first doesn't respond.

After selecting a window, open the printed URL (e.g. `http://192.168.1.100:8080`) in any browser on the same network.

> No TURN server is configured, so streams only work within the local network.

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `-w`, `--window-id` | Window ID to capture (skips picker) | (interactive) |
| `-l`, `--list` | List visible windows (also triggers Screen Recording permission prompt) | |
| `--width` | Max output width in pixels | `1280` |
| `--fps` | Target frame rate (1-60) | `30` |
| `--port` | HTTP server port | `8080` |
| `--json` | JSON output (use with `--list`) | |
| `-h`, `--help` | Show help | |

### Examples

```bash
# Interactive: pick from a list
mac-screen-cast

# List windows (find window IDs)
mac-screen-cast -l

# List windows as JSON (for scripting)
mac-screen-cast -l --json

# Stream a specific window
mac-screen-cast -w 12345

# 720p at 60 fps
mac-screen-cast -w 12345 --width 1280 --fps 60

# Custom port
mac-screen-cast -w 12345 --port 9090
```

## How it works

```
ScreenCaptureKit → CVPixelBuffer → IOSurface
    → VideoToolbox (H.264 encode) → FU-A RTP packets
    → rustrtc (WebRTC) → Browser
```

ScreenCaptureKit delivers GPU-resident `CVPixelBuffer`s directly, avoiding a CPU readback. The `IOSurface` is passed zero-copy to VideoToolbox for hardware encoding. Encoded H.264 NAL units are packetized into RTP (FU-A fragmentation per RFC 6184) and sent over WebRTC via [rustrtc](https://github.com/restsend/rustrtc).

## Performance

Measured at 1280×772 @ 30fps with VideoToolbox hardware encoding on Apple Silicon (M-series):

| Metric | Value |
|--------|-------|
| Pipeline latency (capture → send) | 8–16ms (measured via `/latency`) |
| CPU usage | ~3% of one core |
| Memory | ~22 MB RSS |
| Frame rate | Stable 30 fps |

The pipeline is entirely GPU-accelerated: ScreenCaptureKit delivers GPU-resident buffers, VideoToolbox encodes on the media engine, and the CPU is only used for RTP packetization (~1ms per frame). Total end-to-end latency (server → browser display) on LAN is typically 30–60ms.

## Dependencies

- [rustrtc](https://github.com/restsend/rustrtc) — pure Rust WebRTC
- [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs) — ScreenCaptureKit bindings
- [videotoolbox-rs](https://github.com/doom-fish/videotoolbox-rs) — VideoToolbox bindings

## License

MIT
