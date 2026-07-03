#!/usr/bin/env bash
# Git vs non-git roots: the same tree indexed as a git checkout and as a
# plain folder (a copy with `.git` removed), measuring the cost of the
# filesystem-manifest fallback relative to the git manifest.
#
# Measures with hyperfine:
#   1. cold index    (empty store -> full index + first query)
#   2. warm query    (nothing changed)
#   3. incremental   (one file modified -> reindex + query)
#   4. touch-only    (mtime bumped, content unchanged -> manifest fast path)
#
# Usage: benchmarks/run_git_vs_nogit.sh [REPO_DIR]
# Defaults to a shallow clone of kubernetes/kubernetes under benchmarks/repos/.
# Env: RUNS (default 3), TROUVE_MODEL_NAME for a local model copy.
set -euo pipefail
cd "$(dirname "$0")/.."

REPO="${1:-benchmarks/repos/kubernetes}"
RUST_BIN="$PWD/target/release/trouve"
RUNS="${RUNS:-3}"
QUERY="handle http request routing"
ARGS="-k 5 --max-snippet-lines 0"

command -v hyperfine >/dev/null || { echo "hyperfine is required"; exit 1; }
[ -x "$RUST_BIN" ] || { echo "build first: cargo build --release"; exit 1; }

if [ ! -d "$REPO" ]; then
    mkdir -p "$(dirname "$REPO")"
    git clone --quiet --depth 1 https://github.com/kubernetes/kubernetes "$REPO"
fi
git -C "$REPO" rev-parse --git-dir >/dev/null || { echo "$REPO is not a git repo"; exit 1; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# The non-git variant: an identical tree with no .git directory. cp -a
# preserves mtimes, so both variants start from the same filesystem state.
NOGIT="$WORK/nogit"
cp -a "$REPO" "$NOGIT"
rm -rf "$NOGIT/.git"

GIT_CACHE="$WORK/cache-git"
NOGIT_CACHE="$WORK/cache-nogit"
GIT_CMD="TROUVE_CACHE_LOCATION='$GIT_CACHE' '$RUST_BIN' search '$QUERY' '$REPO' $ARGS > /dev/null"
NOGIT_CMD="TROUVE_CACHE_LOCATION='$NOGIT_CACHE' '$RUST_BIN' search '$QUERY' '$NOGIT' $ARGS > /dev/null"

# `|| true` guards against pipefail turning head's early exit (SIGPIPE on
# large repos) into a script failure.
TOUCH_REL="$(git -C "$REPO" ls-files '*.go' '*.py' '*.rs' '*.ts' '*.js' | head -1 || true)"
[ -n "$TOUCH_REL" ] || { echo "no code file found to modify in $REPO"; exit 1; }

echo "== target repo: $REPO ($(git -C "$REPO" ls-files | wc -l) tracked files) =="
mkdir -p benchmarks/results

echo; echo "== 1. cold index + query (empty store) =="
hyperfine --runs "$RUNS" --export-json benchmarks/results/git_vs_nogit_cold.json \
    --prepare "rm -rf '$GIT_CACHE'" -n "git cold" "$GIT_CMD" \
    --prepare "rm -rf '$NOGIT_CACHE'" -n "non-git cold" "$NOGIT_CMD"

echo; echo "== 2. warm query (nothing changed) =="
eval "$GIT_CMD" && eval "$NOGIT_CMD"
hyperfine --runs "$((RUNS * 3))" --export-json benchmarks/results/git_vs_nogit_warm.json \
    -n "git warm" "$GIT_CMD" \
    -n "non-git warm" "$NOGIT_CMD"

echo; echo "== 3. incremental (one file modified between runs) =="
hyperfine --runs "$RUNS" --export-json benchmarks/results/git_vs_nogit_incremental.json \
    --prepare "printf '\n// bench %s\n' \$RANDOM >> '$REPO/$TOUCH_REL'" -n "git incremental" "$GIT_CMD" \
    --prepare "printf '\n// bench %s\n' \$RANDOM >> '$NOGIT/$TOUCH_REL'" -n "non-git incremental" "$NOGIT_CMD"
git -C "$REPO" checkout --quiet -- "$TOUCH_REL"

echo; echo "== 4. touch-only (mtime bumped, content unchanged) =="
eval "$GIT_CMD" && eval "$NOGIT_CMD"
hyperfine --runs "$RUNS" --export-json benchmarks/results/git_vs_nogit_touch.json \
    --prepare "touch '$REPO/$TOUCH_REL'" -n "git touch-only" "$GIT_CMD" \
    --prepare "touch '$NOGIT/$TOUCH_REL'" -n "non-git touch-only" "$NOGIT_CMD"

echo; echo "results saved under benchmarks/results/"
