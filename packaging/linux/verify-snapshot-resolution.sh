#!/usr/bin/env bash
set -euo pipefail

if [[ "$#" -ne 4 ]]; then
  echo 'snapshot_resolution_arguments_invalid' >&2
  exit 2
fi

snapshot="$1"
indices_file="$2"
plan_file="$3"
uris_file="$4"
snapshot_site="https://snapshot.ubuntu.com/ubuntu/$snapshot"
snapshot_prefix="$snapshot_site/"

[[ "$snapshot" =~ ^[0-9]{8}T[0-9]{6}Z$ ]] || {
  echo 'snapshot_timestamp_invalid' >&2
  exit 2
}
for input in "$indices_file" "$plan_file" "$uris_file"; do
  [[ -f "$input" ]] || { echo 'snapshot_resolution_input_missing' >&2; exit 2; }
done

declare -A logical_seen=()
declare -A snapshot_seen=()
index_count=0
while IFS='|' read -r site release component extra; do
  [[ -n "$site$release$component$extra" ]] || continue
  [[ -z "$extra" ]] || { echo 'snapshot_index_shape_invalid' >&2; exit 2; }
  [[ "$release" =~ ^jammy(-updates|-security)?$ ]] || {
    echo 'snapshot_index_release_mismatch' >&2
    exit 2
  }
  [[ "$component" =~ ^(main|universe|restricted|multiverse)$ ]] || {
    echo 'snapshot_index_component_mismatch' >&2
    exit 2
  }
  key="$release|$component"
  expected_logical_site=http://archive.ubuntu.com/ubuntu
  [[ "$release" != jammy-security ]] || expected_logical_site=http://security.ubuntu.com/ubuntu
  if [[ "$site" == "$expected_logical_site" ]]; then
    logical_seen[$key]=$((${logical_seen[$key]:-0} + 1))
  elif [[ "$site" == "$snapshot_site" ]]; then
    snapshot_seen[$key]=$((${snapshot_seen[$key]:-0} + 1))
  elif [[ "$site" == https://snapshot.ubuntu.com/ubuntu/* ]]; then
    echo 'snapshot_index_pair_mismatch' >&2
    exit 2
  else
    echo 'snapshot_index_site_mismatch' >&2
    exit 2
  fi
  index_count=$((index_count + 1))
done <"$indices_file"
[[ "$index_count" -eq 24 ]] || { echo 'snapshot_index_pair_mismatch' >&2; exit 2; }
for release in jammy jammy-updates jammy-security; do
  for component in main universe restricted multiverse; do
    key="$release|$component"
    [[ "${logical_seen[$key]:-0}" -eq 1 && "${snapshot_seen[$key]:-0}" -eq 1 ]] || {
      echo 'snapshot_index_pair_mismatch' >&2
      exit 2
    }
  done
done

temporary_root="$(mktemp -d)"
trap 'rm -rf -- "$temporary_root"' EXIT
plan_resolutions="$temporary_root/plan"
uri_resolutions="$temporary_root/uris"

LC_ALL=C awk '
  /^Inst / {
    name=$2
    sub(/:.*/, "", name)
    found=0
    for (field=3; field<=NF; field++) {
      if (substr($field, 1, 1) == "(") {
        version=$field
        sub(/^\(/, "", version)
        print name "=" version
        found=1
        break
      }
    }
    if (!found) exit 3
  }
' "$plan_file" | LC_ALL=C sort >"$plan_resolutions" || {
  echo 'snapshot_install_plan_invalid' >&2
  exit 2
}
plan_count="$(grep -c '^Inst ' "$plan_file" || true)"
[[ "$plan_count" -gt 0 && "$plan_count" -eq "$(wc -l <"$plan_resolutions")" ]] || {
  echo 'snapshot_install_plan_empty_or_incomplete' >&2
  exit 2
}

decode_percent_filename() {
  local remaining="$1"
  local decoded=''
  local prefix hex character
  while [[ "$remaining" == *%* ]]; do
    prefix="${remaining%%\%*}"
    remaining="${remaining#*%}"
    [[ "${#remaining}" -ge 2 ]] || return 1
    hex="${remaining:0:2}"
    [[ "$hex" =~ ^[0-9A-Fa-f]{2}$ ]] || return 1
    printf -v character '%b' "\\x$hex"
    decoded+="$prefix$character"
    remaining="${remaining:2}"
  done
  printf '%s%s' "$decoded" "$remaining"
}

uri_count=0
: >"$uri_resolutions"
uri_line_pattern="^'([^']+)'[[:space:]]+([^[:space:]]+)[[:space:]]"
while IFS= read -r line; do
  [[ "$line" == \'* ]] || continue
  [[ "$line" =~ $uri_line_pattern ]] || {
    echo 'snapshot_uri_line_invalid' >&2
    exit 2
  }
  uri="${BASH_REMATCH[1]}"
  encoded_download_filename="${BASH_REMATCH[2]}"
  [[ "$uri" == "$snapshot_prefix"pool/* ]] || {
    echo 'snapshot_uri_mismatch' >&2
    exit 2
  }
  encoded_pool_filename="${uri##*/}"
  pool_filename="$(decode_percent_filename "$encoded_pool_filename")" || {
    echo 'snapshot_uri_filename_invalid' >&2
    exit 2
  }
  download_filename="$(decode_percent_filename "$encoded_download_filename")" || {
    echo 'snapshot_uri_filename_invalid' >&2
    exit 2
  }
  filename_pattern='^[a-z0-9][a-z0-9+.-]*_[A-Za-z0-9.+:~-]+_(all|amd64)\.deb$'
  [[ "$pool_filename" =~ $filename_pattern && "$download_filename" =~ $filename_pattern ]] || {
    echo 'snapshot_uri_filename_invalid' >&2
    exit 2
  }
  without_suffix="${download_filename%.deb}"
  package_name="${without_suffix%%_*}"
  version_and_architecture="${without_suffix#*_}"
  version="${version_and_architecture%_*}"
  architecture="${version_and_architecture##*_}"
  pool_without_suffix="${pool_filename%.deb}"
  pool_package_name="${pool_without_suffix%%_*}"
  pool_version_and_architecture="${pool_without_suffix#*_}"
  pool_version="${pool_version_and_architecture%_*}"
  pool_architecture="${pool_version_and_architecture##*_}"
  expected_pool_version="$version"
  if [[ "$expected_pool_version" =~ ^[0-9]+:(.+)$ ]]; then
    expected_pool_version="${BASH_REMATCH[1]}"
  fi
  [[ "$pool_package_name" == "$package_name" && \
    "$pool_version" == "$expected_pool_version" && \
    "$pool_architecture" == "$architecture" ]] || {
    echo 'snapshot_uri_filename_mismatch' >&2
    exit 2
  }
  printf '%s=%s\n' "$package_name" "$version" >>"$uri_resolutions"
  uri_count=$((uri_count + 1))
done <"$uris_file"
LC_ALL=C sort -o "$uri_resolutions" "$uri_resolutions"
[[ "$uri_count" -gt 0 ]] || { echo 'snapshot_uri_plan_empty' >&2; exit 2; }
cmp -s "$plan_resolutions" "$uri_resolutions" || {
  echo 'snapshot_resolution_mismatch' >&2
  exit 2
}

echo 'ubuntu-snapshot-resolution: verified'
