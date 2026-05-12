# Round 9 — 3B: Stale comment sweep

Branch: `review/ai3`. Re-derived from code without consulting round-7 / round-8 reports.

Scope: every committed Rust comment, doc-comment, and module-level `//!` header in `crates/**/src/`, plus the workspace `README.md`.  Test-file comments swept for the specific cases the prompt called out.

## Summary

The codebase is in good shape; most "post-rename" tombstones have already been cleaned.  Live findings cluster in three areas:

1. **`U80` / `U512` doc lag.**  Several `NodeOutputType` doc strings list "U8/U16/U32/U64/U128/U256" or call out only `U256` as the wide-storage variant — `U80` and `U512` were added later and the prose did not follow.
2. **Doc-link drift around CallOther / validation.**  `ir::error::UnknownCallOtherError` claims to be raised by `FunctionBuilder::build_call_other` (a function that no longer exists) and `FunctionBuilder::build`'s `# Errors` section names a `ValidationFailed` variant that no longer exists.  Both are user-facing surface (HIGH).
3. **R1 / R2 plan-round breadcrumbs.**  Three test sites and one in-source comment in `opt::indirect_branch_resolve::stack_array` still refer to the historical "R1 / R2" implementation rounds.  One of them claims a refactor "before R2" — the refactor is already in (the code uses `pattern::and` / `pattern::or`), so the breadcrumb is no longer just an aide-memoire but actively misleading (MED).

The `CallOtherElide` references that round 7 deliberately preserved as historical breadcrumbs in `crates/opt/src/lib.rs` and `crates/opt/README.md` are still in place; their justification (telling the reader why the pass is missing from the pass list) still applies, so I have NOT flagged them.

## Critical Issues (HIGH severity)

### 1. `ir::error::UnknownCallOtherError` references a non-existent method
- **Where:** `crates/ir/src/error.rs:10-16` (the doc) describes `crates/ir/src/builder/call.rs:192,228+` (the actual emitters) and `crates/strider/src/strider/insn/mod.rs:135` (where the error is actually constructed).
- **What's stale:** The doc says: "Returned by [`crate::FunctionBuilder::build_call_other`] when the supplied user-op `name` has no entry in [`target::call_other_abi::classify`]."  There is no `FunctionBuilder::build_call_other` method — the IR builder splits the operation into `build_call_other_modeled` (Call-class ABI) and `build_call_other_terminal` (NoReturn-class).  Furthermore, neither of those methods produces `UnknownCallOtherError`.  The error is constructed in the **strider** crate (`handle_call_other`) when `target::call_other_abi::classify` returns `None`.
- **Fix:** Rewrite the docstring to:
  > Returned by `strider::IrStrider::handle_call_other` when the lifted insn's user-op `name` has no entry in [`target::call_other_abi::classify`].  The `ir` crate owns the type so both `strider` and `strider-py`'s typed-error converter can reach it without a circular dep.

### 2. `FunctionBuilder::build` doc names a non-existent error variant
- **Where:** `crates/ir/src/builder/mod.rs:545-550`.
- **What's stale:** "Returns `ValidationFailed` wrapping a [`crate::validate::ValidationErrors`] bundle if the built graph fails any of validate's three layers".  There is no `ValidationFailed` enum variant in either `ir` or `opt` (`opt/src/error.rs` is now just `pub type Result<T> = anyhow::Result<T>`).  `validate` returns `Result<(), ValidationErrors>` and the `?` propagates it through `anyhow`, so the caller recovers the bundle via `err.downcast_ref::<ir::ValidationErrors>()`, not via a `ValidationFailed` branch.
- **Fix:**
  > Returns an `anyhow::Error` carrying a [`crate::validate::ValidationErrors`] bundle (recoverable via `err.downcast_ref::<ValidationErrors>()`) if the built graph fails any of validate's three layers (local typing, use-list consistency, graph-level invariants).
- **Knock-on:** `crates/opt/src/pipeline.rs:366` repeats "surfaces as `ValidationFailed`" — also stale; same fix (downcast against `ValidationErrors`).

### 3. Top-level `README.md` uses the deprecated `x86_64_systemv_abi` alias
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/README.md:256`.
- **What's stale:** The "Rust API" quickstart calls `CallingConvention::x86_64_systemv_abi().build(...)`.  That symbol exists only as a `#[deprecated(...)]` shim (`crates/target/src/calling_convention/mod.rs:299-307`); every other site in the workspace uses `x86_64_systemv()`.  Building the README example as written produces a `deprecated` lint warning under default settings and an error under `-D warnings`.
- **Fix:** Replace `x86_64_systemv_abi()` → `x86_64_systemv()`.

