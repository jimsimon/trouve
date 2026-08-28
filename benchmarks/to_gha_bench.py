#!/usr/bin/env python3
"""Convert benchmark results to the benchmark gate's common JSON format.

Emits the `customSmallerIsBetter` format from either criterion output
directories (``--criterion target/criterion``) or hyperfine JSON exports
(``--hyperfine a.json b.json``), so CI can gate on regressions with one
uniform data file per suite. Criterion entries use the median it reports;
hyperfine entries gate on the minimum run, because contention on shared CI
runners only ever adds time — the minimum is the closest observation to the
true cost, while a median over a handful of runs fails whenever the runner
is busy for most of the measurement window.
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
                    # The " (min)" suffix versions the history series: the
                    # gate keys history by name, and a minimum screened
                    # against pre-change median history would be
                    # systematically lenient. A fresh series has no history,
                    # which the comparison treats as inconclusive, so the
                    # transition rides the like-for-like base confirmation.
                    "name": f"{result['command']} (min)",
                    "unit": "ms",
                    "value": result["min"] * 1000.0,
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
