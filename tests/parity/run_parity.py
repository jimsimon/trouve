#!/usr/bin/env python3
"""Golden parity harness: compare the Rust port against upstream Python semble.

Compares, over real source files from the upstream repository:
  1. Chunk boundaries (start/end lines + content) — must match exactly.
  2. BM25 tokenization — must match exactly.
  3. BM25 scores (lucene variant) — must match within 1e-4 relative error.
  4. End-to-end search results — top-1 file agreement and top-5 overlap.

Usage:
  python3 tests/parity/run_parity.py --binary target/release/semble \
      [--reference reference/semble] [--skip-search]

The search comparison downloads the potion-code-16M model; pass --skip-search
for offline runs.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

TOKENIZE_SAMPLES = [
    "HandlerStack",
    "getHTTPResponse",
    "XMLParser",
    "my_func",
    "_private",
    "simple",
    "utf8Decode",
    "def save_model(path): return None",
    "class DatabaseConnectionPool:",
    "fn process_request(req: &Request) -> Response {",
    "SELECT * FROM users WHERE id = 42",
    "",
    "     ",
    "123 456",
]

SEARCH_QUERIES = [
    "tokenize identifiers for bm25",
    "chunk source code with tree sitter",
    "walk files respecting gitignore",
    "apply_query_boost",
    "rerank_topk",
    "reciprocal rank fusion",
    "save index to cache",
    "detect language from file extension",
    "SembleIndex",
    "MCP server tools",
]


def sh(cmd: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, capture_output=True, text=True, **kwargs)


def sample_files(reference: Path, limit: int = 60) -> list[Path]:
    root = reference / "src"
    files = sorted(p for p in root.rglob("*.py") if p.is_file())
    files += sorted((reference / "tests").rglob("*.py"))
    files += sorted(reference.rglob("*.toml"))[:5]
    files += sorted(reference.rglob("*.md"))[:5]
    return files[:limit]


def parity_chunks(binary: Path, reference: Path) -> int:
    sys.path.insert(0, str(reference / "src"))
    from semble.chunking.chunking import chunk_source  # noqa: PLC0415
    from semble.index.files import detect_language  # noqa: PLC0415

    failures = 0
    files = sample_files(reference)
    for path in files:
        source = path.read_text(encoding="utf-8", errors="replace")
        language = detect_language(path)
        expected = chunk_source(source, str(path), language)
        proc = sh([str(binary), "debug", "chunk", str(path)])
        if proc.returncode != 0:
            print(f"  FAIL {path}: rust exited {proc.returncode}: {proc.stderr.strip()}")
            failures += 1
            continue
        actual = json.loads(proc.stdout)
        exp = [(c.start_line, c.end_line, c.content) for c in expected]
        act = [(c["start_line"], c["end_line"], c["content"]) for c in actual]
        if exp != act:
            failures += 1
            print(f"  FAIL {path}: {len(exp)} python chunks vs {len(act)} rust chunks")
            for i, (e, a) in enumerate(zip(exp, act)):
                if e != a:
                    print(f"    first divergence at chunk {i}:")
                    print(f"      python: lines {e[0]}-{e[1]} {e[2][:80]!r}")
                    print(f"      rust:   lines {a[0]}-{a[1]} {a[2][:80]!r}")
                    break
    print(f"chunk parity: {len(files) - failures}/{len(files)} files identical")
    return failures


def parity_tokenize(binary: Path, reference: Path) -> int:
    sys.path.insert(0, str(reference / "src"))
    from semble.tokens import tokenize  # noqa: PLC0415

    failures = 0
    samples = list(TOKENIZE_SAMPLES)
    # Also tokenize whole real files for broad coverage.
    for path in sample_files(reference, limit=20):
        samples.append(path.read_text(encoding="utf-8", errors="replace"))

    for text in samples:
        expected = tokenize(text)
        proc = sh([str(binary), "debug", "tokenize", text or ""])
        actual = json.loads(proc.stdout)
        if expected != actual:
            failures += 1
            preview = text[:60].replace("\n", "\\n")
            print(f"  FAIL tokenize({preview!r})")
            for i, (e, a) in enumerate(zip(expected, actual)):
                if e != a:
                    print(f"    first divergence at token {i}: python={e!r} rust={a!r}")
                    break
            if len(expected) != len(actual):
                print(f"    lengths: python={len(expected)} rust={len(actual)}")
    print(f"tokenize parity: {len(samples) - failures}/{len(samples)} samples identical")
    return failures


def parity_bm25(binary: Path, reference: Path) -> int:
    sys.path.insert(0, str(reference / "src"))
    import bm25s  # noqa: PLC0415
    import numpy as np  # noqa: PLC0415
    from semble.tokens import tokenize  # noqa: PLC0415

    docs: list[str] = []
    for path in sample_files(reference, limit=25):
        text = path.read_text(encoding="utf-8", errors="replace")
        docs.extend(text[i : i + 700] for i in range(0, min(len(text), 3500), 700))
    corpus_tokens = [tokenize(d) for d in docs]

    retriever = bm25s.BM25()
    retriever.index(corpus_tokens, show_progress=False)

    queries = ["tokenize identifiers", "chunk boundary merge", "def search", "index cache"]
    failures = 0
    for query in queries:
        q_tokens = tokenize(query)
        expected = retriever.get_scores(q_tokens)
        proc = sh([str(binary), "debug", "bm25", query], input=json.dumps(docs))
        actual = np.array(json.loads(proc.stdout))
        if expected.shape != actual.shape:
            print(f"  FAIL bm25({query!r}): shape {expected.shape} vs {actual.shape}")
            failures += 1
            continue
        denom = np.maximum(np.abs(expected), 1e-9)
        rel = np.max(np.abs(expected - actual) / denom) if len(expected) else 0.0
        if rel > 1e-4:
            failures += 1
            worst = int(np.argmax(np.abs(expected - actual) / denom))
            print(
                f"  FAIL bm25({query!r}): max rel err {rel:.2e} at doc {worst} "
                f"(python={expected[worst]:.6f} rust={actual[worst]:.6f})"
            )
    print(f"bm25 parity: {len(queries) - failures}/{len(queries)} queries within 1e-4")
    return failures


def parity_search(binary: Path, reference: Path) -> int:
    sys.path.insert(0, str(reference / "src"))
    from semble.index import SembleIndex  # noqa: PLC0415

    print("building python index (downloads model on first run)...")
    index = SembleIndex.from_path(str(reference / "src"))

    failures = 0
    top1_agree = 0
    overlaps = []
    for query in SEARCH_QUERIES:
        py_results = index.search(query, top_k=5)
        py_files = [r.chunk.file_path for r in py_results]
        proc = sh(
            [str(binary), "search", query, str(reference / "src"), "--top-k", "5",
             "--max-snippet-lines", "0"],
        )
        if proc.returncode != 0:
            print(f"  FAIL search({query!r}): {proc.stderr.strip()}")
            failures += 1
            continue
        rs_files = [r["file_path"] for r in json.loads(proc.stdout).get("results", [])]
        # Python stores absolute-ish relative paths from its own walk root;
        # compare basenames + parents to be layout-independent.
        norm = lambda p: "/".join(Path(p).parts[-3:])  # noqa: E731
        py_norm = [norm(p) for p in py_files]
        rs_norm = [norm(p) for p in rs_files]
        overlap = len(set(py_norm) & set(rs_norm)) / max(len(py_norm), 1)
        overlaps.append(overlap)
        agree = bool(py_norm and rs_norm and py_norm[0] == rs_norm[0])
        top1_agree += agree
        marker = "ok " if overlap >= 0.6 else "LOW"
        print(f"  [{marker}] overlap@5={overlap:.0%} top1_agree={agree} q={query!r}")
        if overlap < 0.4:
            failures += 1
            print(f"        python: {py_norm}")
            print(f"        rust:   {rs_norm}")

    mean_overlap = sum(overlaps) / len(overlaps) if overlaps else 0.0
    print(
        f"search parity: mean overlap@5 {mean_overlap:.0%}, "
        f"top-1 agreement {top1_agree}/{len(SEARCH_QUERIES)}"
    )
    if mean_overlap < 0.6:
        print("  FAIL: mean overlap@5 below 60%")
        failures += 1
    return failures


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, default=REPO_ROOT / "target/release/semble")
    parser.add_argument("--reference", type=Path, default=REPO_ROOT / "reference/semble")
    parser.add_argument("--skip-search", action="store_true")
    args = parser.parse_args()

    if not args.binary.exists():
        print(f"binary not found: {args.binary} (run: cargo build --release)")
        return 2
    if not args.reference.exists():
        print(f"reference not found: {args.reference} (run: ./scripts/fetch-reference.sh)")
        return 2

    failures = 0
    print("== chunk boundaries ==")
    failures += parity_chunks(args.binary, args.reference)
    print("\n== tokenization ==")
    failures += parity_tokenize(args.binary, args.reference)
    print("\n== bm25 scoring ==")
    failures += parity_bm25(args.binary, args.reference)
    if not args.skip_search:
        print("\n== end-to-end search ==")
        failures += parity_search(args.binary, args.reference)

    print(f"\n{'PARITY OK' if failures == 0 else f'PARITY FAILURES: {failures}'}")
    return 0 if failures == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
