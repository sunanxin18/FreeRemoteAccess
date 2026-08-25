#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then echo 'usage: verify-package.sh DIST_DIR' >&2; exit 2; fi
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$(cd "$1" && pwd)"
manifest="$dist_dir/artifact-manifest.json"
manifest_tool="$repo_root/packaging/package_manifest.py"
python3 "$manifest_tool" --repo "$repo_root" verify --manifest "$manifest"
version="$(python3 "$manifest_tool" --repo "$repo_root" version)"
prefix="FreeRemoteAccess-$version-linux-x86_64"
appdir="$dist_dir/AppDir"
appdir_archive="$dist_dir/$prefix-AppDir.tar.zst"
deb="$dist_dir/$prefix.deb"
appimage="$dist_dir/$prefix.AppImage"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/frd-linux-package.XXXXXX")"
trap 'rm -rf -- "$work_dir"' EXIT

fdk_package="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fdk_aac"]["package"])' "$manifest")"
fdk_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fdk_aac"]["version"])' "$manifest")"
fdk_checksum="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fdk_aac"]["crate_sha256"])' "$manifest")"

assert_support() {
  local root="$1"
  local support="$root/THIRD_PARTY/$fdk_package-$fdk_version"
  local notice="$support/aac/NOTICE"
  local source="$support/source/$fdk_package-$fdk_version.crate"
  [[ -f "$notice" && -f "$source" ]] || { echo 'package_fdk_support_missing' >&2; return 1; }
  [[ "$(sha256sum "$source" | awk '{print tolower($1)}')" == "$fdk_checksum" ]] || {
    echo 'package_fdk_source_hash_mismatch' >&2; return 1;
  }
  cmp -s "$notice" "$dist_dir/THIRD_PARTY/$fdk_package-$fdk_version/aac/NOTICE" || {
    echo 'package_fdk_notice_mismatch' >&2; return 1;
  }
}

assert_appdir() {
  local root="$1"
  [[ -x "$root/usr/bin/freeremoteaccess" ]] || { echo 'appdir_binary_missing' >&2; return 1; }
  [[ -L "$root/AppRun" && "$(readlink "$root/AppRun")" == 'usr/bin/freeremoteaccess' ]] || {
    echo 'appdir_apprun_invalid' >&2; return 1;
  }
  cmp -s "$root/usr/share/doc/freeremoteaccess/runtime-libraries.txt" \
    "$repo_root/packaging/linux/runtime-libraries.txt" || { echo 'runtime_library_declaration_mismatch' >&2; return 1; }
  assert_support "$root/usr/share/doc/freeremoteaccess"
}

assert_appdir "$appdir"
mkdir "$work_dir/appdir-archive"
tar --zstd -xf "$appdir_archive" -C "$work_dir/appdir-archive"
assert_appdir "$work_dir/appdir-archive/AppDir"

mkdir "$work_dir/deb"
dpkg-deb -x "$deb" "$work_dir/deb"
[[ -x "$work_dir/deb/usr/bin/freeremoteaccess" ]] || { echo 'deb_binary_missing' >&2; exit 1; }
assert_support "$work_dir/deb/usr/share/doc/freeremoteaccess"

mkdir "$work_dir/appimage"
(cd "$work_dir/appimage" && "$appimage" --appimage-extract >/dev/null)
assert_appdir "$work_dir/appimage/squashfs-root"

binary="$appdir/usr/bin/freeremoteaccess"
ldconfig_output="$(ldconfig -p)"
while IFS= read -r soname; do
  [[ -z "$soname" || "$soname" == \#* ]] && continue
  grep -F "$soname" <<<"$ldconfig_output" >/dev/null || { echo "runtime_library_missing:$soname" >&2; exit 1; }
  strings "$binary" | grep -Fi "${soname%%.so*}" >/dev/null || {
    echo "runtime_library_not_declared_by_binary:$soname" >&2; exit 1;
  }
done < "$repo_root/packaging/linux/runtime-libraries.txt"
ldd "$binary" | tee "$work_dir/ldd.txt"
! grep -q 'not found' "$work_dir/ldd.txt" || { echo 'elf_dependency_missing' >&2; exit 1; }
max_glibc="$(objdump -T "$binary" | sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' | sort -V | tail -n1)"
[[ -n "$max_glibc" ]] || { echo 'glibc_abi_not_found' >&2; exit 1; }
[[ "$(printf '%s\n%s\n' "$max_glibc" '2.35' | sort -V | tail -n1)" == '2.35' ]] || {
  echo "glibc_abi_too_new:$max_glibc" >&2; exit 1;
}

set +e
timeout --signal=TERM 5s xvfb-run -a env \
  LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=vulkan \
  "$appdir/AppRun" >"$work_dir/x11-gui.log" 2>&1
launch_status=$?
set -e
[[ "$launch_status" == 124 ]] || {
  cat "$work_dir/x11-gui.log" >&2
  echo "linux_gui_survival_failed:$launch_status" >&2
  exit 1
}
echo 'linux-package-verification: ok'
