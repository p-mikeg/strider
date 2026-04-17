#!/usr/bin/env bash
# Captures analyzer-output baseline for the refactor. Run once before Phase 0.
set -euo pipefail
cd "$(dirname "$0")/.."
OUT_DIR="scripts/baseline"
mkdir -p "$OUT_DIR"
cargo run --quiet --example analyzer
cp cfg.html  "$OUT_DIR/cfg.html"
cp graph.html "$OUT_DIR/graph.html"
cp cfg.dot    "$OUT_DIR/cfg.dot"
cp graph.dot  "$OUT_DIR/graph.dot"
echo "baseline captured in $OUT_DIR"
