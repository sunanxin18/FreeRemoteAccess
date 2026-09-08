#!/usr/bin/env bash
set -euo pipefail
# 只在临时 XDG 数据与独立 D-Bus 中运行合成凭据；不接触用户现有 keyring。
[[ "$(uname -s)" == Linux ]] || { echo '此验证需要 Linux 原生运行环境' >&2; exit 2; }
for tool in dbus-run-session gnome-keyring-daemon gdbus cargo; do
    command -v "$tool" >/dev/null || { echo "缺少测试依赖：$tool" >&2; exit 2; }
done
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
frd_keyring_test_root="$(mktemp -d)"
trap 'rm -rf "$frd_keyring_test_root"' EXIT
chmod 700 "$frd_keyring_test_root"
export XDG_DATA_HOME="$frd_keyring_test_root/data"
export XDG_CONFIG_HOME="$frd_keyring_test_root/config"
export XDG_CACHE_HOME="$frd_keyring_test_root/cache"
export XDG_RUNTIME_DIR="$frd_keyring_test_root/runtime"
mkdir -m 700 "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_RUNTIME_DIR"
export FRD_LINUX_KEYRING_TEST=1
# 非秘密固定测试口令仅从 stdin 进入临时 daemon；不经环境或 argv。
dbus-run-session -- bash -euo pipefail <<'PRIVATE_BUS'
printf '%s' 'frd-isolated-keyring-fixture-v1' |
    gnome-keyring-daemon --foreground --unlock --components=secrets > "$XDG_RUNTIME_DIR/daemon.log" 2>&1 &
frd_keyring_daemon_pid=$!
trap 'kill "$frd_keyring_daemon_pid" 2>/dev/null || true; wait "$frd_keyring_daemon_pid" 2>/dev/null || true' EXIT
gdbus wait --session --timeout 10 org.freedesktop.secrets
cargo test --locked -p frd-platform-linux --lib secure_credentials::native_tests::native_secret_service_authenticated_commit_roundtrip -- --ignored --exact |
    tee "$XDG_RUNTIME_DIR/test-result.log"
# --exact 拼写或 cfg 改动导致零测试时必须失败，不能形成空通过。
grep -F 'test result: ok. 1 passed; 0 failed; 0 ignored;' "$XDG_RUNTIME_DIR/test-result.log" >/dev/null
PRIVATE_BUS
