#!/usr/bin/env python3
"""Lock the complete RSA dependency graph behind the temporary audit exception."""

from __future__ import annotations

import argparse
import json
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
SOURCE_ESCAPE = re.compile(r"(?:\binclude\s*!\s*\(|#\s*\[\s*path\s*=)")
PRIVATE_BOUNDARY_API = re.compile(
    r"(?:Credentials\s*::\s*SmartCard|SmartCardIdentity|"
    r"(?:\b|::)(?:sspi|picky|winscard|rsa)\s*::)"
)


def load_metadata(repo: Path) -> dict[str, Any]:
    completed = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
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


def _resolved_relative(path: str, repo: Path) -> str:
    try:
        return Path(path).resolve(strict=False).relative_to(repo.resolve(strict=True)).as_posix()
    except (OSError, ValueError) as error:
        raise ValueError("path_dependency_outside_repository") from error


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
    for package in packages:
        if package.get("source") is None:
            relative = _resolved_relative(package["manifest_path"], repo)
            path_packages.add((package["name"], package["version"], relative))
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


def validate_product_api(repo: Path) -> None:
    source_roots = (repo / "src", repo / "vendor" / "ironrdp-client" / "src")
    if any(not root.is_dir() for root in source_roots):
        raise ValueError("product_source_root_missing")
    for source_root in source_roots:
        for path in sorted(source_root.rglob("*.rs")):
            if path.is_symlink():
                raise ValueError(f"product_rust_source_escape:{path.relative_to(repo).as_posix()}")
            source = path.read_text(encoding="utf-8")
            if SOURCE_ESCAPE.search(source):
                raise ValueError(f"product_rust_source_escape:{path.relative_to(repo).as_posix()}")
            if "vendor/ironrdp-client" in path.as_posix().replace("\\", "/"):
                if PRIVATE_BOUNDARY_API.search(source):
                    raise ValueError(
                        f"transitive_private_api_reachable:{path.relative_to(repo).as_posix()}"
                    )
            elif re.search(r"Credentials\s*::\s*SmartCard|SmartCardIdentity", source):
                raise ValueError(
                    f"transitive_private_api_reachable:{path.relative_to(repo).as_posix()}"
                )
    connector = repo / "vendor" / "ironrdp-client" / "src" / "config.rs"
    connector_source = connector.read_text(encoding="utf-8")
    if len(re.findall(r"Credentials\s*::\s*UsernamePassword", connector_source)) != 1:
        raise ValueError("username_password_connector_fingerprint_changed")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    args = parser.parse_args(argv)
    try:
        repo = Path(args.repo).resolve(strict=True)
        validate_metadata(load_metadata(repo), repo)
        validate_product_api(repo)
    except (KeyError, OSError, UnicodeError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 2
    print("rsa-dependency-boundary: locked; product-api: username-password-only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
