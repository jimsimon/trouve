#!/usr/bin/env python3
"""Verify the checksummed binary set required by trouve self-updates."""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLATFORMS = ROOT / "npm" / "platforms.json"
TAG_RE = re.compile(r"^v\d+\.\d+\.\d+$")
SHA256_RE = re.compile(r"^[0-9a-fA-F]{64}$")


class AssetError(ValueError):
    """The release asset set cannot serve every supported updater."""


def release_targets(platforms_path: Path = PLATFORMS) -> list[str]:
    records = json.loads(platforms_path.read_text(encoding="utf-8"))
    targets = [record["target"] for record in records]
    if len(targets) != len(set(targets)):
        raise AssetError("npm/platforms.json contains duplicate targets")
    return targets


def artifact_name(component: str, tag: str, target: str) -> str:
    extension = "zip" if "-windows-" in target else "tar.gz"
    return f"{component}-{tag}-{target}.{extension}"


def expected_assets(
    tag: str,
    *,
    allow_missing_search: bool = False,
    targets: list[str] | None = None,
) -> set[str]:
    if not TAG_RE.fullmatch(tag):
        raise AssetError(f"release tag {tag!r} is not canonical vX.Y.Z")
    targets = release_targets() if targets is None else targets
    expected = {
        artifact_name("trouve-server", tag, target) for target in targets
    }
    expected.update(
        artifact_name("trouve", tag, target)
        for target in targets
        # The Wry desktop is built only for the glibc Linux targets; the
        # updater rejects Desktop + musl before querying the release channel.
        if "-musl" not in target
    )
    if not allow_missing_search:
        expected.update(
            artifact_name("trouve-search", tag, target) for target in targets
        )
    return expected


def checksum_assets(contents: str) -> set[str]:
    assets: set[str] = set()
    for line_number, line in enumerate(contents.splitlines(), 1):
        fields = line.split()
        if len(fields) != 2:
            raise AssetError(f"invalid SHA256SUMS line {line_number}")
        digest, name = fields
        name = name.removeprefix("*")
        if not SHA256_RE.fullmatch(digest):
            raise AssetError(f"invalid SHA-256 on line {line_number}")
        if name in assets:
            raise AssetError(f"duplicate SHA256SUMS entry for {name}")
        assets.add(name)
    return assets


def verify(
    checksum_file: Path,
    tag: str,
    *,
    allow_missing_search: bool = False,
    targets: list[str] | None = None,
) -> None:
    present = checksum_assets(checksum_file.read_text(encoding="utf-8"))
    missing = sorted(
        expected_assets(
            tag,
            allow_missing_search=allow_missing_search,
            targets=targets,
        )
        - present
    )
    if missing:
        raise AssetError(
            "release is missing self-update assets:\n  " + "\n  ".join(missing)
        )


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("checksum_file", type=Path)
    parser.add_argument("tag")
    parser.add_argument(
        "--allow-missing-search",
        action="store_true",
        help="allow an old release backfill with no standalone search assets",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        verify(
            args.checksum_file,
            args.tag,
            allow_missing_search=args.allow_missing_search,
        )
    except (AssetError, OSError, json.JSONDecodeError) as error:
        print(f"Release asset verification failed: {error}", file=sys.stderr)
        return 1
    print(f"Release assets satisfy the self-update contract for {args.tag}.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
