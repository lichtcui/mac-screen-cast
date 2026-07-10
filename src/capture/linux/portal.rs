//! xdg-desktop-portal D-Bus 交互
//!
//! 通过 zbus 调用 org.freedesktop.portal.ScreenCast 接口，
//! 获取 PipeWire node ID + FD 用于屏幕捕获。

use std::os::fd::OwnedFd;

/// Portal 交互的结果：PipeWire node ID 和文件描述符
pub struct PortalSession {
    pub pw_node_id: u32,
    pub pw_fd: OwnedFd,
}

/// 通过 D-Bus Portal 获取 PipeWire 屏幕捕获资源
///
/// 流程：
/// 1. CreateSession — 创建 Portal session（handle_token 配对）
/// 2. SelectSources — 选择屏幕/窗口捕获源（types: MONITOR | WINDOW）
/// 3. Start — 开始捕获，获得 PipeWire stream node ID + FD
///
/// # 参数
/// - `window_id`: 可选，指定窗口 ID；None 则让用户选择
///
/// # 返回
/// - `PortalSession` 包含 PipeWire node ID 和 FD
pub async fn create_portal_session(
    window_id: Option<u32>,
) -> Result<PortalSession, Box<dyn std::error::Error>> {
    // TODO: 完整实现 zbus D-Bus Portal 交互
    //
    // 关键 D-Bus 调用：
    // - org.freedesktop.portal.ScreenCast.CreateSession
    //     → options: { handle_token, session_handle_token }
    //     → 接收 Response 信号获取 session_handle
    // - org.freedesktop.portal.ScreenCast.SelectSources
    //     → options: { types: MONITOR | WINDOW }
    //     → 系统弹出选择器
    // - org.freedesktop.portal.ScreenCast.Start
    //     → 接收 Response 信号获取 PipeWire node info
    //     → 从中提取 pw_node_id 和 pw_fd (fd 在 D-Bus FD 列表里)
    //
    // 实现要求：
    // 1. zbus Connection::session().await
    // 2. 使用 request/session handle token 机制配对信号
    // 3. 解析 Response 信号中的 results (a{sv}) 字典
    // 4. 从 results["handle"] 提取 stream_node_id
    // 5. 从 D-Bus 消息附属 FD 列表获取 pw_fd
    //
    // 注意：此函数需要运行在 tokio 运行时中，
    // 且系统中需运行 xdg-desktop-portal 后端。

    let _ = window_id; // 抑制 unused 警告
    Err("Portal D-Bus interaction not yet implemented. Need running xdg-desktop-portal.".into())
}
