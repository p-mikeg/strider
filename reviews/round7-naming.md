# Round 7 — Naming Consistency Audit

Audit of `tier1` / `tier2` terminology and other unclear names across the strider workspace, focused on the user's stated goal: indirect-branch resolution all happens during optimization, so the "tier" naming is misleading.

Total `tier[12]` / `tier 1` / `tier 2` occurrences: **121** lines across **37 files** (verified via `grep -rn -iE "tier[ _-]?[12]" crates/`).

## Vocabulary observed in the code

Cross-checking each occurrence to its surrounding code, "tier 1" and "tier 2" mean very specific things:

- **"tier 1" = the cfg-time mini-graph resolver** at `crates/cfg/src/cfg/builder/indirect_resolve.rs`. It is invoked **inline during cfg construction**, builds a single-block IR for a single region, runs `ConstantFold + KnownBits + RedundantPhis` (+ optional `LoadReadOnly`) on that mini-graph, and classifies the BranchIndirect's target as `Single(K)` / `LinkRegister` / unresolvable. Returns `Ok(None)` when it can't classify, deferring to the post-IR resolver.
- **"tier 2" = the post-lift / post-optimization indirect-branch resolver** in two places:
  - The classifier `crates/strider/src/indirect_resolve/classify.rs` (a thin shim over `opt::classify_anchor*`, which has the real arms `link_register`, `int_const`, `multiple_int_const`, `jump_table`, `stack_array`).
  - The fixed-point loop in `crates/strider/src/orchestrator.rs` that drives classify→in-place-edit-or-CFG-rebuild until convergence.

The user is right: there is **no real "tiering"** between the two — both run during the optimization pipeline. Tier 1 runs `opt` passes on a mini-graph during cfg build; tier 2 runs `opt` passes on the full graph during the orchestrator's fixed-point loop. Both are "indirect-branch resolution", separated by **when** they run (cfg-time vs full-IR-time) and **what scope** they see (single region vs whole function).

## Section A — Tier rename mapping

### A.1 Proposed terminology

| Old | New |
|---|---|
| "tier 1" / "tier-1" / "tier_1" | "cfg-time" / "lift-time" indirect resolver (single-region mini-graph) |
| "tier 2" / "tier-2" / "tier_2" | "ir-level" / "post-opt" indirect resolver (full-graph classifier + fixed-point loop) |

### A.2 File renames

| Current path | Proposed path | Justification |
|---|---|---|
| `crates/strider/tests/tier2_orchestrator.rs` | `crates/strider/tests/orchestrator.rs` | These integration tests verify `strider::run` (the orchestrator); "tier2_" is redundant. |
| `crates/strider/tests/tier2_optimizer_tiers.rs` | `crates/strider/tests/optimizer_pipeline_subsets.rs` | Module docstring (lines 1-21) explicitly says these "pin the soundness contracts the strider fixed-point orchestrator depends on", asserting `stable_default_pipeline` vs `destructive_default_pipeline`. The "subsets" word matches `crates/opt/tests/pipeline_subsets.rs`. |

### A.3 Module / type renames

| Current symbol | File | Proposed |
|---|---|---|
| `SpecialTerm::Unresolved(rsleigh::Vn, cfg::PcodeInsnAddr)` | `crates/strider/src/strider/pipeline.rs:494` | `SpecialTerm::PendingIndirect { target_vn, addr }` (keep struct-style fields for clarity); also renames the matches at `pipeline.rs:383,510,542`. |
| `IrStrider::unresolved_branches` | `crates/strider/src/strider/mod.rs:23`, `pipeline.rs:65,481` | Already a reasonable name. **Keep**. Rename the doc comment at `mod.rs:18` from "Anchors for the tier-2 resolver" → "Anchors for the indirect-branch resolver." |

(Note: `cfg::RegionTerminator::UnresolvedIndirectBranch` is already well-named — keep it.)

### A.4 Comment / docstring rewrites — every occurrence