## Improvement Opportunities (MED severity)

### 4. `NodeOutputType::is_float` doc misses `F80`
- **Where:** `crates/ir/src/node/output_type.rs:144-149`.
- **What's stale:** "Returns `true` if this type is `F32` or `F64`."  The body returns true for any float-category variant, and `F80` is marked `Float` in the type-info table at line 68.  The unit test at line 365 (`is_float()` on `F80`) confirms.
- **Fix:** "Returns `true` if this type is `F32`, `F64`, or `F80`."

### 5. `NodeOutputType::is_integer` doc misses `U80` and `U512`
- **Where:** `crates/ir/src/node/output_type.rs:136-138`.
- **What's stale:** "Returns `true` if this type is one of the unsigned integer variants (U8, U16, U32, U64, U128, U256)."  The actual `Int` category covers `U8/U16/U32/U64/U80/U128/U256/U512` per the table at lines 58-65.
- **Fix:** "(U8, U16, U32, U64, U80, U128, U256, U512)".

### 6. `NodeOutputType::fits_u64` doc misses widths
- **Where:** `crates/ir/src/node/output_type.rs:119-127`.
- **What's stale:** "Returns `false` for `U128` and `U256`."  Misses `U80` (10 bytes), `U512` (64 bytes), and `F80` (10 bytes) — all of which also exceed 8.
- **Fix:** "Returns `false` for `U80`, `U128`, `U256`, `U512`, and `F80`." (or simplify to "Returns `false` for any width >= 10 bytes.")

### 7. `NodeOutputType::to_natural_int_type` doc mapping table incomplete
- **Where:** `crates/ir/src/node/output_type.rs:151-152`.
- **What's stale:** "(Bool→U8, F32→U32, F64→U64, Ux→Ux)".  Misses the explicit `F80→U80` mapping (the implementation handles it at line 161, and a dedicated test pins it at line 374).  The "Ux→Ux" shorthand also obscures that `U80` was added.
- **Fix:** "(Bool→U8, F32→U32, F64→U64, F80→U80, U8/U16/U32/U64/U80/U128/U256/U512 unchanged)".

### 8. `NodeOutputType::get_signed_int` doc still calls out only `U256`
- **Where:** `crates/ir/src/node/output_type.rs:204-207`.
- **What's stale:** "or its width exceeds 128 bits (`U256` — unreachable in `IntConst` land today)".  The check at line 214 is `bits > 128` which now also rejects `U512`; calling out only `U256` understates the constraint.  The "unreachable in `IntConst` land" parenthetical is also outdated: U256/U512 ARE constructed today via `IntConstWide` (see `wide_const.rs`); they just don't go through `IntConst`.
- **Fix:** "or its width exceeds 128 bits (`U256` and `U512` — both stored in `IntConstWide`, not `IntConst`, and outside the 128-bit signed-extend domain)".

### 9. `ir::lib.rs` enum-summary line misses `U512`
- **Where:** `crates/ir/src/lib.rs:36-37`.
- **What's stale:** "[`node::NodeOutputType`] — `Bool`, integers `U8`/`U16`/`U32`/`U64`/`U80`/`U128`/`U256`, floats `F32`/`F64`/`F80`".  Missing `U512`.
- **Fix:** Insert `/`U512`` after `/`U256``.

### 10. `build_int_const` `# Errors` doc misses `U512`
- **Where:** `crates/ir/src/builder/nodes.rs:81-85`; the rejection check is at lines 96-100.
- **What's stale:** "Returns an error when `output_type` is not an integer type, or is `U256` (which is not yet representable in the u128 storage that `IntConst` uses)."  The actual rejection covers both `U256` AND `U512`, and the error message itself names both ("use build_int_const_wide for U256/U512").
- **Fix:** "...or is `U256`/`U512` (which exceed the u128 storage `IntConst` uses — call `build_int_const_wide` for those widths)."

### 11. `opt::lib.rs` pass table lists only 5 of 12+ passes
- **Where:** `crates/opt/src/lib.rs:13-19`.
- **What's stale:** The module-level `# Passes` table enumerates `ConstantFold`, `KnownBits`, `RedundantPhis`, `DeadBranchElimination`, `LoadReadOnly` — but the crate also publicly exports `FlagCmpCanonicalize`, `IfCondInversion`, `FunctionArgDetect`, `IndirectBranchResolve`, `StackLoadForward`, `StackStoreDetect`, `CallStackArgCollect` (see `pub use` block at lines 52-68).  A reader scanning the lib doc to find pass names misses more than half of the ones they could plug into a pipeline.
- **Fix:** Add table rows for the seven missing passes; or rewrite the docstring to point at `default_pipeline` / `stable_default_pipeline` / `destructive_default_pipeline` for the canonical list and demote the table to "selected passes".

