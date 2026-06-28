# screenstream

macOS 屏幕采集 + H.264 硬件编码 + WebRTC 推流到浏览器。

## 特性

- **VideoToolbox H.264 硬件编码**：通过 Apple VideoToolbox 框架调用 GPU 进行 H.264 编码，相比软件编码大幅降低 CPU 占用，是实现低延迟高帧率推流的关键
- **macOS 窗口/屏幕捕获**：支持指定窗口或全屏捕获
- **WebRTC 推流**：使用 webrtc-rs 实现低延迟流媒体传输，浏览器端通过 WebSocket 信令 + WebRTC 播放

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

`CGImage` → `CVPixelBuffer` → `VTCompressionSessionEncodeFrame` → H.264 NAL units → WebRTC

## 依赖

- macOS (VideoToolbox, CoreGraphics, CoreVideo, CoreMedia)
- [webrtc-rs](https://github.com/webrtc-rs/webrtc)
