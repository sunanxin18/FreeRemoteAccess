#!/bin/bash
# 验证 Linux 随包 FFmpeg 解码器的文件集合、ELF 架构、SONAME/依赖和插件 ABI。
# 交叉架构 bundle 只做静态验证；设置 FRD_LINUX_FFMPEG_RUNTIME_SMOKE=1 时，
# 在当前可执行架构上额外通过 ctypes 调用版本化 ABI 入口。
set -euo pipefail

bundle="${1:?需要 Linux FFmpeg bundle 路径}"
profile="${2:?需要 linux-x86_64、linux-x86 或 linux-aarch64 profile}"

[[ -d "$bundle" ]] || { echo "Linux FFmpeg bundle 目录不存在: $bundle" >&2; exit 1; }
command -v file >/dev/null || { echo "verifier 需要 file" >&2; exit 2; }
command -v readelf >/dev/null || { echo "verifier 需要 readelf" >&2; exit 2; }
command -v nm >/dev/null || { echo "verifier 需要 nm" >&2; exit 2; }

case "$profile" in
  linux-x86_64)
    elf_pattern='ELF 64-bit.*x86-64'
    expected_class='ELF64'
    expected_data="2's complement, little endian"
    expected_machine='Advanced Micro Devices X86-64'
    ;;
  linux-x86)
    elf_pattern='ELF 32-bit.*Intel 80386'
    expected_class='ELF32'
    expected_data="2's complement, little endian"
    expected_machine='Intel 80386'
    ;;
  linux-aarch64)
    elf_pattern='ELF 64-bit.*ARM aarch64'
    expected_class='ELF64'
    expected_data="2's complement, little endian"
    expected_machine='AArch64'
    ;;
  *) echo "不支持的 Linux FFmpeg verifier 架构: $profile" >&2; exit 1 ;;
esac

expected_files=(
  FFmpeg-LGPL-2.1-or-later.txt
  FFmpeg-NOTICE.txt
  libavcodec.so.62
  libavutil.so.60
  libfreeremotedesk_ffmpeg.so
)

for name in "${expected_files[@]}"; do
  path="$bundle/$name"
  [[ -f "$path" && ! -L "$path" ]] || {
    echo "Linux FFmpeg bundle 必须包含普通文件: $path" >&2
    exit 1
  }
done

expected_set="$(printf '%s\n' "${expected_files[@]}" | LC_ALL=C sort)"
actual_set="$(find "$bundle" -mindepth 1 -maxdepth 1 -type f -printf '%f\n' 2>/dev/null | LC_ALL=C sort)"
[[ "$actual_set" == "$expected_set" ]] || {
  echo "Linux FFmpeg bundle 文件集合不匹配: $bundle" >&2
  printf '实际文件:\n%s\n期望文件:\n%s\n' "$actual_set" "$expected_set" >&2
  exit 1
}
# 上面的精确集合不能允许目录、FIFO 或其他未列出的目录项混入 bundle。
[[ "$(find "$bundle" -mindepth 1 -maxdepth 1 -print | wc -l | tr -d ' ')" == "5" ]] || {
  echo "Linux FFmpeg bundle 只能包含五个顶层文件: $bundle" >&2
  exit 1
}

assert_elf() {
  local path="$1"
  local description header elf_class elf_data elf_machine
  description="$(LC_ALL=C file -b "$path")"
  [[ "$description" =~ $elf_pattern ]] || {
    echo "Linux FFmpeg bundle ELF 架构不匹配: $path ($description; 要求 $expected_machine)" >&2
    exit 1
  }
  header="$(readelf -h "$path")"
  elf_class="$(awk -F: '/^[[:space:]]*Class:/ { value=$2; sub(/^[[:space:]]+/, "", value); sub(/[[:space:]]+$/, "", value); print value; exit }' <<<"$header")"
  elf_data="$(awk -F: '/^[[:space:]]*Data:/ { value=$2; sub(/^[[:space:]]+/, "", value); sub(/[[:space:]]+$/, "", value); print value; exit }' <<<"$header")"
  elf_machine="$(awk -F: '/^[[:space:]]*Machine:/ { value=$2; sub(/^[[:space:]]+/, "", value); sub(/[[:space:]]+$/, "", value); print value; exit }' <<<"$header")"
  [[ "$elf_class" == "$expected_class" && "$elf_data" == "$expected_data" && "$elf_machine" == "$expected_machine" ]] || {
    echo "Linux FFmpeg bundle ELF header 不匹配: $path (Class=${elf_class:-missing}, Data=${elf_data:-missing}, Machine=${elf_machine:-missing}; 要求 $expected_class, $expected_data, $expected_machine)" >&2
    exit 1
  }
  if grep -aEq '/(Users|home|opt|build|runner)/|[A-Za-z]:\\\\' "$path"; then
    echo "Linux FFmpeg bundle 含开发机绝对路径: $path" >&2
    exit 1
  fi
}

assert_soname() {
  local path="$1"
  local expected="$2"
  local soname
  soname="$(readelf -d "$path" | awk -F'[][]' '/SONAME/ { print $2; exit }')"
  [[ "$soname" == "$expected" ]] || {
    echo "ELF SONAME 不匹配: $path (实际 ${soname:-missing}; 要求 $expected)" >&2
    exit 1
  }
}

