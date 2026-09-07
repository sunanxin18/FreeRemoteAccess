#!/bin/bash
# 从固定、校验过的 LGPL 源码生成 macOS 随包解码器；不使用 Homebrew 动态库。
set -euo pipefail
cd "$(dirname "$0")/.."
frd_root="$PWD"
frd_build="$frd_root/target/ffmpeg-macos"
if [[ "$(uname -m)" == "x86_64" ]] && ! command -v nasm >/dev/null; then
  echo "macOS x86_64 FFmpeg build requires NASM/x86asm" >&2
  exit 2
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
./configure --prefix="$frd_prefix" --disable-static --enable-shared --disable-programs \
  --disable-doc --disable-everything --enable-decoder=hevc,h264 --enable-parser=hevc,h264 \
  --disable-gpl --disable-nonfree --disable-version3 --disable-autodetect --disable-network \
  --disable-debug --enable-stripping --disable-avdevice --disable-avformat --disable-avfilter \
  --disable-swresample --disable-swscale
make -j "$(sysctl -n hw.ncpu)"
if [[ "$(uname -m)" == "x86_64" ]]; then
  grep -q '^#define HAVE_X86ASM 1$' config.h || {
    echo "FFmpeg x86_64 build did not enable HAVE_X86ASM" >&2
    exit 1
  }
fi
make install
cd "$frd_root"
FFMPEG_DIR="$frd_prefix" cargo build --release -p frd-video-ffmpeg-plugin --features native-ffmpeg
frd_bundle="$frd_attempt/bundle"
mkdir -p "$frd_bundle"
cp -L "$frd_prefix/lib/libavcodec.62.dylib" "$frd_prefix/lib/libavutil.60.dylib" "$frd_bundle/"
cp target/release/libfreeremotedesk_ffmpeg.dylib "$frd_bundle/"
for frd_lib in "$frd_bundle/"*.dylib; do
  install_name_tool -id "@loader_path/$(basename "$frd_lib")" "$frd_lib"
  for frd_dep in libavcodec.62.dylib libavutil.60.dylib; do
    install_name_tool -change "$frd_prefix/lib/$frd_dep" "@loader_path/$frd_dep" "$frd_lib"
  done
  codesign --force --sign - "$frd_lib"
done
cp third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1 "$frd_bundle/FFmpeg-LGPL-2.1-or-later.txt"
cat > "$frd_bundle/FFmpeg-NOTICE.txt" <<'NOTICE'
FFmpeg 8.1.2, LGPL-2.1-or-later; source is unmodified, H.264 and HEVC decoder/parser only.
Source archive SHA-256: 464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
Corresponding source is distributed beside the application as ffmpeg-8.1.2.tar.xz.
Rebuild with tools/build-ffmpeg-macos.sh; LGPL libraries remain dynamically replaceable.
NOTICE
if [[ -e "$frd_build/bundle" ]]; then
  mv "$frd_build/bundle" "$frd_build/bundle.previous.$(date +%s).$$"
fi
mv "$frd_bundle" "$frd_build/bundle"
echo "macOS FFmpeg bundle: $frd_build/bundle"
