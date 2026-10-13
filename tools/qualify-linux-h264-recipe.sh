#!/bin/bash
# 仅原生 Linux ARM64 的显式配方资格门；Python 将路径作为数据处理。
set -euo pipefail
exec python3 -B "$(dirname "$0")/qualify-linux-h264-recipe.py" "$@"
