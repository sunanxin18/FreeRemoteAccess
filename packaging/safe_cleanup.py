#!/usr/bin/env python3
"""Fail-closed ownership check used before native package directory cleanup."""

from __future__ import annotations

import argparse
import os
import sys
from pathlib import Path, PurePosixPath
from typing import Sequence


def validate_cleanup_target(repo: Path, target: Path, expected: str) -> Path:
    expected_relative = PurePosixPath(expected)
    if (
        expected_relative.is_absolute()
        or not expected_relative.parts
        or ".." in expected_relative.parts
    ):
        raise ValueError("package_cleanup_expected_invalid")

    repo_lexical = Path(os.path.abspath(repo))
    target_lexical = Path(os.path.abspath(target))
    expected_target = repo_lexical.joinpath(*expected_relative.parts)
    if os.path.normcase(str(target_lexical)) != os.path.normcase(str(expected_target)):
        raise ValueError("package_cleanup_target_invalid")

    real_repo = repo_lexical.resolve(strict=True)
    component = repo_lexical
    if component.is_symlink():
        raise ValueError("package_cleanup_symlink_rejected")
    for part in expected_relative.parts:
        component = component / part
        if os.path.lexists(component) and component.is_symlink():
            raise ValueError("package_cleanup_symlink_rejected")

    try:
        target_lexical.parent.resolve(strict=False).relative_to(real_repo)
    except ValueError as error:
        raise ValueError("package_cleanup_resolved_parent_outside_repo") from error
    return target_lexical


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--target", required=True)
    parser.add_argument("--expected", required=True)
    args = parser.parse_args(argv)
    try:
        validated = validate_cleanup_target(
            Path(args.repo), Path(args.target), args.expected
        )
    except (OSError, ValueError) as error:
        print(str(error), file=sys.stderr)
        return 2
    print(validated)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
