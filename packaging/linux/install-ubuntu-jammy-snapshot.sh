#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
package_lock="$repo_root/packaging/linux/ubuntu-jammy-packages.lock"
snapshot=20260810T000000Z
minimum_apt=2.4.11
ca_bootstrap_deb=''
ca_bootstrap_sha256=6e8cdcc8c86103acd4fc14649eac62ff2037108389074a7b167567af33c32245
resolution_verifier="$repo_root/packaging/linux/verify-snapshot-resolution.sh"

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --ca-bootstrap-deb)
      [[ "$#" -ge 2 ]] || { echo 'ubuntu_snapshot_ca_argument_missing' >&2; exit 2; }
      ca_bootstrap_deb="$2"
      shift 2
      ;;
    *)
      echo 'ubuntu_snapshot_unknown_argument' >&2
      exit 2
      ;;
  esac
done

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

temporary_root="$(mktemp -d)"
snapshot_sources="$temporary_root/sources.list"
ca_bundle=''
trap 'rm -rf -- "$temporary_root"' EXIT

if [[ -n "$ca_bootstrap_deb" ]]; then
  [[ -f "$ca_bootstrap_deb" ]] || { echo 'ubuntu_snapshot_ca_package_missing' >&2; exit 2; }
  actual_ca_sha256="$(sha256sum "$ca_bootstrap_deb" | cut -d' ' -f1)"
  [[ "$actual_ca_sha256" == "$ca_bootstrap_sha256" ]] || {
    echo 'ubuntu_snapshot_ca_package_sha256_mismatch' >&2
    exit 2
  }
  ca_root="$temporary_root/ca-root"
  dpkg-deb -x "$ca_bootstrap_deb" "$ca_root"
  ca_bundle="$temporary_root/ca-certificates.crt"
  mapfile -d '' -t ca_files < <(
    find "$ca_root/usr/share/ca-certificates" -type f -name '*.crt' -print0 | sort -z
  )
  [[ "${#ca_files[@]}" -gt 0 ]] || { echo 'ubuntu_snapshot_ca_bundle_empty' >&2; exit 2; }
  : >"$ca_bundle"
  for certificate in "${ca_files[@]}"; do
    cat -- "$certificate" >>"$ca_bundle"
    printf '\n' >>"$ca_bundle"
  done
fi

cat >"$snapshot_sources" <<EOF
deb [snapshot=$snapshot] http://archive.ubuntu.com/ubuntu jammy main universe restricted multiverse
deb [snapshot=$snapshot] http://archive.ubuntu.com/ubuntu jammy-updates main universe restricted multiverse
deb [snapshot=$snapshot] http://security.ubuntu.com/ubuntu jammy-security main universe restricted multiverse
EOF
grep -Fqx "deb [snapshot=$snapshot] http://archive.ubuntu.com/ubuntu jammy main universe restricted multiverse" "$snapshot_sources"
grep -Fqx "deb [snapshot=$snapshot] http://archive.ubuntu.com/ubuntu jammy-updates main universe restricted multiverse" "$snapshot_sources"
grep -Fqx "deb [snapshot=$snapshot] http://security.ubuntu.com/ubuntu jammy-security main universe restricted multiverse" "$snapshot_sources"

apt_options=(
  -o "Dir::Etc::sourcelist=$snapshot_sources"
  -o "Dir::Etc::sourceparts=-"
  -o "APT::Get::List-Cleanup=0"
  -o "Acquire::Retries=3"
  -o "APT::Update::Error-Mode=any"
)
if [[ -n "$ca_bundle" ]]; then
  apt_options+=( -o "Acquire::https::CaInfo=$ca_bundle" )
fi
apt-get "${apt_options[@]}" update

index_targets="$temporary_root/index-targets"
install_plan="$temporary_root/install-plan"
install_uris="$temporary_root/install-uris"
apt-get "${apt_options[@]}" indextargets \
  'Identifier: Packages' \
  --format '$(IDENTIFIER)|$(CREATED_BY)|$(SITE)|$(RELEASE)|$(COMPONENT)' \
  >"$index_targets"
LC_ALL=C apt-get "${apt_options[@]}" --simulate install \
  --no-install-recommends --allow-downgrades "${locked_packages[@]}" >"$install_plan"
LC_ALL=C apt-get "${apt_options[@]}" --print-uris --yes install \
  --no-install-recommends --allow-downgrades "${locked_packages[@]}" >"$install_uris"
bash "$resolution_verifier" "$snapshot" "$index_targets" "$install_plan" "$install_uris"

for package in "${locked_packages[@]}"; do
  package_name="${package%%=*}"
  locked_version="${package#*=}"
  candidate="$(apt-cache "${apt_options[@]}" policy "$package_name" | sed -n 's/^[[:space:]]*Candidate:[[:space:]]*//p')"
  [[ "$candidate" == "$locked_version" ]] || {
    echo 'ubuntu_snapshot_candidate_mismatch' >&2
    exit 2
  }
done

DEBIAN_FRONTEND=noninteractive apt-get "${apt_options[@]}" install \
  --yes --no-install-recommends --allow-downgrades "${locked_packages[@]}"
