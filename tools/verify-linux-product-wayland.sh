#!/bin/bash
# 在调用方提供的私有 Weston/Wayland compositor 中启动正式 GTK 产品入口。
# 只验证 app_id、窗口启动和 close-request 清理；Wayland 不允许本脚本注入
# 全局物理键鼠事件，因此不把该 smoke 当成远程输入证明。
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'Wayland 产品 smoke 需要 Linux 宿主' >&2; exit 2; }
binary="${1:?需要 Linux 客户端 ELF 绝对路径}"
[[ "$binary" == /* && -f "$binary" && -x "$binary" ]] || {
  echo '客户端路径必须是绝对可执行文件' >&2
  exit 2
}
[[ -n "${WAYLAND_DISPLAY:-}" && -n "${XDG_RUNTIME_DIR:-}" ]] || {
  echo '需要调用方提供 WAYLAND_DISPLAY 和 XDG_RUNTIME_DIR' >&2
  exit 2
}
for tool in timeout dbus-run-session grep; do
  command -v "$tool" >/dev/null || { echo "缺少 $tool" >&2; exit 2; }
done

log_dir="${FRD_LINUX_PRODUCT_SMOKE_LOG_DIR:-${TMPDIR:-/tmp}/freeremotedesk-linux-wayland-smoke}"
mkdir -p "$log_dir"
debug_log="$log_dir/wayland-debug.log"
stdout_log="$log_dir/stdout.log"
env -u DISPLAY -u WAYLAND_SOCKET GDK_BACKEND=wayland \
  FRD_LINUX_PRODUCT_SMOKE_MILLIS=1000 WAYLAND_DEBUG=1 \
  timeout --signal=TERM --kill-after=5s 20s \
  dbus-run-session -- "$binary" >"$stdout_log" 2>"$debug_log"
grep -Fq 'set_app_id("com.sunanxin18.freeremotedesk")' "$debug_log" || {
  echo 'Wayland xdg_toplevel app_id 与产品身份不一致' >&2
  cat "$debug_log" >&2
  exit 1
}
grep -Fq 'set_title("FreeRemoteDesk")' "$debug_log" || {
  echo 'Wayland 产品窗口标题未提交' >&2
  cat "$debug_log" >&2
  exit 1
}
printf 'linux_product_wayland app_id=com.sunanxin18.freeremotedesk window_started=1 close_request=1 clean_exit=1 physical_input=unverified\n'
