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
frd_version="8.1.2"
frd_sha256="464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c"
frd_build="${FRD_FFMPEG_BUILD_ROOT:-$frd_root/target/ffmpeg-linux}"
frd_archive="$frd_build/ffmpeg-$frd_version.tar.xz"
frd_arch="${FRD_FFMPEG_ARCH:-$(uname -m)}"
frd_host_arch="$(uname -m)"
frd_cross_build=0

case "$frd_arch" in
  x86|i686)
    frd_platform="linux-x86"
    frd_arch="x86"
    command -v nasm >/dev/null || { echo "Linux x86 FFmpeg build requires NASM/x86asm" >&2; exit 2; }
    ;;
  x86_64)
    frd_platform="linux-x86_64"
    command -v nasm >/dev/null || { echo "Linux x86_64 FFmpeg build requires NASM/x86asm" >&2; exit 2; }
    ;;
  aarch64|arm64) frd_platform="linux-aarch64"; frd_arch="aarch64" ;;
  *) echo "不支持的 Linux FFmpeg 架构: $frd_arch" >&2; exit 2 ;;
esac

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
  frd_cargo_target_env="${frd_cargo_target_env^^}"
  export "CC_${frd_cargo_target_env}=$FRD_FFMPEG_CC"
  export "CARGO_TARGET_${frd_cargo_target_env}_LINKER=$FRD_FFMPEG_CC"
  if command -v "${FRD_FFMPEG_CROSS_PREFIX}ar" >/dev/null; then
    export "AR_${frd_cargo_target_env}=${FRD_FFMPEG_CROSS_PREFIX}ar"
  fi
fi

mkdir -p "$frd_build"
if [[ ! -f "$frd_archive" ]]; then
  curl --fail --location --max-time 120 \
    "https://ffmpeg.org/releases/ffmpeg-$frd_version.tar.xz" -o "$frd_archive"
fi
[[ "$(sha256sum "$frd_archive" | cut -d ' ' -f 1)" == "$frd_sha256" ]]

frd_attempt=$(mktemp -d "$frd_build/build.XXXXXX")
trap 'rm -rf "$frd_attempt"' EXIT
tar -xf "$frd_archive" -C "$frd_attempt"
frd_prefix="$frd_attempt/dist"
cd "$frd_attempt/ffmpeg-$frd_version"
frd_configure_args=(
  --prefix="$frd_prefix" --arch="$frd_arch" --target-os=linux
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
make -j "$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 2)"
make install

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
  FFMPEG_DIR="$frd_prefix" cargo build --release --target "$FRD_CARGO_TARGET" \
    -p frd-video-ffmpeg-plugin --features native-ffmpeg
  frd_plugin="target/$FRD_CARGO_TARGET/release/libfreeremotedesk_ffmpeg.so"
else
  FFMPEG_DIR="$frd_prefix" cargo build --release -p frd-video-ffmpeg-plugin --features native-ffmpeg
  frd_plugin="target/release/libfreeremotedesk_ffmpeg.so"
fi
frd_bundle="$frd_build/bundle/$frd_platform"
rm -rf "$frd_bundle"
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
frd_bundle_files=()
while IFS= read -r -d '' frd_entry; do
  frd_bundle_files+=("$(basename "$frd_entry")")
done < <(find "$frd_bundle" -mindepth 1 -maxdepth 1 -type f -print0)
frd_actual_files="$(printf '%s\n' "${frd_bundle_files[@]}" | LC_ALL=C sort)"
frd_expected_files="$(printf '%s\n' \
  FFmpeg-LGPL-2.1-or-later.txt FFmpeg-NOTICE.txt libavcodec.so.62 \
  libavutil.so.60 libfreeremotedesk_ffmpeg.so | LC_ALL=C sort)"
[[ "$frd_actual_files" == "$frd_expected_files" ]] || {
  echo "Linux FFmpeg bundle 文件集合不匹配: $frd_bundle" >&2
  exit 1
}
echo "Linux FFmpeg bundle: $frd_bundle"
