# Round 9 / 2A — Production-panic audit

**Branch:** `feature/ai`. Independent re-derivation; round-7/round-8 not consulted.

## Scope

Every `unwrap()`, `expect()`, `panic!()`, `unreachable!()`, `assert!()`, `debug_assert!()`, `todo!()`, `unimplemented!()` in production `crates/*/src/`. Excluded: `tests/`, `examples/`, `benches/`, inline `#[cfg(test)]` modules.

Workspace clippy denies: `clippy::unwrap_used`, `clippy::expect_used`, `clippy::panic`, `clippy::unreachable`, `clippy::todo`. `assert!` and `debug_assert!` are not denied.

## Findings

| Severity | Count |
|----------|-------|
| HIGH — unjustified panic on in-spec input | 0 |
| MED — justified but missing `#[allow]` or comment | 0 |
| LOW — documentation drift | 0 |

**No actionable findings.**

## Inventory of all 8 production panic sites

| # | Where | Kind | Justified | Annotated |
|---|-------|------|-----------|-----------|
| 1 | `crates/ir/src/graph/compact.rs:116-118` | expect | Pass-1 populates remap; pass-2 lookups can't fail | `#[allow]` ✓ |
| 2 | `crates/ir/src/graph/compact.rs:126-129` | expect | Same invariant | `#[allow]` ✓ |
| 3 | `crates/ir/src/function.rs:170-173` | expect | Entry node always in own reachable set | `#[allow]` ✓ |
| 4 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:134-137` | expect | match_at returned Some; lhs_capture binds value-output | `#[allow]` ✓ |
| 5 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:140-143` | expect | Same for rhs_capture | `#[allow]` ✓ |
| 6 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:177-178` | expect | create_node with single-output kind slice | `#[allow]` ✓ |
| 7 | `crates/opt/src/flag_cmp_canonicalize/mod.rs:191-192` | expect | Same for BoolNeg | `#[allow]` ✓ |
| 8 | `crates/opt/src/indirect_branch_resolve/inplace.rs:63-66` | debug_assert | IndirectBranch must have ≥3 inputs (control+memory+target) | n/a (debug_assert) |

All 8 sites have inline comments naming the invariant. Site 8 is new relative to round 8 (round 8 didn't enumerate `debug_assert!`). All 5 round-8 sites unchanged.

## Coverage

~105 production source files inspected across all 11 crates. Files with zero panic sites: `cfg`, `dot`, `entity-utils`, `graphwalk`, `pcode-lift`, `pattern`, `reader`, `strider`, `strider-py`, `target`. All 8 sites are in `ir` (3) and `opt` (5).

**HIGH: 0 / MED: 0 / LOW: 0.**