needed_libraries() {
  readelf -d "$1" | awk -F'[][]' '/NEEDED/ { print $2 }'
}

assert_no_absolute_rpath() {
  local path="$1"
  local rpath entry
  local -a rpath_entries
  rpath="$(readelf -d "$path" | awk -F'[][]' '/(RPATH|RUNPATH)/ { print $2; exit }')"
  [[ -z "$rpath" ]] && return 0
  IFS=: read -r -a rpath_entries <<<"$rpath"
  for entry in "${rpath_entries[@]}"; do
    [[ "$entry" == '$ORIGIN' ]] || {
      echo "ELF RPATH/RUNPATH 只能包含独立的 \$ORIGIN 项: $path ($rpath)" >&2
      exit 1
    }
  done
}

for library in "$bundle/libavcodec.so.62" "$bundle/libavutil.so.60" \
  "$bundle/libfreeremotedesk_ffmpeg.so"; do
  assert_elf "$library"
  assert_no_absolute_rpath "$library"
done
assert_soname "$bundle/libavcodec.so.62" 'libavcodec.so.62'
assert_soname "$bundle/libavutil.so.60" 'libavutil.so.60'

codec_needed="$(needed_libraries "$bundle/libavcodec.so.62")"
grep -Fxq 'libavutil.so.60' <<<"$codec_needed" || {
  echo 'libavcodec.so.62 必须依赖随包 libavutil.so.60' >&2
  exit 1
}
plugin_needed="$(needed_libraries "$bundle/libfreeremotedesk_ffmpeg.so")"
for required in libavcodec.so.62 libavutil.so.60; do
  grep -Fxq "$required" <<<"$plugin_needed" || {
    echo "FFmpeg 插件必须依赖随包 $required" >&2
    exit 1
  }
done

nm -D --defined-only "$bundle/libfreeremotedesk_ffmpeg.so" \
  | awk '$3 == "frd_ffmpeg_get_api_v1" { found = 1 } END { exit(found ? 0 : 1) }' || {
    echo 'FFmpeg 插件没有导出 frd_ffmpeg_get_api_v1' >&2
    exit 1
  }

if [[ "${FRD_LINUX_FFMPEG_RUNTIME_SMOKE:-0}" == 1 ]]; then
  host_arch="$(uname -m)"
  case "$host_arch" in
    x86_64) host_profile='linux-x86_64' ;;
    i686|x86) host_profile='linux-x86' ;;
    aarch64|arm64) host_profile='linux-aarch64' ;;
    *) echo "无法识别 runtime smoke 主机架构: $host_arch" >&2; exit 2 ;;
  esac
  [[ "$profile" == "$host_profile" ]] || {
    echo "runtime smoke 只能加载当前主机架构 bundle: $profile != $host_profile" >&2
    exit 2
  }
  command -v python3 >/dev/null || { echo 'runtime smoke 需要 python3' >&2; exit 2; }
  LD_LIBRARY_PATH="$bundle${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    python3 - "$bundle/libfreeremotedesk_ffmpeg.so" <<'PY'
import ctypes
import ctypes.util
import os
import sys

class Api(ctypes.Structure):
    _fields_ = [
        ("struct_size", ctypes.c_uint32),
        ("struct_alignment", ctypes.c_uint32),
        ("abi_version", ctypes.c_uint32),
        ("avcodec_major", ctypes.c_uint32),
        ("contract_flags", ctypes.c_uint32),
        ("codec_capabilities", ctypes.c_uint32),
        ("create_decoder", ctypes.c_size_t),
        ("submit", ctypes.c_size_t),
        ("receive", ctypes.c_size_t),
        ("flush", ctypes.c_size_t),
        ("destroy", ctypes.c_size_t),
        ("reclaim_frame", ctypes.c_size_t),
    ]

path = os.path.abspath(sys.argv[1])
plugin = ctypes.CDLL(path, mode=getattr(ctypes, "RTLD_LOCAL", 0))
get_api = plugin.frd_ffmpeg_get_api_v1
get_api.argtypes = [ctypes.POINTER(Api), ctypes.c_size_t]
get_api.restype = ctypes.c_int32
api = Api()
status = get_api(ctypes.byref(api), ctypes.sizeof(api))
if status != 0:
    raise SystemExit(f"FFmpeg 插件 ABI 入口返回 {status}")
if (api.struct_size, api.struct_alignment, api.abi_version, api.avcodec_major) != (
    ctypes.sizeof(Api), ctypes.alignment(Api), 1, 62
):
    raise SystemExit("FFmpeg 插件 ABI 头字段不匹配")
if api.contract_flags & 0xF != 0xF or api.codec_capabilities == 0:
    raise SystemExit("FFmpeg 插件 ABI contract/capability 不完整")
if not all((api.create_decoder, api.submit, api.receive, api.flush, api.destroy, api.reclaim_frame)):
    raise SystemExit("FFmpeg 插件 ABI 回调表含空指针")
PY
fi

printf 'Linux FFmpeg bundle verifier 通过: %s (%s)\n' "$bundle" "$profile"
