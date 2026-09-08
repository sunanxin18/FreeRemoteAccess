#!/bin/bash
# 静态验证所有目标；运行证明必须显式请求，同架构或 x64 内核实际执行 i686。
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'Linux 包验证需要 Linux 工具宿主' >&2; exit 2; }
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
package="${1:?需要完整客户端包目录}"
platform="${2:?需要 linux-x86、linux-x86_64 或 linux-aarch64}"
python3 "$repo_root/packaging/linux/verify_package.py" "$package" "$platform" "$repo_root"
"$repo_root/tools/verify-linux-ffmpeg-bundle.sh" "$package/codecs/ffmpeg-8.1.2/$platform" "$platform"
if [[ "${FRD_LINUX_PACKAGE_RUNTIME_SMOKE:-0}" == 1 ]]; then
  case "$(uname -m)" in
    x86_64) host=linux-x86_64;; i686|x86) host=linux-x86;; aarch64|arm64) host=linux-aarch64;;
    *) echo '不支持的运行验证宿主' >&2; exit 2;;
  esac
  [[ "$platform" == "$host" || ( "$host" == linux-x86_64 && "$platform" == linux-x86 && -f /lib/ld-linux.so.2 ) ]] || { echo "不支持跨架构包运行验证：$host -> $platform；静态检查不代表运行通过" >&2; exit 2; }
  # 仅在已核验架构的可信本地产物上请求动态加载器列出依赖；不使用 ldd 的回退执行路径。
  case "$platform" in
    linux-x86) loader=/lib/ld-linux.so.2 ;;
    linux-x86_64) loader=/lib64/ld-linux-x86-64.so.2 ;;
    linux-aarch64) loader=/lib/ld-linux-aarch64.so.1 ;;
  esac
  env -u LD_LIBRARY_PATH -u LD_PRELOAD "$loader" --verify "$package/freeremotedesk-linux"
  dependencies="$(env -u LD_LIBRARY_PATH -u LD_PRELOAD "$loader" --list "$package/freeremotedesk-linux")"
  if grep -Fq 'not found' <<< "$dependencies"; then
    echo '客户端缺少系统运行库' >&2; exit 1
  fi
  # 禁止外部库路径让缺失的随包依赖被开发环境掩盖。
  env -u LD_LIBRARY_PATH -u LD_PRELOAD "$package/freeremotedesk-linux" --verify-codec-bundle
  printf 'Linux 客户端目标进程解码器加载验证通过：%s\n' "$platform"
fi
printf 'Linux 完整客户端包静态验证通过：%s (%s)\n' "$package" "$platform"
