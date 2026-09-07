#!/usr/bin/env bash
# 运行不需要凭据或目标服务器的 RDP/EGFX 本地验证阶梯。
# 该脚本不会发起 RDP 连接，也不会读取 FRD_USERNAME/FRD_PASSWORD。真实
# AVC420、AVC444、HEVC 首帧/持续刷新/恢复门禁必须由授权的 live probe 单独
# 记录。设置 FRD_VERIFY_STRICT=1 后，缺少本机可执行的 target/package
# artifact 也会使脚本失败，避免把未执行的 gate 当成通过。
set -u -o pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

rust_toolchain="${FRD_RUST_TOOLCHAIN:-1.96.0}"
if [[ -n "${FRD_RUST_TOOLCHAIN_BIN:-}" ]]; then
    export PATH="$FRD_RUST_TOOLCHAIN_BIN:$PATH"
elif command -v rustup >/dev/null 2>&1; then
    cargo_path="$(rustup which cargo --toolchain "$rust_toolchain" 2>/dev/null || true)"
    if [[ -x "$cargo_path" ]]; then
        export PATH="$(dirname "$cargo_path"):$PATH"
    fi
fi

rustc_version="$(rustc --version 2>/dev/null | awk '{print $2}' || true)"
cargo_version="$(cargo --version 2>/dev/null | awk '{print $2}' || true)"
if [[ "$rustc_version" != "$rust_toolchain" || "$cargo_version" != "$rust_toolchain" ]]; then
    echo "需要 Rust/Cargo $rust_toolchain；当前 rustc=$rustc_version cargo=$cargo_version" >&2
    echo "可设置 FRD_RUST_TOOLCHAIN_BIN 指向固定 toolchain 的 bin 目录" >&2
    exit 2
fi

strict="${FRD_VERIFY_STRICT:-0}"
failures=0
unavailable=0

run_gate() {
    local label="$1"
    shift
    printf '\n[RUN] %s\n' "$label"
    if "$@"; then
        printf '[PASS] %s\n' "$label"
    else
        printf '[FAIL] %s\n' "$label" >&2
        failures=$((failures + 1))
    fi
}

optional_missing() {
    local label="$1"
    if [[ "$strict" == 1 ]]; then
        printf '[FAIL] %s（严格模式要求该 gate 已准备）\n' "$label" >&2
        failures=$((failures + 1))
    else
        printf '[SKIP] %s（本机没有可执行输入；未计为通过）\n' "$label"
        unavailable=$((unavailable + 1))
    fi
}

run_gate 'Rust 格式检查' cargo fmt --all -- --check

