# Demo 视频设计方案

## 目标

为 mac-screen-cast 项目的 GitHub README 制作一个使用效果演示动画 WebP，展示从 CLI 启动到浏览器看到实时画面的完整流程。

## 方案概述

**单次全屏录制 → ffmpeg 裁剪合成 → 导出动画 WebP**

终端（CLI）和浏览器（WebRTC 播放页面）同时显示在桌面左右两侧，录制一次全屏后通过 ffmpeg 从同一段视频中裁剪两个区域再并排合成。天然同步，无对时问题。

## 输出规格

| 属性 | 值 |
|------|------|
| 格式 | 动画 WebP（animated WebP） |
| 画质 | libwebp `-q:v 75`（平衡模式） |
| 半屏宽度 | 520px（左右并排后总宽 ~1050px） |
| 预估体积 | 1-2MB |
| 循环 | 无限循环（`-loop 0`） |
| 音频 | 无 |
| 嵌入方式 | `<img src="docs/demo.webp" alt="mac-screen-cast demo" width="100%">` |

## 操作步骤

### 1. 桌面布置

在录制前手动排列窗口：
- **左半屏**：Terminal.app 或 iTerm2（~50% 屏幕宽度）
- **右半屏**：Safari/Chrome（~50% 屏幕宽度）

macOS 窗口贴边自动分屏功能可以快速对齐。

### 2. 录制内容脚本

| 时间段 | 操作 | 画面说明 |
|--------|------|---------|
| 0-2s | 终端执行 `mac-screen-cast` | CLI 启动，显示窗口列表 |
| 2-4s | 终端中选择窗口，开始串流 | 显示 WebRTC offer、QR 码、URL |
| 4-6s | 光标移到浏览器地址栏 | 输入 `http://...:8080` |
| 6-8s | 浏览器加载播放页面 | 显示黑色播放器，等待连接 |
| 8-12s | WebRTC 连接建立 | 显示实时画面，延迟数字稳定在个位数 ms |
| 12-14s | 终端中按 Ctrl+C | 停止流 |

### 3. 录制工具

- **QuickTime Player**（`文件 → 新建屏幕录制`）
- 或 macOS 15+ 的 `screenrecord` CLI 工具
- 输出格式为 `.mov`（ProRes 422 或 H.264）

### 4. ffmpeg 处理命令

```bash
# 环境准备
brew install ffmpeg   # 确保 ffmpeg 包含 libwebp

# 查看原视频尺寸
ffprobe -v error -select_streams v:0 \
  -show_entries stream=width,height \
  -of csv=p=0 recording.mov
# 例如输出: 3024,1964 (MacBook Pro 14")

HALF=$((3024/2))     # 半屏宽度 = 1512
HEIGHT=1964          # 全屏高度

# 裁剪 → 缩放 → 合成 → 导出 WebP
ffmpeg -i recording.mov \
  -filter_complex "
    [0]crop=${HALF}:${HEIGHT}:0:0[t];
    [0]crop=${HALF}:${HEIGHT}:${HALF}:0[b];
    [t]scale=520:-1:flags=lanczos[t_s];
    [b]scale=520:-1:flags=lanczos[b_s];
    [t_s][b_s]hstack=inputs=2,
    drawbox=x=${520}:w=3:h=ih+4:t=2:c=#666666[out]
  " \
  -map "[out]" -vcodec libwebp_anim \
  -lossless 0 -compression_level 6 \
  -q:v 75 -loop 0 -an docs/demo.webp
```

**裁剪坐标说明：**
- `crop=1512:1964:0:0` — 取左半屏（终端区域）
- `crop=1512:1964:1512:0` — 取右半屏（浏览器区域）
- 具体数值根据 `ffprobe` 输出的实际分辨率计算

**可选：裁剪录制前后无用片段**
```bash
# 从第1秒开始，到第14秒结束
ffmpeg -ss 1 -to 14 -i recording.mov \
  -filter_complex [...] \
  -map "[out]" -vcodec libwebp_anim \
  -lossless 0 -compression_level 6 \
  -q:v 75 -loop 0 -an docs/demo.webp
```

### 5. README 嵌入

```markdown
## Demo

<img src="docs/demo.webp" alt="mac-screen-cast demo" width="100%">
```

## 备选方案对比

| 方案 | 同步性 | 工具依赖 | 复杂度 | 适用场景 |
|------|--------|---------|--------|---------|
| **单次录制 + ffmpeg 裁剪（本方案）** | ✅ 天然同步 | ffmpeg + QuickTime | ⭐⭐ | README 快速演示 |
| OBS 多源录制 | ✅ 实时合成 | OBS Studio | ⭐ | 需要实时预览 |
| 分别录制 + 手动后期 | ⚠️ 需对齐点 | ffmpeg + 剪辑软件 | ⭐⭐⭐ | 需要精细化后期 |

## 故障排除

**Q: ffmpeg 没有 libwebp 编码器？**
```bash
brew install ffmpeg    # 重新安装带 libwebp 的版本
ffmpeg -encoders | grep webp  # 验证
```

**Q: 裁剪区域不精确（窗口不在正中央）？**
使用 `ffplay` + `cropdetect` 滤镜自动检测窗口边界：
```bash
ffmpeg -i recording.mov -vf cropdetect -f null - 2>&1 | grep crop
```
或手动微调裁剪坐标。

**Q: WebP 文件太大？**
- 降低 `-q:v` 值（60-70）
- 减小 `scale` 宽度（如 400px）
- 缩短录制时长
- 降低帧率（`-r 10` 插入在 `-i recording.mov` 后面）
