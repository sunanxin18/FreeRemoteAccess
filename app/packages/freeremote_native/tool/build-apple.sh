#!/usr/bin/env bash
set -euo pipefail

platform="${1:?platform is required}"
configuration="${2:?configuration is required}"
architectures="${3:?architectures are required}"
output_library="${4:?output library is required}"
script_directory="$(cd -P "$(dirname "$0")" && pwd -P)"
repository_root="$(cd "$script_directory/../../../.." && pwd -P)"
target_dir="${DERIVED_FILE_DIR:-${TMPDIR:-/tmp}/freeremote-native}/cargo"
profile="release"

if [[ "$configuration" == "Debug" ]]; then
  profile="debug"
fi

libraries=()
for architecture in $architectures; do
  case "$platform:$architecture:${PLATFORM_NAME:-}" in
    ios:arm64:iphoneos*) rust_target="aarch64-apple-ios" ;;
    ios:arm64:iphonesimulator*) rust_target="aarch64-apple-ios-sim" ;;
    ios:x86_64:iphonesimulator*) rust_target="x86_64-apple-ios" ;;
    macos:arm64:*) rust_target="aarch64-apple-darwin" ;;
    macos:x86_64:*) rust_target="x86_64-apple-darwin" ;;
    *)
      echo "Unsupported Apple build target: $platform/$architecture/${PLATFORM_NAME:-unknown}" >&2
      exit 2
      ;;
  esac

  cargo_args=(build -p freeremote_ffi --target "$rust_target" --target-dir "$target_dir")
  if [[ "$profile" == "release" ]]; then
    cargo_args+=(--release)
  fi
  (cd "$repository_root" && cargo "${cargo_args[@]}")
  libraries+=("$target_dir/$rust_target/$profile/libfreeremote_native.a")
done

mkdir -p "$(dirname "$output_library")"
lipo -create "${libraries[@]}" -output "$output_library"
