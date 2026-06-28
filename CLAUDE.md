# screenstream

macOS screen capture + H.264 encoding + WebRTC streaming to browser.

## WebRTC

This project uses [webrtc-rs](https://github.com/webrtc-rs/webrtc). When modifying WebRTC-related code:

- Always check the [official examples](https://github.com/webrtc-rs/webrtc/tree/master/examples) for reference patterns, especially `broadcast`, `reflect`, and `play-from-disk-renegotiation`.
- The crate version 0.10/0.11 had significant bugs (DTLS `invalid named curve`, ICE candidate handling). Current version is 0.17.1.
- The API in examples (master branch) uses `PeerConnectionBuilder` (v0.20+). This project still uses the older `APIBuilder` + `RTCPeerConnection` API from 0.17.x.
