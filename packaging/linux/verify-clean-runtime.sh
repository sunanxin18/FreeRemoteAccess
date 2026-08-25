#!/usr/bin/env bash
set -euo pipefail

if [[ $# -gt 2 ]]; then
  echo 'usage: verify-clean-runtime.sh [INSTALLED_GUI] [WORK_DIR]' >&2
  exit 2
fi

binary="${1:-/usr/bin/freeremoteaccess}"
owned_work_dir=0
if [[ $# -eq 2 ]]; then
  work_dir="$2"
  mkdir -p "$work_dir"
else
  work_dir="$(mktemp -d "${TMPDIR:-/tmp}/frd-linux-clean-runtime.XXXXXX")"
  owned_work_dir=1
fi
weston_pid=''
cleanup() {
  if [[ -n "$weston_pid" ]]; then
    kill "$weston_pid" 2>/dev/null || true
    wait "$weston_pid" 2>/dev/null || true
  fi
  if [[ "$owned_work_dir" == 1 ]]; then rm -rf -- "$work_dir"; fi
}
trap cleanup EXIT

[[ -x "$binary" ]] || { echo 'clean_runtime_binary_missing' >&2; exit 1; }
ldd "$binary" >"$work_dir/ldd.txt"
grep -F 'not found' "$work_dir/ldd.txt" >/dev/null && {
  cat "$work_dir/ldd.txt" >&2
  echo 'clean_runtime_dependency_missing' >&2
  exit 1
}

set +e
timeout --signal=TERM 5s xvfb-run -a env -u WAYLAND_DISPLAY \
  LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=vulkan \
  "$binary" >"$work_dir/x11-gui.log" 2>&1
x11_status=$?
set -e
[[ "$x11_status" == 124 ]] || {
  cat "$work_dir/x11-gui.log" >&2
  echo "clean_runtime_x11_survival_failed:$x11_status" >&2
  exit 1
}

xdg_runtime="$work_dir/xdg-runtime"
mkdir -p "$xdg_runtime"
chmod 700 "$xdg_runtime"
XDG_RUNTIME_DIR="$xdg_runtime" weston --backend=headless-backend.so \
  --use-pixman --socket=wayland-frd --idle-time=0 \
  >"$work_dir/weston.log" 2>&1 &
weston_pid=$!
for _ in {1..40}; do
  [[ -S "$xdg_runtime/wayland-frd" ]] && break
  kill -0 "$weston_pid" 2>/dev/null || {
    cat "$work_dir/weston.log" >&2
    echo 'clean_runtime_weston_failed' >&2
    exit 1
  }
  sleep 0.25
done
[[ -S "$xdg_runtime/wayland-frd" ]] || { echo 'clean_runtime_wayland_socket_timeout' >&2; exit 1; }

set +e
timeout --signal=TERM 5s env -u DISPLAY \
  XDG_RUNTIME_DIR="$xdg_runtime" WAYLAND_DISPLAY=wayland-frd \
  LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=vulkan \
  "$binary" >"$work_dir/wayland-gui.log" 2>&1
wayland_status=$?
set -e
[[ "$wayland_status" == 124 ]] || {
  cat "$work_dir/wayland-gui.log" >&2
  echo "clean_runtime_wayland_survival_failed:$wayland_status" >&2
  exit 1
}

echo 'linux-clean-runtime-verification: ok'
