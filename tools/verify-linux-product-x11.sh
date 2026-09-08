#!/bin/bash
# 在隔离 Xvfb 中启动正式 Linux GTK 产品入口；只验证窗口、焦点、XTEST 输入
# 和干净关闭，不连接远程主机，也不把这个 smoke 当成 RDP 互操作证明。
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'Linux GTK 产品 smoke 需要 Linux 宿主' >&2; exit 2; }
binary="${1:?需要 Linux 客户端 ELF 绝对路径}"
[[ "$binary" == /* && -f "$binary" && -x "$binary" ]] || {
  echo '客户端路径必须是绝对可执行文件' >&2
  exit 2
}
for tool in xvfb-run xdotool xprop timeout dbus-run-session; do
  command -v "$tool" >/dev/null || { echo "缺少 $tool" >&2; exit 2; }
done

env -u DISPLAY -u WAYLAND_DISPLAY -u WAYLAND_SOCKET \
  GDK_BACKEND=x11 GSK_RENDERER=gl LIBGL_ALWAYS_SOFTWARE=1 \
  timeout --signal=TERM --kill-after=5s 45s \
  xvfb-run -a -s '-screen 0 2400x1800x24 -nolisten tcp' \
  dbus-run-session -- bash -s -- "$binary" <<'ISOLATED_X11'
set -euo pipefail
binary="$1"
log_dir="${TMPDIR:-/tmp}/freeremotedesk-linux-product-smoke"
mkdir -p "$log_dir"
stdout_log="$log_dir/stdout.log"
stderr_log="$log_dir/stderr.log"
"$binary" >"$stdout_log" 2>"$stderr_log" &
pid=$!
cleanup() {
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT
window_id=""
for attempt in {1..100}; do
  kill -0 "$pid" 2>/dev/null || {
    echo 'GTK 产品在窗口映射前退出' >&2
    cat "$stderr_log" >&2 || true
    exit 1
  }
  matches="$(xdotool search --onlyvisible --all --pid "$pid" --name '^FreeRemoteDesk$' 2>/dev/null || true)"
  if [[ "$matches" =~ ^[0-9]+$ ]]; then
    window_id="$matches"
    break
  fi
  [[ -z "$matches" ]] || { echo 'GTK 产品窗口匹配不唯一' >&2; exit 1; }
  sleep 0.1
done
[[ -n "$window_id" ]] || { echo 'GTK 产品窗口未在预期时间映射' >&2; exit 1; }
[[ "$(xdotool getwindowpid "$window_id")" == "$pid" ]] || {
  echo 'X11 窗口 PID 与产品进程不一致' >&2
  exit 1
}
geometry="$(xdotool getwindowgeometry --shell "$window_id")"
width="$(awk -F= '$1 == "WIDTH" {print $2}' <<<"$geometry")"
height="$(awk -F= '$1 == "HEIGHT" {print $2}' <<<"$geometry")"
[[ "$width" =~ ^[0-9]+$ && "$height" =~ ^[0-9]+$ ]] || { echo '窗口几何无效' >&2; exit 1; }
(( width >= 400 && height >= 400 )) || { echo 'GTK 产品窗口尺寸异常' >&2; exit 1; }
wm_class="$(xprop -id "$window_id" WM_CLASS)"
grep -Eq '"freeremotedesk", "freeremotedesk"|"freeremotedesk"' <<<"$wm_class" || {
  echo "GTK application id 未映射为预期 WM_CLASS: $wm_class" >&2
  exit 1
}
timeout 2 xdotool windowfocus --sync "$window_id"
[[ "$(xdotool getwindowfocus)" == "$window_id" ]] || { echo 'GTK 产品未取得 X11 焦点' >&2; exit 1; }
# 这些是实际 XTEST 事件，验证正式产品窗口能接收键盘和鼠标事件；
# 表单为空时 Connect 只显示本地校验错误，不会启动网络会话。
xdotool key --clearmodifiers Tab
xdotool key --clearmodifiers F8
xdotool mousemove --sync --window "$window_id" "$((width / 2))" "$((height * 3 / 4))"
xdotool click --clearmodifiers 1
xdotool key --clearmodifiers alt+F4
wait "$pid"
status=$?
[[ "$status" == 0 ]] || { echo "GTK 产品关闭状态异常: $status" >&2; cat "$stderr_log" >&2 || true; exit 1; }
printf 'linux_product_x11 window_mapped=1 focus=1 xtest_keyboard=1 xtest_pointer=1 wm_class=freeremotedesk clean_exit=1\n'
ISOLATED_X11