### 12. `Strider::build_stable_optimizer_pipeline` doc misses `FlagCmpCanonicalize` + `IfCondInversion`
- **Where:** `crates/strider/src/strider/pipeline.rs:209-218`.
- **What's stale:** "Composed of passes whose rewrites survive a later iteration that adds new phi inputs: `ConstantFold`, `KnownBits`, `StackStoreDetect`, `StackLoadForward`, and the `FunctionArgDetect` post-pass."  But the body calls `opt::stable_default_pipeline()` first, which adds `FlagCmpCanonicalize` (line 117) and `IfCondInversion` (line 124) ahead of the strider-side additions.  CLAUDE.md gets this right; the docstring on the pub method does not.
- **Fix:** Insert "...`ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`, and the `FunctionArgDetect` post-pass.".

### 13. `cfg::Builder` doc-links use `target::Arch::*` which doesn't exist
- **Where:** `crates/cfg/src/cfg/builder/mod.rs:67`, `:148`, `:151`.
- **What's stale:** Three intra-doc links to `[`target::Arch::X86_64`]` / `[`target::Arch`]`.  The actual type is `target::ArchPreset` (see `crates/target/src/arch.rs:91` and `cfg/builder/mod.rs:70` itself, which uses the right name).  These render as broken `rustdoc` links.
- **Fix:** Replace `target::Arch` → `target::ArchPreset` everywhere in this file's docstrings.

### 14. `crate::pcode_lift` doc-link unresolved (wrong crate prefix)
- **Where:** `crates/ir/src/ops/op_kinds.rs:96`.
- **What's stale:** "See [`crate::pcode_lift`] dispatch site for the rsleigh → IR mapping."  `crate` here is `ir`, which has no `pcode_lift` module; the lifter lives in the separate `pcode-lift` crate, so the correct rendered link is `[`pcode_lift`]`.
- **Fix:** Drop the `crate::` prefix → `[`pcode_lift`]`.

### 15. `opt::indirect_branch_resolve::stack_array` "before R2's refactor" — refactor is in
- **Where:** `crates/opt/src/indirect_branch_resolve/stack_array.rs:617-623`.
- **What's stale:** "These tests pin the contract of `strip_target_mask` before R2's refactor migrates the manual NodeKind matching to `pattern::and` / `pattern::or`.  The pre-refactor implementation hand-rolled commutative operand checks; pattern's auto-commutative `and` / `or` express the same shape with auto-handled operand swapping."  But `strip_target_mask` already uses `and_pat(any_int_const(...), var(...))` (line 189) and `or_pat(...)` (line 210) with the auto-commutative match.  The "before R2" framing implies the migration is pending; it isn't.
- **Fix:** Delete the temporal qualifier and re-frame as a regression-pinning test:
  > These tests pin the contract of `strip_target_mask`.  The hand-rolled NodeKind matching that preceded the migration to `pattern::and` / `pattern::or` had to enumerate operand orderings explicitly; the current pattern-based form relies on auto-commutative matching.  We pin both orderings here so any future change cannot accidentally narrow what we accept.

### 16. `crates/cfg/src/cfg/builder/indirect_resolve.rs` "Multiple is reserved for the future jump-table resolver" is stale
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:40-44`.
- **What's stale:** "[`ResolvedTargets::Multiple`] is reserved for the future jump-table resolver and is not constructed by this round; the variant exists so adding jump-table support later is purely additive."  The IR-level resolver in `opt::indirect_branch_resolve::{classify::classify_anchor_with_rom_and_sp, jump_table, stack_array}` actively constructs `Multiple` today (see `classify.rs:178`, `stack_array.rs:125`, `jump_table.rs:10`); the cfg builder consumes it (`region_builder.rs:495`).  What's true for **this file** is only that the cfg-time mini-graph never returns Multiple — that's the narrower claim that should appear here.
- **Fix:**
  > [`ResolvedTargets::Multiple`] is constructed by the IR-level resolver in `opt::indirect_branch_resolve` (`classify_anchor_with_rom_and_sp`, jump-table, stack-array arms) — but never by *this* cfg-time mini-graph, which only ever returns `Single` / `LinkRegister` / `None`.  Multi-target dispatches reach the cfg builder via the `known_targets` feedback path.

### 17. `strider/src/test_utils.rs:5-12` claims feature gating that does not exist
- **Where:** `crates/strider/src/test_utils.rs:5-12`.  Contradicting site: `crates/strider/src/lib.rs:42-50` (`pub mod test_utils;` UNCONDITIONALLY).
- **What's stale:** "Exported under `#[cfg(any(feature = "test-utils", test))]` so it's available to: …".  No such gate exists on the `pub mod test_utils;` declaration; the lib.rs comment immediately above the declaration explicitly explains that the gate was removed because it forces a circular dev-dep on integration tests.  Bullet 3 ("Other crates that opt in via `strider = { workspace = true, features = ["test-utils"] }`") is also misleading: there is no `test-utils` feature in `crates/strider/Cargo.toml`.
- **Fix:** Replace the gating paragraph with a pointer to lib.rs's existing rationale comment, or rewrite to read:
  > `pub` unconditionally — see the rationale comment on `pub mod test_utils;` in `crates/strider/src/lib.rs`.  Available to: in-crate unit tests, integration tests under `crates/strider/tests/`, and any downstream crate that adds `strider` as a dev-dep.

