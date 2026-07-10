//! PipeWire stream 接收 + DMA-BUF fd 提取
//!
//! 通过 PipeWire stream 连接接收屏幕捕获帧，
//! 在 on_process 回调中从 spa_buffer 提取 DMA-BUF fd。

/// PipeWire stream 管理器
///
/// 封装 PipeWire 主循环和 stream 连接。
/// on_process 回调中提取 DMA-BUF fd 并通过通道发送给编码器线程。
pub struct PipeWireCapture {
    // TODO: pw::main_loop::MainLoop 句柄
    // TODO: pw::context::Context
    // TODO: pw::stream::Stream
    // TODO: 线程 join handle
}

impl PipeWireCapture {
    /// 创建 PipeWire 捕获流
    ///
    /// # 参数
    /// - `pw_node_id`: Portal Start 返回的 PipeWire node ID
    /// - `width`: 输出宽度（像素）
    /// - `height`: 输出高度（像素）
    /// - `fps`: 目标帧率
    ///
    /// # 实现步骤（待完成）
    /// 1. `pw::init()` — 初始化 PipeWire
    /// 2. `pw::main_loop::MainLoop::new(None)` — 创建主循环
    /// 3. `pw::context::Context::new(&main_loop)` — 创建上下文
    /// 4. `core.connect(None)` — 连接到 PipeWire 守护进程
    /// 5. `pw::stream::Stream::new(&core, ...)` — 创建 stream
    /// 6. 配置 stream 参数（视频格式、分辨率等）
    /// 7. `stream.connect(Input, pw_node_id, ...)` — 连接到 node
    /// 8. 注册 on_process 回调提取 DMA-BUF fd
    ///    - spa_buffer[0].datas[0].type == SPA_DATA_DmaBuf
    ///    - fd, modifier, stride
    /// 9. 启动线程运行 main_loop
    pub fn new(
        _pw_node_id: u32,
        _width: u32,
        _height: u32,
        _fps: u32,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // PipeWire 绑定需要在运行时加载 libpipewire，
        // 需要 Linux 环境才能测试。
        Err("PipeWire capture not yet implemented".into())
    }
}
