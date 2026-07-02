# Production-flow simplification pass

## Goal

Reduce production CLOC across the strider pipeline by removing code not on
any live path and by behavior-preserving shrinkage, **without** degrading
big-O runtime or correctness.

## Scope

In: `strider-ir`, `strider-target`, `strider-reader`, `strider-cfg`,
`strider-lift`, `strider-pattern`, `strider-opt`, `strider-orchestrator`, and
the generic libs `dot`, `entity-utils`, `graphwalk`, `strider-graph`,
`vn-container`, `read-only-memory`.

Out: `strider-py` (its public surface is the external Python API — "unused in
Rust" is not a safe signal) and `strider-ir-test-utils` (test-only).

Only production code is touched — all `#[cfg(test)]` modules and test files are
left alone.

## Method for finding candidates

1. **Compiler/lint sweep** — `dead_code` + `unused` workspace-wide. Newly
   effective after the recent visibility tightening: genuinely-unused
   `pub(crate)`/private fns, fields, variants, and unused params now surface.
2. **Unused deps** — `cargo-machete` / manual `Cargo.toml` audit.
3. **Manual structural review** — single-impl traits / one-call-site
   abstractions, needless wrapper indirection, dead match arms, duplicated
   logic collapsible via an existing helper, over-general signatures, redundant
   clones/allocations.

## Guardrails

- **Big-O:** any change touching a loop / collection / hot path carries a
  stated before→after complexity; nothing that worsens it is applied. Pure
  removals and inlining do not change complexity.
- **Correctness gate (per batch):** `cargo test --workspace` (baseline
  3250/0), `cargo clippy` clean, `pytest` (baseline 873 passed / 1 skipped)
  must stay green.

## Process

Findings-first: produce a ranked findings list (location, what to cut, why it
is safe, CLOC saved, complexity note) and get approval **before** applying.
Then apply in coherent batches, verifying after each, with a running CLOC tally
and a final report.