| File:line | Snippet (verbatim, abbreviated) | Proposed rewrite |
|---|---|---|
| `crates/cfg/src/cfg/types.rs:123` | "which only the strider tier-2 fixed-point loop produces; tier 1 never returns Multiple" | "produced only by the strider IR-level fixed-point loop; the cfg-time mini-graph never returns Multiple" |
| `crates/cfg/src/cfg/types.rs:133` | "the SAME value tier 2 classified" | "the SAME value the IR-level resolver classified" |
| `crates/cfg/src/cfg/types.rs:150` | "BranchIndirect whose target the cfg-time tier-1 resolver" | "BranchIndirect whose target the cfg-time mini-graph resolver" |
| `crates/cfg/src/cfg/types.rs:155` | "and tier-2 resolution" | "and IR-level resolution" |
| `crates/cfg/src/cfg/options.rs:47` | "skips tier 1's mini-graph resolver" | "skips the cfg-time mini-graph resolver" |
| `crates/cfg/src/cfg/options.rs:50` | "wire tier-2 results into a CFG rebuild" | "wire IR-level resolutions into a CFG rebuild" |
| `crates/cfg/src/cfg/builder/mod.rs:216` | "Threads tier-2 results back into the CFG build." | "Threads IR-level indirect-resolver results back into the CFG build." |
| `crates/cfg/src/cfg/builder/mod.rs:220` | "instead of invoking tier 1's mini-graph resolver" | "instead of invoking the cfg-time mini-graph resolver" |
| `crates/cfg/src/cfg/builder/mod.rs:222` | "after tier 2 resolves an indirect branch" | "after the IR-level resolver classifies an indirect branch" |
| `crates/cfg/src/cfg/builder/region_builder.rs:430` | "the strider orchestrator's tier-2 feedback path" | "the strider orchestrator's indirect-resolver feedback path" |
| `crates/cfg/src/cfg/builder/region_builder.rs:472` | "tier 2 on the optimised IR" | "the IR-level resolver on the optimised IR" |
| `crates/cfg/src/cfg/builder/region_builder.rs:475` | "for tier-2 inspection" | "for IR-level resolver inspection" |
| `crates/cfg/src/cfg/builder/region_builder.rs:501` | "Multiple is exclusively a tier-2 feedback shape" | "Multiple is exclusively an IR-level resolver feedback shape" |
| `crates/cfg/src/cfg/builder/region_builder.rs:502` | "tier 1's mini-graph resolver only ever returns…" | "the cfg-time mini-graph resolver only ever returns…" |
| `crates/cfg/src/cfg/builder/indirect_resolve.rs:29,92,231` | "tier-2 resolution" | "IR-level resolution" |
| `crates/cfg/tests/indirect_resolve.rs:341,468,476,488,495,498` | "tier 1 returns Ok…", `tier_1_unresolved_returns_ok_none`, `tier_1_resolved_const_returns_ok_some_single` | Replace prose with "the cfg-time resolver returns Ok"; rename the two `tier_1_*` test fns to `cfg_time_resolver_unresolved_returns_ok_none` and `cfg_time_resolver_resolved_const_returns_ok_some_single`. |
| `crates/cfg/tests/known_targets.rs:2,20,92,108,126` | "thread tier-2 results", `with_known_targets_empty_map_falls_through_to_tier_1`, "Tier-1 still can't classify", "tier 2 had already resolved" | Rewrite prose; rename the test fn → `with_known_targets_empty_map_falls_through_to_cfg_time_resolver`. |
| `crates/cfg/tests/indirect_dispatch.rs:182,211,311,315,318,334` | "the strider-level outer loop can attempt tier-2 resolution"; "until tier 2 resolves"; "A tier-1-resolvable BranchIndirect"; "tier 1 closes trivial cases inline"; "tier 1 returns" | Rewrite to "IR-level resolution" / "cfg-time resolver". |
| `crates/cfg/tests/region_terminator.rs:278,281` | "Tier 1 lifts… loop then attempts tier-2 resolution" | "the cfg-time resolver lifts…; the loop then attempts IR-level resolution" |
| `crates/cfg/tests/region_builder_process.rs:73,77` | "tier-1 cannot prove"; "tier-2 resolution" | "the cfg-time resolver cannot prove"; "IR-level resolution" |
| `crates/opt/src/sp_expr.rs:25` | "the tier-2 indirect-branch classifier consumes…" | "the IR-level indirect-branch classifier (in `opt::indirect_branch_resolve`) consumes…" |
| `crates/opt/src/indirect_branch_resolve/jump_table.rs:1` | "Jump-table arm for the tier-2 indirect-branch classifier." | "Jump-table arm of the IR-level indirect-branch classifier." |
| `crates/opt/src/indirect_branch_resolve/stack_array.rs:1` | "Stack-array-of-labels arm of the tier-2 indirect-branch classifier." | "Stack-array-of-labels arm of the IR-level indirect-branch classifier." |
| `crates/opt/src/stack_load_forward/mod.rs:406,415,458,469,487` | "Public helper for the tier-2 indirect-branch classifier", "Threaded through tier-2", "tier-2 classifier so shared chain prefixes…" | Rewrite as "the IR-level indirect-branch classifier" or, better, the concrete arm name `stack_array::classify_stack_array`. |
| `crates/opt/src/stack_load_forward/tests.rs:875,1170` | "Used by the tier-2…"; "(`tier2_classify`)" | "Used by the IR-level…"; replace `tier2_classify` reference with `indirect_resolve_classify` (the actual test file name now). |
| `crates/opt/tests/pipeline_subsets.rs:14` | "the strider-side tier2_optimizer_tiers" | After the file rename in A.2, change to "the strider-side optimizer_pipeline_subsets". |
| `crates/strider/src/lib.rs:34` | "tier-2 fixed-point loop" | "indirect-branch resolution fixed-point loop" |
| `crates/strider/src/orchestrator.rs:5` | "via the tier-2 fixed-point loop" | "via the indirect-branch fixed-point loop" |
| `crates/strider/src/orchestrator.rs:32` | "spin indefinitely on a tier-2 soundness bug" | "spin indefinitely on an IR-level resolver soundness bug" |
| `crates/strider/src/orchestrator.rs:213` | "Accumulator of tier-2 resolutions across iterations." | "Accumulator of indirect-branch resolutions across iterations." |
| `crates/strider/src/indirect_resolve/inplace.rs:1` | "In-place IR edits for tier-2 resolution." | "In-place IR edits for IR-level indirect-branch resolution." |
| `crates/strider/src/indirect_resolve/mod.rs:1-9` | "Tier-2 (post-IR) resolver…tier 1 (the cfg-time mini-graph)…Tier 2 inspects…tier 1 mini-graph can see." | Rewrite the entire 9-line module docstring without any "tier" terminology. Keep the substance (cfg-time mini-graph vs full-graph classifier scope difference). |
| `crates/strider/src/indirect_resolve/classify.rs:274` | "integration tests in `tests/tier2_classify.rs` cover" | Update path to "integration tests in `tests/indirect_resolve_classify.rs` cover". |
| `crates/strider/src/strider/mod.rs:18` | "Anchors for the tier-2 resolver." | "Anchors for the indirect-branch resolver." |
| `crates/strider/src/strider/pipeline.rs:50` | "the placeholder-anchor side-table the tier-2 resolver consumes" | "the placeholder-anchor side-table the indirect-branch resolver consumes" |
| `crates/strider/src/strider/pipeline.rs:54` | "tier-2-aware callers read" | "callers that drive the indirect-branch resolver read" |
| `crates/strider/src/strider/pipeline.rs:491` | "Tier-2 placeholder: lifts to `Return(target_value)`" | "Indirect-branch placeholder: lifts to `Return(target_value)`" |
| `crates/strider/src/strider/insn/mod.rs:74` | "the cfg builder's tier-1 indirect-branch resolver" | "the cfg builder's cfg-time indirect-branch resolver" |
| `crates/strider/src/strider/insn/control.rs:27,178` | "tier-2's `Multiple` classification"; "the SAME value tier 2 classified." | Replace with "the IR-level resolver's `Multiple` classification" and "the SAME value the IR-level resolver classified." |
| `crates/strider/tests/abi.rs:18` | "Tier 2's `LinkRegister` arm" | "The IR-level resolver's `LinkRegister` arm" |
| `crates/strider/tests/indirect_branch.rs:1,16,18,95,99,107,155,182,190` | All tier-1 / tier-2 references | Replace systematically per the same convention. Notably `:182` references `tier-1 nor tier-2 (incl. F3 stack-array arm)` — rewrite as "neither cfg-time nor IR-level (incl. F3 stack-array arm) classified". |
| `crates/strider/tests/graph_rewriter.rs:12,191` | "tier-2-resolved jump table" | "IR-level-resolved jump table" |
| `crates/strider/tests/indirect_resolve_classify.rs:29,126,156` | "Tier 1's"; "Tier 2 — post-IR resolver" | "The cfg-time resolver's"; "IR-level resolver — post-IR" |
| `crates/strider/tests/jump_table_lifting.rs:7,263,294` | "commit a tier-2 resolution"; `tier_2_multiple_resolution_end_to_end_…` test fn name; "tier-2 `Multiple` resolution" | Rewrite prose; rename test fn → `ir_level_multiple_resolution_end_to_end_produces_lifted_switch_in_ir`. |
| `crates/strider/tests/r1_placeholder.rs:7,31,43,105` | "cfg-tier-1 cannot classify"; "tier 1 cannot classify"; "tier 1's LinkRegister arm"; "Tier 2 (R2) walks this table." | Rewrite uniformly. (R1/R2 are spec-round labels — leave those if the spec docs use them.) |
| `crates/strider/tests/tier2_orchestrator.rs:61,92` | "tier 2 cannot classify"; "tier 2 classifies as `Single(K)`" | Rewrite prose; this file is **also being renamed** per A.2. |
| `crates/strider/tests/tier2_optimizer_tiers.rs:13,120,123,137` | "tier 2's classification produces…"; `fn tier_2_classification_robust_to_destructive_subset`; "Tier 2's"; "tier 2 classification must be invariant…" | Rewrite prose; rename the test fn to `ir_level_classification_robust_to_destructive_subset`. File **also being renamed** per A.2. |
| `crates/strider/tests/common/mod.rs:30,31` | "fixture builders for the tier-2 classifier integration tests in `tests/tier2_classify.rs`" | Rewrite as "fixture builders for the indirect-branch classifier integration tests in `tests/indirect_resolve_classify.rs`". |
| `crates/strider/tests/common/indirect_resolve_helpers/classify.rs:1,39,44,45,47,49,59,66,70,76,86,312,349,425,436,669,672,698,700,717` | 20 occurrences of tier-1/tier-2 in fixture-prep prose | Single sweep replacing "tier 1" with "the cfg-time resolver" / "cfg-time" and "tier 2" with "the IR-level resolver" / "IR-level". Helper-file names need no rename (already `indirect_resolve_helpers`). |
| `crates/strider/tests/common/indirect_resolve_helpers/mod.rs:1` | "tier-2 classifier integration tests" | "indirect-branch classifier integration tests" |
| `crates/strider/tests/common/indirect_resolve_helpers/orchestrator.rs:2,49,89` | "tier-2 fixture builders"; "tier-2 placeholder anchor's `NodeOutputId`"; "exactly one tier-2 placeholder" | Replace with "indirect-branch placeholder anchor". |

