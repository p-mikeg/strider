# Round 9 / 2B — Naming sweep

**Branch:** `review/ai3`. Independent audit; round-7/round-8 not re-read.

## Critical — Factually incorrect comments

### C-1: Stale "returns Unimplemented" claim in `indirect_resolve_in_place_edits.rs:10-12`

**Confidence:** 98.

```
//! `apply_tail_call` is round-2 work; tests here only pin its current
//! "returns Unimplemented" contract.
```

`apply_tail_call` is fully implemented. Lines 75-106 in the same file call `.expect("apply_tail_call")` and assert real IR mutations. "Returns Unimplemented" contract does not exist anywhere.

**Fix:** Delete the stale paragraph; replace with description of what the file actually does.

### C-2: Non-existent `Strider::analyze_cfg_with_unresolved` cited

**Confidence:** 95.

**Where:** `crates/strider/tests/indirect_resolve_classify.rs:4` and `crates/strider/tests/indirect_resolve_jump_table.rs:16`.

`Strider::analyze_cfg_with_unresolved` does not exist. The public API is `Strider::analyze_cfg` returning `AnalyzeOutcome` carrying `unresolved_branches`.

**Fix:** Replace `analyze_cfg_with_unresolved` → `analyze_cfg` (with parenthetical noting the `AnalyzeOutcome` carries unresolved_branches).

### C-3: `indirect_branch.rs:14-20` claims cross-region forwarding not yet implemented

**Confidence:** 92.

Doc says "the initial round of the indirect-branch fixed-point design does not yet implement that layer" and "no stack-array-of-labels arm." The stack-array arm IS now implemented; 8 architectures pass this test without `#[ignore]`.

**Fix:** Replace with current-state description (stack-array classifier arm shipped; 7 arches remain `#[ignore]`-ed due to specific lifter shape gaps).

## Important — Opaque milestone labels

### I-1: 9 locations using `(R2)`, `(R3)`, `(R4)`, `R5`, `R1-R5`

**Confidence:** 85.

Opaque references to internal planning rounds in `indirect_branch_lift_placeholder.rs`, `indirect_resolve_classify.rs`, `indirect_resolve_jump_table.rs`, `jump_table_tests.rs`, `abi.rs`. **Fix:** Replace each with descriptive prose (e.g., `the orchestrator (R3)` → `the orchestrator`).

### I-2: 9 locations using `F3`, `F6`, `F7`

**Confidence:** 82.

In `jump_table_lifting.rs`, `graph_rewriter.rs`, `indirect_branch.rs`. **Fix:** Replace `F7` → `build_switch_if_ladder` / `If-ladder`; `F3` → `stack-array classifier arm`.

### I-3: `G7 from round7-followup plan`

**Confidence:** 82. **Where:** `crates/opt/tests/pipeline_subsets.rs:117`. **Fix:** Delete the parenthetical.

### I-4: `(W7)` in 2 module docs

**Confidence:** 82. **Where:** `common/indirect_resolve_helpers/{orchestrator,classify}.rs`. **Fix:** Delete parentheticals.

### I-5: `tier-1`/`tier-2` prose in 25+ locations

**Confidence:** 82.

Round 7 cleaned identifiers; doc-prose remains. **Fix:** Standardise on `cfg-time resolver` (tier-1) / `IR-level resolver` (tier-2). Already used inconsistently alongside tier labels.

### I-6: `optimizer_pipeline_subsets.rs:89-92` claims orchestrator doesn't use stable subset

**Confidence:** 88.

Comment says "Round 1 doesn't wire the orchestrator to use the stable subset directly… round-2 code that DOES use it." Orchestrator now uses stable subset. **Fix:** Replace with present-tense factual.

## Notes (below threshold)

- `CallOtherElide` tombstones in `opt/src/lib.rs:149,182` — accurate migration notes; conf 75.
- `Kb` struct abbreviation — internal convention; conf 78.
- `round8-*` regression test labels — internal cross-references in test docs only; conf 72.

## Summary

| ID | Severity | Confidence | Locations |
|----|----------|------------|-----------|
| C-1 | Critical | 98 | 1 |
| C-2 | Critical | 95 | 2 |
| C-3 | Critical | 92 | 1 |
| I-1 | Important | 85 | 9 |
| I-6 | Important | 88 | 1 |
| I-2 | Important | 82 | 9 |
| I-3 | Important | 82 | 1 |
| I-4 | Important | 82 | 2 |
| I-5 | Important | 82 | 25+ |

**Highest-impact:** C-1 and C-2 actively mislead developers reading the in-place edit / classifier integration paths.