### 18. `ir::validate::mod` claims "skeleton; concrete checks are added by later tasks"
- **Where:** `crates/ir/src/validate/mod.rs:5-6`.
- **What's stale:** "This module currently contains only the skeleton; concrete checks are added by later tasks."  All three layers (A/B/C) plus the opt-in asm-fingerprint check are implemented (`mod layer_a; mod layer_b; mod layer_c;`), and `validate_with_options` runs every layer.  Round-prompt's "ADR comments that describe a state the code has moved past" criterion fits.
- **Fix:** Remove the sentence; the next paragraph already accurately summarises the current behaviour.

## Lower-priority drift (LOW severity)

### 19. R1-R5 plan reference in test header
- **Where:** `crates/strider/tests/abi.rs:16-17`.
- **What's stale:** "tail_caller: closed under the indirect-branch fixed-point design (R1-R5 of `2026-04-27-indirect-branch-fixedpoint.md`)."  The plan rounds R1-R5 are committed; the cross-reference is meaningful only to readers familiar with the historical plan structure and adds no useful information for current maintainers.
- **Fix:** Drop the "R1-R5 of …" phrase: "tail_caller: closed under the indirect-branch fixed-point design (see `docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md`)."

### 20. R2 plan-round references in `indirect_branch_lift_placeholder.rs`
- **Where:** `crates/strider/tests/indirect_branch_lift_placeholder.rs:104,106`.
- **What's stale:** "the IR-level orchestrator resolver (R2) walks this table." and "Pinning the table now keeps the API surface stable for R2."  R2 is in; the test now exercises the live API, not a forward-looking pin.
- **Fix:** Drop the "(R2)" parenthetical and rewrite the second sentence: "Pinning the table here keeps the API surface (which the orchestrator's IR-level resolver consumes) under regression watch."

### 21. `crates/strider/src/strider/insn/mod.rs` — duplicated lift-addr setup comment
- **Where:** `crates/strider/src/strider/insn/mod.rs:27-44` (two stacked block-comments above one `set_lift_addr` pair).
- **What's stale:** Lines 27-34 and 35-44 describe the same setup ("set the asm-fingerprint attribution context" / "set the lift-addr context for every node `process_insn_inner` produces"); the second block adds the rationale about not using `lift_at`/`LiftAddrGuard`, which is the only new content.
- **Fix:** Merge into one block; keep the single explanatory paragraph plus the `lift_at`/`LiftAddrGuard` note.

### 22. `crates/target/src/call_other_abi.rs` — duplicated ARM SWI ABI comment
- **Where:** `crates/target/src/call_other_abi.rs:78-84`.
- **What's stale:** The ARM Linux SVC/SWI ABI explanation is given twice (lines 78-80 + lines 81-84) with slightly different wording.  The second version subsumes the first.
- **Fix:** Drop the lines 78-80 paragraph; keep lines 81-84 (which adds the per-preset coverage note).

### 23. `crates/opt/src/lib.rs` "FlagCmpCanonicalize ... before IfCondInversion so the BoolNeg-wrapped outputs of the LS / GE / LE rules" — non-existent rule names
- **Where:** `crates/opt/src/lib.rs:113-117`.
- **What's stale:** The comment names "LS / GE / LE rules" inside FlagCmpCanonicalize.  Verifying these names against the implementation would catch any drift; if the actual rule files use different identifiers (`flag_cmp_canonicalize::rules::*`), the comment will mislead readers grepping for the names.  This is worth a 30-second cross-check by the implementer.