## Section B — Other unclear names

### B.1 Half-rename leftovers

| Where | Issue | Proposed |
|---|---|---|
| `crates/pattern/src/matcher/bindings.rs:136` | Comment says "the same 'wrong shape ⇒ None' contract the old typed-Var getters had." `Var` is no longer a public type. | Rewrite as "…the same 'wrong shape ⇒ None' contract earlier capture-typed getters had." Or drop the historical reference. |
| `crates/pattern/src/pat/mod.rs:43-44` | "previous overloading on `Capture` vs typed-Var is gone with the typed Vars themselves." | Drop reference to the old name; say "function-only constructors; no trait dispatch". |

### B.2 Internal shorthand exposed as public API

| Where | Symbol | Issue | Proposed |
|---|---|---|---|
| `crates/opt/src/flag_cmp_canonicalize/mod.rs:104-106,127-129,235-264` | `cap_a`, `cap_b` (struct fields of `Rule`) | Internal shorthand. They are not `pub` (the `Rule` struct is private), so this is purely a readability concern. The doc-comment chain (lines 100-107) makes the meaning clear, but `lhs_capture` / `rhs_capture` would not need the chain. | Rename `cap_a` → `lhs_capture`, `cap_b` → `rhs_capture`. Mechanical rename — only this one file affected. |

