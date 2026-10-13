#!/bin/bash
# 构建 Linux x86/i686、x86_64 或 arm64 随包 FFmpeg 解码器。源码只启用 H.264/HEVC decoder/parser，
# 生产软件解码由 FFmpeg 自带的 x86asm 或 AArch64/NEON 内核承担。
set -euo pipefail

[[ "$(uname -s)" == "Linux" ]] || {
  echo "Linux FFmpeg bundle 必须在 Linux 构建主机或显式 Linux 容器中执行" >&2
  exit 2
}

cd "$(dirname "$0")/.."
frd_root="$PWD"
# Cargo 会从工作区根目录解析相对 CARGO_TARGET_DIR。这里统一保存绝对 target 根，
# 防止插件查找回退到仓库默认 target 目录中的旧产物。
frd_target_dir="${CARGO_TARGET_DIR:-$frd_root/target}"
if [[ "$frd_target_dir" != /* ]]; then
  frd_target_dir="$frd_root/$frd_target_dir"
fi
frd_version="8.1.2"
frd_sha256="464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c"
frd_build="${FRD_FFMPEG_BUILD_ROOT:-$frd_root/target/ffmpeg-linux}"
[[ "$frd_build" == /* ]] || frd_build="$frd_root/$frd_build"
frd_archive="${FRD_FFMPEG_ARCHIVE:-$frd_build/ffmpeg-$frd_version.tar.xz}"
[[ "$frd_archive" == /* ]] || frd_archive="$frd_root/$frd_archive"
frd_recipe="${FRD_FFMPEG_SOURCE_RECIPE:-upstream}"
frd_arch="${FRD_FFMPEG_ARCH:-$(uname -m)}"
frd_host_arch="$(uname -m)"
frd_cross_build=0

case "$frd_arch" in
  x86|i686)
    frd_platform="linux-x86"
    frd_arch="x86"
    ;;
  x86_64)
    frd_platform="linux-x86_64"
    ;;
  aarch64|arm64) frd_platform="linux-aarch64"; frd_arch="aarch64" ;;
  *) echo "不支持的 Linux FFmpeg 架构: $frd_arch" >&2; exit 2 ;;
esac

# recipe/target trust 在创建目录、下载或源码重放前执行；未知或modified+x86立即拒绝。
python3 "$frd_root/tools/desktop-ffmpeg-source.py" metadata \
  --profile "$frd_platform" --source-recipe "$frd_recipe" >/dev/null
if [[ "$frd_arch" == x86 || "$frd_arch" == x86_64 ]]; then
  command -v nasm >/dev/null || { echo 'Linux x86/AMD64 FFmpeg build requires NASM/x86asm' >&2; exit 2; }
fi

case "$frd_host_arch" in
  x86|i686) frd_host_platform="linux-x86" ;;
  x86_64) frd_host_platform="linux-x86_64" ;;
  aarch64|arm64) frd_host_platform="linux-aarch64" ;;
  *) echo "不支持的 Linux 构建主机架构: $frd_host_arch" >&2; exit 2 ;;
esac

if [[ "$frd_platform" != "$frd_host_platform" ]]; then
  frd_cross_build=1
  : "${FRD_FFMPEG_CC:?跨架构 Linux 构建必须显式提供 FRD_FFMPEG_CC}"
  : "${FRD_FFMPEG_CROSS_PREFIX:?跨架构 Linux 构建必须显式提供 FRD_FFMPEG_CROSS_PREFIX}"
  : "${FRD_CARGO_TARGET:?跨架构 Linux 构建必须显式提供 FRD_CARGO_TARGET}"
  frd_cargo_target_env="${FRD_CARGO_TARGET//-/_}"
  frd_cargo_target_env="$(printf '%s' "$frd_cargo_target_env" | LC_ALL=C tr '[:lower:]' '[:upper:]')"
  export "CC_${frd_cargo_target_env}=$FRD_FFMPEG_CC"
  export "CARGO_TARGET_${frd_cargo_target_env}_LINKER=$FRD_FFMPEG_CC"
  if command -v "${FRD_FFMPEG_CROSS_PREFIX}ar" >/dev/null; then
    export "AR_${frd_cargo_target_env}=${FRD_FFMPEG_CROSS_PREFIX}ar"
  fi
fi

# 目标测试必须执行目标二进制。x86_64 Linux 可通过内核 IA32 支持运行
# i686 ELF；其他跨架构组合明确拒绝，不回退到宿主测试。
frd_run_tests="${FRD_FFMPEG_RUN_TARGET_TESTS:-${FRD_FFMPEG_RUN_NATIVE_TESTS:-0}}"
frd_test_target=()
if [[ "$frd_run_tests" == 1 && "$frd_cross_build" == 1 ]]; then
  [[ "$frd_host_platform" == linux-x86_64 && "$frd_platform" == linux-x86 ]] || {
    echo "当前宿主不能直接执行所请求的 Linux 目标测试: $frd_host_platform -> $frd_platform" >&2
    exit 2
  }
  [[ "$FRD_CARGO_TARGET" == i686-unknown-linux-gnu ]] || {
    echo "i686 目标测试要求精确的 i686-unknown-linux-gnu Rust target" >&2
    exit 2
  }
  [[ -f /lib/ld-linux.so.2 ]] || {
    echo "i686 目标测试需要 libc6-i386 提供 /lib/ld-linux.so.2" >&2
    exit 2
  }
  frd_test_target=(--target "$FRD_CARGO_TARGET")
fi

mkdir -p "$frd_build"
if [[ ! -f "$frd_archive" ]]; then
  curl --fail --location --max-time 120 \
    "https://ffmpeg.org/releases/ffmpeg-$frd_version.tar.xz" -o "$frd_archive"
fi
[[ "$(sha256sum "$frd_archive" | cut -d ' ' -f 1)" == "$frd_sha256" ]]

frd_attempt=$(mktemp -d "$frd_build/build.XXXXXX")
trap 'rm -rf "$frd_attempt"' EXIT
frd_source=$(python3 "$frd_root/tools/desktop-ffmpeg-source.py" prepare \
  --profile "$frd_platform" --source-recipe "$frd_recipe" \
  --archive "$frd_archive" --destination "$frd_attempt/source")
frd_stage="$frd_attempt/dist"
frd_prefix="$frd_stage/usr"
cd "$frd_source"
frd_configure_args=(
  # configure 参数会原样进入 avcodec_configuration() 的运行时字符串。
  # 使用固定逻辑安装前缀，临时构建目录仅通过 make 的 DESTDIR 传入，
  # 避免将 CI /home/runner 路径固化进随包动态库。
  --prefix=/usr --arch="$frd_arch" --target-os=linux
  --disable-static --enable-shared
  --disable-programs --disable-doc --disable-everything
  --enable-decoder=hevc,h264 --enable-parser=hevc,h264 --enable-protocol=file
  --disable-gpl --disable-nonfree --disable-version3 --disable-autodetect --disable-network
  --disable-debug --enable-stripping --disable-avdevice --disable-avformat --disable-avfilter
  --disable-swresample --disable-swscale
)
if [[ "$frd_arch" == "aarch64" ]]; then
  frd_configure_args+=(--disable-x86asm)
fi
if [[ "$frd_cross_build" -eq 1 ]]; then
  frd_configure_args+=(
    --enable-cross-compile
    "--cc=$FRD_FFMPEG_CC"
    "--cross-prefix=$FRD_FFMPEG_CROSS_PREFIX"
  )
fi
./configure "${frd_configure_args[@]}"
if [[ "$frd_recipe" != upstream ]]; then
  python3 - "$frd_attempt/configuration.json" "${frd_configure_args[@]}" <<'CONFIG'
import json
from pathlib import Path
import shlex
import subprocess
import sys
settings = dict(line.split('=', 1) for line in Path('ffbuild/config.mak').read_text().splitlines() if '=' in line)
compiler = subprocess.check_output(shlex.split(settings['CC']) + ['--version'], text=True).splitlines()[0]
Path(sys.argv[1]).write_text(json.dumps({'args': sys.argv[2:], 'compiler': compiler}))
CONFIG
fi
make -j "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)"
if [[ "$frd_recipe" != upstream ]]; then
  # install会剥离安装副本；先从本次未strip库取得真实symbol/size/file-backed代码record。
  python3 "$frd_root/tools/linux-ffmpeg-bundle.py" kernel \
    --library "$frd_source/libavcodec/libavcodec.so.62" --output "$frd_attempt/kernel.json"
fi
make DESTDIR="$frd_stage" install

if [[ "$frd_arch" == "x86" || "$frd_arch" == "x86_64" ]]; then
  grep -q '^#define HAVE_X86ASM 1$' config.h || {
    echo "FFmpeg x86 build did not enable HAVE_X86ASM" >&2
    exit 1
  }
else
  grep -q '^#define HAVE_NEON 1$' config.h || {
    echo "FFmpeg AArch64 build did not enable HAVE_NEON" >&2
    exit 1
  }
fi

cd "$frd_root"
if [[ "$frd_cross_build" -eq 1 ]]; then
  FFMPEG_DIR="$frd_prefix" cargo build --locked --release --target "$FRD_CARGO_TARGET" \
    -p frd-video-ffmpeg-plugin --features native-ffmpeg
  frd_plugin="$frd_target_dir/$FRD_CARGO_TARGET/release/libfreeremotedesk_ffmpeg.so"
else
  FFMPEG_DIR="$frd_prefix" cargo build --locked --release -p frd-video-ffmpeg-plugin --features native-ffmpeg
  frd_plugin="$frd_target_dir/release/libfreeremotedesk_ffmpeg.so"
fi
if [[ "$frd_run_tests" == 1 ]]; then
  # 同架构原生执行，或显式 --target i686 执行真实 32 位测试进程。
  FFMPEG_DIR="$frd_prefix" \
    LD_LIBRARY_PATH="$frd_prefix/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    cargo test --locked "${frd_test_target[@]}" -p frd-video-ffmpeg-plugin --features native-ffmpeg --lib -- --nocapture
fi
# 只有所有来源/ELF/包门通过后才替换旧bundle。
frd_bundle="$frd_attempt/bundle"
mkdir -p "$frd_bundle"
cp -L "$frd_prefix/lib/libavcodec.so.62" "$frd_prefix/lib/libavutil.so.60" \
  "$frd_plugin" "$frd_bundle/"
cp "$frd_root/third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1" \
  "$frd_bundle/FFmpeg-LGPL-2.1-or-later.txt"
cat > "$frd_bundle/FFmpeg-NOTICE.txt" <<NOTICE
FFmpeg $frd_version, LGPL-2.1-or-later; H.264 and HEVC decoder/parser only.
Source archive SHA-256: $frd_sha256
Architecture: $frd_platform
Assembly: $([[ "$frd_arch" == "aarch64" ]] && echo "aarch64-neon (have_neon=1)" || echo "x86asm (have_x86asm=1)")
NOTICE

case "$frd_platform" in
  linux-x86) frd_file_pattern='ELF 32-bit.*Intel 80386' ;;
  linux-x86_64) frd_file_pattern='ELF 64-bit.*x86-64' ;;
  linux-aarch64) frd_file_pattern='ELF 64-bit.*ARM aarch64' ;;
esac
for frd_library in "$frd_bundle"/libavcodec.so.62 "$frd_bundle"/libavutil.so.60 \
  "$frd_bundle"/libfreeremotedesk_ffmpeg.so; do
  frd_description="$(LC_ALL=C file -b "$frd_library")"
  [[ "$frd_description" =~ $frd_file_pattern ]] || {
    echo "Linux FFmpeg bundle 架构不匹配: $frd_library ($frd_description)" >&2
    exit 1
  }
done
if [[ "$frd_recipe" != upstream ]]; then
  python3 "$frd_root/tools/linux-ffmpeg-bundle.py" complete \
    --bundle "$frd_bundle" --archive "$frd_archive" --source-tree "$frd_source" \
    --configuration-file "$frd_attempt/configuration.json" \
    --unstripped-library "$frd_source/libavcodec/libavcodec.so.62" --kernel-file "$frd_attempt/kernel.json"
fi
FRD_FFMPEG_SOURCE_RECIPE="$frd_recipe" \
  bash "$frd_root/tools/verify-linux-ffmpeg-bundle.sh" "$frd_bundle" "$frd_platform"
if [[ "$frd_run_tests" == 1 ]]; then
  # fixture 通过实际目标进程加载本次随包插件，并核验 ABI 和解码像素。
  FFMPEG_DIR="$frd_prefix" \
    FRD_FFMPEG_TEST_BUNDLE="$frd_bundle" \
    LD_LIBRARY_PATH="$frd_bundle${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    cargo test --locked "${frd_test_target[@]}" -p frd-video-ffmpeg-plugin --features native-ffmpeg \
      --test main444_decode -- --nocapture
fi
frd_output="$frd_build/bundle/$frd_platform"
mkdir -p "$frd_build/bundle"
[[ ! -L "$frd_output" && ( ! -e "$frd_output" || -d "$frd_output" ) ]] || {
  echo 'Linux bundle 输出必须为普通目录' >&2; exit 1;
}
rm -rf "$frd_output"
mv "$frd_bundle" "$frd_output"
echo "Linux FFmpeg bundle: $frd_output"
if [[ "${FRD_FFMPEG_KEEP_BUILD:-0}" == 1 ]]; then
  # 仅成功发布后显式保留同源实际对象/config/prefix供root数值与基准；失败仍清理。
  trap - EXIT
  printf 'Linux FFmpeg retained configured attempt: %s\n' "$frd_attempt"
  printf 'Linux FFmpeg retained shipping source: %s\n' "$frd_source"
fi
