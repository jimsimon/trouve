#!/usr/bin/env python3
"""Reject workspace crates that do not follow trouve's naming convention."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys


REPOSITORY_ROOT = Path(__file__).resolve().parents[3]
EXEMPT_PACKAGES = {"trouve-app"}


def main() -> int:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=REPOSITORY_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        sys.stderr.write(result.stderr)
        return result.returncode

    packages = json.loads(result.stdout)["packages"]
    invalid = []
    for package in packages:
        name = package["name"]
        directory = Path(package["manifest_path"]).parent.name
        if name not in EXEMPT_PACKAGES and (
            not name.startswith("trouve-") or directory != name
        ):
            invalid.append((name, directory))

    if invalid:
        print("Cargo workspace crate names must use the trouve- prefix:", file=sys.stderr)
        for name, directory in invalid:
            print(f"  package {name!r} in directory {directory!r}", file=sys.stderr)
        return 1

    print(f"validated {len(packages)} workspace crate names")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
