//! mac-screen-cast — internal library crate for benchmark access.
//!
//! The primary entrypoint is `src/main.rs` (binary).
//! This library module exists only to expose key functions for
//! `cargo bench` integration tests. No external API guarantees.

// Most types/functions are only used by the binary crate (main.rs).
// For the lib crate (bench harness), suppress dead-code warnings.
#![allow(dead_code)]

mod h264;
mod webrtc;

pub use h264::avcc_nal_units;
pub use webrtc::packetize_nal;
