#!/usr/bin/env python3
"""Lock the complete RSA dependency graph behind the temporary audit exception."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any, Sequence


CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
EXPECTED_RSA = {
    "0.9.10": {
        "features": {"default", "pem", "std", "u64_digit"},
        "parents": {("freeremotedesk", "0.1.0")},
    },
    "0.10.0-rc.18": {
        "features": {"default", "encoding", "hazmat", "std"},
        "parents": {
            ("picky", "7.0.0-rc.25"),
            ("sspi", "0.21.3"),
            ("winscard", "0.3.3"),
        },
    },
}
EXPECTED_BOUNDARY_FEATURES = {
    ("picky", "7.0.0-rc.25"): {
        "default",
        "http_signature",
        "http_trait_impl",
        "jose",
        "pkcs12",
        "x509",
    },
    ("sspi", "0.21.3"): {
        "__install-crypto-provider",
        "aws-lc-rs",
        "default",
        "scard",
    },
    ("winscard", "0.3.3"): set(),
}
EXPECTED_PATH_PACKAGES = {
    ("freeremotedesk", "0.1.0", "Cargo.toml"),
    ("ironrdp-client", "0.1.0", "vendor/ironrdp-client/Cargo.toml"),
}
EXPECTED_LOCAL_TARGETS = {
    ("freeremotedesk", "0.1.0"): {
        ("freeremotedesk", ("lib",), ("lib",), "src/lib.rs"),
        ("freeremoteaccess-gui", ("bin",), ("bin",), "src/bin/freeremoteaccess-gui.rs"),
        ("freeremotedesk", ("bin",), ("bin",), "src/main.rs"),
        ("core_render_contracts", ("test",), ("bin",), "tests/core_render_contracts.rs"),
        ("rdp_adapter", ("test",), ("bin",), "tests/rdp_adapter.rs"),
        ("remote_texture_state", ("test",), ("bin",), "tests/remote_texture_state.rs"),
        ("rfb_adapter", ("test",), ("bin",), "tests/rfb_adapter.rs"),
        ("session_boundaries", ("test",), ("bin",), "tests/session_boundaries.rs"),
        ("ui_model", ("test",), ("bin",), "tests/ui_model.rs"),
        ("build-script-build", ("custom-build",), ("bin",), "build.rs"),
    },
    ("ironrdp-client", "0.1.0"): {
        (
            "ironrdp_client",
            ("lib",),
            ("lib",),
            "vendor/ironrdp-client/src/lib.rs",
        ),
    },
}
ROOT_IDENTITY = ("freeremotedesk", "0.1.0", None)
RSA_09_IDENTITY = ("rsa", "0.9.10", CRATES_IO)
RSA_10_IDENTITY = ("rsa", "0.10.0-rc.18", CRATES_IO)
PICKY_IDENTITY = ("picky", "7.0.0-rc.25", CRATES_IO)
SSPI_IDENTITY = ("sspi", "0.21.3", CRATES_IO)
WINSCARD_IDENTITY = ("winscard", "0.3.3", CRATES_IO)
IRONRDP_IDENTITY = ("ironrdp", "0.17.0", CRATES_IO)
IRONRDP_ASYNC_IDENTITY = ("ironrdp-async", "0.10.0", CRATES_IO)
IRONRDP_CLIENT_IDENTITY = ("ironrdp-client", "0.1.0", None)
IRONRDP_CONNECTOR_IDENTITY = ("ironrdp-connector", "0.10.0", CRATES_IO)
IRONRDP_TOKIO_IDENTITY = ("ironrdp-tokio", "0.10.0", CRATES_IO)
EXPECTED_RSA_REVERSE_CLOSURES = {
    "0.9.10": {
        "nodes": {ROOT_IDENTITY, RSA_09_IDENTITY},
        "edges": {(ROOT_IDENTITY, "rsa", RSA_09_IDENTITY)},
    },
    "0.10.0-rc.18": {
        "nodes": {
            ROOT_IDENTITY,
            IRONRDP_IDENTITY,
            IRONRDP_ASYNC_IDENTITY,
            IRONRDP_CLIENT_IDENTITY,
            IRONRDP_CONNECTOR_IDENTITY,
            IRONRDP_TOKIO_IDENTITY,
            PICKY_IDENTITY,
            RSA_10_IDENTITY,
            SSPI_IDENTITY,
            WINSCARD_IDENTITY,
        },
        "edges": {
            (ROOT_IDENTITY, "ironrdp", IRONRDP_IDENTITY),
            (IRONRDP_IDENTITY, "ironrdp_client", IRONRDP_CLIENT_IDENTITY),
            (IRONRDP_IDENTITY, "ironrdp_connector", IRONRDP_CONNECTOR_IDENTITY),
            (IRONRDP_ASYNC_IDENTITY, "ironrdp_connector", IRONRDP_CONNECTOR_IDENTITY),
            (IRONRDP_CLIENT_IDENTITY, "ironrdp_connector", IRONRDP_CONNECTOR_IDENTITY),
            (IRONRDP_CLIENT_IDENTITY, "ironrdp_tokio", IRONRDP_TOKIO_IDENTITY),
            (IRONRDP_CONNECTOR_IDENTITY, "picky", PICKY_IDENTITY),
            (IRONRDP_CONNECTOR_IDENTITY, "sspi", SSPI_IDENTITY),
            (IRONRDP_TOKIO_IDENTITY, "ironrdp_async", IRONRDP_ASYNC_IDENTITY),
            (IRONRDP_TOKIO_IDENTITY, "ironrdp_connector", IRONRDP_CONNECTOR_IDENTITY),
            (PICKY_IDENTITY, "rsa", RSA_10_IDENTITY),
            (SSPI_IDENTITY, "picky", PICKY_IDENTITY),
            (SSPI_IDENTITY, "rsa", RSA_10_IDENTITY),
            (SSPI_IDENTITY, "winscard", WINSCARD_IDENTITY),
            (WINSCARD_IDENTITY, "picky", PICKY_IDENTITY),
            (WINSCARD_IDENTITY, "rsa", RSA_10_IDENTITY),
        },
    },
}
SOURCE_ESCAPE = re.compile(
    r"(?:\binclude\s*!\s*\(|#\s*!?\s*\[[^\]]*\bpath\s*=)"
)
TRANSITIVE_PRIVATE_API = re.compile(
    r"(?:\bSmartCard(?:Identity)?\b|(?:\b|::)(?:sspi|picky|winscard|rsa)\s*::)"
)
ANY_CREDENTIALS = re.compile(r"\bCredentials\b")
USERNAME_PASSWORD_CONTEXT = re.compile(
    r"\blet\s+connector\s*=\s*ironrdp_connector\s*::\s*Config\s*\{\s*"
    r"credentials\s*:\s*ironrdp_connector\s*::\s*Credentials\s*::\s*"
    r"UsernamePassword\s*\{\s*"
    r"username\s*:\s*self\s*\.\s*username\s*\.\s*unwrap_or_default\s*\(\s*\)\s*,\s*"
    r"password\s*:\s*self\s*\.\s*password\s*\.\s*unwrap_or_default\s*\(\s*\)\s*,\s*"
    r"\}\s*,\s*domain\s*:\s*self\s*\.\s*domain\s*,"
)
MODULE_DECLARATION = re.compile(
    r"\bmod\s+(?:r#)?([A-Za-z_][A-Za-z0-9_]*)\s*;"
)


def load_metadata(repo: Path) -> dict[str, Any]:
    completed = subprocess.run(
        [
            "cargo",
            "metadata",
            "--locked",
            "--all-features",
            "--format-version",
            "1",
        ],
        cwd=repo,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise ValueError("cargo_metadata_locked_failed")
    try:
        return json.loads(completed.stdout.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError("cargo_metadata_invalid") from error


def _is_link_or_junction(path: Path) -> bool:
    return path.is_symlink() or bool(getattr(path, "is_junction", lambda: False)())


def _canonical_repo_file(
    path: str | Path, repo: Path, error_code: str
) -> tuple[Path, str]:
    repo = repo.resolve(strict=True)
    lexical = Path(path)
    if not lexical.is_absolute():
        lexical = repo / lexical
    try:
        relative = lexical.relative_to(repo)
    except ValueError as exception:
        raise ValueError(f"{error_code}_outside_repository") from exception
    current = repo
    for component in relative.parts:
        current /= component
        if _is_link_or_junction(current):
            raise ValueError(f"{error_code}_symlink_ancestor")
    try:
        resolved = lexical.resolve(strict=True)
        resolved_relative = resolved.relative_to(repo).as_posix()
    except (OSError, ValueError) as exception:
        raise ValueError(f"{error_code}_outside_repository") from exception
    if not resolved.is_file():
        raise ValueError(f"{error_code}_not_file")
    return resolved, resolved_relative


def _mask_non_code(source: str) -> str:
    masked = list(source)
    index = 0
    while index < len(source):
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = len(source) if end < 0 else end
            masked[index:end] = " " * (end - index)
            index = end
            continue
        if source.startswith("/*", index):
            end = index + 2
            depth = 1
            while end < len(source) and depth:
                if source.startswith("/*", end):
                    depth += 1
                    end += 2
                elif source.startswith("*/", end):
                    depth -= 1
                    end += 2
                else:
                    end += 1
            masked[index:end] = " " * (end - index)
            index = end
            continue
        raw = re.match(r"(?:br|r)(?P<hashes>#{0,255})\"", source[index:])
        if raw:
            terminator = '"' + raw.group("hashes")
            content_start = index + raw.end()
            end_at = source.find(terminator, content_start)
            end = len(source) if end_at < 0 else end_at + len(terminator)
            masked[index:end] = " " * (end - index)
            index = end
            continue
        prefix = 2 if source.startswith('b"', index) else 1
        if source[index : index + prefix].endswith('"'):
            end = index + prefix
            escaped = False
            while end < len(source):
                character = source[end]
                end += 1
                if character == '"' and not escaped:
                    break
                if character == "\\" and not escaped:
                    escaped = True
                else:
                    escaped = False
            masked[index:end] = " " * (end - index)
            index = end
            continue
        index += 1
    return "".join(masked)


def validate_metadata(metadata: dict[str, Any], repo: Path) -> None:
    packages = metadata.get("packages")
    resolve = metadata.get("resolve")
    workspace_members = metadata.get("workspace_members")
    if not isinstance(packages, list) or not isinstance(resolve, dict):
        raise ValueError("cargo_metadata_shape_invalid")
    package_by_id = {package["id"]: package for package in packages}
    nodes = resolve.get("nodes")
    if not isinstance(nodes, list):
        raise ValueError("cargo_metadata_shape_invalid")
    node_by_id = {node["id"]: node for node in nodes}

    def identity(package_id: str) -> tuple[str, str, str | None]:
        package = package_by_id[package_id]
        return package["name"], package["version"], package.get("source")

    root_id = resolve.get("root")
    root = package_by_id.get(root_id)
    if root is None or (root.get("name"), root.get("version")) != (
        "freeremotedesk",
        "0.1.0",
    ):
        raise ValueError("workspace_root_changed")
    if workspace_members != [root_id]:
        raise ValueError("workspace_member_set_changed")

    path_packages = set()
    target_directory = Path(metadata.get("target_directory", "")).resolve(strict=False)
    for package in packages:
        if package.get("source") is None:
            _, relative = _canonical_repo_file(
                package["manifest_path"], repo, "path_dependency_manifest"
            )
            path_packages.add((package["name"], package["version"], relative))
            targets = package.get("targets")
            if not isinstance(targets, list) or not targets:
                raise ValueError("product_target_set_missing")
            target_identities = set()
            for target in targets:
                source, source_relative = _canonical_repo_file(
                    target["src_path"], repo, "product_target"
                )
                if source == target_directory or target_directory in source.parents:
                    raise ValueError("product_target_generated_source")
                target_identities.add(
                    (
                        target["name"],
                        tuple(target.get("kind", [])),
                        tuple(target.get("crate_types", [])),
                        source_relative,
                    )
                )
            if target_identities != EXPECTED_LOCAL_TARGETS.get(
                (package["name"], package["version"])
            ):
                raise ValueError("product_target_set_changed")
    if path_packages != EXPECTED_PATH_PACKAGES:
        raise ValueError("path_dependency_set_changed")

    rsa_packages = [package for package in packages if package.get("name") == "rsa"]
    if {package.get("version") for package in rsa_packages} != set(EXPECTED_RSA):
        raise ValueError("rsa_dependency_set_changed")
    for package in rsa_packages:
        version = package["version"]
        if package.get("source") != CRATES_IO:
            raise ValueError("rsa_dependency_source_changed")
        node = node_by_id.get(package["id"])
        if node is None or set(node.get("features", [])) != EXPECTED_RSA[version]["features"]:
            raise ValueError("rsa_feature_set_changed")
        parents = {
            (package_by_id[parent["id"]]["name"], package_by_id[parent["id"]]["version"])
            for parent in nodes
            if any(dependency.get("pkg") == package["id"] for dependency in parent.get("deps", []))
        }
        if parents != EXPECTED_RSA[version]["parents"]:
            raise ValueError("rsa_reverse_dependency_set_changed")

        reverse_parents: dict[
            str, list[tuple[str, str, tuple[tuple[str, str], ...]]]
        ] = {
            package_id: [] for package_id in node_by_id
        }
        for parent in nodes:
            for dependency in parent.get("deps", []):
                dependency_kinds = tuple(
                    sorted(
                        (
                            dependency_kind.get("kind") or "",
                            dependency_kind.get("target") or "",
                        )
                        for dependency_kind in dependency.get("dep_kinds", [])
                    )
                )
                reverse_parents.setdefault(dependency["pkg"], []).append(
                    (parent["id"], dependency["name"], dependency_kinds)
                )
        closure_ids = {package["id"]}
        closure_edges: set[tuple[str, str, str]] = set()
        pending = [package["id"]]
        while pending:
            child = pending.pop()
            for parent, dependency_name, dependency_kinds in reverse_parents.get(
                child, []
            ):
                if dependency_kinds != (("", ""),):
                    raise ValueError("rsa_reverse_closure_changed")
                closure_edges.add((parent, dependency_name, child))
                if parent not in closure_ids:
                    closure_ids.add(parent)
                    pending.append(parent)
        closure = EXPECTED_RSA_REVERSE_CLOSURES[version]
        if {identity(package_id) for package_id in closure_ids} != closure["nodes"]:
            raise ValueError("rsa_reverse_closure_changed")
        if {
            (identity(parent), dependency_name, identity(child))
            for parent, dependency_name, child in closure_edges
        } != closure["edges"]:
            raise ValueError("rsa_reverse_closure_changed")

    for identity, expected_features in EXPECTED_BOUNDARY_FEATURES.items():
        matching = [
            package
            for package in packages
            if (package.get("name"), package.get("version")) == identity
        ]
        if len(matching) != 1 or matching[0].get("source") != CRATES_IO:
            raise ValueError("rsa_boundary_package_changed")
        node = node_by_id.get(matching[0]["id"])
        if node is None or set(node.get("features", [])) != expected_features:
            raise ValueError("rsa_boundary_feature_set_changed")

    root_dependencies = {
        (package_by_id[dependency["pkg"]]["name"], package_by_id[dependency["pkg"]]["version"])
        for dependency in node_by_id[root_id].get("deps", [])
    }
    forbidden_direct = set(EXPECTED_BOUNDARY_FEATURES) | {("rsa", "0.10.0-rc.18")}
    if root_dependencies & forbidden_direct:
        raise ValueError("transitive_rsa_boundary_became_direct")
    if ("rsa", "0.9.10") not in root_dependencies:
        raise ValueError("approved_root_rsa_dependency_missing")


def local_source_files(repo: Path, metadata: dict[str, Any] | None) -> set[Path]:
    source_files: set[Path] = set()
    package_directories: set[Path] = set()
    crate_roots: set[Path] = set()
    if metadata is not None:
        for package in metadata["packages"]:
            if package.get("source") is not None:
                continue
            manifest, _ = _canonical_repo_file(
                package["manifest_path"], repo, "path_dependency_manifest"
            )
            package_directories.add(manifest.parent)
            for target in package["targets"]:
                source, _ = _canonical_repo_file(
                    target["src_path"], repo, "product_target"
                )
                source_files.add(source)
                crate_roots.add(source)
    else:
        package_directories.update((repo, repo / "vendor" / "ironrdp-client"))

    source_roots = set()
    for package_directory in package_directories:
        source_roots.update(
            package_directory / relative_directory
            for relative_directory in ("src", "tests", "benches", "examples")
        )
    for source_root in source_roots:
        if not source_root.exists():
            continue
        for directory, directory_names, file_names in os.walk(
            source_root, followlinks=False
        ):
            directory_path = Path(directory)
            for name in tuple(directory_names):
                candidate = directory_path / name
                if _is_link_or_junction(candidate):
                    raise ValueError("product_source_symlink_ancestor")
            for name in file_names:
                if name.endswith(".rs"):
                    source, _ = _canonical_repo_file(
                        directory_path / name, repo, "product_source"
                    )
                    source_files.add(source)

    pending = list(source_files)
    while pending:
        path = pending.pop()
        code = _mask_non_code(path.read_text(encoding="utf-8"))
        module_base = (
            path.parent
            if path in crate_roots or path.name in {"lib.rs", "main.rs", "mod.rs"}
            else path.parent / path.stem
        )
        for module_name in MODULE_DECLARATION.findall(code):
            candidates = (
                module_base / f"{module_name}.rs",
                module_base / module_name / "mod.rs",
            )
            for candidate in candidates:
                if candidate.exists():
                    module_source, _ = _canonical_repo_file(
                        candidate, repo, "product_source"
                    )
                    if module_source not in source_files:
                        source_files.add(module_source)
                        pending.append(module_source)
                    break
    return source_files


def validate_product_api(repo: Path, metadata: dict[str, Any] | None = None) -> None:
    source_files = local_source_files(repo, metadata)
    if not source_files:
        raise ValueError("product_source_root_missing")
    connector = (repo / "vendor" / "ironrdp-client" / "src" / "config.rs").resolve(
        strict=True
    )
    for path in sorted(source_files):
        source = path.read_text(encoding="utf-8")
        code = _mask_non_code(source)
        relative = path.relative_to(repo.resolve(strict=True)).as_posix()
        if SOURCE_ESCAPE.search(code):
            raise ValueError(f"product_rust_source_escape:{relative}")
        if re.search(r"\bSmartCard(?:Identity)?\b", code):
            raise ValueError(f"transitive_private_api_reachable:{relative}")
        if "vendor/ironrdp-client" in relative and TRANSITIVE_PRIVATE_API.search(code):
            raise ValueError(f"transitive_private_api_reachable:{relative}")
        if path == connector:
            matches = list(USERNAME_PASSWORD_CONTEXT.finditer(code))
            if len(matches) != 1:
                raise ValueError("username_password_connector_fingerprint_changed")
            match = matches[0]
            code = (
                code[: match.start()]
                + " " * (match.end() - match.start())
                + code[match.end() :]
            )
        if ANY_CREDENTIALS.search(code):
            raise ValueError(f"transitive_private_api_reachable:{relative}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    args = parser.parse_args(argv)
    try:
        repo = Path(args.repo).resolve(strict=True)
        metadata = load_metadata(repo)
        validate_metadata(metadata, repo)
        validate_product_api(repo, metadata)
    except (KeyError, OSError, UnicodeError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 2
    print("rsa-dependency-boundary: locked; product-api: username-password-only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
