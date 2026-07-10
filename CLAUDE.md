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

## CLI Parsing

Uses `clap` derive macro (`#[derive(Parser)]`). All flags and options are declared
as struct fields with `#[arg(...)]` attributes. The generated help includes
the HTTP API documentation via `after_help`.

## WebRTC

Uses [rustrtc](https://github.com/restsend/rustrtc). Key patterns:
- `PeerConnection::new(config)` — no `APIBuilder`/`MediaEngine`
- Codec capabilities via `MediaCapabilities` in `RtcConfiguration`
- Tracks: `sample_track(MediaKind::Video, capacity)` + `pc.add_track(track, RtpCodecParameters)`
- ICE: `pc.wait_for_gathering_complete().await` (with 3s timeout + warning)
- FU-A fragmentation done manually in `send_frame` per RFC 6184
- `PeerConnection` is `Clone` (inner `Arc`)
- `rustls::crypto::CryptoProvider::install_default()` required before first `PeerConnection::new()`

## Screen Capture

Uses [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs). Key patterns:
- **Polling-based** via `SCScreenshotManager::capture_sample_buffer()` (not `SCStream`)
  — workaround for macOS 26 native-app windows that return blank buffers from SCStream
- Filter: `SCContentFilter::create().with_window(window).build()`
- Config: `SCStreamConfiguration::default()` with builder-style setters
- Handler: closure `FnMut(CMSampleBuffer, SCStreamOutputType) + Send + 'static` on a dedicated thread
- Zero-copy: `sample.image_buffer() → .io_surface()` → `CompressionSession::encode(&iosurface, ...)`
- Frame-rate control: `spin_sleep` with drift-compensated scheduling
- Error logging throttled to once per 5s to prevent log floods
- Init: `unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }` (required before `start_capture()`)
- Swift rpath configured in `.cargo/config.toml` AND `build.rs` (`@rpath /usr/lib/swift`) — the `build.rs` copy is the fallback when installing from crates.io (which does NOT carry project-level `.cargo/config`)

## H.264 Encoding

Uses [videotoolbox-rs](https://crates.io/crates/videotoolbox). Key patterns:
- Session: `CompressionSession::builder(width, height, Codec::H264)` with `.with_real_time()`, `.with_expected_frame_rate()`, `.with_max_keyframe_interval()`, `.with_average_bit_rate()`, `.with_allow_frame_reordering(false)`
- Bitrate heuristic: 0.07 bpp × w × h × fps, clamped to `[500 Kbps, 10 Mbps]`
- Encode: `session.encode(&iosurface, (pts_value, timescale))` — blocks until done
- Output: `EncodedFrame.data` — AVCC format (4-byte length prefix per NAL)
- SPS/PPS: `CMVideoFormatDescriptionGetH264ParameterSetAtIndex` on the format description from `EncodedFrame.cm_sample_buffer()`
- Keyframe: `encoded.info_flags & 1 != 0`
- `VtEncoder` is `unsafe impl Send + Sync` (VT session handles are thread-safe per Apple docs), with a compile-time `assert_impl_all!(VtEncoder: Send, Sync)` guard

## CLI (AI invocation)

- `mac-screen-cast --list --json` — list windows as JSON array `[{"id":..,"app":..,"title":..}]`
- `mac-screen-cast --list` — human-readable formatted list
- `mac-screen-cast -w <id> [--width px] [--fps N] [--port N]` — start stream
- Window listing uses `swift -e` with CoreGraphics `CGWindowListCopyWindowInfo` (NOT ScreenCaptureKit)

## HTTP API (at runtime)

| Endpoint | Method | Response |
|----------|--------|----------|
| `/` | GET | HTML player page (parsed from `docs/resources/player.html`) |
| `/offer` | GET | SDP offer (text/plain) |
| `/signal` | POST | `{"status":"ok"}` (JSON) |
| `/latency` | GET | latency in ms (number, text/plain) |

## Update Checker

Runs on startup in a background thread. Caches the latest GitHub release tag in `/tmp/msc-version-cache` (24h TTL). HTTP request has 10s timeout. Prints update notice to stderr if a newer version exists.

## Module Architecture

| Module | File | Responsibility |
|--------|------|----------------|
| `main` | `src/main.rs` | CLI arg parsing (clap), shared state wiring, pipeline orchestration |
| `capture` | `src/capture.rs` | SCScreenshotManager polling wrapper (filter, config, timing, error throttle) |
| `server` | `src/server.rs` | HTML player page (`docs/resources/player.html`), window listing via Swift, local IP detection |
| `h264` | `src/h264.rs` | VideoToolbox CompressionSession wrapper, AVCC NAL parsing, SPS/PPS extraction |
| `webrtc` | `src/webrtc.rs` | PeerConnection setup, FU-A RTP packetization, track management |
| `update_checker` | `src/update_checker.rs` | Version check against GitHub releases (background thread, 10s timeout, 24h cache) |

## Demo Video (README)

A split-screen animated WebP (`docs/demo.webp`) shows the CLI + browser stream workflow.

### Update the demo

1. Record a new screen capture:
   - Arrange: **terminal left half** | **browser right half** (macOS window-snap)
   - QuickTime Player → File → New Screen Recording
   - Record the full flow: run `mac-screen-cast` → select window → open browser URL
2. Run the helper script:
   ```bash
   TRIM_START=0.5 TRIM_END=9 ./docs/scripts/make-demo.sh sample.mov
   ```
   The script auto-detects the best encoder path:
   - `libwebp_anim` (ffmpeg built-in)
   - `img2webp` (libwebp tools, fallback)
   - GIF (last resort)
3. Embed in README (already done via `<img src="docs/demo.webp" ...>`)
4. Commit `docs/demo.webp` and any script changes.

## Gotchas

- **Double-tap Ctrl+C force-exit**: second Ctrl+C calls `std::process::exit(1)` — useful if the first Ctrl+C doesn't shut down cleanly (e.g. WebRTC hang)
- **`rw_read`/`rw_write` helpers**: `r.read().unwrap_or_else(|e| e.into_inner())` used throughout for poisoned RwLock recovery
- **Polling-based capture**: `SCScreenshotManager` instead of `SCStream` — necessary for macOS 26 native-app windows (Ghostty, Clash Verge). Performance impact is negligible for ≤60fps.
- **WebRTC offer refresh**: page reload triggers a new `PeerConnection`; the old one is closed via `Tokio::block_on` from the HTTP server thread (not inside a Tokio runtime, so this is safe)
