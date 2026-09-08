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
verify() { FRD_MACOS_ARCH=arm64 bash "$repo_root/tools/verify-macos-package.sh" "$app" > "$work/verify.log" 2>&1; }
expect_rejection() {
    local pattern="$1"
    codesign --force --sign - "$app" >/dev/null 2>&1
    if verify; then echo '被修改的许可包意外通过验证' >&2; exit 1; fi
    grep -F "$pattern" "$work/verify.log" >/dev/null || { cat "$work/verify.log" >&2; exit 1; }
}
restore_licenses
codesign --force --sign - "$app" >/dev/null 2>&1
verify || { cat "$work/verify.log" >&2; exit 1; }
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
printf 'macOS ARM64 synthetic package: positive fixture and 9 rejection cases passed\n'
