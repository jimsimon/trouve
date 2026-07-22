#!/usr/bin/env python3
"""Convert benchmark results to the benchmark gate's common JSON format.

Emits the `customSmallerIsBetter` format from either criterion output
directories (``--criterion target/criterion``) or hyperfine JSON exports
(``--hyperfine a.json b.json``), so CI can gate on regressions with one
uniform data file per suite. Medians are used throughout: they are stable
against the occasional slow outlier on shared CI runners.
"""

import argparse
import json
import pathlib
import sys


def criterion_entries(root: pathlib.Path) -> list[dict]:
    out = []
    # Recursive: grouped/parameterized criterion IDs nest additional
    # directories (group/case/new/estimates.json).
    for estimates in sorted(root.glob("**/new/estimates.json")):
        rel = estimates.parent.parent.relative_to(root)
        if "report" in rel.parts:
            continue
        name = "/".join(rel.parts)
        data = json.loads(estimates.read_text())
        median = data["median"]["point_estimate"]  # nanoseconds
        stderr = data["median"]["standard_error"]
        out.append(
            {
                "name": name,
                "unit": "ns",
                "value": median,
                "range": f"± {stderr:.0f}",
            }
        )
    return out


def hyperfine_entries(paths: list[pathlib.Path]) -> list[dict]:
    out = []
    for path in paths:
        for result in json.loads(path.read_text())["results"]:
            stddev = result.get("stddev") or 0.0
            out.append(
                {
                    "name": result["command"],
                    "unit": "ms",
                    "value": result["median"] * 1000.0,
                    "range": f"± {stddev * 1000.0:.1f}",
                }
            )
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--criterion", type=pathlib.Path, metavar="DIR")
    parser.add_argument("--hyperfine", type=pathlib.Path, nargs="+", metavar="JSON")
    args = parser.parse_args()

    entries: list[dict] = []
    if args.criterion:
        entries.extend(criterion_entries(args.criterion))
    if args.hyperfine:
        entries.extend(hyperfine_entries(args.hyperfine))
    if not entries:
        print("no benchmark results found", file=sys.stderr)
        return 1
    # The comparison gate keys history series by name, so fail loudly instead
    # of silently collapsing duplicate entries.
    names = [e["name"] for e in entries]
    duplicates = {n for n in names if names.count(n) > 1}
    if duplicates:
        print(f"duplicate benchmark names: {sorted(duplicates)}", file=sys.stderr)
        return 1
    json.dump(entries, sys.stdout, indent=2)
    print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
