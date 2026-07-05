#!/usr/bin/env bash
# Speed benchmarks: trouve vs upstream Python semble, using hyperfine.
#
# Measures on a target git repository:
#   1. cold index      (empty cache -> full index + first query)
#   2. warm query      (fully cached index -> query)
#   3. incremental     (one file touched -> reindex + query)
#   4. branch switch   (checkout other branch -> reindex + query)
#
# Usage: benchmarks/run_benchmarks.sh [REPO_DIR]
# Defaults to a pinned clone of pallets/flask under benchmarks/repos/.
# Env: RUNS (default 3), TROUVE_MODEL_NAME for trouve / SEMBLE_MODEL_NAME for
# the Python baseline (default: hub download).
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="${1:-benchmarks/repos/flask}"
RUST_BIN="$PWD/target/release/trouve-search"
PY_BIN="$PWD/.venv/bin/semble"
RUNS="${RUNS:-3}"
QUERY="handle http request routing"
ARGS="-k 5 --max-snippet-lines 0"

command -v hyperfine >/dev/null || { echo "hyperfine is required"; exit 1; }
[ -x "$RUST_BIN" ] || { echo "build first: cargo build --release"; exit 1; }
[ -x "$PY_BIN" ] || { echo "install python semble: python3 -m venv .venv && .venv/bin/pip install './reference/semble[mcp]'"; exit 1; }

if [ ! -d "$REPO" ]; then
    mkdir -p "$(dirname "$REPO")"
    git clone --quiet https://github.com/pallets/flask "$REPO"
    git -C "$REPO" checkout --quiet -b bench-base 3.1.0
    git -C "$REPO" checkout --quiet -b bench-alt 3.0.0
    git -C "$REPO" checkout --quiet bench-base
fi
git -C "$REPO" checkout --quiet bench-base

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
RUST_CACHE="$WORK/rust-cache"
PY_CACHE="$WORK/py-cache"
export TOKENIZERS_PARALLELISM=false

RUST="TROUVE_CACHE_LOCATION='$RUST_CACHE' '$RUST_BIN' search '$QUERY' '$REPO' $ARGS"
PY="SEMBLE_CACHE_LOCATION='$PY_CACHE' '$PY_BIN' search '$QUERY' '$REPO' $ARGS"

echo "== target repo: $REPO ($(git -C "$REPO" ls-files | wc -l) tracked files) =="
mkdir -p benchmarks/results

echo; echo "== 1. cold index + query =="
hyperfine --runs "$RUNS" --export-json benchmarks/results/cold.json \
    --prepare "rm -rf '$RUST_CACHE'" -n "rust cold" "$RUST" \
    --prepare "rm -rf '$PY_CACHE'" -n "python cold" "$PY"

echo; echo "== 2. warm query (index fully cached) =="
eval "$RUST > /dev/null" && eval "$PY > /dev/null"
hyperfine --runs "$((RUNS * 3))" --export-json benchmarks/results/warm.json \
    -n "rust warm" "$RUST" \
    -n "python warm" "$PY"

echo; echo "== 3. incremental (one file touched between runs) =="
TOUCH_FILE="$REPO/$(git -C "$REPO" ls-files '*.py' | head -1)"
hyperfine --runs "$RUNS" --export-json benchmarks/results/incremental.json \
    --prepare "printf '\n# bench %s\n' \$RANDOM >> '$TOUCH_FILE'" -n "rust incremental" "$RUST" \
    --prepare "printf '\n# bench %s\n' \$RANDOM >> '$TOUCH_FILE'" -n "python incremental" "$PY"
git -C "$REPO" checkout --quiet -- .

echo; echo "== 4. branch switch (checkout other branch, then query) =="
# Pre-warm caches on both branches; the rust store is content-addressed so a
# revisited branch is a pure cache hit, while python invalidates on mtime.
for b in bench-base bench-alt; do
    git -C "$REPO" checkout --quiet "$b"
    eval "$RUST > /dev/null" && eval "$PY > /dev/null"
done
git -C "$REPO" checkout --quiet bench-base
hyperfine --runs "$RUNS" --export-json benchmarks/results/branch.json \
    --prepare "git -C '$REPO' checkout --quiet bench-base" \
    -n "rust branch-switch" "git -C '$REPO' checkout --quiet bench-alt && $RUST" \
    --prepare "git -C '$REPO' checkout --quiet bench-base" \
    -n "python branch-switch" "git -C '$REPO' checkout --quiet bench-alt && $PY"
git -C "$REPO" checkout --quiet bench-base

echo; echo "results saved under benchmarks/results/"
