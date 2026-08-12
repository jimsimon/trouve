#!/usr/bin/env python3
"""Evaluate paired per-model edit-strategy benchmark runs from JSON Lines."""

from __future__ import annotations

import argparse
import json
import math
import pathlib
import statistics
import sys
from dataclasses import dataclass


STRATEGIES = {"apply_patch", "hashline"}
ORIGINS = {"local", "external"}


class DataError(ValueError):
    """Raised when benchmark input cannot support a trustworthy comparison."""


@dataclass(frozen=True)
class Run:
    model: str
    strategy: str
    task: str
    run: int
    origin: str
    output_tokens: int
    edit_retries: int
    stale_retries: int
    executor_ms: float
    correct: bool
    tests_passed: bool
    concurrency_safe: bool

    @property
    def safe_and_correct(self) -> bool:
        return self.correct and self.tests_passed and self.concurrency_safe


def require(entry: dict[str, object], field: str, expected: type, location: str):
    value = entry.get(field)
    if isinstance(value, bool) != (expected is bool) or not isinstance(value, expected):
        raise DataError(f"{location}: {field} must be {expected.__name__}")
    return value


def load(path: pathlib.Path) -> list[Run]:
    try:
        lines = path.read_text().splitlines()
    except OSError as error:
        raise DataError(f"cannot read {path}: {error}") from error
    runs: list[Run] = []
    seen: set[tuple[str, str, str, int, str]] = set()
    for number, raw in enumerate(lines, 1):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        location = f"{path}:{number}"
        try:
            entry = json.loads(raw)
        except json.JSONDecodeError as error:
            raise DataError(f"{location}: invalid JSON: {error}") from error
        if not isinstance(entry, dict):
            raise DataError(f"{location}: expected an object")
        model = require(entry, "model", str, location)
        strategy = require(entry, "strategy", str, location)
        task = require(entry, "task", str, location)
        run = require(entry, "run", int, location)
        origin = require(entry, "origin", str, location)
        if origin not in ORIGINS:
            raise DataError(f"{location}: origin must be local or external")
        if not model or not task or run < 1:
            raise DataError(f"{location}: model/task must be non-empty and run must be positive")
        if strategy not in STRATEGIES:
            raise DataError(f"{location}: unsupported strategy {strategy!r}")
        numeric: dict[str, int | float] = {}
        for field, expected in (
            ("output_tokens", int),
            ("edit_retries", int),
            ("stale_retries", int),
            ("executor_ms", (int, float)),
        ):
            value = entry.get(field)
            if isinstance(value, bool) or not isinstance(value, expected):
                raise DataError(f"{location}: {field} must be numeric")
            if value < 0 or not math.isfinite(float(value)):
                raise DataError(f"{location}: {field} must be finite and non-negative")
            numeric[field] = value
        key = (model, strategy, task, run, origin)
        if key in seen:
            raise DataError(f"{location}: duplicate run {key}")
        seen.add(key)
        runs.append(
            Run(
                model=model,
                strategy=strategy,
                task=task,
                run=run,
                origin=origin,
                output_tokens=int(numeric["output_tokens"]),
                edit_retries=int(numeric["edit_retries"]),
                stale_retries=int(numeric["stale_retries"]),
                executor_ms=float(numeric["executor_ms"]),
                correct=require(entry, "correct", bool, location),
                tests_passed=require(entry, "tests_passed", bool, location),
                concurrency_safe=require(entry, "concurrency_safe", bool, location),
            )
        )
    if not runs:
        raise DataError(f"{path}: no benchmark rows")
    return runs


def percentile(values: list[float], fraction: float) -> float:
    ordered = sorted(values)
    index = math.ceil(len(ordered) * fraction) - 1
    return ordered[max(0, min(index, len(ordered) - 1))]


