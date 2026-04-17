#!/usr/bin/env bash
# Compares current analyzer output against baseline. Semantic check only:
# same node-kind counts and same edge count. Node IDs are allowed to shift.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo run --quiet --example analyzer

# Extract node-kind label frequencies (from the `[label="…"]` attribute, first line).
kind_counts() {
    grep -oE 'label="[^"\\]*' "$1" | sort | uniq -c | sort -k2
}

edge_count() {
    grep -cE '^\s*"[0-9]+"\s*->\s*"[0-9]+"' "$1"
}

diff <(kind_counts scripts/baseline/graph.dot) <(kind_counts graph.dot) \
    || { echo "graph.dot node-kind counts differ"; exit 1; }

b_edges=$(edge_count scripts/baseline/graph.dot)
c_edges=$(edge_count graph.dot)
[ "$b_edges" = "$c_edges" ] || { echo "graph.dot edge count differs: $b_edges vs $c_edges"; exit 1; }

echo "analyzer output semantically equivalent to baseline"
