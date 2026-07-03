#!/usr/bin/env bash
# CI end-to-end benchmark: rust-only scenarios on a pinned flask checkout,
# exported as hyperfine JSON for regression gating (see workflows/bench.yml).
#
# Scenarios (full CLI invocations, like run_benchmarks.sh):
#   1. cold index + query   (empty store)
#   2. warm query           (nothing changed, snapshot mmap path)
#   3. incremental          (one file modified -> patch + query)
#   4. non-git warm query   (same tree without .git, filesystem manifest)
#
# Usage: benchmarks/run_ci_bench.sh [OUT_DIR]
# Env: RUNS (default 5), TROUVE_MODEL_NAME for a local model copy.
set -euo pipefail
cd "$(dirname "$0")/.."

OUT="${1:-benchmarks/results/ci}"
RUST_BIN="$PWD/target/release/trouve"
RUNS="${RUNS:-5}"
QUERY="handle http request routing"
REPO="benchmarks/repos/flask-ci"

command -v hyperfine >/dev/null || { echo "hyperfine is required"; exit 1; }
[ -x "$RUST_BIN" ] || { echo "build first: cargo build --release"; exit 1; }

if [ ! -d "$REPO" ]; then
    mkdir -p "$(dirname "$REPO")"
    git clone --quiet --depth 1 --branch 3.1.0 https://github.com/pallets/flask "$REPO"
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
NOGIT="$WORK/nogit"
cp -a "$REPO" "$NOGIT"
rm -rf "$NOGIT/.git"

CACHE="$WORK/cache"
NOGIT_CACHE="$WORK/cache-nogit"
TOUCH_FILE="$REPO/src/flask/app.py"

# Command strings for hyperfine (which runs them through a shell), with every
# interpolated value shell-escaped. Warm-up invocations outside hyperfine
# call the binary directly instead.
q() { printf '%q' "$1"; }
CMD="TROUVE_CACHE_LOCATION=$(q "$CACHE") $(q "$RUST_BIN") search $(q "$QUERY") $(q "$REPO") -k 5 --max-snippet-lines 0 > /dev/null"
NOGIT_CMD="TROUVE_CACHE_LOCATION=$(q "$NOGIT_CACHE") $(q "$RUST_BIN") search $(q "$QUERY") $(q "$NOGIT") -k 5 --max-snippet-lines 0 > /dev/null"
run_search() { # <cache-dir> <repo-root>
    TROUVE_CACHE_LOCATION="$1" "$RUST_BIN" search "$QUERY" "$2" -k 5 --max-snippet-lines 0 > /dev/null
}

mkdir -p "$OUT"

hyperfine --runs "$RUNS" --export-json "$OUT/cold.json" \
    --prepare "rm -rf $(q "$CACHE")" -n "cold index + query" "$CMD"

run_search "$CACHE" "$REPO"
hyperfine --warmup 1 --runs "$((RUNS * 2))" --export-json "$OUT/warm.json" \
    -n "warm query" "$CMD"

hyperfine --runs "$RUNS" --export-json "$OUT/incremental.json" \
    --prepare "printf '\n# bench %s\n' \$RANDOM >> $(q "$TOUCH_FILE")" \
    -n "incremental (1 file modified)" "$CMD"
git -C "$REPO" checkout --quiet -- .

run_search "$NOGIT_CACHE" "$NOGIT"
hyperfine --warmup 1 --runs "$((RUNS * 2))" --export-json "$OUT/nogit-warm.json" \
    -n "non-git warm query" "$NOGIT_CMD"

python3 benchmarks/to_gha_bench.py \
    --hyperfine "$OUT/cold.json" "$OUT/warm.json" "$OUT/incremental.json" "$OUT/nogit-warm.json" \
    > "$OUT/e2e.json"
echo "wrote $OUT/e2e.json"