def paired_runs(
    runs: list[Run], model: str, baseline: str, candidate: str, origin: str
) -> list[tuple[Run, Run]]:
    selected = [
        run
        for run in runs
        if run.model == model
        and run.origin == origin
        and run.strategy in {baseline, candidate}
    ]
    indexed = {(run.strategy, run.task, run.run): run for run in selected}
    baseline_keys = {
        (run.task, run.run) for run in selected if run.strategy == baseline
    }
    candidate_keys = {
        (run.task, run.run) for run in selected if run.strategy == candidate
    }
    if baseline_keys != candidate_keys:
        missing_candidate = sorted(baseline_keys - candidate_keys)
        missing_baseline = sorted(candidate_keys - baseline_keys)
        details = []
        if missing_candidate:
            details.append(f"missing {candidate} rows for {missing_candidate}")
        if missing_baseline:
            details.append(f"missing {baseline} rows for {missing_baseline}")
        raise DataError(f"{model} {origin} evidence is not fully paired: {'; '.join(details)}")
    return [
        (indexed[(baseline, task, number)], indexed[(candidate, task, number)])
        for task, number in sorted(baseline_keys)
    ]


def summarize(pairs: list[tuple[Run, Run]]) -> dict[str, object]:
    baseline = [pair[0] for pair in pairs]
    candidate = [pair[1] for pair in pairs]
    return {
        "pairs": len(pairs),
        "baseline_correct": sum(run.safe_and_correct for run in baseline) / len(pairs),
        "candidate_correct": sum(run.safe_and_correct for run in candidate) / len(pairs),
        "baseline_tokens_median": statistics.median(run.output_tokens for run in baseline),
        "candidate_tokens_median": statistics.median(run.output_tokens for run in candidate),
        "baseline_retries_mean": statistics.fmean(run.edit_retries for run in baseline),
        "candidate_retries_mean": statistics.fmean(run.edit_retries for run in candidate),
        "baseline_stale_retries_mean": statistics.fmean(run.stale_retries for run in baseline),
        "candidate_stale_retries_mean": statistics.fmean(run.stale_retries for run in candidate),
        "baseline_executor_ms_median": statistics.median(run.executor_ms for run in baseline),
        "candidate_executor_ms_median": statistics.median(run.executor_ms for run in candidate),
        "baseline_executor_ms_p95": percentile([run.executor_ms for run in baseline], 0.95),
        "candidate_executor_ms_p95": percentile([run.executor_ms for run in candidate], 0.95),
    }


def corpus_shape_error(
    pairs: list[tuple[Run, Run]], minimum_tasks: int, minimum_runs_per_task: int
) -> str | None:
    task_runs: dict[str, set[int]] = {}
    for baseline, _ in pairs:
        task_runs.setdefault(baseline.task, set()).add(baseline.run)
    if len(task_runs) < minimum_tasks:
        return f"requires at least {minimum_tasks} distinct tasks; found {len(task_runs)}"
    short = sorted(
        (task, len(runs))
        for task, runs in task_runs.items()
        if len(runs) < minimum_runs_per_task
    )
    if short:
        details = ", ".join(f"{task} ({count})" for task, count in short)
        return (
            f"requires at least {minimum_runs_per_task} independent runs per task; "
            f"under-represented: {details}"
        )
    return None


def decision(summary: dict[str, object], minimum_pairs: int, token_improvement: float) -> tuple[str, str]:
    if summary["pairs"] < minimum_pairs:
        return "insufficient", f"requires at least {minimum_pairs} paired local runs"
    if summary["candidate_correct"] < 1.0:
        return "reject", "candidate had a correctness, test, or concurrency failure"
    if summary["candidate_correct"] < summary["baseline_correct"]:
        return "reject", "candidate correctness regressed"
    token_ratio = summary["candidate_tokens_median"] / max(1, summary["baseline_tokens_median"])
    retries_improved = summary["candidate_retries_mean"] < summary["baseline_retries_mean"]
    if token_ratio <= 1.0 - token_improvement or (token_ratio <= 1.0 and retries_improved):
        return "pass", "candidate reduces tokens or retries without a safety regression"
    return "reject", "candidate did not provide the required token/retry benefit"


