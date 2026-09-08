#!/bin/bash
set -euo pipefail
requested_arch="${FRD_MACOS_ARCH:-$(uname -m)}"
case "$requested_arch" in
    arm64|aarch64) requested_arch=arm64; expected_arch=arm64; codec_arch=aarch64 ;;
    x86_64) echo "macOS 仅验证 ARM64 分发包，不再支持 Intel/x86_64 包" >&2; exit 1 ;;
    *) echo "不支持的 macOS verifier 架构: $requested_arch" >&2; exit 1 ;;
esac

app="${1:?需要 FreeRemoteDesk.app 路径}"
plist="$app/Contents/Info.plist"
plutil -lint "$plist"
[[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$plist")" == org.freeremotedesk.macos ]]
[[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleExecutable' "$plist")" == freeremotedesk-macos ]]
[[ "$(/usr/libexec/PlistBuddy -c 'Print LSMinimumSystemVersion' "$plist")" == 12.0 ]]
test -x "$app/Contents/MacOS/freeremotedesk-macos"
test -s "$app/Contents/Resources/FreeRemoteDesk.icns"

# 归属资源采用固定文件集合和固定hash；不能通过同时改manifest与文件绕过。
license_dir="$app/Contents/Resources/licenses"
[[ -d "$license_dir" && ! -L "$license_dir" ]] || { echo 'Progressive 许可目录缺失或不是普通目录' >&2; exit 1; }
expected_license_files=(FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt FreeRDP-SHA256SUMS.txt)
actual_license_files=()
while IFS= read -r -d '' resource; do
    [[ -f "$resource" && ! -L "$resource" ]] || { echo "Progressive 许可必须是普通文件: $resource" >&2; exit 1; }
    actual_license_files+=("$(basename "$resource")")
done < <(find "$license_dir" -mindepth 1 -maxdepth 1 -print0)
[[ "$(printf '%s\n' "${actual_license_files[@]}" | LC_ALL=C sort)" == "$(printf '%s\n' "${expected_license_files[@]}" | LC_ALL=C sort)" ]] || {
    echo 'Progressive 许可文件集合不匹配' >&2; exit 1
}
for name in "${expected_license_files[@]}"; do
    case "$name" in
        FreeRDP-APACHE-2.0.txt) expected_hash=cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30 ;;
        FreeRDP-NOTICE.txt) expected_hash=0dae42bc255d72bd75cebd02d50b74746f218fd37281930e85dad96133e2b141 ;;
        FreeRDP-SHA256SUMS.txt) expected_hash=94ceb958c66970b2d598df7d2ba8ea0024955cc1e44b162702550bb7ed4b279a ;;
    esac
    actual_hash="$(shasum -a 256 "$license_dir/$name" | awk '{print $1}')"
    [[ "$actual_hash" == "$expected_hash" ]] || { echo "Progressive 许可/manifest hash不匹配: $name" >&2; exit 1; }
done


codesign --verify --strict "$app"

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
