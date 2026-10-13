#!/bin/bash
# 合成 Mach-O 包验证许可门禁；不代表GUI、codec或远程连接验收。
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
[[ "$(uname -s)" == Darwin && "$(uname -m)" == arm64 ]] || { echo '需要原生macOS ARM64' >&2; exit 1; }
work="$(mktemp -d "${TMPDIR:-/tmp}/frd-license-package-tests.XXXXXX")"
trap 'rm -rf "$work"' EXIT
app="$work/FreeRemoteDesk.app"
licenses="$app/Contents/Resources/licenses"
mkdir -p "$app/Contents/MacOS" "$licenses"
cp "$repo_root/packaging/macos/Info.plist" "$app/Contents/Info.plist"
printf 'synthetic icon fixture\n' > "$app/Contents/Resources/FreeRemoteDesk.icns"
printf 'int main(void) { return 0; }\n' > "$work/main.c"
clang -arch arm64 -mmacosx-version-min=12.0 "$work/main.c" -o "$app/Contents/MacOS/freeremotedesk-macos"
restore_licenses() {
    rm -rf "$licenses"
    mkdir -p "$licenses"
    cp "$repo_root/packaging/windows/licenses/FreeRDP-APACHE-2.0.txt" "$licenses/"
    cp "$repo_root/packaging/windows/licenses/FreeRDP-NOTICE.txt" "$licenses/"
    (cd "$licenses"; shasum -a 256 FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt > FreeRDP-SHA256SUMS.txt)
}
verify() { FRD_MACOS_ARCH=arm64 FRD_MACOS_SIGNING_IDENTITY=- bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1; }
expect_rejection() {
    local pattern="$1"
    codesign --force --sign - "$app" >/dev/null 2>&1
    if verify; then echo '被修改的许可包意外通过验证' >&2; exit 1; fi
    grep -F "$pattern" "$work/verify.log" >/dev/null || { cat "$work/verify.log" >&2; exit 1; }
}
restore_licenses
codesign --force --sign - "$app" >/dev/null 2>&1
verify || { cat "$work/verify.log" >&2; exit 1; }
# 不访问或创建系统证书，只证明显式稳定身份请求不能接受临时签名包。
if FRD_MACOS_ARCH=arm64 FRD_MACOS_SIGNING_IDENTITY='Synthetic requested identity' bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1; then
    echo '显式证书签名请求意外接受 ad-hoc 包' >&2; exit 1
fi
grep -F '已请求证书签名，但产物不是证书签名' "$work/verify.log" >/dev/null || { cat "$work/verify.log" >&2; exit 1; }
if FRD_MACOS_SIGNING_IDENTITY='' bash "$repo_root/tools/stage-macos-package.sh" > "$work/verify.log" 2>&1; then
    echo '空签名身份意外通过' >&2; exit 1
fi
grep -F 'FRD_MACOS_SIGNING_IDENTITY 不能为空' "$work/verify.log" >/dev/null || { cat "$work/verify.log" >&2; exit 1; }
# 隔离codesign输出，确定性验证证书身份比较，不读取Keychain或创建证书。
mkdir -p "$work/bin"
cat > "$work/bin/codesign" <<'SH'
#!/bin/bash
set -euo pipefail
if [[ "$1" == --verify ]]; then exit 0; fi
if [[ "$2" == --extract-certificates=* ]]; then
    [[ "$#" == 3 ]]
    prefix="${2#--extract-certificates=}"
    if [[ "${FRD_TEST_NESTED_MISMATCH:-0}" == 1 && "$3" == *.dylib ]]; then
        printf 'synthetic leaf C' > "${prefix}0"
    else
        printf 'synthetic leaf B' > "${prefix}0"
    fi
else
    [[ "$#" == 3 && "$2" == --verbose=2 ]]
    printf 'Authority=Synthetic Leaf B\n' >&2
