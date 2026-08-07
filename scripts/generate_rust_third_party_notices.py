#!/usr/bin/env python3
"""Generate and verify the locked Rust dependency/license inventory."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "THIRD_PARTY_NOTICES.md"

# Exact expressions are intentional: a dependency introducing a new license
# must receive a human review before this allowlist and the notice are updated.
APPROVED_LICENSES = {
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
    "BSD-3-Clause",
    "BSD-3-Clause OR Apache-2.0",
    "BSD-3-Clause OR MIT OR Apache-2.0",
    "BSL-1.0",
    "CC0-1.0 OR Apache-2.0",
    "CC0-1.0 OR Apache-2.0 OR Apache-2.0 WITH LLVM-exception",
    "CC0-1.0 OR MIT-0 OR Apache-2.0",
    "CDLA-Permissive-2.0",
    "GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0",
    "ISC",
    "ISC AND (Apache-2.0 OR ISC)",
    "ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)",
    "MIT",
    "MIT / Apache-2.0",
    "MIT AND BSD-3-Clause",
    "MIT OR Apache-2.0",
    "MIT OR Apache-2.0 OR BSD-1-Clause",
    "MIT OR Apache-2.0 OR LGPL-2.1-or-later",
    "MIT OR Apache-2.0 OR Zlib",
    "MIT OR Zlib OR Apache-2.0",
    "MIT/Apache-2.0",
    "MPL-2.0",
    "Unicode-3.0",
    "Unlicense",
    "Unlicense OR MIT",
    "Unlicense/MIT",
    "Zlib",
    "Zlib OR Apache-2.0 OR MIT",
}

# These crates publish a license file but omit an SPDX expression from Cargo
# metadata. Their locked source archives contain the MIT text and copyright.
LICENSE_FILE_OVERRIDES = {
    ("model2vec-rs", "0.2.1"): "MIT",
    ("tree-sitter-graphql", "0.1.0"): "MIT",
}


def cargo_metadata() -> dict[str, object]:
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def generate(metadata: dict[str, object]) -> str:
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
    lines = [
        "# Third-party notices — Rust workspace",
        "",
        "This generated inventory covers every third-party package in the locked root",
        "Cargo workspace graph, including development dependencies. Regenerate it with",
        "`python3 scripts/generate_rust_third_party_notices.py` and verify it with",
        "`python3 scripts/generate_rust_third_party_notices.py --check`.",
        "",
        "The distributed Slint frontend and widgets retain the required AboutSlint",
        "attribution and use Slint under its Royalty-Free license while they remain in",
        "shipping artifacts. The Lit frontend's npm inventory is generated separately in",
        "[`web/app-ui/THIRD_PARTY_NOTICES.md`](web/app-ui/THIRD_PARTY_NOTICES.md).",
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


def generate_sbom(metadata: dict[str, object]) -> str:
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
    document = {
        "bomFormat": "CycloneDX",
        "specVersion": "1.6",
        "version": 1,
        "metadata": {
            "component": {
                "type": "application",
                "bom-ref": "pkg:cargo/trouve",
                "name": "trouve",
            }
        },
        "components": components,
    }
    return json.dumps(document, indent=2, sort_keys=True) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--sbom", type=Path)
    args = parser.parse_args()
    metadata = cargo_metadata()
    generated = generate(metadata)
    if args.check:
        existing = OUTPUT.read_text() if OUTPUT.exists() else ""
        if existing != generated:
            raise SystemExit(
                "Rust third-party notices are stale; run "
                "python3 scripts/generate_rust_third_party_notices.py"
            )
    else:
        OUTPUT.write_text(generated)
    if args.sbom is not None:
        output = args.sbom if args.sbom.is_absolute() else ROOT / args.sbom
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(generate_sbom(metadata))


if __name__ == "__main__":
    main()
