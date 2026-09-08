#!/bin/bash
set -euo pipefail
# 本地 macOS 应用包；默认 debug 便于 GUI 验收，release 用于分发前检查。
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
profile="${1:-debug}"
case "$profile" in debug|release) ;; *) echo '用法：stage-macos-package.sh [debug|release]' >&2; exit 2;; esac
if [[ "$(uname -s)" != Darwin ]]; then echo '需要 macOS 原生工具链' >&2; exit 1; fi
export MACOSX_DEPLOYMENT_TARGET=12.0
host_arch="$(uname -m)"
requested_arch="${FRD_MACOS_ARCH:-$host_arch}"
case "$requested_arch" in
    arm64|aarch64) requested_arch=arm64; codec_arch=aarch64; expected_arch=arm64; rust_target=aarch64-apple-darwin ;;
    x86_64) echo "macOS 仅构建 ARM64，不再构建 Intel/x86_64" >&2; exit 2 ;;
    *) echo "不支持的 macOS package 架构: $requested_arch" >&2; exit 2 ;;
esac
cross_build=0
if [[ "$host_arch" != "$requested_arch" ]]; then cross_build=1; fi
cd "$repo_root"
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
# Cargo resolves a relative CARGO_TARGET_DIR from the repository working
# directory below.  Keep the staged App beside that same target root instead
# of silently writing it to the repository's default target directory.
if [[ "$target_dir" != /* ]]; then
    target_dir="$repo_root/$target_dir"
fi
if [[ "$cross_build" -eq 1 ]]; then
    if [[ "$profile" == release ]]; then
        cargo build --locked -p freeremotedesk-macos --release --target "$rust_target"
    else
        cargo build --locked -p freeremotedesk-macos --target "$rust_target"
    fi
    binary="$target_dir/$rust_target/$profile/freeremotedesk-macos"
else
    if [[ "$profile" == release ]]; then
        cargo build --locked -p freeremotedesk-macos --release
    else
        cargo build --locked -p freeremotedesk-macos
    fi
    binary="$target_dir/$profile/freeremotedesk-macos"
fi
app="$target_dir/macos/$profile/FreeRemoteDesk.app"
# 清理当前 Cargo target root 下的生成目录，避免旧版编解码器或说明残留影响签名。
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/freeremotedesk-macos"
cp "$repo_root/packaging/macos/Info.plist" "$app/Contents/Info.plist"
icon_work="$(mktemp -d "${TMPDIR:-/tmp}/frd-macos-icons.XXXXXX")"
trap 'rm -rf "$icon_work"' EXIT
swift "$repo_root/tools/create-macos-icon.swift" "$repo_root/assets/app-icon/apple" "$icon_work/FreeRemoteDesk.iconset"
iconutil -c icns "$icon_work/FreeRemoteDesk.iconset" -o "$app/Contents/Resources/FreeRemoteDesk.icns"
cp "$repo_root/assets/app-icon/README.md" "$app/Contents/Resources/Icon-Provenance.md"
# Progressive 归属资源由 Windows/macOS 共用；目录名不限定其适用平台。
license_dest="$app/Contents/Resources/licenses"
mkdir -p "$license_dest"
for name in FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt; do
    cp "$repo_root/packaging/windows/licenses/$name" "$license_dest/$name"
done
(
    cd "$license_dest"
    shasum -a 256 FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt > FreeRDP-SHA256SUMS.txt
)

# 同架构的固定版本 FFmpeg bundle 可由 build-ffmpeg-macos.sh 生成。
codec_build_root="${FRD_FFMPEG_BUILD_ROOT:-$repo_root/target/ffmpeg-macos}"
codec_bundle="$codec_build_root/bundle"
if [[ -d "$codec_bundle" ]]; then
    expected_bundle_files=(
        "FFmpeg-LGPL-2.1-or-later.txt"
        "FFmpeg-NOTICE.txt"
        "libavcodec.62.dylib"
        "libavutil.60.dylib"
        "libfreeremotedesk_ffmpeg.dylib"
    )
    bundle_files=()
    while IFS= read -r -d '' resource; do
        [[ -f "$resource" && ! -L "$resource" ]] || {
            echo "FFmpeg bundle 只能包含普通文件: $resource" >&2
            exit 1
        }
        bundle_files+=("$(basename "$resource")")
    done < <(find "$codec_bundle" -mindepth 1 -maxdepth 1 -print0)
    actual_bundle_set="$(printf '%s\n' "${bundle_files[@]}" | LC_ALL=C sort)"
    expected_bundle_set="$(printf '%s\n' "${expected_bundle_files[@]}" | LC_ALL=C sort)"
    [[ "$actual_bundle_set" == "$expected_bundle_set" ]] || {
        echo "FFmpeg bundle 文件集合不匹配: $codec_bundle" >&2
        exit 1
    }
    codec_dest="$app/Contents/MacOS/codecs/ffmpeg-8-1-2/macos-$codec_arch"
    mkdir -p "$codec_dest"
    codec_notices="$app/Contents/Resources/codecs/ffmpeg-8-1-2"
    mkdir -p "$codec_notices"
    for resource in "$codec_bundle"/*; do
        case "$resource" in *.dylib) cp "$resource" "$codec_dest/" ;; *) cp -R "$resource" "$codec_notices/" ;; esac
    done
    while IFS= read -r -d '' library; do codesign --force --sign - "$library"; done < <(find "$codec_dest" -type f -name '*.dylib' -print0)
fi
# 仅本机 ad-hoc 签名；不声称 Developer ID 签名、公证或通用二进制。
codesign --force --sign - "$app"
FRD_MACOS_ARCH="$requested_arch" "$repo_root/tools/verify-macos-package.sh" "$app"
printf '%s\n' "$app"
