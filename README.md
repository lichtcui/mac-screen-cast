# mac-screen-cast

[![Crates.io](https://img.shields.io/crates/v/mac-screen-cast)](https://crates.io/crates/mac-screen-cast)
[![Downloads](https://img.shields.io/crates/d/mac-screen-cast)](https://crates.io/crates/mac-screen-cast)
[![License](https://img.shields.io/crates/l/mac-screen-cast)](LICENSE)

Stream macOS screen to browser over LAN. Uses ScreenCaptureKit for zero-copy capture, VideoToolbox for hardware H.264 encoding, and WebRTC for low-latency delivery.

## Requirements

- macOS 12.3+
- Screen Recording permission

Running the tool will automatically prompt for permission. If denied, reset and re-trigger:

```bash
tccutil reset ScreenCapture
mac-screen-cast -l
```

Alternatively, grant manually in **System Settings > Privacy & Security > Screen Recording**.

## Installation

### From source (cargo)

```bash
cargo install mac-screen-cast
```

### Pre-built binary (macOS)

Download the latest binary for your architecture from [GitHub Releases](https://github.com/lichtcui/mac-screen-cast/releases):

```bash
# Download (example: x86_64)
curl -LO https://github.com/lichtcui/mac-screen-cast/releases/latest/download/mac-screen-cast-x86_64-apple-darwin

# Remove quarantine attribute (Gatekeeper bypass)
xattr -d com.apple.quarantine mac-screen-cast-*

# Make executable and run
chmod +x mac-screen-cast-*
./mac-screen-cast-*
```

> **Why `xattr -d`?** macOS automatically adds a `com.apple.quarantine` flag to files downloaded via a browser. This triggers Gatekeeper, which blocks unsigned binaries from opening. Removing the attribute tells macOS the file is safe to run locally. This is standard practice for unsigned open-source macOS tools.

## Usage

Just run it — you'll be prompted to pick a window:

```bash
mac-screen-cast
```

After selecting a window, open the printed URL (e.g. `http://192.168.1.100:8080`) in any browser on the same network.

> No TURN server is configured, so streams only work within the local network.

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `-w`, `--window-id` | Window ID to capture (skips picker) | (interactive) |
| `-l`, `--list` | List visible windows | |
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

## Performance

Measured at 1280×772 @ 30fps with VideoToolbox hardware encoding on Apple Silicon (M-series):

| Metric | Value |
|--------|-------|
| Pipeline latency (capture → send) | 8–16ms (measured via `/latency`) |
| CPU usage | ~3% of one core |
| Memory | ~22 MB RSS |
| Frame rate | Stable 30 fps |

The pipeline is entirely GPU-accelerated: ScreenCaptureKit delivers GPU-resident buffers, VideoToolbox encodes on the media engine, and the CPU is only used for RTP packetization (~1ms per frame). Total end-to-end latency (server → browser display) on LAN is typically 30–60ms.

## How it works

```
ScreenCaptureKit → CVPixelBuffer → IOSurface
    → VideoToolbox (H.264 encode) → FU-A RTP packets
    → rustrtc (WebRTC) → Browser
```

ScreenCaptureKit delivers GPU-resident `CVPixelBuffer`s directly, avoiding a CPU readback. The `IOSurface` is passed zero-copy to VideoToolbox for hardware encoding. Encoded H.264 NAL units are packetized into RTP (FU-A fragmentation per RFC 6184) and sent over WebRTC via [rustrtc](https://github.com/restsend/rustrtc).

## Dependencies

- [rustrtc](https://github.com/restsend/rustrtc) — pure Rust WebRTC
- [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs) — ScreenCaptureKit bindings
- [videotoolbox-rs](https://github.com/doom-fish/videotoolbox-rs) — VideoToolbox bindings

## License

MIT
