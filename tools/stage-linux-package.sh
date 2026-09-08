#!/bin/bash
# 原生 Linux 完整客户端包；交叉构建必须显式选择 Rust target 和链接器。
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'Linux 客户端包需要 Linux 构建宿主' >&2; exit 2; }
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
profile="${1:-release}"
case "$profile" in debug|release) ;; *) echo '用法：stage-linux-package.sh [debug|release]' >&2; exit 2;; esac
case "${FRD_LINUX_ARCH:-$(uname -m)}" in
  i686|x86) platform=linux-x86; rust_target=i686-unknown-linux-gnu ;;
  x86_64) platform=linux-x86_64; rust_target=x86_64-unknown-linux-gnu ;;
  aarch64|arm64) platform=linux-aarch64; rust_target=aarch64-unknown-linux-gnu ;;
  *) echo '不支持的 Linux 架构' >&2; exit 2 ;;
esac
target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
[[ "$target_dir" == /* ]] || target_dir="$repo_root/$target_dir"
codec_root="${FRD_FFMPEG_BUILD_ROOT:-$repo_root/target/ffmpeg-linux}"
codec_bundle="$codec_root/bundle/$platform"
# 缺少 native codec 时立即失败，不生成只有图标或宿主可执行文件的假包。
"$repo_root/tools/verify-linux-ffmpeg-bundle.sh" "$codec_bundle" "$platform"
build_args=(build --locked -p freeremotedesk-linux --target "$rust_target")
[[ "$profile" != release ]] || build_args+=(--release)
cargo "${build_args[@]}"
binary="$target_dir/$rust_target/$profile/freeremotedesk-linux"
[[ -f "$binary" && ! -L "$binary" && -x "$binary" ]] || { echo '缺少目标客户端 ELF' >&2; exit 1; }
parent="$target_dir/linux/$profile/$platform"
mkdir -p "$parent"
attempt="$(mktemp -d "$parent/.stage.XXXXXX")"
trap 'rm -rf "$attempt"' EXIT
package="$attempt/FreeRemoteDesk"
umask 022
mkdir -p "$package/codecs/ffmpeg-8.1.2/$platform" "$package/share/applications" "$package/share/licenses/FreeRemoteDesk"
mkdir -p "$package/share/fonts/freeremotedesk"
install -m 755 "$binary" "$package/freeremotedesk-linux"
install -m 644 "$codec_bundle"/* "$package/codecs/ffmpeg-8.1.2/$platform/"
install -m 644 assets/fonts/noto-sans-sc/NotoSansSC-VariableFont_wght.ttf \
  "$package/share/fonts/freeremotedesk/NotoSansSC-VariableFont_wght.ttf"
install -m 644 packaging/linux/freeremotedesk.desktop "$package/share/applications/"
install -m 644 packaging/linux/README.md "$package/README.md"
install -m 644 assets/app-icon/README.md "$package/share/licenses/FreeRemoteDesk/Icon-Provenance.md"
install -m 644 assets/ui-icons/LICENSE-APACHE-2.0.txt "$package/share/licenses/FreeRemoteDesk/Material-Symbols-APACHE-2.0.txt"
install -m 644 assets/fonts/noto-sans-sc/OFL.txt "$package/share/licenses/FreeRemoteDesk/Noto-Sans-SC-OFL.txt"
for name in FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt; do
  install -m 644 "packaging/windows/licenses/$name" "$package/share/licenses/FreeRemoteDesk/"
done
for size in 16 32 48 64 128 256 512; do
  dest="$package/share/icons/hicolor/${size}x${size}/apps"
  mkdir -p "$dest"
  install -m 644 "assets/app-icon/linux/hicolor/${size}x${size}/apps/freeremotedesk.png" "$dest/"
done
"$repo_root/tools/verify-linux-package.sh" "$package" "$platform"
output="$parent/FreeRemoteDesk"
[[ ! -L "$output" ]] || { echo '输出路径不得为符号链接' >&2; exit 1; }
[[ ! -e "$output" || -d "$output" ]] || { echo '输出路径不是目录' >&2; exit 1; }
# 仅替换已验证的目标构建目录；不安装到系统，也不触碰运行中的其他平台客户端。
rm -rf "$output"
mv "$package" "$output"
printf 'Linux 完整客户端包：%s\n' "$output"
