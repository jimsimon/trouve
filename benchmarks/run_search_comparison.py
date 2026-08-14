#!/usr/bin/env python3
"""Compare agent-facing code search latency and output tokens.

The benchmark contrasts a warm trouve-search index with recursive GNU grep and
ripgrep over common source-file extensions.  Each case pairs a natural-language
intent for trouve with the regular expression a developer would formulate for
the lexical tools.  Tool stdout is the context returned to a model, so its
estimated input-token count is also reported.

Examples:
  cargo build --release -p trouve-search
  python3 benchmarks/run_search_comparison.py --runs 10 \
      --output benchmarks/results/search-comparison.json
  python3 benchmarks/run_search_comparison.py /path/to/repo \
      --cases /path/to/cases.json
"""

from __future__ import annotations

import argparse
import json
import math
import os
import pathlib
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parent.parent
DEFAULT_CASES = ROOT / "benchmarks" / "search_comparison_cases.json"
DEFAULT_BINARY = ROOT / "target" / "release" / "trouve-search"
DEFAULT_EXTENSIONS = (
    ".c",
    ".cc",
    ".cpp",
    ".cs",
    ".go",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".jsx",
    ".kt",
    ".kts",
    ".mjs",
    ".py",
    ".rb",
    ".rs",
    ".sh",
    ".swift",
    ".ts",
    ".tsx",
    ".vue",
    ".zig",
)
EXCLUDED_DIRS = (
    ".cache",
    ".eggs",
    ".git",
    ".hg",
    ".mypy_cache",
    ".next",
    ".pytest_cache",
    ".ruff_cache",
    ".semble",
    ".svn",
    ".tox",
    ".trouve",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
)
TOOLS = ("trouve-search", "grep", "ripgrep")


@dataclass(frozen=True)
class SearchCase:
    name: str
    intent: str
    pattern: str


def load_cases(path: pathlib.Path) -> list[SearchCase]:
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"cannot read cases from {path}: {error}") from error
    if not isinstance(raw, list) or not raw:
        raise ValueError(f"{path}: expected a non-empty JSON array")

    cases: list[SearchCase] = []
    names: set[str] = set()
    for index, item in enumerate(raw):
        if not isinstance(item, dict):
            raise ValueError(f"{path}: case {index} must be an object")
        values = [item.get(key) for key in ("name", "intent", "pattern")]
        if not all(isinstance(value, str) and value.strip() for value in values):
            raise ValueError(
                f"{path}: case {index} needs non-empty name, intent, and pattern strings"
            )
        name, intent, pattern = values
        if name in names:
            raise ValueError(f"{path}: duplicate case name {name!r}")
        names.add(name)
        cases.append(SearchCase(name, intent, pattern))
    return cases


def estimated_input_tokens(output: bytes) -> int:
    """Use the repository's documented four-characters-per-token estimate."""
    characters = len(output.decode("utf-8", errors="replace"))
    return math.ceil(characters / 4)


def command_for(
    tool: str,
    case: SearchCase,
    binary: pathlib.Path,
    extensions: tuple[str, ...],
    top_k: int,
    snippet_lines: int,
) -> list[str]:
    if tool == "trouve-search":
        return [
            str(binary),
            "search",
            case.intent,
            ".",
            "--top-k",
            str(top_k),
            "--max-snippet-lines",
            str(snippet_lines),
        ]
    if tool == "grep":
        includes = [f"--include=*{extension}" for extension in extensions]
        excludes = [f"--exclude-dir={directory}" for directory in EXCLUDED_DIRS]
        return [
            "grep",
            "-r",
            "-n",
            "-E",
            "-I",
            *includes,
            *excludes,
            "--",
            case.pattern,
            ".",
        ]
    if tool == "ripgrep":
        includes = [
            argument
            for extension in extensions
            for argument in ("--glob", f"*{extension}")
        ]
        excludes = [
            argument
            for directory in EXCLUDED_DIRS
            for argument in ("--glob", f"!{directory}/**")
        ]
        return [
            "rg",
            "--line-number",
            "--no-heading",
            "--color=never",
            "--hidden",
            "--no-ignore",
            *includes,
            *excludes,
            "--",
            case.pattern,
            ".",
        ]
    raise ValueError(f"unknown tool {tool!r}")


