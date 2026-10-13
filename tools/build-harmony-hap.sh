#!/usr/bin/env bash
# 正常 API26 phone/2in1 HAP 构建。签名由正常开发配置/专用本地工具完成。
set -euo pipefail
if (($# > 1)); then printf '只接受 --release 或 --help 参数。\n' >&2; exit 2; fi
mode=debug
rust_args=(--build -p frd-shell-ohos-ffi)
case "${1:-}" in
    --release) mode=release; rust_args+=(--release) ;;
    '') ;;
    --help|-h) printf '用法：tools/build-harmony-hap.sh [--release]\n仅构建 HAP；签名通过正常开发签名流程配置。\n'; exit 0 ;;
    *) printf '未知参数：%s\n' "$1" >&2; exit 2 ;;
esac
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
studio="${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}"
if [[ "$studio" == *.app ]]; then studio="$studio/Contents"; fi
export DEVECO_SDK_HOME="${DEVECO_SDK_HOME:-$studio/sdk}"
export JAVA_HOME="$studio/jbr/Contents/Home"
export NODE_HOME="$studio/tools/node"
export PATH="$NODE_HOME/bin:$JAVA_HOME/bin:$studio/tools/ohpm/bin:$PATH"
"$repository_root/tools/build-harmony.sh" "${rust_args[@]}"
cd "$repository_root/apps/freeremotedesk-ohos"
"$studio/tools/ohpm/bin/ohpm" install
"$studio/tools/hvigor/bin/hvigorw" --mode module -p product=default -p module=entry@default -p buildMode="$mode" assembleHap --no-daemon
python3 "$repository_root/tools/check-harmony-hap.py" \
    --hap entry/build/default/outputs/default/entry-default-unsigned.hap \
    --readelf "$DEVECO_SDK_HOME/default/openharmony/native/llvm/bin/llvm-readelf"
