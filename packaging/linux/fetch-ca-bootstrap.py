#!/usr/bin/env python3
"""Fetch the one pinned Jammy CA package used by the no-CA runtime image."""

from __future__ import annotations

import argparse
import hashlib
import os
import sys
import tempfile
import urllib.request
from pathlib import Path
from typing import Sequence


PACKAGE_NAME = "ca-certificates_20260601~22.04.1_all.deb"
PACKAGE_URL = (
    "https://snapshot.ubuntu.com/ubuntu/20260810T000000Z/pool/main/c/"
    f"ca-certificates/{PACKAGE_NAME}"
)
PACKAGE_SHA256 = "6e8cdcc8c86103acd4fc14649eac62ff2037108389074a7b167567af33c32245"
MAX_PACKAGE_BYTES = 256 * 1024


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(64 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(output_directory: Path) -> Path:
    output_directory.mkdir(parents=True, exist_ok=True)
    output = output_directory / PACKAGE_NAME
    if output.is_file() and _sha256(output) == PACKAGE_SHA256:
        return output

    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{PACKAGE_NAME}.", dir=output_directory
    )
    os.close(descriptor)
    temporary = Path(temporary_name)
    try:
        request = urllib.request.Request(
            PACKAGE_URL, headers={"User-Agent": "FreeRemoteAccess-packaging/1"}
        )
        with urllib.request.urlopen(request, timeout=30) as response, temporary.open(
            "wb"
        ) as destination:
            total = 0
            while chunk := response.read(64 * 1024):
                total += len(chunk)
                if total > MAX_PACKAGE_BYTES:
                    raise ValueError("ca_bootstrap_package_too_large")
                destination.write(chunk)
        if _sha256(temporary) != PACKAGE_SHA256:
            raise ValueError("ca_bootstrap_sha256_mismatch")
        temporary.replace(output)
    finally:
        temporary.unlink(missing_ok=True)
    return output


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-directory", required=True)
    args = parser.parse_args(argv)
    try:
        result = fetch(Path(args.output_directory).resolve())
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 2
    print(result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
