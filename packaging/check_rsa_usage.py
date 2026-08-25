#!/usr/bin/env python3
"""Reject RSA private-key operations outside Rust cfg(test) modules."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import Sequence


CFG_TEST_MODULE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]\s*mod\s+\w+\s*\{")
PRIVATE_RSA_TOKEN = re.compile(r"\b(?:RsaPrivateKey|PrivateKeyParts)\b")
PRIVATE_DECRYPT_CALL = re.compile(r"\.\s*decrypt(?:_blinded)?\s*\(")


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


def validate_repository(repo: Path) -> None:
    source_root = repo / "src"
    if not source_root.is_dir():
        raise ValueError("rsa_guard_source_root_missing")
    for path in sorted(source_root.rglob("*.rs")):
        production = _without_cfg_test_modules(path.read_text(encoding="utf-8"))
        code = _mask_non_code(production)
        has_rsa_context = "rsa::" in code or "use rsa" in code
        if PRIVATE_RSA_TOKEN.search(code) or (
            has_rsa_context and PRIVATE_DECRYPT_CALL.search(code)
        ):
            relative = path.relative_to(repo).as_posix()
            raise ValueError(f"production_rsa_private_operation_rejected:{relative}")


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
