# Round 10 — Naming Sweep

Workspace-wide naming sweep across `crates/`. Findings derived from code shape and grep verification.

---

## Tombstones

### `Round N …` / `Ask-N RN FN` / `RN-*` / `wave N` plan-tracking breadcrumbs

- **Where:** ~120+ lines across 32 files. Representative sample:
  - `crates/target/src/arch.rs:133-154` — four accessor doc-comments each "Round 9 V7 (R9-2D L5)…"
  - `crates/cfg/src/cfg/types.rs:41,79,88,96` — "Round 9 V3 (R9-2D H2)" on every `MachineInsnAddr`/`PcodeInsnAddr` accessor
  - `crates/opt/src/pipeline.rs:36,76,136` — "Round 9 wave 28 (H-9/D2)" in `OptimizationResult::after_replace` and `with_rewrite_ctx`
  - `crates/strider/src/strider/pipeline.rs:86,159` — "Round 9 P3 (R9-2D M3)" in `SortedVns` doc
  - `crates/strider/src/orchestrator.rs:201,215` — "Round 9 wave 27 (I-7)" and "Pre-fix (round 9 Ask-8 R2 F7)"
  - `crates/ir/src/validate/tests.rs:308,1027` — "round 9's reachability gate (Ask-8 R2 F2)"
  - `crates/ir/src/validate/layer_c.rs:65` — "Round 9 Ask-8 R2 F2: gate the entire ControlState check…"
  - `crates/target/src/calling_convention/mod.rs:186,640,683` — "round 9 V4", "Round 9 wave 24"
  - `crates/ir/src/builder/vars.rs:13` — "Round 9 V1"
  - `crates/opt/src/indirect_branch_resolve/mod.rs:102,275` — "round 9 P5", "Round 9 V5"
  - `crates/ir/src/function.rs:115,170` — "Round 9 V2", "Round 9 H-9/D2"
  - `crates/opt/src/indirect_branch_resolve/stack_array.rs:117,141,426` — "Round 9 IMPORTANT (R9-EA3 IMP-1 / arch wave)"
  - `crates/strider-py/src/pattern.rs:453,475,498` — "Round 9 H-8"
  - `crates/reader/tests/elf_relocations.rs:65,89` — "Round 9 M-2 (R9-EA1 Finding 2)"
  - `crates/opt/tests/asm_fingerprint_propagation.rs:163,259` — "Round 9 H-2"
  - `crates/cfg/src/cfg/options.rs:9,100` — "Round 9 P2 (R9-2D M3)"
  - `crates/pattern/src/rewrite.rs:178,189,260` — "Round 9 wave 26 (H-9/D2)"
  - `crates/dot/src/lib.rs:197` — "Round 9 S1 (R9-2C OK table)"
  - `crates/pattern/tests/matching/control_flow.rs:66` — "Round 9 test-plan I-11"
  - `crates/pattern/tests/matching/support/graph.rs:182` — "Round 9 D20 (R9-1D MED)"
  - `crates/reader/src/elf.rs:585` — "Round 9 M3 (R9-EA1 Finding 2)"
  - `crates/opt/src/if_cond_inversion/tests.rs:177` — "Regression for round8-correctness-invariants H-2"
  - `crates/strider-py/src/opt.rs:166` — "Round 9 H-IMP I-5 (R9-1F-03)"
  - `crates/opt/src/flag_cmp_canonicalize/mod.rs:152` — "Round 9 wave 31 (R9-1C Issue 2)"
  - `crates/target/src/calling_convention/tests.rs:154,682` — "round 9 wave 24", "Round 9 Ask-8 R5 I-1"
  - `crates/ir/src/builder/mod.rs:18` — "Round 9 D1"
- **Issue:** Internal planning breadcrumbs (iteration numbers, wave numbers, review-ticket codes). The information that matters (what + why) is usually present; the `Round N X-N` prefix is bolted on.
- **Proposed rename:** Strip the `Round N …` / `Ask-N RN FN` / `R9-…` / `wave N` prefix; retain only the explanatory prose.

### `TODO(Task17)` — opaque task-tracker reference

- **Where:** `crates/cfg/src/cfg/decode_cache.rs:35`, `crates/strider/src/orchestrator.rs:287`, `crates/strider/src/strider/pipeline.rs:43`
- **Proposed rename:** `TODO: remove after incremental indirect-resolve lands (…)` — drop the `(Task17)` parenthetical.

### `R5` comment in jump-table test

- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:655`
- **Proposed rename:** Replace with plain-English description of the limitation.

---

## Half-rename leftovers

### `OptionsBuilder` field `lifter_options`

- **Where:** `crates/cfg/src/cfg/options.rs:166` (and 5 accesses on lines 182, 194, 209, 219, 226)
- **Issue:** Residual label from when `Options` was called `LifterOptions`. The public type is now `Options`; internal field name should match.
- **Proposed rename:** `lifter_options` → `options`.

### `ret_val_regs_slice` vs peers `call_clobbered_regs` / `call_other_clobbered_regs`

- **Where:** `crates/ir/src/function.rs:131`
- **Issue:** Three canonical accessors introduced together; first two follow `<field>_regs`, third uses `_slice`. The `_slice` suffix is a workaround for the field-name collision but is the odd one out.
- **Proposed rename:** `ret_val_regs_slice` → `ret_val_regs_as_slice` (follows `as_` view convention).

### Ghost references to `opt::with_built`

- **Where:** `crates/ir/src/function.rs:153,173`, `crates/pattern/src/rewrite.rs:161`, `crates/strider/src/rewrite.rs:65`
- **Issue:** `opt::with_built` was replaced by `with_rewrite_ctx`; comments still reference the dead name.
- **Proposed rename:** `opt::with_built` → `opt::with_rewrite_ctx`.

---

## Unclear test names

### `analysis_loop_without_build_round_trips` — "round trips" is ambiguous

- **Where:** `crates/ir/tests/builder_extended_use.rs:26`
- **Proposed rename:** `in_place_mutations_without_build_preserve_graph_validity`.

### `check_node_output_defintions` — misspelling

- **Where:** `crates/ir/src/graph/tests.rs:36` (used at line 68)
- **Proposed rename:** `check_node_output_definitions`.

---

## Abbreviation choices

### `bfg` vs `fg` inconsistency

- **Where:** ~76 uses of `bfg` across 9 test files vs ~1600 uses of `fg` workspace-wide.
- **Issue:** Same type (`BuiltFunctionGraph`) named differently in tests vs production. Standardize on `fg`.
- **Proposed rename:** `bfg` → `fg` in tests (76 sites).

### `fg` (informational, not a finding)

- **Issue:** Opaque to first-time readers. Given prevalence + length of `BuiltFunctionGraph`, **LOW** — noted for awareness only.

### `build_iter_0` — bakes in internal iteration concept

- **Where:** `crates/strider/src/orchestrator.rs:368`
- **Proposed rename:** `build_initial_iteration` or `lift_initial`.

---

## Generic type names — no findings

`LoopState`, `RegionIndex`, `RunOpts`, `DecodeCache`, `RegionBuilder`, `ProcessInsnRes` are sufficiently scoped within their modules.

---

## Side-effect-lying / visibility-mismatched names

### `Matcher::options_for_test`

- **Where:** `crates/pattern/src/matcher/mod.rs:203` (9 test sites)
- **Issue:** `pub` method, not `#[cfg(test)]` gated. `_for_test` suffix lies — production callers can reach it.
- **Proposed rename:** `options` (regular accessor) OR add `#[cfg(test)]` gate.

### `Match::new_for_test`

- **Where:** `crates/pattern/src/matcher/match_result.rs:32` (6 test sites)
- **Proposed rename:** Add `#[cfg(test)]` to existing `pub` visibility.

### `resolve_indirect_target_for_test`

- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:359` (9 test sites)
- **Proposed rename:** Add `#[cfg(test)] pub`.

---

## Doc-prose drift

### `with_rewrite_ctx` doc still mentions replaced `with_built`

- **Where:** `crates/opt/src/pipeline.rs:76-78`
- **Proposed fix:** "replaces the previous `with_built` adapter…" → forward-looking statement about `OptimizerOnBuilt`.

### `from_graph_and_entry_for_rewrite` doc mentions `opt::with_built` (ghost)

- **Where:** `crates/ir/src/function.rs:153`
- **Proposed fix:** Update to surviving callers (`strider::rewrite::GraphRewriter` + `opt::with_rewrite_ctx`).

---

## Rename mapping

| Identifier / Pattern | Proposed target | Sites |
|---|---|---|
| `Round N … (RN-…)` doc prefixes | Strip prefix | ~120+ across 32 files |
| `Ask-N RN FN` / `wave N` refs | Same | ~30 across 9 files |
| `TODO(Task17)` | Remove `(Task17)` | 3 |
| `R5` comment (jump_table_tests.rs:655) | Plain-English | 1 |
| `OptionsBuilder::lifter_options` | `options` | 6 |
| `BuiltFunctionGraph::ret_val_regs_slice` | `ret_val_regs_as_slice` | ~12 across 5 files |
| `opt::with_built` references | `opt::with_rewrite_ctx` | 4 |
| `analysis_loop_without_build_round_trips` | `in_place_mutations_without_build_preserve_graph_validity` | 1 |
| `check_node_output_defintions` | `check_node_output_definitions` | 2 |
| `bfg` (tests) | `fg` | ~76 across 9 files |
| `build_iter_0` | `build_initial_iteration` | 2 |
| `Matcher::options_for_test` | `options` or `#[cfg(test)]` | 10 |
| `Match::new_for_test` | `#[cfg(test)]` gate | 7 |
| `resolve_indirect_target_for_test` | `#[cfg(test)]` gate | 10 |
