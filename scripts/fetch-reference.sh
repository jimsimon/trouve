#!/usr/bin/env bash
# Fetch the upstream Python semble implementation into reference/ for
# parity testing and benchmarking. The clone is not committed.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -d reference/semble/.git ]; then
    git -C reference/semble pull --ff-only
else
    mkdir -p reference
    git clone --depth 50 https://github.com/MinishLab/semble reference/semble
fi
git -C reference/semble log -1 --format='reference/semble at %H (%s)'