### B.3 Inconsistent acronyms

The codebase uses `vn` (varnode) and `cc` (calling convention) widely; CLAUDE.md and most code use the short forms consistently and match the field name pattern (`target_vn`, `cc_link_register_vn`). No rename needed — these are well-established and consistent.

One edge case: `Strider::sleigh_regs` and `cfg::SleighRegisters` — the field name is fine; both forms appear coherently throughout.

### B.4 Stale doc references (cross-listed from round7 reports — verified)

| Where | Stale | Proposed |
|---|---|---|
| `crates/ir/src/graph/mod.rs:96` | `IfCase` listed as an asm-fingerprint exemption — `NodeKind` no longer has an `IfCase` variant. | Remove `IfCase` from the doc comment. |
| `crates/strider-py/src/pattern.rs:20-21` | TODO comment claims "op-variant accessors are not yet exposed" — they are at `matcher.rs:119-181`. | Delete the stale TODO. |
| `crates/strider-py/src/pattern.rs:1564-1568` | `if_node()` docstring claims symmetric matching with branch-swap retry — code at `pattern/src/pat/builders/branch.rs` only matches direct layout (`IfCondInversion` canonicalises). | Rewrite to match reality. |
| `crates/pattern/src/pat/ctor/control.rs:120-127` | Same false symmetric-matching paragraph. | Rewrite to match reality. |
| `crates/pattern/src/pat/ctor/variant_agnostic.rs:197` | `float_cmp_any` mentions `NotEqual` — not an IR primitive. | Rewrite: "`Equal` is commutative; `NotEqual`/`LessEqual` are not IR primitives — use `float_ne`/`float_le` aliases." |
| `crates/strider/src/strider/pipeline.rs:43-44` | TODO(Task17) tag refers to a plan path — verify the plan still references this. | Verify and either close or retain. |

