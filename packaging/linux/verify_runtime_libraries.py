#!/usr/bin/env python3
"""Fail-closed validation of Linux ELF and runtime-loaded library roots."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path


SYSTEM_ABI_SONAMES = {
    "libc.so.6",
    "libdl.so.2",
    "libgcc_s.so.1",
    "libm.so.6",
    "libpthread.so.0",
    "librt.so.1",
    "ld-linux-x86-64.so.2",
    "ld-linux-aarch64.so.1",
}
SONAME_RE = re.compile(r"^[A-Za-z0-9+_.-]+\.so\.[0-9][A-Za-z0-9._-]*$")
NEEDED_RE = re.compile(r"\(NEEDED\).*Shared library: \[([^]]+)]")
LDCONFIG_RE = re.compile(r"^\s*(\S+)\s+(?:\([^)]*\)\s+)?=>\s+(\S+)\s*$")


def read_declarations(path: Path) -> set[str]:
    declarations: list[str] = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if not SONAME_RE.fullmatch(line):
            raise ValueError(f"runtime_library_declaration_invalid:{line}")
        declarations.append(line)
    if not declarations:
        raise ValueError("runtime_library_declaration_empty")
    if len(declarations) != len(set(declarations)):
        raise ValueError("runtime_library_declaration_duplicate")
    return set(declarations)


def parse_needed(output: str) -> set[str]:
    return set(NEEDED_RE.findall(output))


def parse_ldconfig(output: str) -> dict[str, str]:
    libraries: dict[str, str] = {}
    for line in output.splitlines():
        match = LDCONFIG_RE.match(line)
        if match:
            libraries.setdefault(match.group(1), match.group(2))
    return libraries


def validate_runtime_model(
    *,
    binary_path: str,
    declared: set[str],
    needed: set[str],
    binary_bytes: bytes,
    ldconfig_output: str,
    ldd_results: dict[str, tuple[int, str]],
) -> None:
    undeclared = sorted(needed - SYSTEM_ABI_SONAMES - declared)
    if undeclared:
        raise ValueError(f"runtime_library_undeclared:{undeclared[0]}")

    for soname in sorted(declared):
        if soname not in needed and soname.encode("ascii") not in binary_bytes:
            raise ValueError(f"runtime_library_not_used:{soname}")

    cache = parse_ldconfig(ldconfig_output)
    resolved: list[str] = []
    for soname in sorted(declared):
        path = cache.get(soname)
        if path is None:
            raise ValueError(f"runtime_library_missing:{soname}")
        resolved.append(path)

    for path in [binary_path, *sorted(set(resolved))]:
        result = ldd_results.get(path)
        if result is None:
            raise ValueError(f"ldd_result_missing:{path}")
        returncode, output = result
        if returncode != 0:
            raise ValueError(f"ldd_failed:{path}")
        if re.search(r"=>\s+not found(?:\s|$)", output):
            raise ValueError(f"elf_dependency_missing:{path}")


def run_capture(command: list[str]) -> tuple[int, str]:
    completed = subprocess.run(
        command,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    return completed.returncode, completed.stdout


def require_command(command: list[str], label: str) -> str:
    returncode, output = run_capture(command)
    if returncode != 0:
        raise ValueError(f"{label}_failed")
    return output


def verify(binary: Path, declarations: Path) -> None:
    if not binary.is_file():
        raise ValueError("runtime_binary_missing")
    declared = read_declarations(declarations)
    needed = parse_needed(require_command(["readelf", "-d", str(binary)], "readelf"))
    if not needed:
        raise ValueError("elf_needed_empty")
    ldconfig_output = require_command(["ldconfig", "-p"], "ldconfig")
    cache = parse_ldconfig(ldconfig_output)
    roots = [str(binary)]
    for soname in sorted(declared):
        path = cache.get(soname)
        if path is not None:
            roots.append(path)
    ldd_results = {path: run_capture(["ldd", path]) for path in sorted(set(roots))}
    validate_runtime_model(
        binary_path=str(binary),
        declared=declared,
        needed=needed,
        binary_bytes=binary.read_bytes(),
        ldconfig_output=ldconfig_output,
        ldd_results=ldd_results,
    )
    for path in sorted(ldd_results):
        print(f"ldd:{path}")
        print(ldd_results[path][1], end="")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--declarations", required=True, type=Path)
    args = parser.parse_args()
    try:
        verify(args.binary, args.declarations)
    except (OSError, UnicodeError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
