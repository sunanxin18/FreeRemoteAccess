#!/usr/bin/env python3
"""Reject RSA private-key operations outside Rust cfg(test) modules."""

from __future__ import annotations

import argparse
import datetime
import importlib.util
import re
import sys
from pathlib import Path
from typing import Sequence


CFG_TEST_MODULE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+\w+\s*\{")
APPROVED_OWNER = "src/vnc/rsa_srp.rs"
REVIEW_EXPIRES_UTC = datetime.date(2026, 11, 30)
APPROVED_RSA_API = (
    (r"\buse\s+rsa\s*::\s*pkcs1v15\s*::\s*Pkcs1v15Encrypt\s*;", 1),
    (r"\buse\s+rsa\s*::\s*pkcs8\s*::\s*DecodePublicKey\s*;", 1),
    (r"\buse\s+rsa\s*::\s*traits\s*::\s*PublicKeyParts\s*;", 1),
    (r"\buse\s+rsa\s*::\s*RsaPublicKey\s*;", 1),
    (r"\bRsaPublicKey\s*::\s*from_public_key_der\s*\(", 1),
    (r"\brsa\s*::\s*rand_core\s*::\s*OsRng\b", 1),
    (r"\.\s*encrypt\s*\(", 1),
    (r"\bPkcs1v15Encrypt\b", 1),
    (r"\.\s*n\s*\(", 2),
)
RSA_API_MARKER = re.compile(
    r"(?:\buse\s+(?:::)?\s*rsa\b|\bextern\s+crate\s+rsa\b|\brsa\s*::|"
    r"\b(?:RsaPublicKey|RsaPrivateKey|"
    r"Pkcs1v15Encrypt|DecryptingKey|EncryptingKey|DecodePublicKey|PublicKeyParts|"
    r"PrivateKeyParts|RandomizedDecryptor|decrypt_with_rng|decrypt_blinded|hazmat|"
    r"rsa_decrypt|rsa_decrypt_and_check)\b)"
)
APPROVED_OWNER_RESIDUAL_MARKER = re.compile(
    RSA_API_MARKER.pattern
    + r"|\.\s*(?:encrypt|decrypt)\s*\(|::\s*(?:encrypt|decrypt)\s*\("
)


def _mask_non_code(source: str) -> str:
    """Keep Rust punctuation positions while masking comments and literals."""
    masked = list(source)
    index = 0
    length = len(source)
    while index < length:
        if source.startswith("//", index):
            end = source.find("\n", index + 2)
            end = length if end < 0 else end
            masked[index:end] = " " * (end - index)
            index = end
            continue
        if source.startswith("/*", index):
            end = index + 2
            depth = 1
            while end < length and depth:
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
            end = length if end_at < 0 else end_at + len(terminator)
            masked[index:end] = " " * (end - index)
            index = end
            continue

        prefix = 2 if source.startswith('b"', index) else 1
        if source[index : index + prefix].endswith('"'):
            end = index + prefix
            escaped = False
            while end < length:
                char = source[end]
                end += 1
                if char == '"' and not escaped:
                    break
                if char == "\\" and not escaped:
                    escaped = True
                else:
                    escaped = False
            masked[index:end] = " " * (end - index)
            index = end
            continue

        index += 1
    return "".join(masked)


def _without_cfg_test_modules(source: str) -> str:
    code = _mask_non_code(source)
    output = list(source)
    cursor = 0
    while True:
        match = CFG_TEST_MODULE.search(code, cursor)
        if match is None:
            break
        opening = code.find("{", match.start(), match.end())
        depth = 0
        end = opening
        while end < len(code):
            if code[end] == "{":
                depth += 1
            elif code[end] == "}":
                depth -= 1
                if depth == 0:
                    end += 1
                    break
            end += 1
        if depth != 0:
            raise ValueError("rsa_guard_cfg_test_module_unclosed")
        output[match.start() : end] = " " * (end - match.start())
        cursor = end
    return "".join(output)


def _consume_approved_owner_api(code: str) -> str:
    remaining = code
    for pattern, expected_count in APPROVED_RSA_API:
        compiled = re.compile(pattern)
        matches = list(compiled.finditer(remaining))
        if len(matches) != expected_count:
            raise ValueError("production_rsa_approved_api_fingerprint_changed")
        remaining = compiled.sub(lambda match: " " * len(match.group(0)), remaining)
    return remaining


def _load_dependency_boundary():
    boundary_path = Path(__file__).with_name("check_rsa_dependency_boundary.py")
    spec = importlib.util.spec_from_file_location(
        "rsa_dependency_boundary", boundary_path
    )
    if spec is None or spec.loader is None:
        raise ValueError("rsa_dependency_boundary_unavailable")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def validate_repository(
    repo: Path,
    *,
    today: datetime.date | None = None,
    metadata: dict | None = None,
) -> None:
    current_utc_date = today or datetime.datetime.now(datetime.timezone.utc).date()
    if current_utc_date >= REVIEW_EXPIRES_UTC:
        raise ValueError("rsa_advisory_exception_review_expired")
    boundary = _load_dependency_boundary()
    if metadata is None and (repo / "Cargo.toml").is_file():
        metadata = boundary.load_metadata(repo)
    source_paths = boundary.local_source_files(repo, metadata)
    if not source_paths:
        raise ValueError("rsa_guard_source_root_missing")
    for path in sorted(source_paths):
        relative = path.relative_to(repo).as_posix()
        if relative == APPROVED_OWNER:
            production = _without_cfg_test_modules(path.read_text(encoding="utf-8"))
            code = _mask_non_code(production)
            code = _consume_approved_owner_api(code)
            marker = APPROVED_OWNER_RESIDUAL_MARKER
        else:
            code = _mask_non_code(path.read_text(encoding="utf-8"))
            marker = RSA_API_MARKER
        if marker.search(code):
            raise ValueError(f"production_rsa_api_not_allowlisted:{relative}")


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    args = parser.parse_args(argv)
    try:
        validate_repository(Path(args.repo).resolve(strict=True))
    except (OSError, UnicodeError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 2
    print("production-rsa-usage: public-encrypt-only")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
