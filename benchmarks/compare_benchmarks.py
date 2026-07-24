#!/usr/bin/env python3
"""Compare smaller-is-better benchmark results against a median baseline."""

import argparse
import json
import math
import pathlib
import statistics
import sys
from dataclasses import dataclass


@dataclass(frozen=True)
class Benchmark:
    name: str
    unit: str
    value: float


class BenchmarkDataError(ValueError):
    """Raised when a benchmark result file has an invalid shape."""


def load_suite(path: pathlib.Path) -> dict[str, Benchmark]:
    try:
        raw = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise BenchmarkDataError(f"cannot read {path}: {error}") from error

    if not isinstance(raw, list):
        raise BenchmarkDataError(f"{path}: expected a JSON array")

    suite: dict[str, Benchmark] = {}
    for index, entry in enumerate(raw):
        location = f"{path}[{index}]"
        if not isinstance(entry, dict):
            raise BenchmarkDataError(f"{location}: expected an object")
        name = entry.get("name")
        unit = entry.get("unit")
        value = entry.get("value")
        if not isinstance(name, str) or not name:
            raise BenchmarkDataError(f"{location}: name must be a non-empty string")
        if not isinstance(unit, str) or not unit:
            raise BenchmarkDataError(f"{location}: unit must be a non-empty string")
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise BenchmarkDataError(f"{location}: value must be a number")
        numeric_value = float(value)
        if not math.isfinite(numeric_value) or numeric_value <= 0:
            raise BenchmarkDataError(f"{location}: value must be finite and positive")
        if name in suite:
            raise BenchmarkDataError(f"{path}: duplicate benchmark name {name!r}")
        suite[name] = Benchmark(name, unit, numeric_value)
    if not suite:
        raise BenchmarkDataError(f"{path}: benchmark array is empty")
    return suite


def format_value(value: float, unit: str) -> str:
    return f"{value:.4g} {unit}"


def compare(
    candidate: dict[str, Benchmark],
    baselines: list[dict[str, Benchmark]],
    threshold: float,
    minimum_baselines: int,
    title: str,
) -> tuple[int, str]:
    lines = [
        f"### {title}",
        "",
        "| Benchmark | Candidate | Baseline median | Ratio | Status |",
        "| --- | ---: | ---: | ---: | --- |",
    ]
    exit_code = 0

    for name, current in sorted(candidate.items()):
        values = []
        for baseline in baselines:
            previous = baseline.get(name)
            if previous is None:
                continue
            if previous.unit != current.unit:
                raise BenchmarkDataError(
                    f"{name!r}: candidate unit {current.unit!r} does not match "
                    f"baseline unit {previous.unit!r}"
                )
            values.append(previous.value)

        display_name = name.replace("|", "\\|")
        if not values:
            lines.append(
                f"| {display_name} | {format_value(current.value, current.unit)} "
                "| — | — | New benchmark |"
            )
            continue

        baseline_median = statistics.median(values)
        ratio = current.value / baseline_median
        if len(values) < minimum_baselines:
            status = f"Insufficient history ({len(values)}/{minimum_baselines})"
            exit_code = max(exit_code, 2)
        elif ratio > threshold:
            status = f"Regression (>{threshold:.2f}×)"
            exit_code = max(exit_code, 1)
        else:
            status = "Pass"
        lines.append(
            f"| {display_name} | {format_value(current.value, current.unit)} | "
            f"{format_value(baseline_median, current.unit)} | {ratio:.2f}× | {status} |"
        )

    return exit_code, "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, type=pathlib.Path)
    parser.add_argument("--baseline", nargs="*", type=pathlib.Path, default=[])
    parser.add_argument("--threshold", required=True, type=float)
    parser.add_argument("--minimum-baselines", type=int, default=1)
    parser.add_argument("--title", default="Benchmark comparison")
    parser.add_argument("--summary", type=pathlib.Path)
    args = parser.parse_args()

    if args.threshold <= 1:
        parser.error("--threshold must be greater than 1")
    if args.minimum_baselines < 1:
        parser.error("--minimum-baselines must be at least 1")

    try:
        candidate = load_suite(args.candidate)
        baselines = [load_suite(path) for path in args.baseline]
        if len(baselines) < args.minimum_baselines:
            raise BenchmarkDataError(
                f"only {len(baselines)} baseline file(s) available; "
                f"need {args.minimum_baselines}"
            )
        exit_code, report = compare(
            candidate,
            baselines,
            args.threshold,
            args.minimum_baselines,
            args.title,
        )
    except BenchmarkDataError as error:
        report = f"### {args.title}\n\nUnable to compare benchmarks: {error}\n"
        exit_code = 2

    print(report, end="")
    if args.summary:
        try:
            with args.summary.open("a") as summary:
                summary.write(report)
                summary.write("\n")
        except OSError as error:
            print(f"cannot write summary {args.summary}: {error}", file=sys.stderr)
            return 2
    return exit_code


if __name__ == "__main__":
    sys.exit(main())
