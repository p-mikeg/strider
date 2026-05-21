# Round 12 — CLAUDE.md correctness diff

These are concrete CLAUDE.md edits derived from `round12-3A-doc-verify.md` (3 stale/refuted claims out of 28 sampled) plus targeted fixes for drift surfaced in `round12-2D-types.md` and `round12-1A-ir.md`.

## E1. `OptimizerOnBuilt` trait name reversed

**Current (CLAUDE.md `opt` crate section):**

> Most passes implement `Optimizer` (`fn optimize(&self, &mut Graph, NodeId)`); a `OptimizerOnBuilt` companion trait targets `pattern::RewriteCtx<'_>` (a `&mut Graph + entry: NodeId` rewrite-only context with `Deref<Target=Graph>` + `preorder()`/`preorder_kind()` ergonomics), and a blanket `impl<T: OptimizerOnBuilt> Optimizer for T` wires both kinds into the same pipeline.

**Why stale:** The trait was renamed in Round 11 W15 ("`OptimizerOnBuilt → Optimizer` trait collapse"). Today the situation is reversed: `Optimizer` (`RewriteCtx`-based) is the primary trait; `OptimizerRaw` (`Graph + NodeId`) is the low-level companion; the blanket impl goes `impl<T: Optimizer> OptimizerRaw for T` via `with_rewrite_ctx`.

**Proposed replacement:**

> Most passes implement `Optimizer` (`fn optimize(&self, &mut RewriteCtx<'_>)`), the rewrite-only context with `Deref<Target=Graph>` + `preorder()`/`preorder_kind()` ergonomics. An `OptimizerRaw` companion trait takes `&mut Graph + NodeId` directly for low-level passes; a blanket `impl<T: Optimizer> OptimizerRaw for T` adapts via `with_rewrite_ctx`. The pipeline stores `Box<dyn OptimizerRaw>` so both kinds share one queue.

## E2. `pattern::float_is_nan` Rust constructor (root README + CLAUDE.md cross-reference)

**Current (CLAUDE.md pattern crate "lift-time canonicalisation aliases" subsection):**

