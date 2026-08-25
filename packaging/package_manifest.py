#!/usr/bin/env python3
"""Canonical, fail-closed native package manifest tooling."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import subprocess
import tarfile
import tomllib
from pathlib import Path
from typing import NamedTuple, Sequence


SCHEMA = "FRDPKG01"
PRODUCT = "FreeRemoteAccess"
RELEASE_STATUS = ["UNSIGNED", "NOT FOR PUBLIC DISTRIBUTION"]
MANIFEST_FIELDS = {
    "schema",
    "product",
    "version",
    "platform",
    "arch",
    "release_status",
    "fdk_aac",
    "artifacts",
    "files",
}
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
SEMVER_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")
MAX_REGISTRY_ARCHIVES = 64

REQUIRED_ARTIFACTS = {
    "windows": {"gui-exe", "portable-zip", "msi"},
    "macos": {"app-archive", "dmg"},
    "linux": {"appdir-archive", "deb", "appimage"},
}
OPTIONAL_ARTIFACTS = {
    "windows": set(),
    "macos": {"pkg"},
    "linux": {"rpm"},
}
ARTIFACT_SUFFIXES = {
    "windows": {
        "gui-exe": ".exe",
        "portable-zip": "-portable.zip",
        "msi": ".msi",
    },
    "macos": {
        "app-archive": "-app.zip",
        "dmg": ".dmg",
        "pkg": ".pkg",
    },
    "linux": {
        "appdir-archive": "-AppDir.tar.zst",
        "deb": ".deb",
        "appimage": ".AppImage",
        "rpm": ".rpm",
    },
}


class LockedIdentity(NamedTuple):
    version: str
    fdk_name: str
    fdk_version: str
    fdk_source: str
    fdk_checksum: str


class FdkBundle(NamedTuple):
    notice: Path
    source_archive: Path


def _fail(code: str) -> ValueError:
    return ValueError(code)


def _run_cargo_metadata(repo_root: Path) -> dict:
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=repo_root,
        check=False,
        capture_output=True,
        text=True,
        encoding="utf-8",
    )
    if completed.returncode != 0:
        raise _fail("cargo_metadata_locked_failed")
    try:
        return json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        raise _fail("cargo_metadata_invalid_json") from error


def resolve_locked_identity(repo_root: Path) -> LockedIdentity:
    repo_root = repo_root.resolve()
    metadata = _run_cargo_metadata(repo_root)
    root_manifest = (repo_root / "Cargo.toml").resolve()
    roots = [
        package
        for package in metadata.get("packages", [])
        if Path(package.get("manifest_path", "")).resolve() == root_manifest
    ]
    if len(roots) != 1:
        raise _fail("root_package_resolution_failed")
    version = roots[0].get("version", "")
    if not SEMVER_RE.fullmatch(version):
        raise _fail("root_package_version_invalid")

    fdk_packages = [
        package
        for package in metadata.get("packages", [])
        if package.get("name") == "fdk-aac-sys"
    ]
    if len(fdk_packages) != 1:
        raise _fail("fdk_package_resolution_failed")
    fdk = fdk_packages[0]
    fdk_version = fdk.get("version", "")
    fdk_source = fdk.get("source", "")
    if not SEMVER_RE.fullmatch(fdk_version) or not fdk_source.startswith("registry+"):
        raise _fail("fdk_metadata_invalid")

    try:
        lock = tomllib.loads((repo_root / "Cargo.lock").read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as error:
        raise _fail("cargo_lock_invalid") from error
    locked = [
        package
        for package in lock.get("package", [])
        if package.get("name") == "fdk-aac-sys"
        and package.get("version") == fdk_version
        and package.get("source") == fdk_source
    ]
    if len(locked) != 1:
        raise _fail("fdk_lock_resolution_failed")
    checksum = locked[0].get("checksum", "").lower()
    if not SHA256_RE.fullmatch(checksum):
        raise _fail("fdk_lock_checksum_invalid")
    return LockedIdentity(version, "fdk-aac-sys", fdk_version, fdk_source, checksum)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _cargo_home() -> Path:
    configured = os.environ.get("CARGO_HOME")
    return Path(configured).expanduser() if configured else Path.home() / ".cargo"


def _find_verified_crate(identity: LockedIdentity) -> Path:
    cache = _cargo_home() / "registry" / "cache"
    name = f"{identity.fdk_name}-{identity.fdk_version}.crate"
    candidates = sorted(cache.glob(f"*/{name}"))
    if len(candidates) > MAX_REGISTRY_ARCHIVES:
        raise _fail("fdk_archive_search_bound_exceeded")
    for candidate in candidates:
        if candidate.is_file() and sha256_file(candidate) == identity.fdk_checksum:
            return candidate
    raise _fail("fdk_verified_registry_archive_missing")


def _verified_notice_bytes(crate_archive: Path, identity: LockedIdentity) -> bytes:
    member_name = f"{identity.fdk_name}-{identity.fdk_version}/aac/NOTICE"
    try:
        with tarfile.open(crate_archive, mode="r:gz") as archive:
            member = archive.getmember(member_name)
            if not member.isfile() or member.size <= 0:
                raise _fail("fdk_notice_invalid")
            extracted = archive.extractfile(member)
            if extracted is None:
                raise _fail("fdk_notice_invalid")
            notice = extracted.read()
    except (tarfile.TarError, KeyError, OSError) as error:
        raise _fail("fdk_notice_missing_from_archive") from error
    text = notice.decode("utf-8")
    required = (
        "Software License for The Fraunhofer FDK AAC Codec Library",
        "NO EXPRESS OR IMPLIED LICENSES TO ANY PATENT CLAIMS",
        "complete source code",
    )
    if any(fragment not in text for fragment in required):
        raise _fail("fdk_notice_required_terms_missing")
    return notice


def prepare_fdk_bundle(
    repo_root: Path, destination: Path, identity: LockedIdentity | None = None
) -> FdkBundle:
    identity = identity or resolve_locked_identity(repo_root)
    crate_archive = _find_verified_crate(identity)
    notice_bytes = _verified_notice_bytes(crate_archive, identity)
    package_root = destination / f"{identity.fdk_name}-{identity.fdk_version}"
    notice = package_root / "aac" / "NOTICE"
    source_archive = package_root / "source" / crate_archive.name
    notice.parent.mkdir(parents=True, exist_ok=True)
    source_archive.parent.mkdir(parents=True, exist_ok=True)
    notice.write_bytes(notice_bytes)
    shutil.copyfile(crate_archive, source_archive)
    if sha256_file(source_archive) != identity.fdk_checksum:
        raise _fail("fdk_copied_archive_hash_mismatch")
    return FdkBundle(notice, source_archive)


def _artifact_prefix(identity: LockedIdentity, platform: str, arch: str) -> str:
    return f"{PRODUCT}-{identity.version}-{platform}-{arch}"


def _validate_artifacts(
    identity: LockedIdentity,
    platform: str,
    arch: str,
    artifacts: Sequence[tuple[str, Path]],
) -> None:
    if platform not in REQUIRED_ARTIFACTS:
        raise _fail("artifact_platform_invalid")
    kinds = [kind for kind, _ in artifacts]
    if len(kinds) != len(set(kinds)):
        raise _fail("artifact_kind_duplicate")
    allowed = REQUIRED_ARTIFACTS[platform] | OPTIONAL_ARTIFACTS[platform]
    if not set(kinds).issubset(allowed):
        raise _fail("artifact_kind_unknown")
    if not REQUIRED_ARTIFACTS[platform].issubset(kinds):
        raise _fail("artifact_required_kind_missing")
    prefix = _artifact_prefix(identity, platform, arch)
    for kind, path in artifacts:
        expected = f"{prefix}{ARTIFACT_SUFFIXES[platform][kind]}"
        if path.name != expected:
            raise _fail("artifact_name_invalid")
        if not path.is_file():
            raise _fail("artifact_missing")
        if path.stat().st_size <= 0:
            raise _fail("artifact_empty")


def _relative_file_entry(dist: Path, path: Path) -> dict:
    try:
        relative = path.resolve().relative_to(dist.resolve())
    except ValueError as error:
        raise _fail("manifest_file_outside_dist") from error
    if not path.is_file():
        raise _fail("artifact_missing")
    size = path.stat().st_size
    if size <= 0:
        raise _fail("artifact_empty")
    return {"path": relative.as_posix(), "size": size, "sha256": sha256_file(path)}


def write_sha256_sidecar(path: Path) -> Path:
    if not path.is_file() or path.stat().st_size <= 0:
        raise _fail("sidecar_source_invalid")
    sidecar = Path(f"{path}.sha256")
    sidecar.write_text(f"{sha256_file(path)}  {path.name}\n", encoding="utf-8")
    return sidecar


def write_manifest(
    repo_root: Path,
    dist: Path,
    platform: str,
    arch: str,
    artifacts: Sequence[tuple[str, Path]],
    support: FdkBundle,
) -> Path:
    identity = resolve_locked_identity(repo_root)
    dist = dist.resolve()
    _validate_artifacts(identity, platform, arch, artifacts)
    files = [_relative_file_entry(dist, path) for _, path in artifacts]
    files.extend(
        [_relative_file_entry(dist, support.notice), _relative_file_entry(dist, support.source_archive)]
    )
    for entry in files:
        write_sha256_sidecar(dist / entry["path"])
    manifest = {
        "schema": SCHEMA,
        "product": PRODUCT,
        "version": identity.version,
        "platform": platform,
        "arch": arch,
        "release_status": RELEASE_STATUS,
        "fdk_aac": {
            "package": identity.fdk_name,
            "version": identity.fdk_version,
            "source": identity.fdk_source,
            "crate_sha256": identity.fdk_checksum,
            "notice_path": support.notice.resolve().relative_to(dist).as_posix(),
            "source_archive_path": support.source_archive.resolve().relative_to(dist).as_posix(),
        },
        "artifacts": [
            {
                "kind": kind,
                "path": path.resolve().relative_to(dist).as_posix(),
            }
            for kind, path in artifacts
        ],
        "files": sorted(files, key=lambda entry: entry["path"]),
    }
    manifest_path = dist / "artifact-manifest.json"
    manifest_path.write_text(
        json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    write_sha256_sidecar(manifest_path)
    return manifest_path


def _read_manifest(path: Path) -> dict:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise _fail("artifact_manifest_invalid") from error


def _verify_sidecar(path: Path) -> None:
    sidecar = Path(f"{path}.sha256")
    if not sidecar.is_file():
        raise _fail("artifact_sidecar_missing")
    expected = f"{sha256_file(path)}  {path.name}\n"
    if sidecar.read_text(encoding="utf-8") != expected:
        raise _fail("artifact_sidecar_mismatch")


def verify_manifest(repo_root: Path, manifest_path: Path) -> None:
    identity = resolve_locked_identity(repo_root)
    manifest_path = manifest_path.resolve()
    dist = manifest_path.parent
    manifest = _read_manifest(manifest_path)
    if set(manifest) != MANIFEST_FIELDS:
        raise _fail("artifact_manifest_unknown_field")
    if manifest.get("schema") != SCHEMA or manifest.get("product") != PRODUCT:
        raise _fail("artifact_manifest_schema_invalid")
    if manifest.get("version") != identity.version:
        raise _fail("artifact_manifest_version_mismatch")
    if manifest.get("release_status") != RELEASE_STATUS:
        raise _fail("artifact_release_status_invalid")
    platform = manifest.get("platform")
    arch = manifest.get("arch")
    artifact_rows = manifest.get("artifacts")
    if not isinstance(artifact_rows, list):
        raise _fail("artifact_manifest_entries_invalid")
    artifacts = []
    artifact_paths = set()
    for row in artifact_rows:
        if not isinstance(row, dict) or set(row) != {"kind", "path"}:
            raise _fail("artifact_manifest_entries_invalid")
        path_text = row.get("path")
        if not isinstance(path_text, str):
            raise _fail("artifact_manifest_path_invalid")
        relative = Path(path_text)
        if relative.is_absolute() or ".." in relative.parts or not relative.parts:
            raise _fail("artifact_manifest_path_invalid")
        normalized = relative.as_posix()
        if normalized in artifact_paths:
            raise _fail("artifact_path_duplicate")
        artifact_paths.add(normalized)
        artifacts.append((row.get("kind"), dist / relative))
    _validate_artifacts(identity, platform, arch, artifacts)

    fdk = manifest.get("fdk_aac", {})
    if not isinstance(fdk, dict) or set(fdk) != {
        "package",
        "version",
        "source",
        "crate_sha256",
        "notice_path",
        "source_archive_path",
    }:
        raise _fail("fdk_manifest_identity_mismatch")
    expected_fdk = {
        "package": identity.fdk_name,
        "version": identity.fdk_version,
        "source": identity.fdk_source,
        "crate_sha256": identity.fdk_checksum,
    }
    if any(fdk.get(key) != value for key, value in expected_fdk.items()):
        raise _fail("fdk_manifest_identity_mismatch")

    files = manifest.get("files")
    if not isinstance(files, list) or not files:
        raise _fail("artifact_manifest_files_invalid")
    seen = set()
    for entry in files:
        if (
            not isinstance(entry, dict)
            or set(entry) != {"path", "size", "sha256"}
            or not isinstance(entry.get("path"), str)
        ):
            raise _fail("artifact_manifest_files_invalid")
        path_text = entry["path"]
        if path_text in seen or Path(path_text).is_absolute() or ".." in Path(path_text).parts:
            raise _fail("artifact_manifest_path_invalid")
        seen.add(path_text)
        path = dist / path_text
        if not path.is_file():
            raise _fail("artifact_missing")
        if path.stat().st_size != entry.get("size"):
            raise _fail("artifact_size_mismatch")
        if sha256_file(path) != entry.get("sha256"):
            raise _fail("artifact_hash_mismatch")
        _verify_sidecar(path)
    required_file_paths = {
        *(row[1].resolve().relative_to(dist).as_posix() for row in artifacts),
        fdk.get("notice_path"),
        fdk.get("source_archive_path"),
    }
    if not required_file_paths.issubset(seen):
        raise _fail("artifact_manifest_file_coverage_missing")
    source_path = dist / fdk["source_archive_path"]
    if sha256_file(source_path) != identity.fdk_checksum:
        raise _fail("fdk_source_archive_hash_mismatch")
    archived_notice = _verified_notice_bytes(source_path, identity)
    delivered_notice = (dist / fdk["notice_path"]).read_bytes()
    if delivered_notice != archived_notice:
        raise _fail("fdk_notice_copy_mismatch")
    _verify_sidecar(manifest_path)
    known_files = {manifest_path.resolve(), Path(f"{manifest_path}.sha256").resolve()}
    for path_text in seen:
        path = (dist / path_text).resolve()
        known_files.add(path)
        known_files.add(Path(f"{path}.sha256").resolve())
    for path in dist.rglob("*"):
        if path.is_file() and path.resolve() not in known_files:
            raise _fail("artifact_unlisted_file")


def _repo_root(value: str) -> Path:
    return Path(value).resolve()


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", default=str(Path(__file__).resolve().parent.parent))
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("version")
    prepare = commands.add_parser("prepare-fdk")
    prepare.add_argument("--dest", required=True)
    write = commands.add_parser("write")
    write.add_argument("--dist", required=True)
    write.add_argument("--platform", choices=sorted(REQUIRED_ARTIFACTS), required=True)
    write.add_argument("--arch", required=True)
    write.add_argument("--support-root", required=True)
    write.add_argument("--artifact", action="append", default=[], required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--manifest", required=True)
    args = parser.parse_args(argv)
    repo = _repo_root(args.repo)
    identity = resolve_locked_identity(repo)
    if args.command == "version":
        print(identity.version)
    elif args.command == "prepare-fdk":
        bundle = prepare_fdk_bundle(repo, Path(args.dest), identity)
        print(json.dumps({"notice": str(bundle.notice), "source_archive": str(bundle.source_archive)}))
    elif args.command == "write":
        artifacts = []
        for value in args.artifact:
            if "=" not in value:
                raise _fail("artifact_argument_invalid")
            kind, path = value.split("=", 1)
            artifacts.append((kind, Path(path)))
        support_root = Path(args.support_root)
        bundle = FdkBundle(
            support_root / f"{identity.fdk_name}-{identity.fdk_version}" / "aac" / "NOTICE",
            support_root
            / f"{identity.fdk_name}-{identity.fdk_version}"
            / "source"
            / f"{identity.fdk_name}-{identity.fdk_version}.crate",
        )
        print(write_manifest(repo, Path(args.dist), args.platform, args.arch, artifacts, bundle))
    elif args.command == "verify":
        verify_manifest(repo, Path(args.manifest))
        print("artifact-manifest: ok")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except ValueError as error:
        print(str(error), file=os.sys.stderr)
        raise SystemExit(2) from error