### 24. `crates/strider/src/strider/pipeline.rs:87` "(which is the standard public entry, not deprecated)"
- **Where:** `crates/strider/src/strider/pipeline.rs:85-89`.
- **What's stale:** "Empty defaults match the [`Strider::analyze_cfg(cfg)`] convenience behaviour (which is the standard public entry, not deprecated)".  The "not deprecated" qualifier is a defensive disclaimer that implies the function once *was* — there is no deprecation marker on `analyze_cfg` and the remark adds noise.
- **Fix:** Drop the parenthetical: "Empty defaults match the [`Strider::analyze_cfg(cfg)`] convenience behaviour: ...".

### 25. `crates/cfg/src/cfg/types.rs:127-134` "incremental-rebuild round" plumbing
- **Where:** `crates/cfg/src/cfg/types.rs:127-134`.
- **What's stale:** "The cfg builder always sets this to `None`; it is plumbing for an incremental-rebuild round that preserves the previous iteration's IR."  The "incremental-rebuild round" is `Task17` (still plan-only per `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`), so the comment is technically accurate but vague.  Adding the `TODO(Task17)` marker would tie it to the live tracking task and make the link clear.
- **Fix:** Change "an incremental-rebuild round" → "the deferred incremental-rebuild work tracked by `TODO(Task17)` (see `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`)".

## Positive Findings

- The **`CallOtherElide` tombstone breadcrumbs** in `crates/opt/src/lib.rs:148-151,181-183` and `crates/opt/README.md:89-91` survive round 9 with their justification still intact: each one accompanies the relevant `default_pipeline()` / `destructive_default_pipeline()` definition and tells the reader that CallOther no-op handling moved to `target::call_other_abi::classify`.  No fix needed.
- **TODO markers** are tightly scoped: only three (`crates/cfg/src/cfg/decode_cache.rs:35`, `crates/strider/src/orchestrator.rs:244`, `crates/strider/src/strider/pipeline.rs:43`) and all three correctly cite `Task17` (which is still open per the plan).
- **`#[deprecated]` shim** for `x86_64_systemv_abi()` (`crates/target/src/calling_convention/mod.rs:299-307`) carries a clear migration note in the attribute and a doc-comment explanation; the only live caller is the README example flagged above.
- **`crates/pattern/src/pat/builders/branch.rs`** module-level header (`IfPat` direct-layout-only contract) is exemplary: it gives the reader the canonical layout rule, points at the responsible canonicalisation pass (`opt::IfCondInversion`), and explains why the symmetric two-layout matching that previously lived in `IfPat` is gone.

## Files swept (coverage manifest)

- **All `crates/*/src/**`** read or grepped for stale-symbol patterns (`CallOtherElide`, `Var`/`NodeVar`, `r1_placeholder`, `x86_64_systemv_abi`, `Tier 1` / `Tier 2`, `R1` / `R2`, `TaskNN`, `// removed`, `formerly`, `previously`, `legacy`, `deleted`, `WithCapture`/`WithPredicate`, `PatKind`, `ValidationFailed`).  No matches outside the items listed above.
- **Top-level docs**: `README.md`, `CLAUDE.md` (both kept open during the sweep but not modified — `CLAUDE.md` is project meta-doc, not source code).
- **Per-crate READMEs**: `crates/opt/README.md` (the `CallOtherElide` tombstone there is intentional, see "Positive Findings").  Other crate READMEs not exhaustively re-read this round; round-8 covered them and the cross-reference checks done here did not surface drift.
- **Tests**: only those flagged by the prompt's "test comments that reference behaviour the test no longer asserts" rule were inspected; the R1/R2 references at `tests/abi.rs` and `tests/indirect_branch_lift_placeholder.rs` are the only matches.
- **NOT swept** per prompt: `reviews/round7-*.md`, `reviews/round8-*.md`, plan documents under `docs/superpowers/plans/`, spec documents under `docs/superpowers/specs/`.

## Recommended action ordering

1. Fix the three HIGH items (#1, #2, #3) — they are user-facing.
2. Fix MED items #4-#10 (the `U80`/`U512`/`F80` doc lag) in one pass through `crates/ir/src/node/output_type.rs` and `crates/ir/src/lib.rs` — they all stem from the same wide-int / x87-extended additions and a single editor session covers them.
3. Fix the doc-link breakages (#13, #14) in one pass — both are mechanical replacements (`target::Arch` → `target::ArchPreset`; drop `crate::` from `pcode_lift`).
4. The R1/R2 / "before R2" / "skeleton" set (#15, #18-#22) is a single editorial pass over four files.
5. LOW items can be deferred to opportunistic touches.
