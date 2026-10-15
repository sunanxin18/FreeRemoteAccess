#!/usr/bin/env bash
# 正常 API26 phone/2in1 无签名 HAP；只使用显式接收的 OHOS FFmpeg bundle。
set -euo pipefail
usage() {
    cat <<'USAGE'
用法：tools/build-harmony-hap.sh [--release] [--normal-hap-rdp-validation] --ffmpeg-bundle OHOS包目录 \
  --ffmpeg-manifest-sha256 独立接收报告中的SHA256
--normal-hap-rdp-validation：沿用正常表单/应用路径的 RDP 有界验证，默认关闭，不自动连接；CARGO_TARGET_DIR 仅允许仓库 target，签前核对本次 FFI 经所选 SDK strip 派生的精确字节。
必须指定已验证 OHOS 三库 bundle 及独立接收的 manifest 摘要；不自动构建或选择缓存。
固定 coherent Rust 1.96.0、官方 API26 SDK/Hvigor；输出无签名 HAP。签名另走正常开发流程。
USAGE
}
mode=debug
bundle=''; manifest_sha=''
rdp_validation=0
rust_args=(--build -p frd-shell-ohos-ffi)
while (($#)); do
    case "$1" in
        --release) mode=release; rust_args+=(--release); shift ;;
        --normal-hap-rdp-validation) rdp_validation=1; rust_args+=(--normal-hap-rdp-validation); shift ;;
        --ffmpeg-bundle) bundle="${2:?需要 OHOS bundle 目录}"; shift 2 ;;
        --ffmpeg-manifest-sha256) manifest_sha="${2:?需要 manifest SHA256}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) printf '未知参数：%s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done
if [[ -z "$bundle" || -z "$manifest_sha" ]]; then usage >&2; exit 2; fi
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
# CMake 默认导入 repository/target；显式验证不能静默包装其它目录的旧库。
if ((rdp_validation)) && [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
    requested_target="$CARGO_TARGET_DIR"
    if [[ "$requested_target" != /* ]]; then requested_target="$repository_root/$requested_target"; fi
    if [[ "$requested_target" != "$repository_root/target" ]]; then
        requested_real="$(cd "$requested_target" 2>/dev/null && pwd -P)" || requested_real=''
        default_real="$(cd "$repository_root/target" 2>/dev/null && pwd -P)" || default_real=''
        if [[ -z "$requested_real" || "$requested_real" != "$default_real" ]]; then
            printf '普通 RDP 验证要求 CARGO_TARGET_DIR 指向仓库 target；必须与 CMake 导入路径一致。\n' >&2
            exit 2
        fi
    fi
fi
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

if ((rdp_validation)); then
    python3 - "$repository_root/apps/freeremotedesk-ohos/entry/build/default/outputs/default/entry-default-unsigned.hap" \
        "$repository_root/target/aarch64-unknown-linux-ohos/$mode/libfrd_shell_ohos_ffi.so" \
        "$DEVECO_SDK_HOME/default/openharmony/native/llvm/bin/llvm-strip" <<'PY_FFI'
import hashlib
from pathlib import Path
import sys
import subprocess
import tempfile
import zipfile

with zipfile.ZipFile(sys.argv[1]) as package:
    entries = [entry for entry in package.infolist()
               if entry.filename == 'libs/arm64-v8a/libfrd_shell_ohos_ffi.so']
    if len(entries) != 1:
        raise SystemExit('普通 RDP 验证 HAP 必须只有一个 FFI 库条目')
    packaged = package.read(entries[0])
built = Path(sys.argv[2]).read_bytes()
# 按实际 Hvigor native-strip.js 使用默认 strip 并保留 .permission；原库只读。
with tempfile.TemporaryDirectory(prefix='frd-rdp-hap-ffi-') as temporary:
    expected = Path(temporary) / 'libfrd_shell_ohos_ffi.so'
    subprocess.run([sys.argv[3], '--keep-section=.permission', sys.argv[2], '-o' + str(expected)], check=True)
    derived = expected.read_bytes()
if packaged != derived:
    raise SystemExit('普通 RDP 验证 HAP FFI 与本次构建的 SDK strip 派生字节不匹配')
print('普通 RDP 验证 HAP FFI sourceSHA256=' + hashlib.sha256(built).hexdigest()
      + ' packagedSHA256=' + hashlib.sha256(derived).hexdigest())
PY_FFI
fi
