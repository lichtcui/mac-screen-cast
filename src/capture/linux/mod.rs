//! Linux screen capture — Wayland (PipeWire) primary, X11 fallback.

use std::sync::mpsc;
use std::sync::Arc;

use super::{CaptureResult, FrameBuffer, FrameBufferHandle, ScreenCapture};

mod portal;
mod pipewire;

/// Linux 屏幕捕获实现
///
/// 自动检测运行环境：
/// - WAYLAND_DISPLAY 已设置 → Wayland (PipeWire) 路径
/// - 仅 DISPLAY 已设置 → X11 fallback 路径（后续实现）
pub struct LinuxCapture;

impl ScreenCapture for LinuxCapture {
    type Options = (u32, u32, u32); // (window_id, width, height)

    fn create(_options: Self::Options) -> CaptureResult<Self> {
        // 检测运行环境
        let is_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
        if !is_wayland {
            // X11 fallback — 暂未实现，返回错误
            return Err("X11 capture not yet implemented; run under Wayland".into());
        }
        Ok(LinuxCapture)
    }

    fn start<F>(&mut self, _on_frame: F) -> CaptureResult<()>
    where
        F: FnMut(FrameBuffer) + Send + 'static,
    {
        // 需要在 tokio 运行时中异步执行 Portal → PipeWire 流程
        // 完整实现需要 Linux 环境测试
        Err("Linux capture not yet fully implemented".into())
    }

    fn stop(&mut self) -> CaptureResult<()> {
        Ok(())
    }
}
