# Round 9 — Consolidated simplification proposal

Branch: `feature/ai`. Inputs: every `reviews/round9-*.md` (excluding the
coverage manifest and any test-plan file). Each entry below cites the
originating finding plus a confidence ≥ 75. Entries flagged with
`[CONTRACT]` would touch a documented public/CLAUDE.md contract and need
the contract revised in the same change-set.

---

## 1. Delete

### D1. `ir::LiftAddrGuard` re-export — zero callers `[CONTRACT]`
Source: R9-1A I1 (95).
Where: `crates/ir/src/lib.rs:60`, struct at `crates/ir/src/builder/lift_addr.rs:16-35`.
What: `pub use builder::lift_addr::LiftAddrGuard` — `LiftAddrGuard::set` has zero call sites; the strider per-region driver explicitly avoids it (CLAUDE.md notes the `set_lift_addr(Some) … set_lift_addr(None)` pair).
Change: drop the `pub use` line. Either delete the type or keep it `pub(super)` until a future caller materialises. CLAUDE.md mentions "explicitly avoids it" but doesn't promise a public guard, so no doc churn.
Blast radius: 0 external sites; one re-export deletion.

### D2. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` — partial-state ctor `[CONTRACT]`
Source: R9-2D H1 (high), R9-correctness-invariants V7.
Where: `crates/ir/src/function.rs:117`.
What: returns a `BuiltFunctionGraph` with empty `variables`, `call_clobbered`, `ret_val_regs`, `call_other_clobbered`. Every consumer that touches a CC field on this form silently gets `None`. Round 8's `pattern::RewriteCtx { graph, entry }` is the right shape for the rewrite-only path.
Change: delete the constructor. Migrate `opt::pipeline::with_built` to use `RewriteCtx::new` (or split `OptimizerOnBuilt` into `OptimizerOnRewriteCtx` for passes that don't need CC fields). Migrate the 5 test sites (`pattern/tests/get_vn_with_callother_clobber.rs` ×3, `pattern/tests/get_vn_with_call_override.rs`, `pattern/tests/matching/control_flow.rs`) to `Matcher::for_graph(graph, entry)`.
Blast radius: 1 prod call (`opt::with_built`) + 5 test files.

### D3. Dead `Truncate(_)` arm in `classify_anchor_with_rom_and_sp`
Source: R9-1B Finding 2 (92).
Where: `crates/opt/src/indirect_branch_resolve/classify.rs:233-250`.
What: `ConstantFold` rule 4 + `FunctionBuilder::truncate_if_needed` already fold `Truncate(IntConst)` eagerly; the arm only fires when the builder is bypassed (test-only).
Change: delete the arm or retain with `#[cfg(test)]` access. Either way the production classifier becomes simpler.
Blast radius: 1 file; affects test that bypasses the builder.

### D4. Dead `Extend(_)` arm in `classify_anchor_with_rom_and_sp` (double bug)
Source: R9-1B Finding 1 (95) + R9-1C Issue 1 (82).
Where: `crates/opt/src/indirect_branch_resolve/classify.rs:252-269`.
What: same dead-in-production root cause as D3 (ConstantFold rules 5/6 + `extend_if_needed` fold `ZeroExtend(IntConst)` and `SignExtend(IntConst)` eagerly). And while live, the arm is wrong for `SignExtend(IntConst)` of negatives — uses `(*k) as u64` instead of sign-extending via `get_signed_int`.
Change: delete the arm, or if kept, branch on `ExtendOp` and use `get_signed_int` for `SignExtend`. Deletion is the simpler fix because the live shape can't reach this site in production.
Blast radius: 1 file; one unit-test case (ZeroExtend-only) drops or rewires.

### D5. `CallingConvention::x86_64_systemv_abi` deprecated alias `[CONTRACT]`
Source: R9-1E "Simplification candidates", R9-3A Issue B (100), R9-3B #3 (100).
Where: `crates/target/src/calling_convention/mod.rs:299-307`. Only live caller is the README example.
Change: fix `README.md:256` to `x86_64_systemv()`, fix `CLAUDE.md:77`, then delete the `#[deprecated]` shim.
Blast radius: 2 doc sites + 1 deprecated function.

### D6. `opt::lib.rs` and `opt::README.md` `CallOtherElide` tombstones — keep
Source: R9-2B Notes (75), R9-3B Positive Findings.
Where: `crates/opt/src/lib.rs:148-151,181-183`, `crates/opt/README.md:89-91`.
Change: do nothing. Both audits flagged these as legitimate breadcrumbs explaining why the pass is missing from `default_pipeline()`. Listed here so the audit trail is complete.

### D7. Stale R1/R2/R3/R4/R5 / F3/F6/F7 / G7 / W7 plan-round breadcrumbs
Source: R9-2B I-1, I-2, I-3, I-4 (82-85), R9-3B #19, #20.
Where: `indirect_branch_lift_placeholder.rs`, `indirect_resolve_classify.rs`, `indirect_resolve_jump_table.rs`, `jump_table_tests.rs`, `abi.rs`, `jump_table_lifting.rs`, `graph_rewriter.rs`, `indirect_branch.rs`, `optimizer_pipeline_subsets.rs:117`, `common/indirect_resolve_helpers/{orchestrator,classify}.rs`.
Change: replace each `(R2)` / `(R3)` / `F7` / `G7` / `(W7)` with descriptive prose (`the orchestrator`, `build_switch_if_ladder`, `the stack-array classifier arm`).
Blast radius: ~22 sites across test/source comments. Pure prose.

### D8. Stale "before R2's refactor" comment block
Source: R9-3B #15 (med).
Where: `crates/opt/src/indirect_branch_resolve/stack_array.rs:617-623`.
What: refactor to `pattern::and` / `pattern::or` is in (lines 189, 210); breadcrumb is now actively misleading.
Change: rewrite as a regression-pin doc that doesn't reference rounds.

### D9. Stale "skeleton" claim in validator
Source: R9-3B #18 (med).
Where: `crates/ir/src/validate/mod.rs:5-6`. All three layers + opt-in fingerprint check are implemented.
Change: delete the "skeleton; concrete checks are added by later tasks" sentence.

### D10. Stale "Multiple is reserved for the future jump-table resolver"
Source: R9-3B #16 (med).
Where: `crates/cfg/src/cfg/builder/indirect_resolve.rs:40-44`.
What: the IR-level resolver constructs `Multiple` today; the doc here narrows the claim to the cfg mini-graph but currently overclaims.
Change: rewrite as "Multiple is constructed by `opt::indirect_branch_resolve` (classify, jump-table, stack-array arms); never by *this* mini-graph, which only ever returns `Single` / `LinkRegister` / `None`."

### D11. Stale `apply_tail_call` "returns Unimplemented" header
Source: R9-2B C-1 (98).
Where: `crates/strider/tests/indirect_resolve_in_place_edits.rs:10-12`.
Change: delete the paragraph; rewrite to describe what the file actually pins.

### D12. Stale `Strider::analyze_cfg_with_unresolved` cite
Source: R9-2B C-2 (95).
Where: `crates/strider/tests/indirect_resolve_classify.rs:4`, `crates/strider/tests/indirect_resolve_jump_table.rs:16`.
Change: replace with `Strider::analyze_cfg` returning `AnalyzeOutcome { unresolved_branches }`.

### D13. Stale "round-2 doesn't use stable subset" comment
Source: R9-2B I-6 (88).
Where: `crates/opt/tests/optimizer_pipeline_subsets.rs:89-92`. Orchestrator does use the stable subset today.
Change: rewrite to present-tense factual.

### D14. Stale "R3" / "stack-array not yet implemented" doc on `indirect_branch.rs`
Source: R9-2B C-3 (92).
Where: `crates/strider/tests/indirect_branch.rs:14-20`. Stack-array classifier arm shipped; 7 ignored cases remain for specific lifter shape gaps.
Change: rewrite as current-state description.

### D15. `indirect_branch.rs` test using `Builder::with_endianness` (preset bug)
Source: R9-correctness-cross-arch C-1 (95), R9-1B Finding 3 (88).
Where: `crates/strider/tests/indirect_branch.rs:91`. ARM/AArch64/MIPS/PPC tests silently dispatch CallOther through the x86_64 table.
Change: replace with `cfg::Builder::for_arch(&sleigh_arch, sleigh, addr, cfg_opts)`. Apply the same fix to `crates/cfg/tests/known_targets.rs:30,71,104,143,158,203` and `crates/cfg/tests/indirect_dispatch.rs:159` (R9-correctness-cross-arch I-2).
Blast radius: ~10 test sites + bug class disappears.

### D16. Duplicated lift-addr setup comment block
Source: R9-3B #21 (low).
Where: `crates/strider/src/strider/insn/mod.rs:27-44`. Two stacked block comments describe the same setup.
Change: merge into one.

### D17. Duplicated ARM SWI ABI comment
Source: R9-3B #22 (low).
Where: `crates/target/src/call_other_abi.rs:78-84`. Two paragraphs say the same thing.
Change: drop lines 78-80; keep 81-84.

### D18. Defensive "(which is the standard public entry, not deprecated)"
Source: R9-3B #24 (low).
Where: `crates/strider/src/strider/pipeline.rs:85-89`.
Change: drop the parenthetical.

### D19. `test_int_cmp_op_recovery` non-existent op-name strings
Source: R9-1F-01 (95).
Where: `crates/strider-py/tests/python/test_pattern_full_builders.py:351-355`. Asserts `LessEqual`, `SlessEqual`, `Borrow` — none exist in `IntCmpOp`.
Change: narrow the allowed set to `{Equal, Less, Sless, Carry, Scarry, Sborrow}`.

### D20. `Tb::neg` test helper dispatches to `BitNot`
Source: R9-1D MED (90).
Where: `crates/pattern/tests/matching/support/graph.rs:182-184`.
Change: dispatch to `IntUnaryOp::Neg` (or rename to `bit_not` and add a real `neg`).

---

## 2. Merge

### M1. Unify `tier-1` / `tier-2` doc-prose `[CONTRACT]`
Source: R9-2B I-5 (82).
Where: 25+ sites across `opt`, `strider`, `cfg`, `pattern` doc comments and test fixtures. Round 7 cleaned up identifiers; doc-prose is inconsistent (some files say `tier-1`, others `cfg-time resolver`, others `IR-level resolver`).
Change: standardise on `cfg-time resolver` (was tier-1) / `IR-level resolver` (was tier-2). Apply `Edit … replace_all` per file.
Blast radius: ~25 sites, prose only.

### M2. Merge identical multi-arch test wrappers in `test_utils.rs` + `tests/common/mod.rs::analyze`
Source: R9-1E "Simplification candidates" (low).
Where: `crates/strider/src/test_utils.rs` (4 wrappers: x86_64/x86/aarch64/arm) and `crates/strider/tests/common/mod.rs:176-182` duplicates the `probe_regs + Strider::new` pattern that `strider_for(arch)` (line 140) already encapsulates.
Change: remove the wrappers; single-source through `strider_for(arch)`. Add MIPS / PowerPC wrappers if the wrappers stay.
Blast radius: 4 wrapper sites.

### M3. Merge GOT-PLT / generic relocation Section-error classification
Source: R9-EA1 Finding 2 (med), R9-1E MED (88).
Where: `crates/reader/src/elf.rs:584` (generic) vs `:533-540` (GOT-PLT path).
What: GOT-PLT path correctly buckets section-index errors as `skipped_malformed_target`; generic path uses `skipped_unresolved_target` for the same shape.
Change: change `:584` to `skipped_malformed_target += 1`. Per the doc on `RelocationStats`, malformed-section-index belongs in malformed bucket.
Blast radius: 1 line + doc fix.

### M4. Merge `Truncate(_)` and `Extend(_)` arms (or remove via D3+D4)
Source: R9-1B Findings 1+2.
Where: `classify.rs:233-269`. Two arms with structurally identical logic differing only in op type.
Change: covered by D3+D4 deletion. If kept, fold into one match arm with op-discriminated subbranches.

---

## 3. Inline

### IN1. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` callsites → `Matcher::for_graph` / `RewriteCtx::new`
Source: R9-2D H1 (covered by D2).
Inline at 5 test sites + `opt::with_built`. Already has paved alternatives. See D2.

### IN2. `pattern::sub` / `pattern::int_le` / `pattern::int_sle` etc. — keep as wrappers
Source: R9-1D verified-correct (no finding).
Note: these are intentional ergonomic aliases over the canonicalised lifted shapes. Do NOT inline — they hide a multi-node IR pattern behind one call. Listed for completeness so reviewers don't mistakenly target them.

### IN3. `read_or_init_var` — fold into `build_anchor_calling_context` once Result-flavoured
Source: R9-2C #1 (high), #2 (high).
Where: `crates/strider/src/orchestrator.rs:786` and `:729-731`.
Change: not pure inlining — needs to switch from `Option` to `Result` to surface `TryFrom<u32> for NodeOutputType` errors. Mentioned because the silent-drop pattern is structurally a "bypass that returns None" wrapper that should propagate.

---

## 4. Stdlib idioms

### S1. `let _ = std::fmt::write(&mut out, ...)` — replace with `write!`
Source: R9-2C OK table.
Where: `crates/dot/src/lib.rs:196`. `String`'s `Write` is infallible, so `let _ = …` is hiding a non-failing call.
Change: use `write!(out, "...")` and `.expect("write to String")` if needed, or just `_ = write!(...)`.
Blast radius: 1 site.

### S2. `let _ = self.try_successors(...)` — `try_successors` returns `ControlFlow<()>` that's never `Break`
Source: R9-2C OK table.
Where: `crates/graphwalk/src/lib.rs:48,79`.
Change: simplify the call site by switching `try_successors` to a non-`ControlFlow` return when the closure is known infallible, or document the discard with one comment instead of two `let _`s.

### S3. Replace handwritten `unwrap_or_else(|p| p.into_inner())` poison-recovery with helper
Source: R9-2C OK table notes 4 sites.
Where: `crates/strider-py/src/{pattern,reader}.rs` ×4. Each has a 4-6 line comment justifying poison recovery.
Change: factor a `recover_poison<T>(guard: LockResult<T>) -> T` helper to single-source the pattern. Cosmetic, but tightens the file.

---

## 5. Visibility tightening

### V1. `read_variable_optional` `pub` → `pub(super)`
Source: R9-1A I2 (85).
Where: `crates/ir/src/builder/vars.rs:17`. Only call site is `crates/ir/src/builder/call.rs:118`.
Change: `pub(super)`. Non-breaking.
Blast radius: 1 declaration; 1 caller already in scope.

### V2. `BuiltFunctionGraph` CC fields → `pub(crate)` `[CONTRACT]`
Source: R9-2D H4 (high).
Where: `crates/ir/src/function.rs:59-79`. `call_clobbered`, `ret_val_regs`, `call_other_clobbered`, `variables`, `entry`, `graph` all `pub`. External code can mutate (`bfg.call_clobbered = Box::new([])`) and silently break `Match::get_vn`.
Change: tighten to `pub(crate)`; expose accessors only (`graph()`, `graph_mut()`, `entry()`, `call_clobbered()`, etc.). Most accessors already exist for Strider's use.
Blast radius: ~6 read sites in `pattern::matcher::match_result.rs`, `strider::orchestrator.rs`, `strider::pipeline.rs`. Mechanical.

### V3. `cfg::PcodeInsnAddr` and `cfg::MachineInsnAddr` fields → `pub(crate)` `[CONTRACT]`
Source: R9-2D H2 (high).
Where: `crates/cfg/src/cfg/types.rs:50-55` and `:29`. Doc warns "Do not reorder the fields" — Ord/Hash invariant is unenforced and triple-dot deep-field access pattern leaks to ~30 sites.
Change: fields → `pub(crate)`; add `PcodeInsnAddr::new`, `machine_addr()`, `insn_index()`, `machine_addr_u64()` accessors; same for `MachineInsnAddr.addr` → `as_u64()` + existing `From<u64>`. Migrate 30 sites mechanically.
Blast radius: ~30 sites, mechanical. Affects `cfg::dot.rs`, `cfg::region_builder.rs`, `cfg::tests/*`, `strider::pipeline.rs`. Doc note in CLAUDE.md if any.

### V4. `target::BuiltCallingConventionParts` fields → `pub(crate)` + validation `[CONTRACT]`
Source: R9-2D H3 (high).
Where: `crates/target/src/calling_convention/mod.rs:127`. `from_parts` does no validation; tests in `pattern/tests/get_vn_with_call_override.rs:31` and `ir/tests/build_call_with_cc.rs:67,132` build by hand. A typo overlapping `arg_passing_regs` with `callee_saved_regs` would silently miscompile.
Change: fields → `pub(crate)`; expose builder pattern (`new(stack_ptr_vn).with_arg_passing_regs(...).with_callee_saved_regs(...).build_validated()`). Validate disjointness of arg/callee-saved sets, ret-stack-pop ↔ link-register coupling, syscall-vn ↔ syscall-CC coupling. Optional `from_parts_unchecked` for tests.
Blast radius: 3 test sites + production CC presets if they call `from_parts` directly.

### V5. `IndirectBranchResolve` `unresolved_anchors`/`anchor_contexts` → `pub(crate)` + lockstep type `[CONTRACT]`
Source: R9-2D H5 (high).
Where: `crates/opt/src/indirect_branch_resolve/mod.rs:112-159`. Doc says "orchestrator populates `anchor_contexts` and `unresolved_anchors` in lockstep — a missing entry here means an upstream contract was broken" but the type allows them to drift.
Change: fields → `pub(crate)`; replace twin lists with `Vec<(AnchorAddr, NodeOutputId, AnchorCallingContext)>`, OR add `add_anchor(addr, output, ctx)` builder method that populates both. Either rebuild or extend setters for `is_tail_call`, `link_register_vn`, `stack_ptr_vn`, `rom`. Tests at `:433-488` need adjusting.
Blast radius: 1 type + ~5 test/orchestrator sites.

### V6. `cfg::Cfg<R>` `start_addr_to_region_id` → `pub(crate)`
Source: R9-2D M2 (med).
Where: `crates/cfg/src/cfg/mod.rs:37-67`. Derived lookup index; mutating `graph` directly without updating it would silently break `region_id_at_start`. Currently no mutation paths but the surface allows it.
Change: `pub(crate)`; expose via existing `region_id_at_start`.
Blast radius: minimal; query.rs and dot.rs already go through accessors.

### V7. `target::SleighArch` fields → `pub(crate)`
Source: R9-2D L5 (low).
Where: `crates/target/src/arch.rs:119-130`. `pub` allows a user to set `preset: ArchPreset::X86` while `sla_spec: SLA_SPEC_AARCH64` and silently misclassify CallOthers.
Change: fields → `pub(crate)`; add `endianness()`, `preset()`, `sla_spec()`, `pspec()` accessors. Construction is via preset factories already.
Blast radius: low — `cfg/builder/mod.rs:139-140` reads `arch.endianness, arch.preset` once.

### V8. `ir::FunctionGraph` fields → `pub(crate)`
Source: R9-2D L23 (low).
Where: `crates/ir/src/function.rs:14`. Type isn't re-exported from `ir::lib.rs`, but a future `pub use` would leak `pub` fields.
Change: fields → `pub(crate)` defensively. Only intra-crate consumer is `FunctionBuilder`.

### V9. `opt::Optimizer` and `opt::OptimizerOnBuilt` traits → add `Send + Sync` bound `[CONTRACT]`
Source: R9-2D M4 (med).
Where: `crates/opt/src/pipeline.rs:114-141, 151-162, 180-191`. All current passes already satisfy `Send + Sync` (no `Rc`/`RefCell`); pre-empts breakage when adding parallelism.
Change: `pub trait Optimizer: Send + Sync { … }`. Verified zero impl breakage by inspection of all 12 passes.
Blast radius: 0 impl breaks; declarative tightening only.

---

## 6. Drop wrappers

### W1. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` is a partial-state newtype (covered by D2)
The entire reason this constructor exists is to pretend a CC-less graph is a full `BuiltFunctionGraph`. Round 8 introduced `RewriteCtx { graph, entry }` as the right shape. Drop the wrapper-via-empty-fields pattern; use `RewriteCtx::new` directly.

### W2. `MachineInsnAddr` — the inner `pub addr: u64` adds no invariant
Source: R9-2D H2 (covered by V3).
The newtype provides `Ord`-keyed-by-u64 plus a `From<u64>` ctor — both inherent to `u64`. The wrapping is justified for type-safety against bare `u64` mixups, but the public field defeats the type-safety. V3 covers tightening; the type itself stays.

### W3. `pattern::AnchorAddr` — `(machine: u64, insn_index: u64)` shadow of `cfg::PcodeInsnAddr`
Source: R9-2D M1 (med).
Where: `crates/opt/src/indirect_branch_resolve/mod.rs:191-198`. Layering workaround because opt can't depend on cfg.
Change: do NOT drop. Keep the wrapper but tighten fields to `pub(crate)` + add `from_packed(machine, insn_index)` ctor and a `parts() -> (u64, u64)` accessor.
Blast radius: ~3 sites in `strider::orchestrator` that round-trip via field access.

### W4. `LiftAddrGuard` — covered by D1; the type itself adds an RAII invariant but no caller uses it.

---

## 7. Partial-state types

### P1. `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` (covered by D2 + W1) `[CONTRACT]`
Source: R9-2D H1.
The single most acute partial-state instance. Doc explicitly warns "Callers MUST pass it only to consumers that touch graph and entry; consulting any other field returns a meaningless empty value silently." Sum-type alternative: `enum BuiltOrRewriteOnly { Built(BuiltFunctionGraph), RewriteOnly(RewriteCtx<'_>) }` — but the simpler fix (D2) is just to delete the partial constructor and migrate to `RewriteCtx`.

### P2. `RunConfig::{fn_max_size, allow_code_before_start_addr}` coupling `[CONTRACT]`
Source: R9-2D M3 (med).
Where: `crates/strider/src/orchestrator.rs:60-106`. Doc says "when `fn_max_size.is_some()`, `allow_code_before_start_addr` is ignored." This is a real coupling that the type doesn't express.
Change: replace pair with sum type
```rust
pub enum FunctionBoundary {
    Unbounded { allow_code_before_start: bool },
    Bounded { max_size: u64 },
}
```
This makes "ignored when bounded" unrepresentable.
Blast radius: `RunConfig` constructors + cfg builder consumers (~5 sites). CLAUDE.md note: bounded-lift section already documents the dual semantics; rewrite that paragraph in terms of `FunctionBoundary`.

### P3. `AnalyzeOptions::all_vns` sorted-by-`vn_sort_key` invariant `[CONTRACT]`
Source: R9-2D M3.
Where: `crates/strider/src/strider/pipeline.rs:90-107`. Public `Vec<Vn>` field with documented sort property.
Change: introduce `SortedVns(Vec<Vn>)` newtype with `try_from_vec` (validates) and `from_sorted_unchecked` constructor.
Blast radius: 2-3 callers.

### P4. `RegionLiftHandles` field-by-field arity invariants
Source: R9-2D M3.
Where: `crates/strider/src/strider/pipeline.rs:21-47`. Every field `pub`. `entry_var_phis` keyed by Vn; `exit_vn_to_value: Arc<...>` "never mutated post-build." The `Arc`'s read-only property is enforced by the wrapping; the field-by-field construction is fragile.
Change: expose `pub(super)` fields + builder method. Lower priority than P2.

### P5. `ResolvedTargets::Multiple(Vec<u64>)` empty-vec invariant `[CONTRACT]`
Source: R9-2D M6 (med).
Where: `crates/opt/src/indirect_branch_resolve/mod.rs:82-91`. The classifier checks `!targets.is_empty()` at one site; a future arm forgetting the check would emit `Multiple(vec![])` and `edge_set_of` would silently iterate zero times.
Change: introduce `NonEmptyVec<u64>` (~30 LOC newtype in `entity-utils`) or fold the invariant into a checked ctor `ResolvedTargets::multiple(targets) -> Result<Self, EmptyError>`.
Blast radius: 4 construction sites + 1 consumer.

### P6. `cfg::Region.insns` "Never empty" doc-only invariant
Source: R9-2D L15 (low).
Where: `crates/cfg/src/cfg/types.rs:182-205`. `add_region` actually accepts the empty case (documented for OOB-CondBranch). Invariant is conditional.
Change: tighten the doc to match the impl ("Empty only when terminator is `Branch` after OOB-CondBranch fold") rather than retype. Defer.

### P7. `cfg::RegionTerminator::Switch::target_value` "same value" prose contract
Source: R9-2D L16 (low).
Where: `crates/cfg/src/cfg/types.rs:131-148`. Today the cfg builder always sets to `None`. Latent.
Change: defer; mark as TODO(Task17) tied to the open incremental-rebuild plan.

### P8. `opt::Kb` `ones & zeros == 0` invariant (no-op today)
Source: R9-2D L18 (low).
Where: `crates/opt/src/known_bits/mod.rs:38-43`. Public fields. `KnownBits` only feeds `Kb` through `merge` and `from_const`, both of which check.
Change: defer. No external constructors today.

---

## Cross-cutting recommendations

- **CLAUDE.md edits required**: D1 (drop `LiftAddrGuard` mention if any), D5 (rename `x86_64_systemv_abi` → `x86_64_systemv`), V2-V5/M1 (note the new visibility/accessor surfaces), P2 (note `FunctionBoundary` sum type), P3 (note `SortedVns`).
- **Net deletion estimate**: ~150 LOC (dead arms D3+D4, deprecated alias D5, plan-round breadcrumbs D7, `LiftAddrGuard` re-export D1, partial-state ctor D2, ~25 stale prose lines).
- **Suggested ordering**:
  1. **First batch (low risk, high yield)**: D1, D3, D4, D5, D7-D14, D16-D20, V1, M1, M3.
  2. **Second batch (mechanical migrations)**: V2, V3, V6, V7, V8, V9.
  3. **Third batch (type surface)**: D2 (+W1, P1), V4, V5, P2, P3, P5.
  4. **Defer / opportunistic**: P4, P6, P7, P8, S3.
- **Items audited but explicitly NOT to change**:
  - `pattern::sub` / `int_le` / `int_sle` / `float_*` aliases — intentional ergonomic wrappers for the lifter's canonicalised shapes (R9-1D verified).
  - `pattern::Capture` shape — already well-encapsulated (R9-2D M5 downgraded).
  - `pattern::Pat` opaque wrapper — excellent encapsulation (R9-2D L10).
  - `pattern::RewriteCtx`/`BuildCtx` — transparent argument bundles (L11/L12).
  - `cfg::DecodeCache` mutex-poison recovery — documented + sound (R9-2C OK).
  - The deferred round-8 carryovers L13 (Pattern trait sealing) and L14 (`MatchCtx::graph`) — already mooted by `pub(crate) mod traits;` (R9-2D L13/L14).
- **Out of scope but flagged for separate fix**: round 9 also surfaced several non-simplification correctness items that should be tracked as bugs rather than rolled into this proposal:
  - R9-EA3 C-1: `sysret` mis-classified as `NoReturn` (88) — control-flow bug.
  - R9-correctness-invariants F1: `FunctionArgDetect::detect_stack_args` drops Load fingerprint on direct-width replacement (85) — asm-fingerprint contract violation.
  - R9-EA1 F1: `ConstantFold` multi-node rules (`rule_and_dist`, `(x+C1)+C2`) emit unattributed intermediate nodes (high) — same fingerprint contract.
  - R9-2C #1-#5: silent-drop bugs in `read_or_init_var`, clobber loop, KB classify, `wrap_when` raw-pointer hazard, Python predicate exception swallowing.
  - R9-2A: zero panic findings (good).
