# mac-screen-cast

macOS 屏幕采集 + H.264 硬件编码 + WebRTC 推流到浏览器。

## 特性

- **VideoToolbox H.264 硬件编码**：通过 Apple VideoToolbox 框架调用 GPU 进行 H.264 编码，相比软件编码大幅降低 CPU 占用，是实现低延迟高帧率推流的关键
- **ScreenCaptureKit 屏幕捕获**：使用 macOS ScreenCaptureKit 框架采集窗口，零 CPU 拷贝直接传递 `CVPixelBuffer` 到 VideoToolbox
- **WebRTC 推流**：使用 [rustrtc](https://github.com/restsend/rustrtc) 实现低延迟流媒体传输，浏览器端通过 WebSocket 信令 + WebRTC 播放

## 使用

```bash
# 列出窗口
cargo run -- -l

# 推流指定窗口
cargo run -- -w <window-id>

# 指定分辨率和帧率
cargo run -- -w <window-id> --width 1280 --fps 30

# 使用 --help 查看完整参数
```

## 编码流程

`ScreenCaptureKit` → `CVPixelBuffer` → `VTCompressionSessionEncodeFrame` → H.264 NAL units → WebRTC

相比旧方案（CGImage → CVPixelBuffer CPU 拷贝），ScreenCaptureKit 直接提供 GPU 端的 `CVPixelBuffer`，省去一次 CPU readback 和 CGContext 绘制。

## 依赖

- macOS (VideoToolbox, ScreenCaptureKit, CoreMedia, CoreVideo)
- [rustrtc](https://github.com/restsend/rustrtc) (v0.3.x) — 纯 Rust WebRTC 实现
- [screencapturekit-rs](https://github.com/doom-fish/screencapturekit-rs) (v8.0.0) — ScreenCaptureKit Rust 绑定
