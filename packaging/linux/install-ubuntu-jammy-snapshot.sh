#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
package_lock="$repo_root/packaging/linux/ubuntu-jammy-packages.lock"
snapshot=20260810T000000Z
minimum_apt=2.4.11

if [[ "$(id -u)" -ne 0 ]]; then
  echo 'ubuntu_snapshot_installer_requires_root' >&2
  exit 2
fi

apt_version_output="$(apt-get --version)"
apt_version="$(sed -n '1s/^apt \([^ ]*\).*/\1/p' <<<"$apt_version_output")"
if [[ -z "$apt_version" ]] || ! dpkg --compare-versions "$apt_version" ge "$minimum_apt"; then
  echo 'ubuntu_snapshot_apt_version_unsupported' >&2
  exit 2
fi

mapfile -t locked_packages < <(sed -e '/^[[:space:]]*#/d' -e '/^[[:space:]]*$/d' "$package_lock")
if [[ "${#locked_packages[@]}" -eq 0 ]]; then
  echo 'ubuntu_snapshot_package_lock_empty' >&2
  exit 2
fi
for package in "${locked_packages[@]}"; do
  if [[ ! "$package" =~ ^[a-z0-9][a-z0-9+.-]*=[^[:space:]=]+$ ]]; then
    echo 'ubuntu_snapshot_package_lock_invalid' >&2
    exit 2
  fi
done

snapshot_sources="$(mktemp)"
trap 'rm -f -- "$snapshot_sources"' EXIT
cat >"$snapshot_sources" <<EOF
deb [snapshot=$snapshot] http://archive.ubuntu.com/ubuntu jammy main universe restricted multiverse
deb [snapshot=$snapshot] http://archive.ubuntu.com/ubuntu jammy-updates main universe restricted multiverse
deb [snapshot=$snapshot] http://security.ubuntu.com/ubuntu jammy-security main universe restricted multiverse
EOF

apt_options=(
  -o "Dir::Etc::sourcelist=$snapshot_sources"
  -o "Dir::Etc::sourceparts=-"
  -o "APT::Get::List-Cleanup=0"
  -o "Acquire::Retries=3"
)
apt-get "${apt_options[@]}" update
DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" install \
  --yes --no-install-recommends --allow-downgrades "${locked_packages[@]}"
