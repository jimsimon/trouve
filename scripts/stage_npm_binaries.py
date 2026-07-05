#!/usr/bin/env python3
"""Copy release binaries into npm platform package bin/ directories."""

from __future__ import annotations

import json
import sys
import tarfile
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PLATFORMS = ROOT / "npm" / "platforms.json"


def extract_binary(artifact: Path, binary_name: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    if artifact.suffix == ".gz" and artifact.name.endswith(".tar.gz"):
        with tarfile.open(artifact, "r:gz") as tar:
            member = tar.getmember(binary_name)
            extracted = tar.extractfile(member)
            if extracted is None:
                sys.exit(f"failed to read {binary_name} from {artifact}")
            dest.write_bytes(extracted.read())
    elif artifact.suffix == ".zip":
        with zipfile.ZipFile(artifact) as zf:
            dest.write_bytes(zf.read(binary_name))
    else:
        sys.exit(f"unsupported artifact format: {artifact}")
    dest.chmod(dest.stat().st_mode | 0o111)


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} ARTIFACTS_DIR")
    artifacts_dir = Path(sys.argv[1])
    platforms = json.loads(PLATFORMS.read_text(encoding="utf-8"))

    for entry in platforms:
        target = entry["target"]
        binary = entry["binary"]
        matches = sorted(artifacts_dir.glob(f"trouve-search-*-{target}.tar.gz"))
        matches += sorted(artifacts_dir.glob(f"trouve-search-*-{target}.zip"))
        if not matches:
            sys.exit(f"no release artifact found for target {target}")
        dest = ROOT / "npm" / entry["dir"] / "bin" / binary
        extract_binary(matches[0], binary, dest)
        print(f"staged {dest.relative_to(ROOT)} from {matches[0].name}")


if __name__ == "__main__":
    main()
