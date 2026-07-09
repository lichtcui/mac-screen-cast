#!/usr/bin/env bash
#
# make-demo.sh — 从全屏录制生成 README 用的动画 WebP 演示
#
# 用法:
#   1. QuickTime Player → 文件 → 新建屏幕录制
#      桌面布局: 终端左半屏 | 浏览器右半屏
#      录制内容: 运行 mac-screen-cast → 选窗口 → 浏览器打开 URL
#   2. ./scripts/make-demo.sh path/to/recording.mov
#
# 输出: docs/demo.webp (已就绪可直接嵌入 README)
#
# 需要: ffmpeg (带 libwebp), ffprobe

set -euo pipefail

INPUT="${1:-}"
if [ -z "$INPUT" ] || [ ! -f "$INPUT" ]; then
    echo "用法: $0 <录制文件.mov>"
    echo ""
    echo "步骤:"
    echo "  1. 桌面布置: 终端左半屏 | 浏览器右半屏"
    echo "  2. QuickTime Player 录制全屏"
    echo "  3. $0 ~/Desktop/屏幕录制.mov"
    exit 1
fi

# 检测 ffmpeg
if ! command -v ffmpeg &>/dev/null; then
    echo "错误: 需要安装 ffmpeg → brew install ffmpeg"
    exit 1
fi
if ! ffmpeg -encoders 2>/dev/null | grep -q libwebp_anim; then
    echo "错误: ffmpeg 缺少 libwebp 编码器"
    echo "请使用 Homebrew 重新安装: brew reinstall ffmpeg"
    exit 1
fi

# ── 配置 ──
HALF_WIDTH=520        # 半屏输出宽度 (px)
QUALITY=75            # WebP 编码质量 (0-100)
COMPRESSION=6         # WebP 压缩等级 (0-6)
TRIM_START="${TRIM_START:-1}"    # 裁剪开始秒数 (可通过环境变量覆盖)
TRIM_END="${TRIM_END:-}"         # 裁剪结束秒数

# ── 读取源视频尺寸 ──
echo "→ 分析源视频: $INPUT"
read -r WIDTH HEIGHT < <(
    ffprobe -v error -select_streams v:0 \
        -show_entries stream=width,height \
        -of csv=p=0 "$INPUT"
)
echo "  分辨率: ${WIDTH}x${HEIGHT}"

HALF=$((WIDTH / 2))
echo "  半屏: ${HALF}x${HEIGHT}"

OUTPUT="docs/demo.webp"
mkdir -p "$(dirname "$OUTPUT")"

# ── 构建 filter graph ──
# 左半屏 (终端)  → scale  → 并排
# 右半屏 (浏览器) → scale  → 加分隔线
FILTER="
    [0]crop=${HALF}:${HEIGHT}:0:0[t];
    [0]crop=${HALF}:${HEIGHT}:${HALF}:0[b];
    [t]scale=${HALF_WIDTH}:-1:flags=lanczos[t_s];
    [b]scale=${HALF_WIDTH}:-1:flags=lanczos[b_s];
    [t_s][b_s]hstack=inputs=2,
    drawbox=x=${HALF_WIDTH}:w=3:h=ih+4:t=2:c=#444444[out]
"

# ── 构建 ffmpeg 命令 ──
TIMECUT=""
[ -n "$TRIM_START" ] && TIMECUT="$TIMECUT -ss $TRIM_START"
[ -n "$TRIM_END" ]   && TIMECUT="$TIMECUT -to $TRIM_END"

echo "→ 生成动画 WebP ..."
echo "  输出: $OUTPUT"
echo "  质量: $QUALITY"
echo "  压缩: $COMPRESSION"
[ -n "$TRIM_START" ] && echo "  起始: ${TRIM_START}s"
[ -n "$TRIM_END" ]   && echo "  结束: ${TRIM_END}s"
echo ""

ffmpeg $TIMECUT -i "$INPUT" \
    -filter_complex "$FILTER" \
    -map "[out]" -vcodec libwebp_anim \
    -lossless 0 \
    -compression_level "$COMPRESSION" \
    -q:v "$QUALITY" \
    -loop 0 -an \
    -y "$OUTPUT"

# ── 统计 ──
FILESIZE=$(stat -f%z "$OUTPUT" 2>/dev/null || stat -c%s "$OUTPUT" 2>/dev/null)
if [ "$FILESIZE" -ge 1048576 ]; then
    SIZE_STR="$(echo "scale=1; $FILESIZE / 1048576" | bc) MB"
elif [ "$FILESIZE" -ge 1024 ]; then
    SIZE_STR="$(echo "scale=1; $FILESIZE / 1024" | bc) KB"
else
    SIZE_STR="${FILESIZE} B"
fi

echo ""
echo "✅ 完成!"
echo "  文件: $OUTPUT ($SIZE_STR)"
echo ""
echo "在 README 中添加:"
echo '  <img src="docs/demo.webp" alt="mac-screen-cast demo" width="100%">'