fi
SH
chmod +x "$work/bin/codesign"
synthetic_sha1="$(printf 'synthetic leaf B' | shasum -a 1 | awk '{print $1}')"
for requested_identity in "$synthetic_sha1" 'Synthetic Leaf B'; do
    PATH="$work/bin:$PATH" FRD_MACOS_SIGNING_IDENTITY="$requested_identity" bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1 || { cat "$work/verify.log" >&2; exit 1; }
done
for wrong_identity in 0000000000000000000000000000000000000000 'Synthetic Leaf A'; do
    if PATH="$work/bin:$PATH" FRD_MACOS_SIGNING_IDENTITY="$wrong_identity" bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1; then
        echo '配置A意外接受证书B' >&2; exit 1
    fi
    grep -F '与请求身份不匹配' "$work/verify.log" >/dev/null || { cat "$work/verify.log" >&2; exit 1; }
done
codec_fixture="$app/Contents/MacOS/codecs/ffmpeg-8-1-2/macos-aarch64"
mkdir -p "$codec_fixture"
codec_materials="$app/Contents/Resources/codecs/ffmpeg-8-1-2"
mkdir -p "$codec_materials"
cp "$repo_root/third_party/ffmpeg/8.1.2/LICENSE.LGPLv2.1" "$codec_materials/FFmpeg-LGPL-2.1-or-later.txt"
cat > "$codec_materials/FFmpeg-NOTICE.txt" <<'NOTICE'
source is unmodified
464beb5e7bf0c311e68b45ae2f04e9cc2af88851abb4082231742a74d97b524c
Architecture: macos-aarch64
NOTICE
for library in libavcodec.62.dylib libavutil.60.dylib libfreeremotedesk_ffmpeg.dylib; do
    clang -arch arm64 -mmacosx-version-min=12.0 -dynamiclib "$work/main.c" -Wl,-install_name,"@loader_path/$library" -o "$codec_fixture/$library"
done
PATH="$work/bin:$PATH" FRD_MACOS_SIGNING_IDENTITY="$synthetic_sha1" bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1 || { cat "$work/verify.log" >&2; exit 1; }
if PATH="$work/bin:$PATH" FRD_TEST_NESTED_MISMATCH=1 FRD_MACOS_SIGNING_IDENTITY="$synthetic_sha1" bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1; then
    echo '嵌套库证书变化意外通过' >&2; exit 1
fi
grep -F '签名leaf证书指纹与请求身份不匹配' "$work/verify.log" >/dev/null || { cat "$work/verify.log" >&2; exit 1; }
rm -rf "$app/Contents/MacOS/codecs"
rm -rf "$app/Contents/Resources/codecs"
for name in FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt FreeRDP-SHA256SUMS.txt; do
    restore_licenses
    rm "$licenses/$name"
    expect_rejection 'Progressive 许可文件集合不匹配'
    restore_licenses
    printf '\nmodified\n' >> "$licenses/$name"
    expect_rejection 'Progressive 许可/manifest hash不匹配'
done
restore_licenses
printf '\nmodified\n' >> "$licenses/FreeRDP-NOTICE.txt"
(cd "$licenses"; shasum -a 256 FreeRDP-APACHE-2.0.txt FreeRDP-NOTICE.txt > FreeRDP-SHA256SUMS.txt)
expect_rejection 'Progressive 许可/manifest hash不匹配'
restore_licenses
printf 'unexpected\n' > "$licenses/extra.txt"
expect_rejection 'Progressive 许可文件集合不匹配'
restore_licenses
mv "$licenses/FreeRDP-NOTICE.txt" "$work/linked-notice.txt"
ln -s "$work/linked-notice.txt" "$licenses/FreeRDP-NOTICE.txt"
expect_rejection 'Progressive 许可必须是普通文件'
printf 'macOS ARM64 synthetic package: license gates and signing identity gates (ad-hoc, leaf hash/name, nested mismatch) passed\n'
