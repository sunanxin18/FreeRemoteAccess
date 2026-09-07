#!/bin/bash
# 从固定、校验过的 LGPL 源码生成 macOS 随包解码器；不使用 Homebrew 动态库。
set -euo pipefail
cd "$(dirname "$0")/.."
frd_root="$PWD"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-12.0}"
frd_host_arch="$(uname -m)"
frd_arch="${FRD_MACOS_ARCH:-$frd_host_arch}"
case "$frd_arch" in
  x86_64)
    frd_platform="macos-x86_64"
    frd_ffmpeg_arch="x86_64"
    frd_clang_arch="x86_64"
    frd_rust_target="x86_64-apple-darwin"
    frd_macho_arch="x86_64"
    frd_assembly_kind="x86asm"
    frd_assembly_gate="have_x86asm=1"
    frd_requires_nasm=1
    ;;
  arm64|aarch64)
    frd_arch="arm64"
    frd_platform="macos-aarch64"
    frd_ffmpeg_arch="aarch64"
    frd_clang_arch="arm64"
    frd_rust_target="aarch64-apple-darwin"
    frd_macho_arch="arm64"
    frd_assembly_kind="aarch64-neon"
    frd_assembly_gate="have_neon=1"
    frd_requires_nasm=0
    ;;
  *)
    echo "不支持的 macOS FFmpeg 架构: $frd_arch" >&2
    exit 2
    ;;
esac
if [[ "$frd_requires_nasm" -eq 1 ]] && ! command -v nasm >/dev/null; then
  echo "macOS $frd_arch FFmpeg build requires NASM/x86asm" >&2
  exit 2
fi
frd_build="${FRD_FFMPEG_BUILD_ROOT:-$frd_root/target/ffmpeg-macos}"
frd_cross_build=0
if [[ "$frd_host_arch" != "$frd_arch" ]]; then
  frd_cross_build=1
fi
mkdir -p "$frd_build"
frd_archive="$frd_build/ffmpeg-8.1.2.tar.xz"
if [[ ! -f "$frd_archive" ]]; then
  curl --fail --location --max-time 120 https://ffmpeg.org/releases/ffmpeg-8.1.2.tar.xz -o "$frd_archive"
fi
[[ "$(shasum -a 256 "$frd_archive" | cut -d ' ' -f 1)" == "464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c" ]]
frd_attempt=$(mktemp -d "$frd_build/build.XXXXXX")
tar -xf "$frd_archive" -C "$frd_attempt"
frd_prefix="$frd_attempt/dist"
cd "$frd_attempt/ffmpeg-8.1.2"
frd_configure_args=(
  "--prefix=$frd_prefix"
  "--arch=$frd_ffmpeg_arch"
  "--target-os=darwin"
  --disable-static --enable-shared --disable-programs
  --disable-doc --disable-everything --enable-decoder=hevc,h264 --enable-parser=hevc,h264 \
  --disable-gpl --disable-nonfree --disable-version3 --disable-autodetect --disable-network \
  --disable-debug --enable-stripping --disable-avdevice --disable-avformat --disable-avfilter \
  --disable-swresample --disable-swscale
)
if [[ "$frd_cross_build" -eq 1 ]]; then
  frd_configure_args+=(
    --enable-cross-compile
    --cc=clang
    "--extra-cflags=-arch $frd_clang_arch"
    "--extra-ldflags=-arch $frd_clang_arch"
  )
fi
if [[ "$frd_requires_nasm" -eq 0 ]]; then
  frd_configure_args+=(--disable-x86asm)
fi
./configure "${frd_configure_args[@]}"
make -j "$(sysctl -n hw.ncpu)"
if [[ "$frd_requires_nasm" -eq 1 ]]; then
  grep -q '^#define HAVE_X86ASM 1$' config.h || {
    echo "FFmpeg $frd_arch build did not enable HAVE_X86ASM" >&2
    exit 1
  }
else
  grep -q '^#define HAVE_NEON 1$' config.h || {
    echo "FFmpeg arm64 build did not enable HAVE_NEON" >&2
    exit 1
  }
fi
make install
cd "$frd_root"
if [[ "$frd_cross_build" -eq 1 ]]; then
  frd_target_dir="${CARGO_TARGET_DIR:-$frd_root/target}"
  FFMPEG_DIR="$frd_prefix" cargo build --locked --release --target "$frd_rust_target" \
    -p frd-video-ffmpeg-plugin --features native-ffmpeg
  frd_plugin="$frd_target_dir/$frd_rust_target/release/libfreeremotedesk_ffmpeg.dylib"
else
  frd_target_dir="${CARGO_TARGET_DIR:-$frd_root/target}"
  FFMPEG_DIR="$frd_prefix" cargo build --locked --release \
    -p frd-video-ffmpeg-plugin --features native-ffmpeg
  frd_plugin="$frd_target_dir/release/libfreeremotedesk_ffmpeg.dylib"
fi
frd_bundle="$frd_attempt/bundle"
mkdir -p "$frd_bundle"
cp -L "$frd_prefix/lib/libavcodec.62.dylib" "$frd_prefix/lib/libavutil.60.dylib" "$frd_bundle/"
cp "$frd_plugin" "$frd_bundle/"
for frd_lib in "$frd_bundle/"*.dylib; do
  install_name_tool -id "@loader_path/$(basename "$frd_lib")" "$frd_lib"
  for frd_dep in libavcodec.62.dylib libavutil.60.dylib; do
    install_name_tool -change "$frd_prefix/lib/$frd_dep" "@loader_path/$frd_dep" "$frd_lib"
  done
  codesign --force --sign - "$frd_lib"
done
for frd_lib in "$frd_bundle/"*.dylib; do
  [[ "$(lipo -archs "$frd_lib")" == "$frd_macho_arch" ]] || {
    echo "macOS FFmpeg bundle 架构不匹配: $frd_lib" >&2
    exit 1
  }
done
if [[ "$frd_cross_build" -eq 0 && "${FRD_FFMPEG_RUN_NATIVE_TESTS:-0}" == 1 ]]; then
  # 仅在 native host 上运行当前 bundle 的 HEVC fixture；跨架构构建不能把 hosted host
  # 的动态库运行结果冒充目标架构证据。
  FFMPEG_DIR="$frd_prefix" \
    FRD_FFMPEG_TEST_BUNDLE="$frd_bundle" \
    DYLD_LIBRARY_PATH="$frd_bundle${DYLD_LIBRARY_PATH:+:$DYLD_LIBRARY_PATH}" \
    cargo test --locked -p frd-video-ffmpeg-plugin --features native-ffmpeg \
      --test main444_decode -- --nocapture
fi
cp third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1 "$frd_bundle/FFmpeg-LGPL-2.1-or-later.txt"
cat > "$frd_bundle/FFmpeg-NOTICE.txt" <<NOTICE
FFmpeg 8.1.2, LGPL-2.1-or-later; source is unmodified, H.264 and HEVC decoder/parser only.
Source archive SHA-256: 464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
Architecture: $frd_platform
Assembly: $frd_assembly_kind ($frd_assembly_gate)
Corresponding source is distributed beside the application as ffmpeg-8.1.2.tar.xz.
Rebuild with tools/build-ffmpeg-macos.sh; LGPL libraries remain dynamically replaceable.
NOTICE
if [[ -e "$frd_build/bundle" ]]; then
  mv "$frd_build/bundle" "$frd_build/bundle.previous.$(date +%s).$$"
fi
mv "$frd_bundle" "$frd_build/bundle"
echo "macOS FFmpeg bundle: $frd_build/bundle"
