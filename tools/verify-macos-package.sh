#!/bin/bash
set -euo pipefail
app="${1:?需要 FreeRemoteDesk.app 路径}"
plist="$app/Contents/Info.plist"
plutil -lint "$plist"
[[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$plist")" == org.freeremotedesk.macos ]]
[[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$plist")" == freeremotedesk-macos ]]
[[ "$(/usr/libexec/PlistBuddy -c 'Print LSMinimumSystemVersion' "$plist")" == 12.0 ]]
test -x "$app/Contents/MacOS/freeremotedesk-macos"
test -s "$app/Contents/Resources/FreeRemoteDesk.icns"
codesign --verify --strict "$app"

case "$(uname -m)" in
    arm64) expected_arch=arm64; codec_arch=aarch64 ;;
    x86_64) expected_arch=x86_64; codec_arch=x86_64 ;;
    *) echo "不支持的 macOS verifier 主机架构: $(uname -m)" >&2; exit 1 ;;
esac

assert_macho_arch() {
    local object="$1"
    local actual
    actual="$(lipo -archs "$object" 2>/dev/null)"
    if [[ "$actual" != "$expected_arch" ]]; then
        echo "Mach-O 架构不匹配: $object (实际: ${actual:-unknown}; 要求: $expected_arch)" >&2
        exit 1
    fi
}

assert_macho_arch "$app/Contents/MacOS/freeremotedesk-macos"
file "$app/Contents/MacOS/freeremotedesk-macos"
# 仅允许系统动态库，防止产物依赖开发机绝对路径。
if otool -L "$app/Contents/MacOS/freeremotedesk-macos" | tail -n +2 | awk '{print $1}' | grep -vE '^(/System/Library/|/usr/lib/|@executable_path/|@rpath/)'; then
    echo '应用包包含非系统开发机动态库依赖' >&2; exit 1
fi
"$app/Contents/MacOS/freeremotedesk-macos" --help > /dev/null

codec_root="$app/Contents/MacOS/codecs/ffmpeg-8-1-2"
if [[ -e "$codec_root" && ! -d "$codec_root" ]]; then
    echo 'FFmpeg codec 根路径必须是目录' >&2
    exit 1
fi
if [[ -d "$codec_root" ]]; then
    codec_dir="$codec_root/macos-$codec_arch"
    [[ -d "$codec_dir" ]] || { echo "FFmpeg codec 架构目录缺失: $codec_dir" >&2; exit 1; }

    codec_children=()
    while IFS= read -r -d '' child; do
        codec_children+=("$(basename "$child")")
    done < <(find "$codec_root" -mindepth 1 -maxdepth 1 -print0)
    [[ "${#codec_children[@]}" -eq 1 && "${codec_children[0]}" == "macos-$codec_arch" ]] || {
        echo "FFmpeg codec 根目录只能包含当前架构目录: $codec_root" >&2
        exit 1
    }

    expected_codec_files=(
        "libavcodec.62.dylib"
        "libavutil.60.dylib"
        "libfreeremotedesk_ffmpeg.dylib"
    )
    codec_files=()
    while IFS= read -r -d '' child; do
        codec_files+=("$(basename "$child")")
    done < <(find "$codec_dir" -mindepth 1 -maxdepth 1 -print0)
    actual_codec_set="$(printf '%s\n' "${codec_files[@]}" | LC_ALL=C sort)"
    expected_codec_set="$(printf '%s\n' "${expected_codec_files[@]}" | LC_ALL=C sort)"
    [[ "$actual_codec_set" == "$expected_codec_set" ]] || {
        echo "FFmpeg codec 文件集合不匹配: $codec_dir" >&2
        exit 1
    }

    for library in "${expected_codec_files[@]}"; do
        path="$codec_dir/$library"
        [[ -f "$path" && ! -L "$path" ]] || { echo "FFmpeg codec 必须是普通文件: $path" >&2; exit 1; }
        assert_macho_arch "$path"
        install_name="$(otool -D "$path" | tail -n +2 | sed '/^[[:space:]]*$/d' | head -n 1)"
        [[ "$install_name" == "@loader_path/$library" ]] || {
            echo "FFmpeg codec install name 必须固定为 @loader_path: $path" >&2
            exit 1
        }
        if otool -L "$path" | tail -n +2 | awk '{print $1}' | grep -vE '^(@loader_path/|/System/Library/|/usr/lib/)'; then
            echo "FFmpeg codec 包含开发机或包外动态库依赖: $path" >&2
            exit 1
        fi
    done

    "$app/Contents/MacOS/freeremotedesk-macos" --verify-codec-bundle
fi
printf '%s\n' 'macOS 应用包结构、签名和 CLI 验证通过；GUI 和实机互操作需独立验收。'