> Pattern crate ergonomic aliases (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`) construct the lowered shapes directly so call-sites still read naturally.

**Verification:** `grep -rn "pub fn float_is_nan" crates/pattern/src/` → 0 hits. Only Python binding has `float_is_nan` (`strider-py/src/pattern.rs:1060`, which delegates to Rust's `pattern::float_ne(x, x)`).

**No edit needed for CLAUDE.md** — the list is already correct (6 aliases, `float_is_nan` not included). However, the **root README's** "Lift-time canonicalisation aliases" section (`README.md:231`) does include `pattern::float_is_nan` in the list — that needs the fix in `round12-readme-diffs.md`.

## E3. `BuiltFunctionGraph` CC fields visibility doc-comment drift

**Current (CLAUDE.md `ir` crate section, derived from comment in `crates/ir/src/function.rs:120-125`):**

The CLAUDE.md description of `BuiltFunctionGraph` should reflect that CC-bearing fields are now `pub(crate)` (W14 H-6 landed). However the source-comment at `crates/ir/src/function.rs:120-125` still says:

> "The fields themselves remain `pub` for back-compat (the workspace has ~30+ direct-field readers), but new code should use these accessors — they're the migration path for tightening field visibility to `pub(crate)` in a future round."

This is stale (Round 12 1A LOW finding). Looking at lines 57, 71, 79, 95, 104 — all five fields are `pub(crate)`. The migration is done.

**Proposed source-comment replacement** (touches `crates/ir/src/function.rs`, not CLAUDE.md directly):

> "The CC-bearing fields are `pub(crate)` (W14 H-6); use the read-only `*_regs` / `*_map` / `no_memory_clobber` accessors externally. The test-only `set_*_for_test` setters allow synthetic graph construction from sibling crates."

**CLAUDE.md edit:** None required — CLAUDE.md doesn't describe the field visibility directly. The doc-comment fix is bundled here for completeness.

## E4. `IndirectBranchResolve` pass not registered in default pipelines (clarification)

**Current (CLAUDE.md `opt::IndirectBranchResolve` paragraph):**

> `IndirectBranchResolve` (`opt::indirect_branch_resolve`) — producer-shape classifier for `BranchIndirect` placeholders. Recognises link-register returns, tail calls, jump-tables, stack-array dispatch. Drives the resolver in `strider::indirect_resolve`. **Note:** implements `Optimizer` but is instantiated *directly* by the strider orchestrator's indirect-branch fixed-point loop — it is NOT registered in `default_pipeline()` / `stable_default_pipeline()` / `destructive_default_pipeline()`.

**Verification:** Round 11 W9 S1.1 deleted the `IndirectBranchResolve` *struct* (-633 LOC). The current `crates/opt/src/indirect_branch_resolve/` module exposes free functions (`classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`, `apply_link_register`, `apply_tail_call`, `classify_jump_table`, `classify_stack_array`) — not a pass struct that implements `Optimizer`.

**Proposed replacement:**

> `indirect_branch_resolve` (`opt::indirect_branch_resolve`) — producer-shape classifier helpers for `BranchIndirect` placeholders. Public functions: `classify_anchor` / `classify_anchor_with_rom` / `classify_anchor_with_rom_and_sp` (recognises link-register returns, jump-tables, stack-array dispatch, tail calls), `apply_link_register`, `apply_tail_call`, `classify_jump_table`, `classify_stack_array`. Called directly by the strider orchestrator's indirect-branch fixed-point loop — NOT a pipeline-registered `Optimizer`.

## E5. ARM Thumb pspec name

**Current (CLAUDE.md `target` crate section):**

> ARM Thumb uses the same `ARM8_le` SLA as ARM with the `ARMCORTEX` pspec — correct.

**Verification (1E focus area 4):**

> `arch.rs:159-352` exposes 15 presets covering x86 / x86_64 / arm / arm_be / arm_thumb / aarch64 / aarch64be / mipsbe32 / mipsle32 / mipsbe64 / mipsle64 / ppc32be / ppc32le / ppc64be / ppc64le.

The 1E audit confirms 15 presets, consistent with what CLAUDE.md describes. No edit needed.

## E6. Per-arch CallOther table entries — five missing x86 entries

**Current (CLAUDE.md `target::call_other_abi::classify` paragraph):**

> The two-arg form supports arch-specific entries (e.g. ARM `swi` reading `r7/r0..r6` vs the x86 `swi` stub); arch-independent entries (`mfence`/`sfence`/`lfence`/`cpuid`/etc.) are matched against any preset.

**Verification (IRA-1):** `cmpxchg16b`, `xsetbv`, `xgetbv`, `monitor`, `mwait` are absent. CLAUDE.md does not enumerate the table; no edit needed at the doc level — the fix is in the code (`crates/target/src/call_other_abi.rs`). However, the CLAUDE.md sentence "Unknown user-op names raise `ir::error::UnknownCallOtherError` so the table grows incrementally with what real lifts emit" continues to be accurate.

**No CLAUDE.md edit needed; flag for next-round table additions.**

## E7. Round 11 stale "migration breadcrumbs" carry over into the next round

`round12-2B-naming.md` lists ~21 breadcrumbs (`T-NN (round 11)`, `R8-…`, etc.). None are in CLAUDE.md itself — they live in source comments. CLAUDE.md does not need an edit on this axis. The breadcrumb cleanup is a separate batch.

## Summary

| Edit | Severity | Where | Action |
|------|----------|-------|--------|
| E1 | HIGH | CLAUDE.md `opt` section | Replace inverted trait description |
| E2 | LOW | CLAUDE.md — already correct | (None — fix in README, see `readme-diffs.md`) |
| E3 | LOW | `crates/ir/src/function.rs:120-125` source comment | Replace stale "remain pub" comment |
| E4 | MED | CLAUDE.md `opt::IndirectBranchResolve` paragraph | Reword as helpers, not a pass struct |
| E5 | — | — | No edit (verified accurate) |
| E6 | — | — | No CLAUDE.md edit; flag for code addition |
| E7 | — | — | No CLAUDE.md edit |

**Net edits:** 2 substantive (E1, E4) + 1 source-comment (E3). All other CLAUDE.md claims sampled in 3A are accurate.
