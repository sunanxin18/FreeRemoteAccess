#!/bin/bash
# 独立 native GTK/GL 技术验证，不进入产品 workspace 或安装路径。
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo '此探针仅在 Linux 原生构建' >&2; exit 2; }
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
compiler="${CC:-cc}"
pkg_config="${PKG_CONFIG:-pkg-config}"
"$pkg_config" --atleast-version=4.6 gtk4
"$pkg_config" --exists epoxy
output_dir="${FRD_LINUX_NATIVE_PROBE_OUTPUT_DIR:-$repo_root/target/linux-native-shell-probe}"
mkdir -p "$output_dir"
read -r -a flags <<< "$("$pkg_config" --cflags --libs gtk4 epoxy)"
"$compiler" -std=c11 -O2 -Wall -Wextra -Werror -Wno-deprecated-declarations \
  "$repo_root/tools/frd-linux-native-shell-probe/main.c" "${flags[@]}" -lm \
  -o "$output_dir/frd-linux-native-shell-probe"
printf '%s\n' "$output_dir/frd-linux-native-shell-probe"
