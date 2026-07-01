# mac-screen-cast

macOS screen capture + H.264 encoding + WebRTC streaming to browser.

## Build & Test

- `cargo build` — debug build
- `cargo build --release` — optimized release (LTO + strip)
- `cargo test` — run unit tests (no Screen Recording permission needed)
- `cargo test -- --ignored` — run ignored tests (requires Screen Recording)
- `cargo audit` — security audit (requires `cargo install cargo-audit`)
- `cargo install --path .` — install locally
- Tests: `tests/e2e.rs` for integration, `#[cfg(test)]` modules in `src/*.rs` for unit

## WebRTC

Uses [rustrtc](https://github.com/restsend/rustrtc). Key patterns:
- `PeerConnection::new(config)` — no `APIBuilder`/`MediaEngine`
- Codec capabilities via `MediaCapabilities` in `RtcConfiguration`
- Tracks: `sample_track(MediaKind::Video, capacity)` + `pc.add_track(track, RtpCodecParameters)`
- ICE: `pc.wait_for_gathering_complete().await`
- FU-A fragmentation done manually in `send_frame` per RFC 6184
- `PeerConnection` is `Clone` (inner `Arc`)
- `rustls::crypto::CryptoProvider::install_default()` required before first `PeerConnection::new()`

## Screen Capture

Uses [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs). Key patterns:
- Filter: `SCContentFilter::create().with_window(window).build()`
- Config: `SCStreamConfiguration::default()` with builder-style setters
- Handler: `stream.add_output_handler(closure, SCStreamOutputType::Screen)` — closures implementing `Fn(CMSampleBuffer, SCStreamOutputType) + Send + Sync + 'static` auto-implement `SCStreamOutputTrait`
- Zero-copy: `sample.image_buffer() → .io_surface()` → `CompressionSession::encode(&iosurface, ...)`
- Init: `unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }` (required before `start_capture()`)
- Swift rpath configured in `.cargo/config.toml` AND `build.rs` (`@rpath /usr/lib/swift`) — the `build.rs` copy is the fallback when installing from crates.io (which does NOT carry project-level `.cargo/config`)

## H.264 Encoding

Uses [videotoolbox-rs](https://crates.io/crates/videotoolbox). Key patterns:
- Session: `CompressionSession::builder(width, height, Codec::H264)` with `.with_real_time()`, `.with_expected_frame_rate()`, `.with_max_keyframe_interval()`, `.with_average_bit_rate()`, `.with_allow_frame_reordering()`
- Encode: `session.encode(&iosurface, (pts_value, timescale))` — blocks until done
- Output: `EncodedFrame.data` — AVCC format (4-byte length prefix per NAL)
- SPS/PPS: `CMVideoFormatDescriptionGetH264ParameterSetAtIndex` on the format description from `EncodedFrame.cm_sample_buffer()`
- Keyframe: scan `data` for NAL type 5 (`byte_at_offset_4 & 0x1f == 5`)

## CLI (AI invocation)

- `mac-screen-cast --list --json` — list windows as JSON array `[{"id":..,"app":..,"title":..}]`
- `mac-screen-cast --list` — human-readable formatted list
- `mac-screen-cast -w <id> [--width px] [--fps N] [--port N]` — start stream
- Window listing uses `swift -e` with CoreGraphics `CGWindowListCopyWindowInfo` (NOT ScreenCaptureKit)

## HTTP API (at runtime)

| Endpoint | Method | Response |
|----------|--------|----------|
| `/` | GET | HTML player page |
| `/offer` | GET | SDP offer (text/plain) |
| `/signal` | POST | `{"status":"ok"}` (JSON) |
| `/latency` | GET | latency in ms (number, text/plain) |

## Update Checker

Runs on startup in a background thread. Caches the latest GitHub release tag in `/tmp/msc-version-cache` (24h TTL). Prints update notice to stderr if a newer version exists.

## Module Architecture

| Module | File | Responsibility |
|--------|------|----------------|
| `main` | `src/main.rs` | CLI arg parsing, wires capture → encode → WebRTC pipeline |
| `capture` | `src/capture.rs` | SCStream wrapper (filter, config, output handler) |
| `server` | `src/server.rs` | HTML player page, window listing via Swift, local IP detection |
| `h264` | `src/h264.rs` | VideoToolbox CompressionSession wrapper, AVCC NAL parsing, SPS/PPS extraction |
| `webrtc` | `src/webrtc.rs` | PeerConnection setup, FU-A RTP packetization, track management |
| `update_checker` | `src/update_checker.rs` | Version check against GitHub releases (background thread, cached) |

## Gotchas

- **Double-tap Ctrl+C force-exit**: second Ctrl+C calls `std::process::exit(1)` — useful if the first Ctrl+C doesn't shut down cleanly (e.g. WebRTC hang)
- **`lock_mutex` helper**: `m.lock().unwrap_or_else(|e| e.into_inner())` used throughout `main.rs` instead of raw `Mutex::lock()` (poisoned mutexes are treated as locked)
