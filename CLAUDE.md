# mac-screen-cast

macOS screen capture + H.264 encoding + WebRTC streaming to browser.

## WebRTC

This project uses [rustrtc](https://github.com/restsend/rustrtc) (v0.3.x). When modifying WebRTC-related code:

- rustrtc is a pure-Rust WebRTC implementation. Peer connections are created directly with `PeerConnection::new(config)` — no `APIBuilder` or `MediaEngine` needed.
- Codec capabilities are configured via `MediaCapabilities` in `RtcConfiguration`.
- Tracks are created with `sample_track(MediaKind::Video, capacity)` and added via `pc.add_track(track, RtpCodecParameters)`.
- ICE gathering uses `pc.wait_for_gathering_complete().await`.
- H.264 RTP fragmentation (FU-A) is done manually in `send_frame` — see [`RFC 6184`](https://datatracker.ietf.org/doc/html/rfc6184) for the packetization scheme.
- `PeerConnection` is `Clone` (internally `Arc`), no mutex wrapping needed.
- Requires `rustls::crypto::CryptoProvider::install_default()` before first `PeerConnection::new()`.

### Reference

- [rustrtc GitHub repo](https://github.com/restsend/rustrtc) — pure-Rust WebRTC implementation
- [docs.rs/rustrtc](https://docs.rs/rustrtc/latest/rustrtc/) — API docs
- [RFC 6184](https://datatracker.ietf.org/doc/html/rfc6184) — RTP payload format for H.264 video (FU-A fragmentation)

## Screen Capture

This project uses [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs) (v8.0.0) by doom-fish for macOS screen capture via ScreenCaptureKit.

### Key API patterns (v8.0.0)

- **Window capture**: `SCContentFilter::create().with_window(window).build()`
- **Config**: `SCStreamConfiguration::default()` with builder-style setters (`set_width`, `set_height`, `set_pixel_format`, `set_minimum_frame_interval`, etc.)
- **Handler**: `stream.add_output_handler(closure, SCStreamOutputType::Screen)` — closures matching `Fn(CMSampleBuffer, SCStreamOutputType) + Send + Sync + 'static` implement `SCStreamOutputTrait` automatically
- **CVPixelBuffer access**: `sample.image_buffer()` returns `Option<CVPixelBuffer>`, extract `IOSurface` via `.io_surface()` for zero-copy encoding with videotoolbox-rs
- **IOSurface → CVPixelBuffer roundtrip**: `pixel_buffer.io_surface()` returns `Option<IOSurface>`, pass `&IOSurface` to `CompressionSession::encode()`
- **EXTRAS**: `CMSampleBufferExt` provides `.image_buffer()`, `.frame_status()`, `.presentation_timestamp()`

### CG initialization

Command-line tools must initialize CoreGraphics before using SCKit:
```rust
unsafe { screencapturekit::ffi::sc_initialize_core_graphics() }
```
Without this, `SCStream::start_capture()` crashes with `CGS_REQUIRE_INIT`.

### Swift runtime rpath

The `screencapturekit` crate links Swift code and the binary needs `@rpath /usr/lib/swift` at runtime. This is configured in `.cargo/config.toml`:
```toml
[target.x86_64-apple-darwin]
rustflags = ["-C", "link-args=-Wl,-rpath,/usr/lib/swift"]
```
The crate's own `cargo:rustc-link-arg` does not propagate to the final binary, so this config is required.

### Reference

- [screencapturekit-rs docs](https://docs.rs/screencapturekit/latest)
- [GitHub repo](https://github.com/doom-fish/screencapturekit-rs) — 23+ examples including basic capture, Metal, wgpu, FFmpeg, egui, Bevy, Tauri
- Minimum macOS version: 12.3
- Uses `apple-cf` for CoreMedia/CoreVideo types and `apple-metal` for Metal integration

## H.264 Encoding

This project uses [videotoolbox-rs](https://crates.io/crates/videotoolbox) (v0.18.x) by doom-fish for hardware H.264 encoding via VideoToolbox.

### Key API patterns (v0.18.x)

- **Session creation**: `CompressionSession::builder(width, height, Codec::H264)` with builder-style setters (`.with_real_time()`, `.with_expected_frame_rate()`, `.with_max_keyframe_interval()`, `.with_average_bit_rate()`, `.with_allow_frame_reordering()`)
- **Encoding**: `session.encode(&iosurface, (pts_value, timescale))` — takes `&IOSurface` and a presentation timestamp tuple. Blocks until the frame is encoded.
- **Output**: `EncodedFrame` with `.data` (AVCC-format NAL units, 4-byte length prefix), `.presentation_time`, `.cm_sample_buffer()` (optional underlying `CMSampleBuffer`)
- **SPS/PPS extraction**: Available via `CMVideoFormatDescriptionGetH264ParameterSetAtIndex` on the format description from `EncodedFrame.cm_sample_buffer()`
- **Keyframe detection**: Scan `EncodedFrame.data` for NAL type 5 (IDR) — AVCC format, byte at offset +4 masked with 0x1f

### Pipeline

```
screencapturekit-rs → CMSampleBuffer → image_buffer() → CVPixelBuffer
    → io_surface() → IOSurface → CompressionSession::encode() → EncodedFrame → H264Frame
```

### Reference

- [crates.io/videotoolbox](https://crates.io/crates/videotoolbox)
- [docs.rs/videotoolbox](https://docs.rs/videotoolbox/latest/videotoolbox/) — API docs
- [GitHub repo](https://github.com/doom-fish/videotoolbox-rs)
- [RFC 6184](https://datatracker.ietf.org/doc/html/rfc6184) — H.264 RTP payload format (FU-A fragmentation)
