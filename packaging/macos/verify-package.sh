#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then echo 'usage: verify-package.sh DIST_DIR' >&2; exit 2; fi
repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
dist_dir="$(cd "$1" && pwd)"
manifest="$dist_dir/artifact-manifest.json"
manifest_tool="$repo_root/packaging/package_manifest.py"
python3 "$manifest_tool" --repo "$repo_root" verify --manifest "$manifest"
version="$(python3 "$manifest_tool" --repo "$repo_root" version)"
prefix="FreeRemoteAccess-$version-macos-universal"
archive="$dist_dir/$prefix-app.zip"
dmg="$dist_dir/$prefix.dmg"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/frd-macos-package.XXXXXX")"
mount_dir="$work_dir/mount"
mounted=0
gui_pid=""
cleanup() {
  if [[ -n "$gui_pid" ]]; then kill "$gui_pid" 2>/dev/null || true; wait "$gui_pid" 2>/dev/null || true; fi
  if [[ "$mounted" == 1 ]]; then hdiutil detach "$mount_dir" -quiet || true; fi
  rm -rf -- "$work_dir"
}
trap cleanup EXIT

fdk_package="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fdk_aac"]["package"])' "$manifest")"
fdk_version="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fdk_aac"]["version"])' "$manifest")"
fdk_checksum="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["fdk_aac"]["crate_sha256"])' "$manifest")"

assert_support() {
  local root="$1"
  local support="$root/THIRD_PARTY/$fdk_package-$fdk_version"
  local notice="$support/aac/NOTICE"
  local source="$support/source/$fdk_package-$fdk_version.crate"
  [[ -f "$notice" && -f "$source" ]] || { echo 'package_fdk_support_missing' >&2; return 1; }
  [[ "$(shasum -a 256 "$source" | awk '{print tolower($1)}')" == "$fdk_checksum" ]] || {
    echo 'package_fdk_source_hash_mismatch' >&2; return 1;
  }
  cmp -s "$notice" "$dist_dir/THIRD_PARTY/$fdk_package-$fdk_version/aac/NOTICE" || {
    echo 'package_fdk_notice_mismatch' >&2; return 1;
  }
}

assert_app() {
  local app="$1"
  local plist="$app/Contents/Info.plist"
  local binary="$app/Contents/MacOS/FreeRemoteAccess"
  [[ -x "$binary" ]] || { echo 'macos_bundle_executable_missing' >&2; return 1; }
  [[ "$(plutil -extract CFBundleShortVersionString raw "$plist")" == "$version" ]] || {
    echo 'macos_bundle_version_mismatch' >&2; return 1;
  }
  [[ "$(plutil -extract CFBundleVersion raw "$plist")" == "$version" ]] || {
    echo 'macos_bundle_build_version_mismatch' >&2; return 1;
  }
  local archs
  archs="$(lipo -archs "$binary")"
  [[ " $archs " == *' arm64 '* && " $archs " == *' x86_64 '* ]] || {
    echo 'macos_bundle_architecture_mismatch' >&2; return 1;
  }
  [[ "$(wc -w <<<"$archs" | tr -d ' ')" == 2 ]] || { echo 'macos_bundle_architecture_extra' >&2; return 1; }
  file "$binary" | grep -q 'Mach-O universal binary' || { echo 'macos_bundle_macho_invalid' >&2; return 1; }
  for arch in arm64 x86_64; do
    local minos
    minos="$(otool -arch "$arch" -l "$binary" | awk '$1 == "minos" { print $2; exit }')"
    [[ -n "$minos" ]] || { echo 'macos_lc_build_version_missing' >&2; return 1; }
    python3 -c 'import sys; value=tuple(map(int,sys.argv[1].split("."))); raise SystemExit(0 if value <= (12,0) else 1)' "$minos" || {
      echo 'macos_deployment_target_too_new' >&2; return 1;
    }
  done
  local signature
  signature="$(codesign -dvvv "$app" 2>&1 || true)"
  [[ "$signature" != *'Authority='* ]] || { echo 'macos_artifact_unexpectedly_signed' >&2; return 1; }
  if spctl --assess --type execute "$app" >/dev/null 2>&1; then
    echo 'macos_unsigned_gate_unexpectedly_passed' >&2; return 1
  fi
  assert_support "$app/Contents/Resources"
}

ditto -x -k "$archive" "$work_dir/archive"
assert_app "$work_dir/archive/FreeRemoteAccess.app"
hdiutil verify "$dmg" >/dev/null
mkdir -p "$mount_dir"
hdiutil attach -readonly -nobrowse -mountpoint "$mount_dir" "$dmg" >/dev/null
mounted=1
assert_app "$mount_dir/FreeRemoteAccess.app"
hdiutil detach "$mount_dir" -quiet
mounted=0

open -n "$work_dir/archive/FreeRemoteAccess.app"
for _ in {1..40}; do
  gui_pid="$(pgrep -f "$work_dir/archive/FreeRemoteAccess.app/Contents/MacOS/FreeRemoteAccess" | head -n1 || true)"
  [[ -n "$gui_pid" ]] && break
  sleep 0.25
done
if [[ -z "$gui_pid" ]] || ! kill -0 "$gui_pid" 2>/dev/null; then
  echo 'macos_windowserver_launch_unavailable' >&2
  exit 1
fi
sleep 3
kill -0 "$gui_pid" 2>/dev/null || { echo 'macos_gui_did_not_survive' >&2; exit 1; }
kill "$gui_pid"
wait "$gui_pid" 2>/dev/null || true
gui_pid=""
echo 'macos-package-verification: ok'
