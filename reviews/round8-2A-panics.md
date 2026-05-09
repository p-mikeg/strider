# Round 8 / 2A — Production-panic audit

**Branch:** `review/ai2`.  Independent audit.

## Summary

- **Total `expect()` / `unwrap()` / `panic!` / `unreachable!` / `assert!` / `debug_assert!` / `todo!` / `unimplemented!` in non-test code:** 5 occurrences.
- **All 5 are justified:** annotated with `#[allow(clippy::expect_used)]` plus an inline invariant comment.
- **Unjustified panics:** **0** (HIGH: 0 / MED: 0 / LOW: 0).

The workspace production code is panic-free in the actionable sense.  No caller with an in-spec input can trigger a panic.

## Justified production panics — recorded for cross-check

### 1. `Graph::retain_reachable` — pass 2 lookups

- **Where:** `crates/ir/src/graph/compact.rs:116-118`, `:126-129`.
- **Annotation:** `#[allow(clippy::expect_used)]` precedes each call.
- **Invariant:** "Pass 1 (above) installed every reachable node into `remap.nodes`; we are iterating the same `reachable` set, so the lookup cannot return None."
- **Verdict:** Bounded by two-pass algorithm structure.  Sound.

### 2. `BuiltFunctionGraph::compact` — entry remapping

- **Where:** `crates/ir/src/function.rs:170-173`.
- **Annotation:** `#[allow(clippy::expect_used)]`.
- **Invariant:** "`retain_reachable` walks forward from `entry`; the entry node is reachable from itself by definition."
- **Verdict:** Sound — `walk_graph` always begins at `entry`.

### 3. `FlagCmpCanonicalize::try_apply_rule` — capture extraction

- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:128`.
- **Annotation:** `#[allow(clippy::expect_used)]`.
- **Invariant:** "`match_at` succeeded above, and the rule's `lhs` always captures `lhs_capture` at a value-producing position."
- **Verdict:** Sound — every `Rule` in `RULES` has `lhs_capture` in a value-producing slot.

### 4-5. `build_int_cmp` / `build_bool_neg` helpers — single output extraction

- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:161, 175`.
- **Annotation:** `#[allow(clippy::expect_used)]`.
- **Invariant:** "Single Bool output by construction; `node_outputs_exact::<1>` enforces and returns it."
- **Verdict:** Sound — `create_node` is called with a literal `[NodeOutputKind::OutputType(NodeOutputType::Bool)]` slice.

## Coverage

All non-test source files inspected across all 11 crates (~106 files).  Files explicitly excluded: `tests.rs`, `*_tests.rs`, `test_api.rs`, `test_support.rs`, `test_utils.rs`, files under `tests/`, `examples/`, `benches/`.

## Cross-cutting note

The codebase enforces the no-panic discipline via `#![deny(clippy::expect_used, clippy::unwrap_used, clippy::panic)]` (or workspace clippy config equivalents).  Every production `.expect()` or `.unwrap()` is gated by `#[allow(...)]` with explanatory comment, which is the correct pattern.

**No HIGH / MED / LOW findings.**
