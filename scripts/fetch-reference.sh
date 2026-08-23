#!/usr/bin/env bash
# Fetch the audited upstream Python semble implementation into reference/ for
# parity testing and benchmarking. The clone is not committed. Override
# SEMBLE_REFERENCE_REF=main to run a rolling comparison against upstream HEAD.
set -euo pipefail
cd "$(dirname "$0")/.."

REFERENCE_URL="https://github.com/MinishLab/semble"
AUDITED_REFERENCE_REF="921849164e2632dd4f0e1c1370f82cfe15ed6d6c"
REFERENCE_REF="${SEMBLE_REFERENCE_REF:-$AUDITED_REFERENCE_REF}"

if [ ! -d reference/semble/.git ]; then
    mkdir -p reference
    git clone --depth 1 --filter=blob:none --no-checkout "$REFERENCE_URL" reference/semble
fi
git -C reference/semble fetch --depth 1 origin "$REFERENCE_REF"
git -C reference/semble checkout --detach FETCH_HEAD
git -C reference/semble log -1 --format='reference/semble at %H (%s)'
