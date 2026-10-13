#!/usr/bin/env bash
# 独立官方 API26 OHOS ARM64 FFmpeg 构建；不调用桌面 builder、不执行目标 ELF。
set -euo pipefail
frd_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
exec python3 "$frd_root/tools/ohos-ffmpeg-bundle.py" build "$@"
