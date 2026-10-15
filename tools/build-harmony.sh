#!/usr/bin/env bash
# 配置官方 OHOS 工具链，编译共享 Rust 核心；不代表 HAP 或互操作验收。
set -euo pipefail

usage() {
    cat <<'USAGE'
用法：tools/build-harmony.sh [--sdk SDK根目录] [--check|--build] [--release] [-p crate]... [--normal-hap-rdp-validation]
默认检查 frd-core/frd-frame/frd-app/frd-shell-runtime 的 aarch64-unknown-linux-ohos。
SDK 根目录可由 OHOS_SDK_ROOT、DEVECO_HOME 或已安装的 DevEco Studio 提供。
--normal-hap-rdp-validation：仅普通 OHOS ARM64 应用的 RDP 有界验证，默认关闭；未指定 -p 时选择 frd-shell-ohos-ffi，不代表互操作通过。
可用 CARGO、RUSTC、PATH 选择安装了 OHOS 标准库的 Rust 工具链。
USAGE
}

sdk_root="${OHOS_SDK_ROOT:-}"
action=check
release=0
rdp_validation=0
package_args=()
while (($#)); do
    case "$1" in
        --sdk) sdk_root="${2:?--sdk 需要 SDK 根目录}"; shift 2 ;;
        --check) action=check; shift ;;
        --build) action=build; shift ;;
        --release) release=1; shift ;;
        --normal-hap-rdp-validation) rdp_validation=1; shift ;;
        -p|--package) package_args+=(-p "${2:?需要 crate 名称}"); shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) printf '未知参数：%s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ -z "$sdk_root" ]]; then
    sdk_candidates=()
    if [[ -n "${DEVECO_HOME:-}" ]]; then
        sdk_candidates+=("$DEVECO_HOME/sdk/default/openharmony" "$DEVECO_HOME/Contents/sdk/default/openharmony")
    fi
    sdk_candidates+=("/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony")
    for candidate in "${sdk_candidates[@]}"; do
        if [[ -d "$candidate/native/sysroot" ]]; then
            sdk_root="$candidate"
            break
        fi
    done
fi
if [[ -z "$sdk_root" || ! -d "$sdk_root/native/sysroot" ]]; then
    printf '未找到官方 OHOS SDK；请设置 OHOS_SDK_ROOT 或 --sdk。\n' >&2
    exit 2
fi
sdk_root="$(cd "$sdk_root" && pwd -P)"
native_root="$sdk_root/native"
compiler="$native_root/llvm/bin/aarch64-unknown-linux-ohos-clang"
cpp_compiler="$native_root/llvm/bin/aarch64-unknown-linux-ohos-clang++"
archiver="$native_root/llvm/bin/llvm-ar"
cmake_bin="$native_root/build-tools/cmake/bin"
for required in "$compiler" "$cpp_compiler" "$archiver" "$cmake_bin/cmake"; do
    if [[ ! -x "$required" ]]; then
        printf 'SDK 缺少工具：%s\n' "$required" >&2
        exit 2
    fi
done

export OHOS_SDK_ROOT="$sdk_root"
export OHOS_NDK_HOME="$native_root"
export PATH="$native_root/llvm/bin:$cmake_bin:$PATH"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER="$compiler"
export CC_aarch64_unknown_linux_ohos="$compiler"
export CXX_aarch64_unknown_linux_ohos="$cpp_compiler"
export AR_aarch64_unknown_linux_ohos="$archiver"
export CFLAGS_aarch64_unknown_linux_ohos="--sysroot=$native_root/sysroot ${CFLAGS_aarch64_unknown_linux_ohos:-}"
export CXXFLAGS_aarch64_unknown_linux_ohos="--sysroot=$native_root/sysroot ${CXXFLAGS_aarch64_unknown_linux_ohos:-}"
export CMAKE_TOOLCHAIN_FILE_aarch64_unknown_linux_ohos="$native_root/build/cmake/ohos.toolchain.cmake"
if ((${#package_args[@]} == 0)); then
    if ((rdp_validation)); then
        package_args=(-p frd-shell-ohos-ffi)
    else
        package_args=(-p frd-core -p frd-frame -p frd-app -p frd-shell-runtime)
    fi
fi
repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$repository_root"
cargo_args=("$action" --locked --target aarch64-unknown-linux-ohos)
if ((release)); then
    cargo_args+=(--release)
fi
if ((rdp_validation)); then
    cargo_args+=(--features frd-shell-ohos-ffi/normal-hap-rdp-validation)
fi
"${CARGO:-cargo}" "${cargo_args[@]}" "${package_args[@]}"
