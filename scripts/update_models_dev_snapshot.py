#!/usr/bin/env python3
"""Refresh the generated offline subset of the public models.dev catalog.

Provider and model metadata are kept for the complete catalog so setup and
model selection remain useful before the first successful runtime refresh.
"""

from __future__ import annotations

import json
import pathlib
import urllib.request


ROOT = pathlib.Path(__file__).resolve().parents[1]
DESTINATION = ROOT / "crates/trouve-providers/data/models-dev-snapshot.json"
SOURCE = "https://models.dev/api.json"
PROVIDER_FIELDS = ("id", "env", "npm", "api", "name", "doc")
MODEL_FIELDS = (
    "id",
    "name",
    "status",
    "attachment",
    "reasoning",
    "reasoning_options",
    "tool_call",
    "temperature",
    "limit",
    "cost",
)


def main() -> None:
    request = urllib.request.Request(SOURCE, headers={"User-Agent": "trouve-snapshot-updater"})
    with urllib.request.urlopen(request, timeout=30) as response:
        source = json.load(response)

    snapshot: dict[str, object] = {}
    for provider_id, provider in sorted(source.items()):
        models = {
            model_id: {
                field: model[field]
                for field in MODEL_FIELDS
                if field in model and model[field] is not None
            }
            for model_id, model in sorted(provider["models"].items())
        }
        snapshot[provider_id] = {
            field: provider[field]
            for field in PROVIDER_FIELDS
            if field in provider and provider[field] is not None
        }
        snapshot[provider_id]["models"] = models

    DESTINATION.write_text(
        json.dumps(snapshot, ensure_ascii=False, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )
    print(f"updated {DESTINATION.relative_to(ROOT)} from {SOURCE}")


if __name__ == "__main__":
    main()
