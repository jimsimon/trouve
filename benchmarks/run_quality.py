#!/usr/bin/env python3
"""NDCG quality adapter for the upstream semble benchmark harness.

Runs the upstream annotated benchmark tasks (reference/semble/benchmarks)
through the Rust binary and, optionally, the Python implementation, and
compares NDCG@5/NDCG@10 per repo. The port passes if its mean NDCG@10 is
within 1% (absolute 0.01) of Python's.

Prerequisites:
  ./scripts/fetch-reference.sh
  pip install './reference/semble[mcp]'      # or use --skip-python
  python3 reference/semble/benchmarks/sync_repos.py   # downloads pinned repos
  cargo build --release

Usage:
  python3 benchmarks/run_quality.py [--repo NAME]... [--skip-python]
"""

from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
REFERENCE = REPO_ROOT / "reference" / "semble"
sys.path.insert(0, str(REFERENCE))
sys.path.insert(0, str(REFERENCE / "src"))

from benchmarks.data import (  # noqa: E402
    RepoSpec,
    Task,
    grouped_tasks,
    load_filtered_tasks,
    target_matches_location,
)
from benchmarks.metrics import ndcg_at_k  # noqa: E402

TOP_K = 10
LATENCY_RUNS = 3


def rust_search(binary: Path, query: str, repo_dir: Path, top_k: int) -> list[dict]:
    proc = subprocess.run(
        [str(binary), "search", query, str(repo_dir), "--top-k", str(top_k),
         "--max-snippet-lines", "0"],
        capture_output=True,
        text=True,
        timeout=300,
    )
    if proc.returncode != 0:
        raise RuntimeError(f"rust search failed: {proc.stderr.strip()}")
    return json.loads(proc.stdout).get("results", [])


def rust_target_rank(results: list[dict], target) -> int | None:
    for i, r in enumerate(results, 1):
        if target_matches_location(r["file_path"], r["start_line"], r["end_line"], target):
            return i
    return None


def evaluate_rust(binary: Path, spec: RepoSpec, tasks: list[Task]) -> dict:
    # Warm the store first so latency measurements reflect a built index.
    rust_search(binary, tasks[0].query, spec.benchmark_dir, TOP_K)
    ndcg5s, ndcg10s, latencies = [], [], []
    for task in tasks:
        results = []
        runs = []
        for _ in range(LATENCY_RUNS):
            started = time.perf_counter()
            results = rust_search(binary, task.query, spec.benchmark_dir, TOP_K)
            runs.append((time.perf_counter() - started) * 1000)
        latencies.append(statistics.median(runs))
        ranks = [r for t in task.all_relevant if (r := rust_target_rank(results, t)) is not None]
        ndcg5s.append(ndcg_at_k(ranks, len(task.all_relevant), 5))
        ndcg10s.append(ndcg_at_k(ranks, len(task.all_relevant), TOP_K))
    return {
        "ndcg5": statistics.mean(ndcg5s),
        "ndcg10": statistics.mean(ndcg10s),
        "p50_ms": statistics.median(latencies),
    }


def evaluate_python(spec: RepoSpec, tasks: list[Task]) -> dict:
    from benchmarks.metrics import target_rank  # noqa: PLC0415
    from semble.index import SembleIndex  # noqa: PLC0415

    index = SembleIndex.from_path(str(spec.benchmark_dir))
    ndcg5s, ndcg10s, latencies = [], [], []
    for task in tasks:
        runs = []
        results = []
        for _ in range(LATENCY_RUNS):
            started = time.perf_counter()
            results = index.search(task.query, top_k=TOP_K)
            runs.append((time.perf_counter() - started) * 1000)
        latencies.append(statistics.median(runs))
        ranks = [r for t in task.all_relevant if (r := target_rank(results, t)) is not None]
        ndcg5s.append(ndcg_at_k(ranks, len(task.all_relevant), 5))
        ndcg10s.append(ndcg_at_k(ranks, len(task.all_relevant), TOP_K))
    return {
        "ndcg5": statistics.mean(ndcg5s),
        "ndcg10": statistics.mean(ndcg10s),
        "p50_ms": statistics.median(latencies),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=REPO_ROOT / "target/release/semble")
    parser.add_argument("--repo", action="append", default=[])
    parser.add_argument("--skip-python", action="store_true")
    args = parser.parse_args()

    repo_specs, tasks = load_filtered_tasks(args.repo or None, None)
    by_repo = grouped_tasks(tasks)

    rows = []
    for repo, repo_tasks in sorted(by_repo.items()):
        spec = repo_specs[repo]
        print(f"[{repo}] {len(repo_tasks)} tasks ({spec.language})", file=sys.stderr)
        rust = evaluate_rust(args.binary, spec, repo_tasks)
        row = {"repo": repo, "language": spec.language, "rust": rust}
        if not args.skip_python:
            row["python"] = evaluate_python(spec, repo_tasks)
        rows.append(row)
        py = row.get("python", {})
        print(
            f"  rust:   ndcg@10={rust['ndcg10']:.4f}  p50={rust['p50_ms']:.1f}ms\n"
            + (f"  python: ndcg@10={py['ndcg10']:.4f}  p50={py['p50_ms']:.1f}ms" if py else ""),
            file=sys.stderr,
        )

    mean_rust = statistics.mean(r["rust"]["ndcg10"] for r in rows)
    print(f"\nmean rust NDCG@10:   {mean_rust:.4f}")
    if not args.skip_python:
        mean_py = statistics.mean(r["python"]["ndcg10"] for r in rows)
        delta = mean_rust - mean_py
        print(f"mean python NDCG@10: {mean_py:.4f}")
        print(f"delta: {delta:+.4f}")
        ok = delta >= -0.01
        print("QUALITY PARITY OK" if ok else "QUALITY PARITY FAILED (rust > 1% below python)")
        print(json.dumps(rows, indent=2))
        return 0 if ok else 1
    print(json.dumps(rows, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
