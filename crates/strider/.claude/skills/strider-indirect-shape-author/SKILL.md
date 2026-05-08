---
name: strider-indirect-shape-author
description: Add a new indirect-branch dispatch shape to the strider resolver across opt classifier, in-place rewrite, orchestrator Decision, and a fixture binary.
---

# strider-indirect-shape-author

## When to use

User wants to teach the indirect-branch resolver about a new dispatch shape. Triggers include "add a new indirect-branch shape: ...", "resolve indirect calls that index a global function table", "BranchIndirect placeholder isn't classified — I see `UnresolvedIndirectBranch`", "this jump table format isn't recognised".

## When NOT to use

- The shape is already classified but the in-place edit produces wrong IR — debug the existing inplace step, do not add a new arm.
- The branch is direct (constant target) — that is CFG / lift-time territory, not the resolver.
- The shape is recognised but a fixture-specific fingerprint or KnownBits issue blocks resolution — diagnose that instead.

## Inputs the skill expects

- A small fixture binary that exhibits the shape (under `fixtures/`, with a `Makefile` build rule).
- A walked example: which producer node feeds the placeholder? Get this from the placeholder's anchored output via `classify_anchor` in `crates/strider/src/indirect_resolve/classify.rs`.

## Procedure

1. Build a fixture in `fixtures/` with a `Makefile` rule. Keep it minimal — one function exhibiting one shape, ELF in `fixtures/out/<arch>/`.
2. Confirm tier-1 doesn't classify it. The cfg-time mini-graph in `cfg::indirect_resolve` runs the opt pipeline locally; if it can't classify, the placeholder propagates to tier 2 (the orchestrator's fixed-point loop).
3. Add a classifier arm in `crates/opt/src/indirect_branch_resolve/<new_shape>.rs`, mirroring `jump_table.rs` / `stack_array.rs`. The arm takes a producer `NodeOutputId` (the value feeding `BranchIndirect`), walks the producer subgraph iteratively (no recursion — see CLAUDE.md note about the 8 MB Rust stack), and returns `Option<ResolvedTargets>` (`LinkRegister`, `Single`, or `Multiple`). Respect `MAX_TABLE_ENTRIES = 4096` for any enumeration step.
4. Wire into `classify_anchor` in `crates/opt/src/indirect_branch_resolve/classify.rs`. Try the new shape after the existing arms. Order matters: cheap classifiers first.
5. Add an in-place edit if appropriate. `crates/opt/src/indirect_branch_resolve/inplace.rs` plus the orchestrator-level bridge in `crates/strider/src/indirect_resolve/inplace.rs` host: `LinkRegister` -> `apply_link_register` (append ABI ret-val regs to placeholder Return); `Single` (tail call) -> `apply_tail_call` (rewrite as `Call` + `Return` chain at the tail); `Multiple` (jump table) -> leave for orchestrator CFG rebuild via `ResolvedTargets` propagation.
6. Wire the orchestrator `Decision` (`crates/strider/src/orchestrator.rs::LoopState`). In-place edits drive `Decision::StableOnly` (rerun stable pipeline, no rebuild). Multiple-target tables drive `Decision::Rebuild` (rebuild CFG with `with_known_targets` map). All anchors resolved with no edits drives `Decision::FixedPoint`.
7. Add tests. `crates/opt/src/indirect_branch_resolve/<new_shape>_tests.rs` for graph-mock unit tests of the classifier. `crates/strider/tests/<shape>_test.rs` for end-to-end against the fixture ELF. Add a cap-respecting test: a mocked oversized table must return `None`.

## Verification

- `cargo test --package opt indirect_branch_resolve`.
- `cargo test --package strider`.
- `cargo test --package strider <new_fixture_test>`.
- `uv run pytest crates/strider-py/tests/python/test_indirect_branch_debug.py` if a Python-visible behaviour changed.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- The fixture binary lifts to a fully-resolved IR (no `UnresolvedIndirectBranchError`).
- No regression in existing switch / jump-table tests under `cargo test --package strider`.
- Fingerprint audit (`strider-fingerprint-audit`) passes on the new shape — the in-place rewrite must propagate fingerprints from the placeholder.
- Cap-respecting test exercises the `MAX_TABLE_ENTRIES = 4096` bound.

## Pitfalls

- Recursing without a bound. Convert all walks to iterative `Vec<...>` worklists or add a `MAX_DEPTH` guard — pathological producer subgraphs blow the 8 MB Rust stack.
- Skipping `MAX_TABLE_ENTRIES = 4096`. Buggy `KnownBits` masks can otherwise force a 4 GiB enumeration.
- Adding a new shape to tier 2 when tier 1 (cfg-time) could handle it. Tier 1 sees only the single region but is much cheaper; prefer tier 1 when the shape is region-local.
- Forgetting to wire the orchestrator `Decision`. The loop will diverge or oscillate forever.
- Re-running `RedundantPhis` / `DeadBranchElimination` mid-fixedpoint. They invalidate the per-iteration `RegionIndex`. Only the destructive-default pipeline runs them, and only at fixed-point exit.
- Not propagating fingerprints across the in-place rewrite. The new `Call` / `Return` / target nodes must inherit from the placeholder's contributors.

## Related skills

- `strider-fingerprint-audit` — mandatory final step.
- `strider-callother-abi` — if the new shape pulls in user-ops not yet classified.
- `strider-pattern-author` — patterns over indirect-resolved IR.