def invoke(
    command: list[str], repo: pathlib.Path, environment: dict[str, str], tool: str
) -> tuple[float, bytes]:
    started = time.perf_counter()
    process = subprocess.run(
        command,
        cwd=repo,
        env=environment,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        timeout=600,
        check=False,
    )
    elapsed_ms = (time.perf_counter() - started) * 1000
    allowed_codes = {0} if tool == "trouve-search" else {0, 1}
    if process.returncode not in allowed_codes:
        stderr = process.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"{tool} failed ({process.returncode}): {stderr}")
    if not process.stdout:
        raise RuntimeError(f"{tool} returned no matches for the benchmark case")
    return elapsed_ms, process.stdout


def first_line(command: list[str]) -> str:
    process = subprocess.run(command, capture_output=True, text=True, check=False)
    output = process.stdout or process.stderr
    return output.splitlines()[0].strip() if output else "unknown"


def git_value(repo: pathlib.Path, *arguments: str) -> str | None:
    process = subprocess.run(
        ["git", "-C", str(repo), *arguments], capture_output=True, text=True, check=False
    )
    return process.stdout.strip() if process.returncode == 0 else None


def source_file_count(repo: pathlib.Path, extensions: tuple[str, ...]) -> int:
    count = 0
    for _, directories, files in os.walk(repo):
        directories[:] = [name for name in directories if name not in EXCLUDED_DIRS]
        count += sum(pathlib.Path(name).suffix.lower() in extensions for name in files)
    return count


def summarize(
    cases: list[dict[str, Any]], input_cost_per_million: float
) -> dict[str, dict[str, float | int]]:
    summary: dict[str, dict[str, float | int]] = {}
    for tool in TOOLS:
        timings = [timing for case in cases for timing in case["tools"][tool]["timings_ms"]]
        tokens = [case["tools"][tool]["estimated_input_tokens"] for case in cases]
        mean_tokens = statistics.mean(tokens)
        summary[tool] = {
            "median_latency_ms": round(statistics.median(timings), 3),
            "mean_output_tokens": round(mean_tokens),
            "suite_output_tokens": sum(tokens),
            "cost_per_1000_searches_usd": round(
                mean_tokens * 1000 * input_cost_per_million / 1_000_000, 6
            ),
        }
    trouve_tokens = summary["trouve-search"]["mean_output_tokens"]
    for values in summary.values():
        values["tokens_relative_to_trouve"] = round(
            float(values["mean_output_tokens"]) / float(trouve_tokens), 3
        )
    return summary


