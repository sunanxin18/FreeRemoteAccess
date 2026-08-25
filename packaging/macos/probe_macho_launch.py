#!/usr/bin/env python3
"""Run a bounded direct GUI executable probe when no Aqua session is available."""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path
from typing import Sequence


PROBE_TIMEOUT_SECONDS = 10
TERMINATE_TIMEOUT_SECONDS = 2
FATAL_LOADER_OR_SIGNAL = re.compile(
    r"(?:\bdyld(?:\[[0-9]+\])?:|Library not loaded:|Symbol not found:|"
    r"image not found|Segmentation fault|Bus error|Illegal instruction|"
    r"Abort trap|Trace/BPT trap)",
    re.IGNORECASE,
)
WINDOWSERVER_UNAVAILABLE = (
    re.compile(
        r"(?:unable|failed|cannot|can't|could not)[^\n]{0,160}"
        r"(?:connect|connection|establish)[^\n]{0,100}window[ ]?server",
        re.IGNORECASE,
    ),
    re.compile(r"\bno[^\n]{0,80}window[ ]?server\b", re.IGNORECASE),
    re.compile(
        r"window[ ]?server[^\n]{0,120}"
        r"(?:unavailable|not available|no connection|connection failed|cannot connect)",
        re.IGNORECASE,
    ),
    re.compile(
        r"CGSConnection[^\n]{0,120}(?:failed|invalid|no window[ ]?server)",
        re.IGNORECASE,
    ),
)


def classify_early_exit(returncode: int, output: str) -> str:
    if returncode < 0:
        raise ValueError("macos_macho_direct_launch_crashed")
    if returncode == 0 or FATAL_LOADER_OR_SIGNAL.search(output):
        raise ValueError("macos_macho_direct_launch_failed")
    if any(pattern.search(output) for pattern in WINDOWSERVER_UNAVAILABLE):
        return "windowserver-unavailable"
    raise ValueError("macos_macho_direct_launch_failed")


def terminate_bounded(process: subprocess.Popen[bytes]) -> None:
    process.terminate()
    try:
        process.wait(timeout=TERMINATE_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=TERMINATE_TIMEOUT_SECONDS)


def probe(binary: Path, log: Path) -> str:
    if not binary.is_file():
        raise ValueError("macos_macho_direct_launch_failed")
    log.parent.mkdir(parents=True, exist_ok=True)
    try:
        with log.open("wb") as output:
            process = subprocess.Popen([str(binary)], stdout=output, stderr=subprocess.STDOUT)
            try:
                returncode = process.wait(timeout=PROBE_TIMEOUT_SECONDS)
            except subprocess.TimeoutExpired:
                terminate_bounded(process)
                return "survived-bounded-probe"
    except (OSError, subprocess.SubprocessError) as error:
        raise ValueError("macos_macho_direct_launch_failed") from error

    captured = log.read_text(encoding="utf-8", errors="replace")
    return classify_early_exit(returncode, captured)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--log", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        result = probe(args.binary, args.log)
    except ValueError as error:
        print(str(error), file=sys.stderr)
        return 2
    if result == "survived-bounded-probe":
        print("macos-macho-direct-launch: survived-bounded-10s-probe")
    else:
        print("macos-macho-direct-launch: windowserver-unavailable-classified")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
