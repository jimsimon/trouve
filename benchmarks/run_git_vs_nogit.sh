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

# Resolve a caller-supplied repo path against the caller's cwd before cd'ing
# to the project root. Only the default location is ever cloned into; a
# user-supplied path must already exist.
REPO=""
if [ $# -ge 1 ]; then
    REPO="$(cd "$1" && pwd)" || { echo "repo dir not found: $1"; exit 1; }
fi
cd "$(dirname "$0")/.."
if [ -z "$REPO" ]; then
    REPO="$PWD/benchmarks/repos/kubernetes"
    if [ ! -d "$REPO" ]; then
        mkdir -p "$(dirname "$REPO")"
        git clone --quiet --depth 1 https://github.com/kubernetes/kubernetes "$REPO"
    fi
fi

RUST_BIN="$PWD/target/release/trouve-search"
RUNS="${RUNS:-3}"
QUERY="handle http request routing"

command -v hyperfine >/dev/null || { echo "hyperfine is required"; exit 1; }
[ -x "$RUST_BIN" ] || { echo "build first: cargo build --release"; exit 1; }
git -C "$REPO" rev-parse --git-dir >/dev/null || { echo "$REPO is not a git repo"; exit 1; }

WORK="$(mktemp -d)"
TOUCH_REL=""
cleanup() {
    rm -rf "$WORK"
    # Always restore the file the incremental scenario modifies, even when a
    # benchmark run fails partway through.
    if [ -n "$TOUCH_REL" ]; then
        git -C "$REPO" checkout --quiet -- "$TOUCH_REL" || true
    fi
}
trap cleanup EXIT

# The non-git variant: an identical tree with no .git directory. cp -a
# preserves mtimes, so both variants start from the same filesystem state.
NOGIT="$WORK/nogit"
cp -a "$REPO" "$NOGIT"
rm -rf "$NOGIT/.git"

GIT_CACHE="$WORK/cache-git"
NOGIT_CACHE="$WORK/cache-nogit"

# hyperfine runs its command/--prepare strings through a shell, so
# shell-escape every interpolated value: the repo path and the filenames it
# yields via git ls-files are caller-controlled.
q() { printf '%q' "$1"; }
GIT_CMD="TROUVE_CACHE_LOCATION=$(q "$GIT_CACHE") $(q "$RUST_BIN") search $(q "$QUERY") $(q "$REPO") -k 5 --max-snippet-lines 0 > /dev/null"
NOGIT_CMD="TROUVE_CACHE_LOCATION=$(q "$NOGIT_CACHE") $(q "$RUST_BIN") search $(q "$QUERY") $(q "$NOGIT") -k 5 --max-snippet-lines 0 > /dev/null"

# Warm-up invocations outside hyperfine call the binary directly (no shell
# string re-parsing).
run_search() { # <cache-dir> <repo-root>
    TROUVE_CACHE_LOCATION="$1" "$RUST_BIN" search "$QUERY" "$2" -k 5 --max-snippet-lines 0 > /dev/null
}

# `|| true` guards against pipefail turning head's early exit (SIGPIPE on
# large repos) into a script failure.
TOUCH_REL="$(git -C "$REPO" ls-files '*.go' '*.py' '*.rs' '*.ts' '*.js' | head -1 || true)"
[ -n "$TOUCH_REL" ] || { echo "no code file found to modify in $REPO"; exit 1; }

echo "== target repo: $REPO ($(git -C "$REPO" ls-files | wc -l) tracked files) =="
mkdir -p benchmarks/results

echo; echo "== 1. cold index + query (empty store) =="
hyperfine --runs "$RUNS" --export-json benchmarks/results/git_vs_nogit_cold.json \
    --prepare "rm -rf $(q "$GIT_CACHE")" -n "git cold" "$GIT_CMD" \
    --prepare "rm -rf $(q "$NOGIT_CACHE")" -n "non-git cold" "$NOGIT_CMD"

echo; echo "== 2. warm query (nothing changed) =="
run_search "$GIT_CACHE" "$REPO"
run_search "$NOGIT_CACHE" "$NOGIT"
hyperfine --runs "$((RUNS * 3))" --export-json benchmarks/results/git_vs_nogit_warm.json \
    -n "git warm" "$GIT_CMD" \
    -n "non-git warm" "$NOGIT_CMD"

echo; echo "== 3. incremental (one file modified between runs) =="
hyperfine --runs "$RUNS" --export-json benchmarks/results/git_vs_nogit_incremental.json \
    --prepare "printf '\n// bench %s\n' \$RANDOM >> $(q "$REPO/$TOUCH_REL")" -n "git incremental" "$GIT_CMD" \
    --prepare "printf '\n// bench %s\n' \$RANDOM >> $(q "$NOGIT/$TOUCH_REL")" -n "non-git incremental" "$NOGIT_CMD"
git -C "$REPO" checkout --quiet -- "$TOUCH_REL"

echo; echo "== 4. touch-only (mtime bumped, content unchanged) =="
run_search "$GIT_CACHE" "$REPO"
run_search "$NOGIT_CACHE" "$NOGIT"
hyperfine --runs "$RUNS" --export-json benchmarks/results/git_vs_nogit_touch.json \
    --prepare "touch $(q "$REPO/$TOUCH_REL")" -n "git touch-only" "$GIT_CMD" \
    --prepare "touch $(q "$NOGIT/$TOUCH_REL")" -n "non-git touch-only" "$NOGIT_CMD"

echo; echo "results saved under benchmarks/results/"
