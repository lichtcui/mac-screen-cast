#!/bin/bash
set -e
cd "$(dirname "$0")"
cargo build --release
cp target/release/screenstream /usr/local/bin/mac-cast
echo "✅ 安装成功: /usr/local/bin/mac-cast"
echo "运行: mac-cast"
