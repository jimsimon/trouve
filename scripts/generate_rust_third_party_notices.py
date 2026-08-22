#!/usr/bin/env python3
"""Generate and verify the locked Rust dependency/license inventory."""

from __future__ import annotations

import argparse
import json
import os
import shlex
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "THIRD_PARTY_NOTICES.md"

# Exact expressions are intentional: a dependency introducing a new license
# must receive a human review before this allowlist and the notice are updated.
APPROVED_LICENSES = {
    "(Apache-2.0 OR MIT) AND BSD-3-Clause",
    "(MIT OR Apache-2.0) AND NCSA",
    "(MIT OR Apache-2.0) AND Unicode-3.0",
    "0BSD OR MIT OR Apache-2.0",
    "Apache-2.0",
    "Apache-2.0 / MIT",
    "Apache-2.0 AND ISC",
    "Apache-2.0 AND MIT",
    "Apache-2.0 OR BSL-1.0",
    "Apache-2.0 OR ISC OR MIT",
    "Apache-2.0 OR MIT",
    "Apache-2.0 OR MIT OR Zlib",
    "Apache-2.0 WITH LLVM-exception",
    "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT",
    "Apache-2.0/MIT",
    "BSD-2-Clause",
    "BSD-2-Clause OR Apache-2.0",
    "BSD-2-Clause OR Apache-2.0 OR MIT",
    "BSD-2-Clause OR MIT OR Apache-2.0",
    "BSD-3-Clause",
    "BSD-3-Clause AND MIT",
    "BSD-3-Clause OR Apache-2.0",
    "BSD-3-Clause OR MIT OR Apache-2.0",
    "BSD-3-Clause/MIT",
    "BSL-1.0",
    "CC0-1.0",
    "CC0-1.0 OR Apache-2.0",
    "CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception",
    "CC0-1.0 OR MIT-0 OR Apache-2.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "ISC AND (Apache-2.0 OR ISC)",
    "ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)",
    "MIT",
    "MIT / Apache-2.0",
    "MIT AND BSD-3-Clause",
    "MIT OR Apache-2.0",
    "MIT OR Apache-2.0 OR BSD-1-Clause",
    "MIT OR Apache-2.0 OR LGPL-2.1-or-later",
    "MIT OR Apache-2.0 OR MPL-2.0",
    "MIT OR Apache-2.0 OR Zlib",
    "MIT OR Zlib OR Apache-2.0",
    "MIT or Apache-2.0",
    "MIT/Apache-2.0",
    "MPL-2.0",
    "MPL-2.0 AND BSD-3-Clause",
    "Unicode-3.0",
    "Unlicense",
    "Unlicense OR MIT",
    "Unlicense/MIT",
    "Zlib",
    "Zlib OR Apache-2.0 OR MIT",
}

# Cargo accepts legacy or otherwise non-SPDX license metadata that CycloneDX's
# SPDX expression field does not. Preserve the published text in notices, but
# emit an equivalent valid SPDX expression in SBOMs.
SPDX_NORMALIZATIONS = {
    "Apache-2.0 / MIT": "Apache-2.0 OR MIT",
    "Apache-2.0/MIT": "Apache-2.0 OR MIT",
    "BSD-3-Clause/MIT": "BSD-3-Clause OR MIT",
    "MIT / Apache-2.0": "MIT OR Apache-2.0",
    "MIT or Apache-2.0": "MIT OR Apache-2.0",
    "MIT/Apache-2.0": "MIT OR Apache-2.0",
    "Unlicense/MIT": "Unlicense OR MIT",
}

# These crates publish a license file but omit an SPDX expression from Cargo
# metadata. Their locked source archives contain the MIT text and copyright.
LICENSE_FILE_OVERRIDES = {
    ("model2vec-rs", "0.2.1"): "MIT",
    ("tree-sitter-graphql", "0.1.0"): "MIT",
}


