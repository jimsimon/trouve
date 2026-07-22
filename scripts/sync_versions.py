#!/usr/bin/env python3
"""Keep every first-party trouve artifact on one workspace version.

The canonical version is `[workspace.package].version` in the root
Cargo.toml. Cargo packages inherit it. This script synchronizes first-party
Node packages and lock records, Claude/Codex plugin manifests, internal Node
dependency pins, root Cargo workspace dependency pins, and Cargo.lock.

Compatibility numbers such as the HTTP protocol, database schema, cache
format, MCP protocol, and third-party dependency versions are intentionally
independent.

Usage:
  python3 scripts/sync_versions.py          # rewrite generated version fields
  python3 scripts/sync_versions.py --check  # fail when anything is out of sync
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
INTERNAL_SCOPE = "@trouve-ai/"
DEPENDENCY_FIELDS = (
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
)
IGNORED_DIRS = {".git", ".venv", "dist", "node_modules", "reference", "target"}
PLUGIN_DIRS = {".claude-plugin", ".codex-plugin"}


class VersionSyncError(RuntimeError):
    """A structural version invariant cannot be fixed safely."""


def load_toml(path: Path) -> dict:
    with path.open("rb") as handle:
        return tomllib.load(handle)


def workspace_version(root: Path = ROOT) -> str:
    manifest_path = root / "Cargo.toml"
    version = (
        load_toml(manifest_path)
        .get("workspace", {})
        .get("package", {})
        .get("version")
    )
    if not isinstance(version, str) or not version:
        raise VersionSyncError(
            f"{manifest_path} has no [workspace.package].version field"
        )
    return version


def workspace_member_manifests(root: Path = ROOT) -> list[Path]:
    workspace = load_toml(root / "Cargo.toml").get("workspace", {})
    members = workspace.get("members", [])
    if not isinstance(members, list):
        raise VersionSyncError("Cargo.toml workspace.members must be an array")

    manifests: set[Path] = set()
    for pattern in members:
        if not isinstance(pattern, str):
            raise VersionSyncError(
                "Cargo.toml workspace member patterns must be strings"
            )
        for member in root.glob(pattern):
            manifest = member / "Cargo.toml" if member.is_dir() else member
            if manifest.is_file():
                manifests.add(manifest)
    return sorted(manifests)


def cargo_inheritance_errors(root: Path, expected: str) -> list[str]:
    errors: list[str] = []
    for path in workspace_member_manifests(root):
        package = load_toml(path).get("package", {})
        inherited = package.get("version")
        if inherited != {"workspace": True}:
            errors.append(
                f"{path.relative_to(root)}: package.version must be inherited "
                f"with `version.workspace = true` (workspace is {expected})"
            )
    return errors


def _dependency_tables(manifest: dict) -> list[tuple[str, dict]]:
    tables: list[tuple[str, dict]] = []
    table_names = ("dependencies", "dev-dependencies", "build-dependencies")
    for table_name in table_names:
        table = manifest.get(table_name)
        if isinstance(table, dict):
            tables.append((table_name, table))
    targets = manifest.get("target", {})
    if isinstance(targets, dict):
        for target, target_config in targets.items():
            if not isinstance(target_config, dict):
                continue
            for table_name in table_names:
                table = target_config.get(table_name)
                if isinstance(table, dict):
                    tables.append((f"target.{target}.{table_name}", table))
    return tables


def cargo_dependency_inheritance_errors(root: Path) -> list[str]:
    member_manifests = workspace_member_manifests(root)
    member_paths = {manifest.resolve() for manifest in member_manifests}
    package_names = workspace_package_names(root)
    workspace_dependencies = (
        load_toml(root / "Cargo.toml").get("workspace", {}).get("dependencies", {})
    )
    internal_dependency_keys: set[str] = set()
    for name, spec in workspace_dependencies.items():
        if not isinstance(spec, dict) or not isinstance(spec.get("path"), str):
            continue
        target = (root / spec["path"] / "Cargo.toml").resolve()
        if target in member_paths:
            internal_dependency_keys.add(name)

    errors: list[str] = []
    for path in member_manifests:
        manifest = load_toml(path)
        for table_name, dependencies in _dependency_tables(manifest):
            for dependency_name, spec in dependencies.items():
                package_name = (
                    spec.get("package", dependency_name)
                    if isinstance(spec, dict)
                    else dependency_name
                )
                is_internal = (
                    dependency_name in internal_dependency_keys
                    or package_name in package_names
                )
                inherited = isinstance(spec, dict) and spec.get("workspace") is True
                if is_internal and not inherited:
                    errors.append(
                        f"{path.relative_to(root)} [{table_name}].{dependency_name}: "
                        "internal dependencies must use `.workspace = true` so "
                        "their version pin comes from root Cargo.toml"
                    )
    return errors


def _rewrite_dependency_version(text: str, name: str, expected: str) -> str:
    pattern = re.compile(
        rf"(?m)^(\s*{re.escape(name)}\s*=\s*\{{[^\n}}]*\bpath\s*=\s*"
        rf'"[^"]+"[^\n}}]*)(\}}\s*)$'
    )
    match = pattern.search(text)
    if match is None:
        raise VersionSyncError(
            f"cannot safely update workspace dependency {name!r}; "
            "keep its path dependency on one line"
        )

    body = match.group(1)
    if re.search(r'\bversion\s*=\s*"[^"]*"', body):
        body = re.sub(
            r'(\bversion\s*=\s*")[^"]*(")',
            rf"\g<1>{expected}\g<2>",
            body,
            count=1,
        )
    else:
        body = body.rstrip()
        separator = " " if body.endswith(",") else ", "
        body = f'{body}{separator}version = "{expected}" '
    return f"{text[:match.start()]}{body}{match.group(2)}{text[match.end():]}"


def sync_workspace_dependency_pins(
    root: Path, expected: str, *, check: bool
) -> list[str]:
    path = root / "Cargo.toml"
    dependencies = load_toml(path).get("workspace", {}).get("dependencies", {})
    member_paths = {manifest.resolve() for manifest in workspace_member_manifests(root)}
    text = path.read_text(encoding="utf-8")
    changes: list[str] = []

    for name, spec in dependencies.items():
        if not isinstance(spec, dict) or not isinstance(spec.get("path"), str):
            continue
        target = (root / spec["path"] / "Cargo.toml").resolve()
        if target not in member_paths:
            continue
        found = spec.get("version")
        if found == expected:
            continue
        changes.append(
            f"Cargo.toml: workspace dependency {name!r} is {found!r} "
            f"(expected {expected!r})"
        )
        if not check:
            text = _rewrite_dependency_version(text, name, expected)

    if changes and not check:
        path.write_text(text, encoding="utf-8")
    return changes


def workspace_package_names(root: Path = ROOT) -> set[str]:
    names: set[str] = set()
    for path in workspace_member_manifests(root):
        name = load_toml(path).get("package", {}).get("name")
        if not isinstance(name, str) or not name:
            raise VersionSyncError(f"{path} has no package.name field")
        names.add(name)
    return names


def _rewrite_cargo_lock_version(text: str, name: str, expected: str) -> str:
    marker = "[[package]]"
    blocks = text.split(marker)
    matches = 0
    for index, block in enumerate(blocks):
        if not re.search(rf'(?m)^name = "{re.escape(name)}"$', block):
            continue
        if re.search(r"(?m)^source = ", block):
            continue
        updated, count = re.subn(
            r'(?m)^(version = ")[^"]+(")$',
            rf"\g<1>{expected}\g<2>",
            block,
            count=1,
        )
        if count != 1:
            raise VersionSyncError(f"Cargo.lock package {name!r} has no version")
        blocks[index] = updated
        matches += 1
    if matches != 1:
        raise VersionSyncError(
            f"Cargo.lock must contain exactly one local package named {name!r}; "
            f"found {matches}"
        )
    return marker.join(blocks)


def sync_cargo_lock(root: Path, expected: str, *, check: bool) -> list[str]:
    path = root / "Cargo.lock"
    if not path.exists():
        if check:
            return ["Cargo.lock: missing"]
        result = subprocess.run(
            ["cargo", "generate-lockfile", "--offline"],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode:
            detail = result.stderr.strip() or result.stdout.strip()
            raise VersionSyncError(f"cargo generate-lockfile failed: {detail}")

    names = workspace_package_names(root)
    lock = load_toml(path)

    def local_workspace_records() -> dict[str, dict]:
        records: dict[str, dict] = {}
        for record in lock.get("package", []):
            if not isinstance(record, dict) or record.get("source") is not None:
                continue
            name = record.get("name")
            if name in names:
                if name in records:
                    raise VersionSyncError(f"Cargo.lock repeats local package {name!r}")
                records[name] = record
        return records

    local_records = local_workspace_records()
    missing = sorted(names - local_records.keys())
    changes: list[str] = []
    if missing and not check:
        changes.append(
            "Cargo.lock: missing local workspace packages: " + ", ".join(missing)
        )
        result = subprocess.run(
            ["cargo", "generate-lockfile", "--offline"],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        if result.returncode:
            detail = result.stderr.strip() or result.stdout.strip()
            raise VersionSyncError(
                f"cargo generate-lockfile failed while refreshing lock: {detail}"
            )
        lock = load_toml(path)
        local_records = local_workspace_records()
        missing = sorted(names - local_records.keys())
    if missing:
        raise VersionSyncError(
            "Cargo.lock is missing workspace packages: " + ", ".join(missing)
        )

    text = path.read_text(encoding="utf-8")
    for name in sorted(names):
        found = local_records[name].get("version")
        if found == expected:
            continue
        changes.append(
            f"Cargo.lock: local package {name!r} is {found!r} "
            f"(expected {expected!r})"
        )
        if not check:
            text = _rewrite_cargo_lock_version(text, name, expected)
    if changes and not check:
        path.write_text(text, encoding="utf-8")
    return changes


def repository_files(root: Path, filename: str) -> list[Path]:
    matches: list[Path] = []
    for current, dirs, files in os.walk(root):
        dirs[:] = sorted(
            directory for directory in dirs if directory not in IGNORED_DIRS
        )
        if filename in files:
            matches.append(Path(current) / filename)
    return sorted(matches)


def load_json(path: Path) -> dict:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise VersionSyncError(f"cannot read {path}: {error}") from error
    if not isinstance(payload, dict):
        raise VersionSyncError(f"{path} must contain a JSON object")
    return payload


def _is_first_party_lock(payload: dict) -> bool:
    if str(payload.get("name", "")).startswith(INTERNAL_SCOPE):
        return True
    root_record = payload.get("packages", {}).get("")
    return isinstance(root_record, dict) and str(
        root_record.get("name", "")
    ).startswith(INTERNAL_SCOPE)


def json_artifact_paths(root: Path = ROOT) -> list[Path]:
    paths: set[Path] = set()
    for path in repository_files(root, "package.json"):
        if str(load_json(path).get("name", "")).startswith(INTERNAL_SCOPE):
            paths.add(path)
    for path in repository_files(root, "package-lock.json"):
        if _is_first_party_lock(load_json(path)):
            paths.add(path)
    for path in repository_files(root, "plugin.json"):
        if path.parent.name in PLUGIN_DIRS:
            paths.add(path)
    return sorted(paths)


def versions_in(payload: dict, path: Path) -> list[tuple[str, dict]]:
    """Return first-party objects whose version fields must match."""
    holders = [("top-level", payload)]
    if path.name != "package-lock.json":
        return holders

    packages = payload.get("packages", {})
    if not isinstance(packages, dict):
        raise VersionSyncError(f"{path} packages field must be an object")
    for location, record in packages.items():
        if not isinstance(record, dict):
            continue
        name = str(record.get("name", ""))
        if location == "" or (
            "node_modules/" not in location and name.startswith(INTERNAL_SCOPE)
        ):
            holders.append((f"packages[{location!r}]", record))
    return holders


def sync_internal_pins(holder: dict, expected: str) -> list[str]:
    changes: list[str] = []
    for field in DEPENDENCY_FIELDS:
        dependencies = holder.get(field)
        if not isinstance(dependencies, dict):
            continue
        for name, version in list(dependencies.items()):
            if name.startswith(INTERNAL_SCOPE) and version != expected:
                dependencies[name] = expected
                changes.append(f"{field}.{name}: {version!r} (expected {expected!r})")
    return changes


def set_version(holder: dict, expected: str) -> None:
    if "version" in holder:
        holder["version"] = expected
        return
    reordered: dict = {}
    inserted = False
    for key, value in holder.items():
        reordered[key] = value
        if key == "name":
            reordered["version"] = expected
            inserted = True
    if not inserted:
        reordered = {"version": expected, **reordered}
    holder.clear()
    holder.update(reordered)


def sync_json_artifacts(root: Path, expected: str, *, check: bool) -> list[str]:
    changes: list[str] = []
    for path in json_artifact_paths(root):
        payload = load_json(path)
        changed = False
        for label, holder in versions_in(payload, path):
            found = holder.get("version")
            if found != expected:
                changes.append(
                    f"{path.relative_to(root)} {label}: version {found!r} "
                    f"(expected {expected!r})"
                )
                set_version(holder, expected)
                changed = True
            for pin_change in sync_internal_pins(holder, expected):
                changes.append(f"{path.relative_to(root)} {label}: {pin_change}")
                changed = True
        if changed and not check:
            path.write_text(
                json.dumps(payload, indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )
    return changes


def check_cargo_metadata(
    root: Path, expected: str, *, locked: bool
) -> list[str]:
    command = [
        "cargo",
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
        "--offline",
    ]
    if locked:
        command.append("--locked")
    result = subprocess.run(
        command, cwd=root, capture_output=True, text=True, check=False
    )
    if result.returncode:
        detail = result.stderr.strip() or result.stdout.strip()
        raise VersionSyncError(f"cargo metadata failed: {detail}")

    metadata = json.loads(result.stdout)
    errors = [
        f"{Path(package['manifest_path']).relative_to(root)}: cargo metadata "
        f"resolved {package['version']!r} (expected {expected!r})"
        for package in metadata.get("packages", [])
        if package.get("version") != expected
    ]
    return errors


def synchronize(root: Path = ROOT, *, check: bool) -> tuple[str, list[str]]:
    expected = workspace_version(root)
    inheritance_errors = cargo_inheritance_errors(root, expected)
    inheritance_errors.extend(cargo_dependency_inheritance_errors(root))
    if inheritance_errors:
        raise VersionSyncError("\n".join(inheritance_errors))

    changes = sync_workspace_dependency_pins(root, expected, check=check)
    changes.extend(sync_cargo_lock(root, expected, check=check))
    changes.extend(sync_json_artifacts(root, expected, check=check))
    metadata_errors = check_cargo_metadata(root, expected, locked=True)
    if metadata_errors:
        raise VersionSyncError("\n".join(metadata_errors))
    return expected, changes


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="fail without modifying out-of-sync files"
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        expected, changes = synchronize(check=args.check)
    except VersionSyncError as error:
        print(f"Version synchronization failed:\n{error}", file=sys.stderr)
        if args.check:
            print("Run `python3 scripts/sync_versions.py` to fix generated fields.")
        return 1

    if not changes:
        print(f"All first-party versions are in sync at {expected}.")
        return 0
    if args.check:
        print(f"Versions out of sync with workspace version {expected}:")
        for change in changes:
            print(f"  {change}")
        print("Run `python3 scripts/sync_versions.py` to fix.")
        return 1
    for change in changes:
        print(f"synced {change}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
