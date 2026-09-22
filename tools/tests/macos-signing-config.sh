#!/bin/bash
# 隔离打包流程的签名参数测试，不访问证书或修改真实应用包。
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/frd-signing-tests.XXXXXX")"
trap 'rm -rf "$work"' EXIT
fixture="$work/repo"
mkdir -p "$fixture/tools" "$fixture/packaging/macos" "$fixture/packaging/windows/licenses" "$fixture/assets/app-icon" "$work/bin" "$work/target/debug" "$work/codec/bundle"
cp "$repo_root/tools/stage-macos-package.sh" "$fixture/tools/"
cp "$repo_root/packaging/macos/Info.plist" "$fixture/packaging/macos/"
cp "$repo_root/packaging/windows/licenses/FreeRDP-APACHE-2.0.txt" "$repo_root/packaging/windows/licenses/FreeRDP-NOTICE.txt" "$fixture/packaging/windows/licenses/"
touch "$fixture/assets/app-icon/README.md" "$work/target/debug/freeremotedesk-macos"
for name in FFmpeg-LGPL-2.1-or-later.txt FFmpeg-NOTICE.txt libavcodec.62.dylib libavutil.60.dylib libfreeremotedesk_ffmpeg.dylib; do
    touch "$work/codec/bundle/$name"
done
cat > "$work/bin/uname" <<'SH'
#!/bin/bash
case "$1" in -s) echo Darwin;; -m) echo arm64;; *) exit 1;; esac
SH
for tool in cargo swift; do printf '#!/bin/bash\nexit 0\n' > "$work/bin/$tool"; done
cat > "$work/bin/iconutil" <<'SH'
#!/bin/bash
touch "${@: -1}"
SH
cat > "$work/bin/codesign" <<'SH'
#!/bin/bash
set -euo pipefail
[[ "$#" == 4 && "$1" == --force && "$2" == --sign ]]
printf '%s\n' "$3" >> "$FRD_TEST_SIGNATURE_LOG"
[[ "$3" == "$FRD_TEST_EXPECTED_IDENTITY" ]]
[[ "${FRD_TEST_SIGNING_FAIL:-0}" == 0 ]]
SH
cat > "$fixture/tools/verify-macos-package.sh" <<'SH'
#!/bin/bash
[[ "$FRD_MACOS_SIGNING_IDENTITY" == "$FRD_TEST_EXPECTED_IDENTITY" ]]
SH
chmod +x "$work/bin/"* "$fixture/tools/verify-macos-package.sh"
export PATH="$work/bin:$PATH" CARGO_TARGET_DIR="$work/target" FRD_FFMPEG_BUILD_ROOT="$work/codec" FRD_TEST_SIGNATURE_LOG="$work/signatures.log"
export FRD_MACOS_SIGNING_CONFIG="$work/development-signing-identity"
unset FRD_MACOS_SIGNING_IDENTITY
export FRD_TEST_EXPECTED_IDENTITY=-
bash "$fixture/tools/stage-macos-package.sh" > /dev/null
[[ "$(wc -l < "$FRD_TEST_SIGNATURE_LOG" | tr -d ' ')" == 4 ]]
# 默认从隔离配置读取证书指纹，不触碰当前用户的实际配置。
configured_identity=0123456789abcdef0123456789abcdef01234567
printf '%s\n' "$configured_identity" > "$FRD_MACOS_SIGNING_CONFIG"
export FRD_TEST_EXPECTED_IDENTITY="$configured_identity"
: > "$FRD_TEST_SIGNATURE_LOG"
bash "$fixture/tools/stage-macos-package.sh" > /dev/null
[[ "$(wc -l < "$FRD_TEST_SIGNATURE_LOG" | tr -d ' ')" == 4 ]]
for invalid in bad multi_line extra_blank; do
    case "$invalid" in
        bad) printf 'not-a-fingerprint\n' > "$FRD_MACOS_SIGNING_CONFIG" ;;
        multi_line) printf '%s\n%s\n' "$configured_identity" "$configured_identity" > "$FRD_MACOS_SIGNING_CONFIG" ;;
        extra_blank) printf '%s\n\n' "$configured_identity" > "$FRD_MACOS_SIGNING_CONFIG" ;;
    esac
    : > "$FRD_TEST_SIGNATURE_LOG"
    if bash "$fixture/tools/stage-macos-package.sh" > /dev/null 2>&1; then
        echo '非法签名配置意外通过' >&2; exit 1
    fi
    [[ ! -s "$FRD_TEST_SIGNATURE_LOG" ]]
done
# 包含空格和shell元字符，必须仍作为一个原样参数传递。
# 显式环境变量优先，即使默认配置损坏也不读取它。
export FRD_MACOS_SIGNING_IDENTITY='Test Identity: Team (123) $literal `literal`'
export FRD_TEST_EXPECTED_IDENTITY="$FRD_MACOS_SIGNING_IDENTITY"
: > "$FRD_TEST_SIGNATURE_LOG"
bash "$fixture/tools/stage-macos-package.sh" > /dev/null
[[ "$(wc -l < "$FRD_TEST_SIGNATURE_LOG" | tr -d ' ')" == 4 ]]
export FRD_TEST_SIGNING_FAIL=1
: > "$FRD_TEST_SIGNATURE_LOG"
if bash "$fixture/tools/stage-macos-package.sh" > /dev/null 2>&1; then
    echo '签名失败未阻止打包' >&2; exit 1
fi
[[ "$(wc -l < "$FRD_TEST_SIGNATURE_LOG" | tr -d ' ')" == 1 ]]
printf '签名参数测试通过：默认临时签名、配置身份及非法配置、显式覆盖、统一证书身份、安全引用及失败无回退。真实证书签名未验证。\n'
