#!/usr/bin/env python3
"""Keep every published artifact's version in sync with the trouve-search crate.

The crate version in crates/trouve-search/Cargo.toml is the single source of
truth for the search tool's distribution artifacts. Everything else that
carries a version — npm packages under npm/*, package-lock.json files,
Claude Code and Codex plugin manifests, @trouve-ai/search-core
optionalDependencies, and @trouve-ai/search-plugin's search-core dependency
— must match it exactly.

Other workspace crates (the trouve harness crates) are versioned
independently and are not covered by this script.

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
CRATE_MANIFEST = ROOT / "crates" / "trouve-search" / "Cargo.toml"


def crate_version() -> str:
    with CRATE_MANIFEST.open("rb") as f:
        manifest = tomllib.load(f)
    version = manifest.get("package", {}).get("version")
    if not isinstance(version, str) or not version:
        sys.exit(f"{CRATE_MANIFEST} has no package.version field")
    return version


def manifest_paths() -> list[Path]:
    paths: list[Path] = []
    for pattern in (
        "npm/*/.claude-plugin/plugin.json",
        "npm/*/.codex-plugin/plugin.json",
        "npm/*/package.json",
        "npm/package-lock.json",
    ):
        paths.extend(sorted(ROOT.glob(pattern)))
    return paths


# Our own packages: any dependency pin on these must equal the crate version.
INTERNAL_PREFIX = "@trouve-ai/search"


def versions_in(payload: dict, path: Path) -> list[dict]:
    """Return every object in `payload` whose `version` field must match."""
    holders = [payload]
    if path.name == "package-lock.json":
        # The workspace lockfile repeats internal packages: workspace records
        # like "search-core" (with their own dep pins) and node_modules links.
        for record in payload.get("packages", {}).values():
            if not isinstance(record, dict) or record is payload:
                continue
            name = record.get("name", "")
            if record is payload.get("packages", {}).get("") or name.startswith(
                INTERNAL_PREFIX
            ):
                holders.append(record)
    return holders


def sync_internal_pins(holder: dict, expected: str) -> bool:
    """Pin every @trouve-ai/search-* dependency in `holder` to the crate version."""
    changed = False
    for field in ("dependencies", "optionalDependencies"):
        deps = holder.get(field)
        if not isinstance(deps, dict):
            continue
        for name, version in list(deps.items()):
            if name.startswith(INTERNAL_PREFIX) and version != expected:
                deps[name] = expected
                changed = True
    return changed


def main() -> None:
    check = "--check" in sys.argv[1:]
    expected = crate_version()
    out_of_sync: list[str] = []

    for path in manifest_paths():
        payload = json.loads(path.read_text(encoding="utf-8"))
        changed = False
        for holder in versions_in(payload, path):
            found = holder.get("version")
            if found is not None and found != expected:
                out_of_sync.append(
                    f"{path.relative_to(ROOT)}: {found!r} (expected {expected!r})"
                )
                holder["version"] = expected
                changed = True
            if sync_internal_pins(holder, expected):
                out_of_sync.append(
                    f"{path.relative_to(ROOT)}: internal @trouve-ai/search-* pins "
                    f"(expected {expected!r})"
                )
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
        print(f"Versions out of sync with crates/trouve-search/Cargo.toml ({expected}):")
        for line in out_of_sync:
            print(f"  {line}")
        print("Run `python3 scripts/sync_versions.py` to fix.")
        sys.exit(1)
    for line in out_of_sync:
        print(f"synced {line}")


if __name__ == "__main__":
    main()
