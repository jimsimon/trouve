#!/usr/bin/env python3
"""Keep every published artifact's version in sync with the trouve crate.

The crate version in Cargo.toml is the single source of truth. Everything
else that carries a version — npm plugin packages (package.json and
package-lock.json under plugins/*/), Claude Code and Codex plugin manifests
— must match it exactly, so a release tag means the same version everywhere.

Usage:
  python3 scripts/sync_versions.py          # rewrite manifests to the crate version
  python3 scripts/sync_versions.py --check  # exit nonzero when out of sync (CI)
"""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def crate_version() -> str:
    with (ROOT / "Cargo.toml").open("rb") as f:
        manifest = tomllib.load(f)
    version = manifest.get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        sys.exit("Cargo.toml has no package.version field")
    return version


def manifest_paths() -> list[Path]:
    paths: list[Path] = []
    for pattern in (
        "plugins/*/package.json",
        "plugins/*/package-lock.json",
        "plugins/*/.claude-plugin/plugin.json",
        "plugins/*/.codex-plugin/plugin.json",
    ):
        paths.extend(sorted(ROOT.glob(pattern)))
    return paths


def versions_in(payload: dict, path: Path) -> list[dict]:
    """Return every object in `payload` whose `version` field must match."""
    holders = [payload]
    if path.name == "package-lock.json":
        # The lockfile repeats the version in its root package record.
        root_record = payload.get("packages", {}).get("")
        if isinstance(root_record, dict):
            holders.append(root_record)
    return holders


def main() -> None:
    check = "--check" in sys.argv[1:]
    expected = crate_version()
    out_of_sync: list[str] = []

    for path in manifest_paths():
        payload = json.loads(path.read_text(encoding="utf-8"))
        changed = False
        for holder in versions_in(payload, path):
            found = holder.get("version")
            if found != expected:
                out_of_sync.append(
                    f"{path.relative_to(ROOT)}: {found!r} (expected {expected!r})"
                )
                holder["version"] = expected
                changed = True
        if changed and not check:
            path.write_text(
                json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )

    if not out_of_sync:
        print(f"All versions in sync at {expected}.")
        return
    if check:
        print(f"Versions out of sync with Cargo.toml ({expected}):")
        for line in out_of_sync:
            print(f"  {line}")
        print("Run `python3 scripts/sync_versions.py` to fix.")
        sys.exit(1)
    for line in out_of_sync:
        print(f"synced {line}")


if __name__ == "__main__":
    main()
