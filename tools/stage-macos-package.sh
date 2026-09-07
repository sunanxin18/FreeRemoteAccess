#!/bin/bash
set -euo pipefail
# 本地 macOS 应用包；默认 debug 便于 GUI 验收，release 用于分发前检查。
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
profile="${1:-debug}"
case "$profile" in debug|release) ;; *) echo '用法：stage-macos-package.sh [debug|release]' >&2; exit 2;; esac
if [[ "$(uname -s)" != Darwin ]]; then echo '需要 macOS 原生工具链' >&2; exit 1; fi
export MACOSX_DEPLOYMENT_TARGET=12.0
cd "$repo_root"
if [[ "$profile" == release ]]; then cargo build -p freeremotedesk-macos --release; else cargo build -p freeremotedesk-macos; fi
binary="${CARGO_TARGET_DIR:-$repo_root/target}/$profile/freeremotedesk-macos"
app="$repo_root/target/macos/$profile/FreeRemoteDesk.app"
# 清理固定生成目录，避免旧版编解码器或说明残留影响签名。
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/freeremotedesk-macos"
cp "$repo_root/packaging/macos/Info.plist" "$app/Contents/Info.plist"
icon_work="$(mktemp -d "${TMPDIR:-/tmp}/frd-macos-icons.XXXXXX")"
trap 'rm -rf "$icon_work"' EXIT
swift "$repo_root/tools/create-macos-icon.swift" "$repo_root/assets/app-icon/apple" "$icon_work/FreeRemoteDesk.iconset"
iconutil -c icns "$icon_work/FreeRemoteDesk.iconset" -o "$app/Contents/Resources/FreeRemoteDesk.icns"
cp "$repo_root/assets/app-icon/README.md" "$app/Contents/Resources/Icon-Provenance.md"
# 同架构的固定版本 FFmpeg bundle 可由 build-ffmpeg-macos.sh 生成。
codec_bundle="$repo_root/target/ffmpeg-macos/bundle"
if [[ -d "$codec_bundle" ]]; then
    case "$(uname -m)" in arm64) codec_arch=aarch64 ;; x86_64) codec_arch=x86_64 ;; *) exit 1 ;; esac
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
"$repo_root/tools/verify-macos-package.sh" "$app"
printf '%s\n' "$app"