def cargo_metadata(manifest_path: Path | None = None) -> dict[str, object]:
    command = ["cargo", "metadata", "--locked", "--format-version", "1"]
    if manifest_path is not None:
        command.extend(["--manifest-path", str(manifest_path)])
    result = subprocess.run(
        command,
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode:
        details = result.stderr.strip() or result.stdout.strip()
        if not details:
            details = f"cargo exited with status {result.returncode}"
        raise SystemExit(f"{shlex.join(command)} failed:\n{details}")
    return json.loads(result.stdout)


def _repository_argument(path: Path) -> Path:
    absolute = path if path.is_absolute() else ROOT / path
    try:
        return absolute.resolve().relative_to(ROOT.resolve())
    except ValueError:
        return path


def notice_commands(manifest_path: Path | None, notice: Path) -> tuple[str, str]:
    command = ["python3", "scripts/generate_rust_third_party_notices.py"]
    if manifest_path is not None:
        command.extend(["--manifest-path", str(_repository_argument(manifest_path))])
    notice_argument = _repository_argument(notice)
    if notice_argument != Path("THIRD_PARTY_NOTICES.md"):
        command.extend(["--notice", str(notice_argument)])
    return shlex.join(command), shlex.join([*command, "--check"])


def frontend_notice_link(notice: Path) -> str:
    absolute_notice = notice if notice.is_absolute() else ROOT / notice
    relative = os.path.relpath(
        ROOT / "web" / "app-ui" / "THIRD_PARTY_NOTICES.md",
        absolute_notice.parent,
    )
    return Path(relative).as_posix()


def generate(
    metadata: dict[str, object],
    graph_name: str = "root Cargo workspace",
    manifest_path: Path | None = None,
    notice: Path = OUTPUT,
) -> str:
    packages = [
        package for package in metadata["packages"] if package.get("source")
    ]
    rows: list[tuple[str, str, str, str]] = []
    errors: list[str] = []
    for package in packages:
        name = str(package["name"])
        version = str(package["version"])
        license_name = package.get("license") or LICENSE_FILE_OVERRIDES.get((name, version))
        if not isinstance(license_name, str):
            errors.append(f"{name}@{version}: missing reviewed license metadata")
            continue
        if license_name not in APPROVED_LICENSES:
            errors.append(f"{name}@{version}: unreviewed license expression {license_name}")
            continue
        source = str(package["source"])
        rows.append((name, version, license_name, source))
    if errors:
        raise SystemExit("\n".join(errors))

    rows.sort(key=lambda row: (row[0].casefold(), row[1], row[3]))
    regenerate_command, check_command = notice_commands(manifest_path, notice)
    frontend_link = frontend_notice_link(notice)
    lines = [
        "# Third-party notices — Rust workspace",
        "",
        f"This generated inventory covers every third-party package in the locked {graph_name}",
        "dependency graph, including development dependencies. Regenerate it with",
        f"`{regenerate_command}` and verify it with",
        f"`{check_command}`.",
        "",
        "The Lit frontend's npm inventory is generated separately in",
        f"[`web/app-ui/THIRD_PARTY_NOTICES.md`]({frontend_link}).",
        "",
        "| Package | Version | License expression | Source |",
        "| --- | --- | --- | --- |",
    ]
    lines.extend(
        f"| {name} | {version} | {license_name.replace('|', '&#124;')} | {source.replace('|', '&#124;')} |"
        for name, version, license_name, source in rows
    )
    lines.append("")
    return "\n".join(lines)


def generate_sbom(
    metadata: dict[str, object], product_name: str = "trouve"
) -> str:
    packages = [
        package for package in metadata["packages"] if package.get("source")
    ]
    components = []
    for package in sorted(
        packages, key=lambda item: (str(item["name"]).casefold(), str(item["version"]))
    ):
        name = str(package["name"])
        version = str(package["version"])
        license_name = package.get("license") or LICENSE_FILE_OVERRIDES.get((name, version))
        if not isinstance(license_name, str):
            raise SystemExit(f"{name}@{version}: missing reviewed license metadata")
        license_name = SPDX_NORMALIZATIONS.get(license_name, license_name)
        source = str(package["source"])
        components.append(
            {
                "type": "library",
                "bom-ref": f"cargo:{name}@{version}:{source}",
                "name": name,
                "version": version,
                "licenses": [{"expression": license_name}],
                "purl": f"pkg:cargo/{name}@{version}",
                "properties": [{"name": "cargo:source", "value": source}],
            }
        )
    member_ids = set(metadata.get("workspace_members", []))
    product_versions = {
        str(package["version"])
        for package in metadata["packages"]
        if package.get("id") in member_ids
    }
    if len(product_versions) != 1:
        raise SystemExit(
            "workspace packages must expose one product version; found "
            + ", ".join(sorted(product_versions))
        )
    product_version = product_versions.pop()
    product_purl = f"pkg:cargo/{product_name}@{product_version}"
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": product_purl,
                "name": product_name,
                "version": product_version,
                "purl": product_purl,
            }
        },
        "components": components,
    }
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def sbom_product_name(
    metadata: dict[str, object], manifest_path: Path | None
) -> str:
    if manifest_path is None:
        return "trouve"
    selected_manifest = manifest_path.resolve()
    for package in metadata["packages"]:
        package_manifest = package.get("manifest_path")
        if (
            isinstance(package_manifest, str)
            and Path(package_manifest).resolve() == selected_manifest
        ):
            return str(package["name"])
    member_ids = set(metadata.get("workspace_members", []))
    members = [
        str(package["name"])
        for package in metadata["packages"]
        if package.get("id") in member_ids
    ]
    if len(members) == 1:
        return members[0]
    raise SystemExit(
        f"cannot identify the SBOM component for manifest {manifest_path}"
    )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--sbom", type=Path)
    parser.add_argument("--manifest-path", type=Path)
    parser.add_argument("--notice", type=Path, default=OUTPUT)
    args = parser.parse_args()
    manifest_path = args.manifest_path
    if manifest_path is not None and not manifest_path.is_absolute():
        manifest_path = ROOT / manifest_path
    metadata = cargo_metadata(manifest_path)
    graph_name = (
        str(args.manifest_path)
        if args.manifest_path is not None
        else "root Cargo workspace"
    )
    notice = args.notice if args.notice.is_absolute() else ROOT / args.notice
    generated = generate(metadata, graph_name, manifest_path, notice)
    if args.check:
        existing = notice.read_text() if notice.exists() else ""
        if existing != generated:
            regenerate_command, _ = notice_commands(manifest_path, notice)
            raise SystemExit(
                f"Rust third-party notices are stale at {notice}; run {regenerate_command}"
            )
    else:
        notice.parent.mkdir(parents=True, exist_ok=True)
        notice.write_text(generated)
    if args.sbom is not None:
        output = args.sbom if args.sbom.is_absolute() else ROOT / args.sbom
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            generate_sbom(metadata, sbom_product_name(metadata, manifest_path))
        )


if __name__ == "__main__":
    main()