def markdown_report(result: dict[str, Any]) -> str:
    lines = [
        "| Tool | Median warm query | Mean provider input tokens | Relative tokens | Cost / 1k searches |",
        "| --- | ---: | ---: | ---: | ---: |",
    ]
    for tool in TOOLS:
        values = result["summary"][tool]
        lines.append(
            f"| {tool} | {values['median_latency_ms']:.2f} ms | "
            f"{values['mean_output_tokens']:,} | {values['tokens_relative_to_trouve']:.2f}x | "
            f"${values['cost_per_1000_searches_usd']:.4f} |"
        )
    lines.extend(
        [
            "",
            f"One-time trouve index build and first query: {result['trouve_cold_ms']:.2f} ms.",
            f"Cost uses ${result['input_cost_per_million_usd']:.2f} per million provider input tokens.",
        ]
    )
    return "\n".join(lines)


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", nargs="?", type=pathlib.Path, default=ROOT)
    parser.add_argument("--binary", type=pathlib.Path, default=DEFAULT_BINARY)
    parser.add_argument("--cases", type=pathlib.Path, default=DEFAULT_CASES)
    parser.add_argument("--runs", type=int, default=10)
    parser.add_argument("--warmups", type=int, default=1)
    parser.add_argument("--top-k", type=int, default=5)
    parser.add_argument("--max-snippet-lines", type=int, default=10)
    parser.add_argument(
        "--input-cost-per-million",
        type=float,
        default=3.0,
        help="illustrative provider input price in USD (default: 3.0)",
    )
    parser.add_argument(
        "--extension",
        action="append",
        dest="extensions",
        help="source extension to scan; repeat to replace the default set",
    )
    parser.add_argument("--output", type=pathlib.Path, help="write full JSON results")
    args = parser.parse_args(argv)
    if args.runs < 1 or args.warmups < 0 or args.top_k < 1 or args.max_snippet_lines < 0:
        parser.error("runs/top-k must be positive; warmups/snippet lines cannot be negative")
    if args.input_cost_per_million < 0:
        parser.error("input cost cannot be negative")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    repo = args.repo.resolve()
    binary = args.binary.resolve()
    cases_path = args.cases.resolve()
    if not repo.is_dir():
        sys.exit(f"repository directory not found: {repo}")
    if not binary.is_file():
        sys.exit(f"trouve-search binary not found: {binary}; run cargo build --release -p trouve-search")
    for executable in ("grep", "rg"):
        if shutil.which(executable) is None:
            sys.exit(f"required executable not found: {executable}")

    extensions = tuple(
        value if value.startswith(".") else f".{value}"
        for value in (args.extensions or DEFAULT_EXTENSIONS)
    )
    try:
        cases = load_cases(cases_path)
    except ValueError as error:
        sys.exit(str(error))

    environment = os.environ.copy()
    environment["TOKENIZERS_PARALLELISM"] = "false"
    case_results: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="trouve-search-comparison-") as cache:
        environment["TROUVE_CACHE_LOCATION"] = cache
        cold_command = command_for(
            "trouve-search", cases[0], binary, extensions, args.top_k, args.max_snippet_lines
        )
        cold_ms, _ = invoke(cold_command, repo, environment, "trouve-search")

        for case in cases:
            tool_results: dict[str, Any] = {}
            for tool in TOOLS:
                command = command_for(
                    tool, case, binary, extensions, args.top_k, args.max_snippet_lines
                )
                for _ in range(args.warmups):
                    invoke(command, repo, environment, tool)
                timings: list[float] = []
                output = b""
                for _ in range(args.runs):
                    elapsed_ms, output = invoke(command, repo, environment, tool)
                    timings.append(elapsed_ms)
                tool_results[tool] = {
                    "command": command,
                    "timings_ms": [round(value, 3) for value in timings],
                    "median_latency_ms": round(statistics.median(timings), 3),
                    "output_bytes": len(output),
                    "estimated_input_tokens": estimated_input_tokens(output),
                }
            case_results.append(
                {
                    "name": case.name,
                    "intent": case.intent,
                    "pattern": case.pattern,
                    "tools": tool_results,
                }
            )

    result = {
        "schema_version": 1,
        "repository": str(repo),
        "git_commit": git_value(repo, "rev-parse", "HEAD"),
        "source_files": source_file_count(repo, extensions),
        "runs": args.runs,
        "warmups": args.warmups,
        "top_k": args.top_k,
        "max_snippet_lines": args.max_snippet_lines,
        "extensions": extensions,
        "input_cost_per_million_usd": args.input_cost_per_million,
        "token_estimate": "ceil(UTF-8 characters / 4)",
        "machine": {
            "platform": platform.platform(),
            "processor": platform.processor() or platform.machine(),
            "logical_cpus": os.cpu_count(),
        },
        "versions": {
            "trouve-search": first_line([str(binary), "--version"]),
            "grep": first_line(["grep", "--version"]),
            "ripgrep": first_line(["rg", "--version"]),
        },
        "trouve_cold_ms": round(cold_ms, 3),
        "cases": case_results,
    }
    result["summary"] = summarize(case_results, args.input_cost_per_million)
    if args.output:
        output = args.output.resolve()
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(markdown_report(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