for script in tools/*.sh; do
    [[ "$script" == "$0" ]] && continue
    run_gate "Shell 语法：$script" bash -n "$script"
done

run_gate '协议、媒体、视频插件与桌面壳 focused tests' \
    cargo test --locked --quiet \
    -p frd-media-api \
    -p frd-video-ffmpeg \
    -p frd-video-ffmpeg-plugin \
    -p frd-protocol-rdp \
    -p frd-shell-desktop

run_gate '顶层 CLI help' cargo run --locked --quiet -- --help
run_gate 'hpssview CLI help' cargo run --locked --quiet -- hpssview --help

if [[ "${FRD_VERIFY_SKIP_WORKSPACE:-0}" == 1 ]]; then
    printf '\n[SKIP] workspace tests（FRD_VERIFY_SKIP_WORKSPACE=1）\n'
else
    run_gate 'workspace tests' cargo test --locked --workspace --quiet
fi

target_crates=(
    -p frd-core
    -p frd-frame
    -p frd-media-api
    -p frd-protocol-api
    -p frd-session
    -p frd-ui-model
    -p frd-video-ffmpeg
    -p frd-video-ffmpeg-plugin
)
target_specs=(
    x86_64-unknown-linux-gnu
    i686-unknown-linux-gnu
    aarch64-unknown-linux-gnu
    x86_64-pc-windows-msvc
    i686-pc-windows-msvc
    aarch64-pc-windows-msvc
)

if [[ "${FRD_VERIFY_SKIP_TARGETS:-0}" == 1 ]]; then
    printf '\n[SKIP] Windows/Linux target checks（FRD_VERIFY_SKIP_TARGETS=1）\n'
else
    for target in "${target_specs[@]}"; do
        if rustc --print target-libdir --target "$target" >/dev/null 2>&1; then
            run_gate "协议中立 core/video/plugin target check：$target" \
                cargo check --locked --target "$target" "${target_crates[@]}"
        else
            optional_missing "协议中立 target check：$target"
        fi
    done
fi

if [[ "${FRD_VERIFY_SKIP_PACKAGE:-0}" == 1 ]]; then
    printf '\n[SKIP] package verifier（FRD_VERIFY_SKIP_PACKAGE=1）\n'
elif [[ "$(uname -s)" == Darwin ]]; then
    mac_target_dir="${CARGO_TARGET_DIR:-$repo_root/target}"
    if [[ "$mac_target_dir" != /* ]]; then
        mac_target_dir="$repo_root/$mac_target_dir"
    fi
    mac_app="${FRD_MACOS_APP:-$mac_target_dir/macos/release/FreeRemoteDesk.app}"
    if [[ -d "$mac_app" && -x tools/verify-macos-package.sh ]]; then
        mac_arch="${FRD_MACOS_ARCH:-$(uname -m)}"
        run_gate "macOS package verifier：$mac_arch" env FRD_MACOS_ARCH="$mac_arch" \
            bash tools/verify-macos-package.sh "$mac_app"
    else
        optional_missing "macOS package verifier：$mac_app"
    fi
elif [[ "${FRD_WINDOWS_PACKAGE_ROOT:-}" != '' && -f tools/verify-windows-package.ps1 ]] \
    && command -v pwsh >/dev/null 2>&1; then
    run_gate 'Windows package verifier' pwsh -NoProfile -File tools/verify-windows-package.ps1 \
        -PackageRoot "$FRD_WINDOWS_PACKAGE_ROOT"
else
    optional_missing '当前平台 package verifier'
fi

if [[ "${FRD_VERIFY_NATIVE_FFMPEG_FIXTURES:-0}" == 1 ]]; then
    # 可用 FRD_FFMPEG_TEST_TARGET 指定交叉 target；macOS x86_64 可在 Rosetta
    # 下设置 x86_64-apple-darwin，仍然只把实际执行结果记为 native fixture 证据。
    native_bundle="${FRD_FFMPEG_TEST_BUNDLE:-}"
    native_dist="${FFMPEG_DIR:-}"
    if [[ -z "$native_bundle" || -z "$native_dist" || ! -d "$native_bundle" || ! -d "$native_dist" ]]; then
        optional_missing 'native FFmpeg HEVC/AVC420/AVC444 fixtures（需要 FRD_FFMPEG_TEST_BUNDLE 与 FFMPEG_DIR）'
    else
        case "$(uname -s)" in
            Darwin) loader_var=DYLD_LIBRARY_PATH ;;
            Linux) loader_var=LD_LIBRARY_PATH ;;
            MINGW*|MSYS*|CYGWIN*) loader_var=PATH ;;
            *) loader_var=LD_LIBRARY_PATH ;;
        esac
        old_loader="${!loader_var:-}"
        if [[ -n "$old_loader" ]]; then
            loader_value="$native_bundle:$old_loader"
        else
            loader_value="$native_bundle"
        fi
        native_target_args=()
        if [[ -n "${FRD_FFMPEG_TEST_TARGET:-}" ]]; then
            native_target_args=(--target "$FRD_FFMPEG_TEST_TARGET")
        fi
        run_gate 'native FFmpeg HEVC/AVC420/AVC444 fixtures' env \
            FFMPEG_DIR="$native_dist" \
            FRD_FFMPEG_TEST_BUNDLE="$native_bundle" \
            "$loader_var=$loader_value" \
            cargo test --locked "${native_target_args[@]}" \
            -p frd-video-ffmpeg-plugin --features native-ffmpeg \
            --test main444_decode -- --nocapture
    fi
else
    if [[ "$strict" == 1 ]]; then
        optional_missing 'native FFmpeg HEVC/AVC420/AVC444 fixtures（设置 FRD_VERIFY_NATIVE_FFMPEG_FIXTURES=1 才执行）'
    else
        printf '\n[SKIP] native FFmpeg fixtures（设置 FRD_VERIFY_NATIVE_FFMPEG_FIXTURES=1 才执行）\n'
    fi
fi

cat <<'EOF'

[INFO] 本阶梯不执行 live RDP，也不读取凭据。
[INFO] AVC420/AVC444/HEVC 的服务器协商、首帧、持续刷新和恢复必须由授权目标单独验证。
EOF

if (( failures > 0 )); then
    printf '[SUMMARY] %d 个本地 gate 失败，%d 个输入不可用\n' "$failures" "$unavailable" >&2
    exit 1
fi
if (( unavailable > 0 )); then
    printf '[SUMMARY] 本地可用 gate 通过；%d 个 gate 未执行（严格模式可使其失败）\n' "$unavailable"
else
    printf '[SUMMARY] 本地验证阶梯通过；live gate 仍需独立证据\n'
fi