def report(
    model: str,
    baseline: str,
    candidate: str,
    local: dict[str, object] | None,
    external: dict[str, object] | None,
    outcome: str,
    reason: str,
    minimum_pairs: int,
    minimum_tasks: int,
    minimum_runs_per_task: int,
    minimum_token_improvement: float,
) -> str:
    lines = [f"# Edit strategy benchmark: {model}", ""]
    for label, summary in (("Local", local), ("External", external)):
        if summary is None:
            lines.extend([f"## {label} evidence", "", "No paired runs.", ""])
            continue
        lines.extend(
            [
                f"## {label} evidence",
                "",
                f"Paired runs: {summary['pairs']}",
                "",
                "| Metric | Baseline | Candidate |",
                "| --- | ---: | ---: |",
                f"| Safe/correct | {summary['baseline_correct']:.1%} | {summary['candidate_correct']:.1%} |",
                f"| Median output tokens | {summary['baseline_tokens_median']:.0f} | {summary['candidate_tokens_median']:.0f} |",
                f"| Mean edit retries | {summary['baseline_retries_mean']:.2f} | {summary['candidate_retries_mean']:.2f} |",
                f"| Mean stale retries | {summary['baseline_stale_retries_mean']:.2f} | {summary['candidate_stale_retries_mean']:.2f} |",
                f"| Median executor time | {summary['baseline_executor_ms_median']:.1f} ms | {summary['candidate_executor_ms_median']:.1f} ms |",
                f"| p95 executor time | {summary['baseline_executor_ms_p95']:.1f} ms | {summary['candidate_executor_ms_p95']:.1f} ms |",
                "",
            ]
        )
    lines.extend(
        [
            "## Gate configuration",
            "",
            f"- Minimum paired local runs: {minimum_pairs}",
            f"- Minimum distinct local tasks: {minimum_tasks}",
            f"- Minimum independent runs per task: {minimum_runs_per_task}",
            f"- Minimum median token improvement: {minimum_token_improvement:.1%}",
            "- Required candidate safe/correct rate: 100%",
            "",
            "## Profile decision",
            "",
            f"**{outcome.upper()}** — {reason}.",
            "",
            f"Comparison: `{baseline}` → `{candidate}`.",
        ]
    )
    if local is None and external is not None:
        lines.extend(
            [
                "",
                "External evidence can prioritize this model for local testing, but cannot enable an enforced profile because tool schemas, prompts, repositories, and concurrency semantics differ.",
            ]
        )
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=pathlib.Path)
    parser.add_argument("--model", required=True)
    parser.add_argument("--baseline", choices=sorted(STRATEGIES), default="apply_patch")
    parser.add_argument("--candidate", choices=sorted(STRATEGIES), default="hashline")
    parser.add_argument("--minimum-pairs", type=int, default=20)
    parser.add_argument("--minimum-tasks", type=int, default=10)
    parser.add_argument("--minimum-runs-per-task", type=int, default=2)
    parser.add_argument("--minimum-token-improvement", type=float, default=0.05)
    args = parser.parse_args()
    if args.baseline == args.candidate:
        parser.error("baseline and candidate must differ")
    if args.minimum_pairs < 1:
        parser.error("--minimum-pairs must be positive")
    if args.minimum_tasks < 1:
        parser.error("--minimum-tasks must be positive")
    if args.minimum_runs_per_task < 1:
        parser.error("--minimum-runs-per-task must be positive")
    if not 0 <= args.minimum_token_improvement < 1:
        parser.error("--minimum-token-improvement must be in [0, 1)")
    try:
        runs = load(args.input)
        local_pairs = paired_runs(runs, args.model, args.baseline, args.candidate, "local")
        external_pairs = paired_runs(runs, args.model, args.baseline, args.candidate, "external")
        local = summarize(local_pairs) if local_pairs else None
        external = summarize(external_pairs) if external_pairs else None
        if local is None:
            outcome, reason = "insufficient", "no paired local runs"
        elif shape_error := corpus_shape_error(
            local_pairs, args.minimum_tasks, args.minimum_runs_per_task
        ):
            outcome, reason = "insufficient", shape_error
        else:
            outcome, reason = decision(local, args.minimum_pairs, args.minimum_token_improvement)
        print(
            report(
                args.model,
                args.baseline,
                args.candidate,
                local,
                external,
                outcome,
                reason,
                args.minimum_pairs,
                args.minimum_tasks,
                args.minimum_runs_per_task,
                args.minimum_token_improvement,
            ),
            end="",
        )
        return {"pass": 0, "reject": 1, "insufficient": 2}[outcome]
    except DataError as error:
        print(f"benchmark data error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
