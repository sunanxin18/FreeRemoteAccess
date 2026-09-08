#!/bin/bash
# 仅在新建的独立 Xvfb 中操作探针；不得向调用者现有 DISPLAY 注入输入。
set -euo pipefail
[[ "$(uname -s)" == Linux ]] || { echo 'X11 输入探针需要 Linux' >&2; exit 2; }
probe="${1:?需要 probe 可执行文件绝对路径}"
report="${2:?需要 JSONL 报告路径}"
scale="${3:?需要 scale 1 或 2}"
[[ "$probe" == /* && -f "$probe" && -x "$probe" ]] || { echo 'probe 必须是可执行文件绝对路径' >&2; exit 2; }
[[ "$scale" == 1 || "$scale" == 2 ]] || { echo '仅验证 scale 1 和 2' >&2; exit 2; }
for tool in xvfb-run Xvfb xdotool timeout dbus-run-session python3; do
  command -v "$tool" >/dev/null || { echo "缺少 $tool" >&2; exit 2; }
done
script_dir="$(cd "$(dirname "$0")" && pwd)"
[[ "$report" == /* ]] || report="$PWD/$report"
mkdir -p "$(dirname "$report")"
[[ ! -e "$report" && ! -L "$report" ]] || { echo '报告路径已存在；请为本次运行选择新路径' >&2; exit 2; }
private_runtime="$(mktemp -d)"
chmod 700 "$private_runtime"
trap 'python3 -c "import shutil,sys; shutil.rmtree(sys.argv[1])" "$private_runtime"' EXIT
# timeout 约束整个隔离 display 会话，xvfb-run 自己拥有并清理所创建的 X server。
env -u DISPLAY -u WAYLAND_DISPLAY GDK_BACKEND=x11 GDK_SCALE="$scale" \
  XDG_RUNTIME_DIR="$private_runtime" \
  timeout --signal=TERM --kill-after=5 30 \
  xvfb-run -a -e "$report.xvfb.log" -s '-screen 0 2400x1800x24 -nolisten tcp' \
  dbus-run-session -- bash -s -- "$probe" "$report" <<'ISOLATED_X11'
set -euo pipefail
probe="$1"
report="$2"
probe_pid=""
cleanup_probe() {
  if [[ -n "$probe_pid" ]]; then
    kill "$probe_pid" 2>/dev/null || true
    wait "$probe_pid" 2>/dev/null || true
  fi
}
trap cleanup_probe EXIT
"$probe" --expect-backend x11 --seconds 10 > "$report" 2> "$report.stderr.log" &
probe_pid=$!
window_id=""
for attempt in {1..60}; do
  kill -0 "$probe_pid" 2>/dev/null || { echo '探针在输入前提前结束' >&2; exit 1; }
  # --all 要求 PID、名字和可见状态同时匹配，不能依赖当前活动窗口。
  matches="$(xdotool search --onlyvisible --all --pid "$probe_pid" --name '^FreeRemoteDesk Linux 窗口技术验证$' 2>/dev/null || true)"
  if [[ "$matches" =~ ^[0-9]+$ ]]; then
    window_id="$matches"
    break
  fi
  [[ -z "$matches" ]] || { echo '探针窗口匹配不唯一' >&2; exit 1; }
  sleep 0.1
done
[[ -n "$window_id" ]] || { echo '找不到本次探针原生窗口' >&2; exit 1; }
[[ "$(xdotool getwindowpid "$window_id")" == "$probe_pid" ]] || { echo '窗口 PID 不匹配' >&2; exit 1; }
geometry="$(xdotool getwindowgeometry --shell "$window_id")"
width="$(awk -F= '$1 == "WIDTH" {print $2}' <<< "$geometry")"
height="$(awk -F= '$1 == "HEIGHT" {print $2}' <<< "$geometry")"
[[ "$width" =~ ^[0-9]+$ && "$height" =~ ^[0-9]+$ ]] || { echo '窗口几何无效' >&2; exit 1; }
(( width >= 200 && height >= 200 )) || { echo '探针内容区域过小' >&2; exit 1; }
# Xvfb 没有窗口管理器，使用 XSetInputFocus；不依赖 _NET_ACTIVE_WINDOW。
timeout 2 xdotool windowfocus --sync "$window_id"
[[ "$(xdotool getwindowfocus)" == "$window_id" ]] || { echo '实际 X11 焦点不匹配' >&2; exit 1; }
xdotool mousemove --sync --window "$window_id" "$((width / 2))" "$((height * 3 / 4))"
xdotool click --clearmodifiers 1
# 不给 key 指定 --window：使用实际焦点上的 XTEST 事件，不用 SendEvent 绕过焦点。
xdotool key --clearmodifiers F8
wait "$probe_pid"
probe_pid=""
ISOLATED_X11
python3 "$script_dir/verify_report.py" "$report" --expect-backend x11 --expect-scale "$scale" --require-input
