#!/usr/bin/env python3
"""Reject Cargo and Node packages that do not follow trouve naming rules."""

from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys


REPOSITORY_ROOT = Path(__file__).resolve().parents[4]
IGNORED_NODE_DIRS = {".git", "dist", "node_modules", "target"}


def repository_files(filename: str) -> list[Path]:
    matches = []
    for directory, child_dirs, filenames in os.walk(REPOSITORY_ROOT):
        child_dirs[:] = [
            child for child in child_dirs if child not in IGNORED_NODE_DIRS
        ]
        if filename in filenames:
            matches.append(Path(directory) / filename)
    return sorted(matches)


def local_lock_packages(lock_path: Path) -> list[tuple[str, str | None]]:
    lock = json.loads(lock_path.read_text())
    return [
        (location or "<root>", package.get("name"))
        for location, package in lock.get("packages", {}).items()
        if location == "" or "node_modules/" not in location
    ]


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
    invalid_crates = []
    for package in packages:
        name = package["name"]
        directory = Path(package["manifest_path"]).parent.name
        if not name.startswith("trouve-") or directory != name:
            invalid_crates.append((name, directory))

    invalid_node_packages = []
    manifests = repository_files("package.json")
    for manifest_path in manifests:
        manifest = json.loads(manifest_path.read_text())
        name = manifest.get("name")
        if not isinstance(name, str) or not name.startswith("@trouve-ai/"):
            invalid_node_packages.append((manifest_path, name))

    invalid_lock_packages = []
    for lock_path in repository_files("package-lock.json"):
        for location, name in local_lock_packages(lock_path):
            if not isinstance(name, str) or not name.startswith("@trouve-ai/"):
                invalid_lock_packages.append((lock_path, location, name))

    if invalid_crates:
        print("Cargo workspace crate names must use the trouve- prefix:", file=sys.stderr)
        for name, directory in invalid_crates:
            print(f"  package {name!r} in directory {directory!r}", file=sys.stderr)

    if invalid_node_packages:
        print("Node package names must use the @trouve-ai/ scope:", file=sys.stderr)
        for manifest_path, name in invalid_node_packages:
            print(f"  package {name!r} in {manifest_path}", file=sys.stderr)

    if invalid_lock_packages:
        print("Local package-lock entries must use the @trouve-ai/ scope:", file=sys.stderr)
        for lock_path, location, name in invalid_lock_packages:
            print(f"  package {name!r} at {location!r} in {lock_path}", file=sys.stderr)

    if invalid_crates or invalid_node_packages or invalid_lock_packages:
        return 1

    print(
        f"validated {len(packages)} workspace crate names and "
        f"{len(manifests)} Node package names"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
