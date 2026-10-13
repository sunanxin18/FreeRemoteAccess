#!/usr/bin/env bash
# 正常 API26 phone/2in1 无签名 HAP；只使用显式接收的 OHOS FFmpeg bundle。
set -euo pipefail
usage() {
    cat <<'USAGE'
用法：tools/build-harmony-hap.sh [--release] --ffmpeg-bundle OHOS包目录 \
  --ffmpeg-manifest-sha256 独立接收报告中的SHA256
必须指定已验证 OHOS 三库 bundle 及独立接收的 manifest 摘要；不自动构建或选择缓存。
固定 coherent Rust 1.96.0、官方 API26 SDK/Hvigor；输出无签名 HAP。签名另走正常开发流程。
USAGE
}
mode=debug
bundle=''; manifest_sha=''
rust_args=(--build -p frd-shell-ohos-ffi)
while (($#)); do
    case "$1" in
        --release) mode=release; rust_args+=(--release); shift ;;
        --ffmpeg-bundle) bundle="${2:?需要 OHOS bundle 目录}"; shift 2 ;;
        --ffmpeg-manifest-sha256) manifest_sha="${2:?需要 manifest SHA256}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) printf '未知参数：%s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done
if [[ -z "$bundle" || -z "$manifest_sha" ]]; then usage >&2; exit 2; fi
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
if [[ "$bundle" != /* ]]; then bundle="$PWD/$bundle"; fi
studio="${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}"
if [[ "$studio" == *.app ]]; then studio="$studio/Contents"; fi
export DEVECO_SDK_HOME="${DEVECO_SDK_HOME:-$studio/sdk}"
export JAVA_HOME="$studio/jbr/Contents/Home"
export NODE_HOME="$studio/tools/node"
export CARGO="$(rustup which --toolchain 1.96.0 cargo)"
export RUSTC="$(rustup which --toolchain 1.96.0 rustc)"
export RUSTDOC="$(rustup which --toolchain 1.96.0 rustdoc)"
export PATH="$(dirname "$RUSTC"):$NODE_HOME/bin:$JAVA_HOME/bin:$studio/tools/ohpm/bin:$PATH"
export PYTHONDONTWRITEBYTECODE=1
entry="$repository_root/apps/freeremotedesk-ohos/entry"
stager="$repository_root/tools/stage-harmony-ffmpeg.py"
cleanup() {
    status=$?
    if ((status != 0)); then python3 "$stager" --entry "$entry" --clean; fi
    exit "$status"
}
trap cleanup EXIT
python3 "$stager" --entry "$entry" --sdk "$DEVECO_SDK_HOME/default/openharmony" \
    --ffmpeg-bundle "$bundle" --ffmpeg-manifest-sha256 "$manifest_sha"
"$repository_root/tools/build-harmony.sh" --sdk "$DEVECO_SDK_HOME/default/openharmony" "${rust_args[@]}"
cd "$repository_root/apps/freeremotedesk-ohos"
"$studio/tools/ohpm/bin/ohpm" install
"$studio/tools/hvigor/bin/hvigorw" --mode module -p product=default -p module=entry@default -p buildMode="$mode" assembleHap --no-daemon
python3 "$repository_root/tools/check-harmony-hap.py" \
    --hap entry/build/default/outputs/default/entry-default-unsigned.hap \
    --readelf "$DEVECO_SDK_HOME/default/openharmony/native/llvm/bin/llvm-readelf" \
    --ffmpeg-bundle "$bundle" --ffmpeg-manifest-sha256 "$manifest_sha"