### B.5 No problems found

- No `_v1`/`_v2`/`legacy_`/`tmp_` markers in production source.
- `process_new_insn` uses `new` in its `process_*_insn` sense (handles a freshly-arrived insn during region build), not as a half-rename. Keep as-is — the CFG builder vocabulary matches.
- `FunctionBuilder::new_raw` is the established public constructor name; the surrounding tests (`new_raw_filters_…`) follow it. Keep as-is.

## Section C — Recommended order of execution

CI must stay green between renames. Each step compiles and passes tests before the next.

1. **Comment-only rewrites first.** Do every "tier 1"/"tier 2" → "cfg-time"/"IR-level" change in **doc comments and module docstrings only**. No struct/field/file renames. ~80 of the 121 occurrences are comments. This is purely textual, breaks no consumers, and stages the vocabulary shift.

2. **Internal-only struct field rename.** Apply the `Rule.cap_a` → `lhs_capture`, `Rule.cap_b` → `rhs_capture` change in `crates/opt/src/flag_cmp_canonicalize/mod.rs`. Single-file mechanical rename.

3. **Test fn renames inside existing files.** Rename:
   - `tier_1_unresolved_returns_ok_none` → `cfg_time_resolver_unresolved_returns_ok_none`
   - `tier_1_resolved_const_returns_ok_some_single` → `cfg_time_resolver_resolved_const_returns_ok_some_single`
   - `with_known_targets_empty_map_falls_through_to_tier_1` → `with_known_targets_empty_map_falls_through_to_cfg_time_resolver`
   - `tier_2_multiple_resolution_end_to_end_…` → `ir_level_multiple_resolution_end_to_end_…`
   - `tier_2_classification_robust_to_destructive_subset` → `ir_level_classification_robust_to_destructive_subset`

   Each test fn rename is independent; cargo's test target is a path, not a name, so no Cargo.toml change.

4. **`SpecialTerm::Unresolved` → `SpecialTerm::PendingIndirect`.** This is a private enum (`crates/strider/src/strider/pipeline.rs:490`). Update the four match-arm sites in the same file (`pipeline.rs:383, 510, 542`). All in one PR.

5. **File renames at the end.** Atomic rename (git-mv) of:
   - `crates/strider/tests/tier2_orchestrator.rs` → `crates/strider/tests/orchestrator.rs`
   - `crates/strider/tests/tier2_optimizer_tiers.rs` → `crates/strider/tests/optimizer_pipeline_subsets.rs`

   No Cargo.toml change required (tests are auto-discovered). Update the cross-reference in `crates/opt/tests/pipeline_subsets.rs:14`.

6. **Stale-doc cleanups (independent, parallelizable).** All B.4 items, including the `IfCase` exemption-doc fix, the strider-py TODO, the symmetric-matching docstrings, and the `float_cmp_any` `NotEqual` mention. Independent of 1-5 and can land separately.

Each step is independent. Steps 1, 6 can land in parallel batches; 2-5 are sequential within their PR.

## Notes

- `cfg::RegionTerminator::UnresolvedIndirectBranch` is well-named (the resolved/unresolved distinction is real here) and **should not** be renamed.
- `IrStrider::unresolved_branches` is well-named for the same reason; only its surrounding doc comment uses "tier-2".
- The crate hierarchy already partitions correctly: `cfg::indirect_resolve` (cfg-time) vs `opt::indirect_branch_resolve` + `strider::indirect_resolve` (IR-level). The only naming debt is in comments and a few file/test-fn names.
