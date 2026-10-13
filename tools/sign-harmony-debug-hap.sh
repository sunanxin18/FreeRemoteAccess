#!/usr/bin/env bash
# 正常开发签名；所有密码由官方工具交互获取，绝不进入 argv 或配置。
set -euo pipefail
usage() {
    cat <<'USAGE'
用法：tools/sign-harmony-debug-hap.sh --hap 未签HAP --output 签名HAP \
  --cert 签名证书链.cer --profile 已签profile.p7b --keystore 密钥库.p12 --alias 别名 \
  --ffmpeg-bundle OHOS包目录 --ffmpeg-manifest-sha256 独立接收摘要
只用于合法开发签名。profile 必须已由正常开发流程签名、权限为 normal，
有效期/设备绑定/应用标识由调用方确认。密码由官方工具交互读取。
USAGE
}
hap=''; output=''; cert=''; profile=''; keystore=''; alias=''; bundle=''; manifest_sha=''
while (($#)); do
    case "$1" in
        --hap) hap="${2:?需要 HAP}"; shift 2 ;;
        --output) output="${2:?需要输出 HAP}"; shift 2 ;;
        --cert) cert="${2:?需要证书链}"; shift 2 ;;
        --profile) profile="${2:?需要签名 profile}"; shift 2 ;;
        --keystore) keystore="${2:?需要密钥库}"; shift 2 ;;
        --alias) alias="${2:?需要别名}"; shift 2 ;;
        --ffmpeg-bundle) bundle="${2:?需要 OHOS bundle}"; shift 2 ;;
        --ffmpeg-manifest-sha256) manifest_sha="${2:?需要 manifest SHA256}"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) printf '未知参数：%s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done
for file in "$hap" "$cert" "$profile" "$keystore"; do
    if [[ -z "$file" || ! -f "$file" ]]; then printf '缺少签名输入文件。\n' >&2; exit 2; fi
done
if [[ -z "$output" || -z "$alias" || -z "$bundle" || -z "$manifest_sha" || "$hap" == "$output" || "$hap" -ef "$output" ]]; then usage >&2; exit 2; fi
studio="${DEVECO_HOME:-/Applications/DevEco-Studio.app/Contents}"
if [[ "$studio" == *.app ]]; then studio="$studio/Contents"; fi
sdk="${DEVECO_SDK_HOME:-$studio/sdk}"
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
checker_args=(--readelf "$sdk/default/openharmony/native/llvm/bin/llvm-readelf" --ffmpeg-bundle "$bundle" --ffmpeg-manifest-sha256 "$manifest_sha")
python3 "$repository_root/tools/check-harmony-hap.py" --hap "$hap" "${checker_args[@]}"
"$studio/jbr/Contents/Home/bin/java" -jar "$sdk/default/openharmony/toolchains/lib/hap-sign-tool.jar" sign-app \
    -mode localSign -keyAlias "$alias" -appCertFile "$cert" -profileFile "$profile" \
    -inFile "$hap" -signAlg SHA256withECDSA -keystoreFile "$keystore" \
    -outFile "$output" -compatibleVersion 26 -signCode 1 -pwdInputMode 1
"$studio/jbr/Contents/Home/bin/java" -jar "$sdk/default/openharmony/toolchains/lib/hap-sign-tool.jar" verify-app \
    -inFile "$output" -outCertChain "$output.verify.cer" -outProfile "$output.verify.p7b"

# 官方验签之后，重新核验最终包并比较所有相关条目字节。
python3 "$repository_root/tools/check-harmony-hap.py" --hap "$output" "${checker_args[@]}" --unsigned-hap "$hap"
