# mac-screen-cast

[![Crates.io](https://img.shields.io/crates/v/mac-screen-cast)](https://crates.io/crates/mac-screen-cast)
[![License](https://img.shields.io/crates/l/mac-screen-cast)](LICENSE)

Stream macOS screen to browser. Uses ScreenCaptureKit for zero-copy capture, VideoToolbox for hardware H.264 encoding, and WebRTC for low-latency delivery.

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

```bash
cargo install mac-screen-cast
```

## Usage

```bash
# List available windows
mac-screen-cast -l

# Stream a specific window
mac-screen-cast -w <window-id>

# Set resolution and frame rate
mac-screen-cast -w <window-id> --width 1280 --fps 30

# Use custom port
mac-screen-cast -w <window-id> --port 9090
```

After starting, open the printed URL (e.g. `http://192.168.1.100:8080`) in a browser to view the stream.

### Options

| Flag | Description | Default |
|------|-------------|---------|
| `-l`, `--list` | List visible windows | |
| `-w`, `--window-id` | Window ID to capture | (interactive picker) |
| `--width` | Max output width in pixels | `1280` |
| `--fps` | Target frame rate (1-60) | `30` |
| `--port` | HTTP server port | `8080` |
| `-h`, `--help` | Show help | |

### Interactive mode

Run without `-w` to pick from a list of visible windows:

```bash
mac-screen-cast
```

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
