#!/usr/bin/env python3
"""Generate npm platform package manifests from npm/platforms.json."""

from __future__ import annotations

import json
from pathlib import Path

from sync_versions import workspace_version

ROOT = Path(__file__).resolve().parent.parent
PLATFORMS = ROOT / "npm" / "platforms.json"


def main() -> None:
    version = workspace_version(ROOT)
    platforms = json.loads(PLATFORMS.read_text(encoding="utf-8"))
    for entry in platforms:
        dir_name = entry["dir"]
        pkg_dir = ROOT / "npm" / dir_name
        pkg_dir.mkdir(parents=True, exist_ok=True)
        (pkg_dir / "bin").mkdir(exist_ok=True)
        gitignore = pkg_dir / "bin" / ".gitignore"
        if not gitignore.exists():
            gitignore.write_text("*\n!.gitignore\n", encoding="utf-8")

        binary = entry["binary"]
        payload: dict = {
            "name": f"@trouve-ai/{dir_name}",
            "version": version,
            "description": f"trouve-search native binary for {entry['target']}",
            "license": "MIT",
            "repository": {
                "type": "git",
                "url": "git+https://github.com/jimsimon/trouve.git",
                "directory": f"npm/{dir_name}",
            },
            "os": entry["os"],
            "cpu": entry["cpu"],
            "files": [f"bin/{binary}"],
            "scripts": {
                # Refuse to publish a platform package whose binary was
                # never staged (scripts/stage_npm_binaries.py).
                "prepublishOnly": (
                    f"node -e \"require('fs').accessSync('bin/{binary}')\""
                ),
            },
        }
        if "libc" in entry:
            payload["libc"] = entry["libc"]

        path = pkg_dir / "package.json"
        path.write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")
        print(f"wrote {path.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
