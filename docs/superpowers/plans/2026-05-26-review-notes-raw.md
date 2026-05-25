# Raw review findings — 2026-05-26

Aggregated from 10 parallel read-only review agents on branch `rewrite/ai`.
**Not yet verified.** Self-review step runs before any of these get into the plan.

---

## Agent A1 — Correctness: strider-ir + strider-lift

**A1-H1** (conf 82) `crates/strider-lift/src/cfg/builder/split.rs:50–88`
  `split_region` round-down fallback: second region's `start_addr` set to requested
  `addr` even when first instruction in the second half is at a later addr; means
  `Region::contains_addr` accepts holes that no instruction covers. Latent fragility.

**A1-H2** (conf 85) `crates/strider-lift/src/cfg/builder/region_builder.rs:94–110`
  `lift_one_cached` returns cached pcode without calling `sleigh.lift_one`, bypassing
  Sleigh's mutable context-register state update. Bug for ARM Thumb-interworking
  (`bx lr` switches mode). Cache should be context-scoped per arch.

**A1-H3** (conf 82) `crates/strider-ir/src/validate/use_list_consistency.rs:39–85`
  Backward sweep only inserts inputs for outputs of *reachable* nodes; if a
  reachable consumer has an input pointing at an unreachable producer's output,
  validator fires wrong diagnostic. Also missing reachability check on consumer
  node_id during backward sweep.

**A1 verified clean:** dedup cache hash, retain_reachable two-pass remap, AArch64
upper-half mask, split_region edge rewiring, phi arity check, integer lowerings,
asm-fingerprint no-shrink contract, gc_wide_consts safety, Subpiece/Piece
invariants.

---

## Baseline tooling

- `cargo clippy --workspace --all-targets`: **0 warnings, 0 errors** (clean).
- `cargo crap --workspace` (no LCOV — CC signal only):
  - 213 functions flagged across workspace (per-crate: dot 2, graphwalk 1,
    strider-analyze 94, strider-ir 41, strider-ir-test-utils 4, strider-lift 44,
    strider-pattern-macros 7, strider-py 13, strider-reader 7, strider-target 3).
  - Top CC offenders (≥20):
    - `lift` `strider-lift/src/pcode_lift/value/mod.rs:108` CC=70
    - `GraphDotDumper::pretty_label` `strider-ir/src/graph_dot/label.rs:96` CC=46
    - `node_known_bits` `strider-analyze/src/opt/known_bits/mod.rs:122` CC=35
    - `expected_signature` `strider-ir/src/node_signature.rs:269` CC=34
    - `eval_int_binary` `strider-analyze/src/opt/constant_fold/eval_int.rs:19` CC=32
    - `build_forked_chains` `strider-analyze/src/opt/alias_split/mod.rs:582` CC=26
    - `collect_stack_args_in_chain_order` `strider-analyze/src/opt/call_stack_args/mod.rs:241` CC=26
    - `PyOptimizerPipeline::register` `strider-py/src/pattern.rs:1874` CC=26
    - `collect_stack_args_partitioned` `strider-analyze/src/opt/call_stack_args/mod.rs:46` CC=25
    - `GraphDotDumper::emit_input_edge` `strider-ir/src/graph_dot/render.rs:198` CC=23
    - `PatLike::into_pat` `strider-py/src/pattern.rs:296` CC=22
    - `eval_int_cmp`, `DirtyStep::classify`, `bound_from_if_condition` CC=21
    - `remove_phis`, `ProbeStep::classify`, `find_stack_stored_value_at_offset`,
      `PerRegionDriver::process_insn_inner` CC=20

---

## Agent A2 — Correctness: strider-analyze opt passes

**A2-H1** (conf 82) `opt/pipeline.rs:224-238`
  Fixed-point loop off-by-one: `iters += 1` after `break`, so 1024 cap fires after
  1025 total passes. Minor in practice; convergence guarantee inaccurate.

**A2-H2** (conf 83) `opt/if_cond_inversion/mod.rs:99-146`
  `extend_asm_fingerprint_from(inner_node, bool_neg_node)` unconditional — when
  `BoolNeg` still has other consumers, `inner_node`'s fingerprint gets contaminated
  with addresses that don't actually contribute to its value. Should only absorb
  fingerprint when BoolNeg becomes dead.

**A2-H3** (conf 81) `opt/alias_split/mod.rs:277-287`
  `find_entry_mem_phi` relies on preorder-first match; latent fragility if lifter
  ever emits two structurally-equivalent entry-MemPhi candidates.

**A2-H4** (conf 80) `opt/call_stack_args/mod.rs:395-412`
  `step_through_transparent` blindly returns `MemProject.inputs[0]` (the unified
  predecessor) without checking which partition slot the walk arrived on; can
  cross partitions and spuriously collect args from the Unknown chain.

**A2-M1** (conf 75) `opt/indirect_branch_resolve/classify.rs:89-90`
  `IntConst(k)` truncation `k as u64` silent; should be checked truncation that
  bails None when high bits set.

**A2-M2** (conf 73) `orchestrator/mod.rs:597-616`
  `Multiple` with all-external targets unnecessarily forces a Rebuild. Perf, not
  correctness.

**A2-L1** (conf 72) `opt/dead_branch/mod.rs:128-161`
  `collect_dead_subgraph` doesn't walk MemProject/MemUnion; conservative (safe)
  but adds fixed-point iters on partitioned IR.

**A2 verified clean:** OptimizerPipeline fixed-point, ConstantFold rules,
KnownBits sign-ext, FlagCmpCanonicalize, IfCondInversion control swap,
DeadBranchElimination escape check, LoadReadOnly width/endianness, AliasSplit
idempotency + IndirectBranch bail, CallStackArgCollect slot ordering,
FunctionArgDetect sub-register fallback, StackLoadForward BE-narrow synthesis,
classify_anchor Phi exclusion, classify_jump_table stride overflow,
orchestrator Decision routing.

---

## Agent A3 — Assembly lifting edge cases

**A3-H1** (conf 85) `opt/indirect_branch_resolve/jump_table.rs:840`
  `read_table_entries` interprets table entries as **absolute** unsigned addresses
  with no path for **signed PC/table-relative offset** tables (common at -O2 on
  PPC/MIPS/x86). Will produce wrong CFG edges silently rather than deferring to
  Unresolved. Fix: detect small-relative-to-base entries and rebase as signed,
  or refuse to classify when stride ≠ entry_size and bases look code-like.

**A3-H2** (conf 100) `pcode_lift/vn_io.rs:285-298` — AArch64 scalar FP
  zero-ext gap. Already documented and pinned `#[ignore]`. Patterns matching on
  q-register reads after a scalar FP write see stale upper bits. Listed for
  visibility; the gap is intentionally deferred per the soundness note.

**A3-H3** (conf 82) `opt/indirect_branch_resolve/classify.rs:88-90` + `jump_table.rs:221-224`
  `IntConst(k as u64)` truncates 128-bit constants silently. Same finding as
  A2-M1. Cluster — fix in one place via a helper `u128_to_u64_strict(k) -> Option<u64>`.

**A3-M1** (conf 80) `cfg/builder/region_builder.rs:147-161`
  `decode_branch_target` `bail!` on unusual sizes instead of returning Unresolved.
  Loses an entire lift to a single weird varnode width.

**A3-M2** (conf 83) `strider-target/src/call_other_abi.rs:159-171` (x86 `swi`)
  INT 0x80 entry stubbed with empty implicit_reads/writes; INT 0x80 syscalls
  lift without their EBX/ECX/EDX/ESI/EDI/EBP reads and EAX write. Should mirror
  the x86_64 `syscall` entry's full reg ABI.

**A3-M3** (conf 80) `opt/indirect_branch_resolve/jump_table.rs:162-170`
  AArch64 `cmp + b.hi` flag-based bounds NOT handled by predecessor-If walk.
  Documented as a known gap; AArch64 -O2 switch dispatch will never resolve.
  Significant coverage hole but conservative-fail-closed.

**A3-L1** (conf 80) `pcode_lift/vn_io.rs:38-48`
  `vn_mask` returns u128::MAX for sizes 16/32/64 grouped in one arm — only 16 is
  actually used for sub-reg aliasing; 32/64 reach degraded mask only via guard
  failure. Split the arms + comment.

**A3-L2** (conf 80) `pcode_lift/value/cast.rs:73`
  `build_bit_field_insert`: `len: u8` allows values 128..255 that pass the `>=128`
  guard into `u128::MAX` — guard fires correctly but the type signature admits
  meaningless widths. Tighten via `try_into::<u7>()` style or explicit width check.

**A3 verified clean:** x86/x86_64 sub-reg aliasing, LE endianness threading,
LoadReadOnly endianness (no double-swap), all lift-time canonicalisations
applied in lifter not optimiser, x86_64 syscall ABI, AArch64 SMCCC, ARM
Thumb-mode context handling, MIPS branch-delay handled by Sleigh, PPC CR bits
as register varnodes, jump-table commutative shape matching + index bounding
KnownBits path + predecessor-If with cycle detection.

---

## Agent A4 — Simplification / generalisation

### High-value unifications
**A4-H1** `opt/call_stack_args/mod.rs:24-26` + `opt/function_args/mod.rs:243-245`
  Byte-identical `was_partitioned` helper. Move to `opt::alias_split::was_partitioned`
  (alongside the rest of partition logic) and delete both copies. ~5 LOC.

**A4-H2** `opt/stack_load_forward/mod.rs:593-691` vs same file's `try_forward_load + probe`
  Two memory-chain walkers share every per-step rule (Store arms, MemProject,
  MemUnion, decompose_sp, ranges_disjoint) — ~70 LOC of parallel match scaffolding.
  Extract `step_store_for_alias_query` + `step_partition_boundary` so the linear
  walker only owns memo+MemPhi-bail. 40-60 LOC saved.

**A4-H3** `opt/call_stack_args`, `opt/function_args`, `opt/stack_load_forward`
  Three implementations of partition-aware MemUnion/MemProject step-through.
  Extract `walk_stack_partition_chain<S: PartitionStep>(graph, mem_in, step)`
  driver, callers declare per-Store verdict only. ~150 LOC across three files.

**A4-H4** `opt/pipeline.rs:364-501` tests reinvent RegisterSet + sp_vn_x86()
  Four tests inline `Vn { addr_off: 0x20, addr_space: REGISTER, size: 4 }` +
  `FunctionBuilder::new_raw(...)`. `strider-ir-test-utils` already exposes
  `sp_vn_x86()` and `RegisterSet`. ~30 LOC.

### Medium
**A4-M1** `opt/alias_split/mod.rs:464-500` PartitionHeads = `[Option<NodeOutputId>; 2]`
  with hand-rolled `partition_index(AliasClass) → usize`. Either swap for
  `FxHashMap<AliasClass, NodeOutputId>` or generate index from
  `ACTIVE_PARTITIONS.iter().position(...)`.

**A4-M2** Four opt passes' `::new` test-convenience constructors are dead in prod
  (AliasSplit, CallStackArgCollect, FunctionArgDetect, StackLoadForward —
  only tests call them). Either `#[cfg(test)]`-gate or collapse into `CcPass`
  trait. ~40 LOC. (CAUTION: verify against Python — if `strider-py` calls
  these from PyO3, they must stay public.)

**A4-M3** `opt/alias_split/mod.rs:155-198` two early `NoChange` bails
  (`MemProject|MemUnion` idempotent; `IndirectBranch` defer). Extract
  `should_skip(function) -> Option<&'static str>`.

**A4-M4** `opt/alias_split/mod.rs:600-616, 1040-1056` two byte-identical
  MemProject creation sites. Extract `create_partition_project(...)`.

**A4-M5** Three test files (call_stack_args/function_args/stack_load_forward
  tests) manually wire MemProject+MemUnion around InitialMemory chain — 50 LOC
  duplicated. Extract `strider-ir-test-utils::manually_partition_stack_chain`.

**A4-M6** `pattern/matcher/match_result.rs:81-153` eight `get_*_op` delegators —
  each is `self.bindings.get_X_op(c, graph)`. Decl_match_op_accessors macro
  or generic `get_op_variant::<T>` — ~60 LOC.

**A4-M7** Three opt passes (KnownBits/RedundantPhis/DeadBranchElimination)
  reimplement `peephole::run_peephole`'s worklist+drain loop. Lift to
  `run_peephole_with_state<S>`. ~30-50 LOC.

### Low / nitpick
**A4-L1** `opt/alias_split/mod.rs:947-974` mem_input_value returns Result,
  mem_input_values returns Vec (no error). Asymmetric.
**A4-L2** `opt/alias_split/mod.rs:528-548` `kind_label: &'static str` param
  only used in error message — recoverable from `function.node_kind(consumer)`.
**A4-L3** `opt/sp_pass_cc.rs:26-58` `minimal_cc_for_sp` is one-line wrapper.
  Inline + delete.
**A4-L4** `opt/pipeline.rs:13-30` `OptimizationResult::from_changed` →
  `impl From<bool>`.
**A4-L5** `opt/mod.rs:46-82` disordered pub mod / mod block — group.
**A4-L6** `opt/redundant_phis/mod.rs:138-180` nested Option-pair match — extract
  `single_reachable_ctrl` and `single_distinct_live_value`.
**A4-L7** `opt/test_support.rs:41-63` `standard_test` should be one-line
  `cf_rp_pipeline + StackLoadForward::new`.

### Rejected (load-bearing rationale exists)
- walk_mem_chain + linear walkers: comment in `mem_walk.rs:30-58` justifies split.
- OptimizerPipeline.passes/post_passes: external API consumed by strider-py.
- Optimizer + Clone trait → enum: load-bearing for external extensibility.
- Per-kind pattern builders consolidation: structurally distinct.

---

## Agent A5 — clippy / cargo-crap / comments / skills

### Task 1 — clippy + cargo crap
- clippy clean (no warnings/errors workspace-wide with `-D warnings`).
- crap top offenders match A4 baseline; also flagged is a TEST function
  `node_kind_name` `crates/strider-analyze/tests/cross_arch_shape.rs:49` CC=80.

### Task 2 — plan-identifier comments
**A5-H1** `crates/strider-analyze/src/strider/insn/mod.rs:131,191,206,218,246`
  References to "seven numbered phases", "Phase-1 helper", "Phases 2+3",
  "Phase-7 helper". Local-algorithm labelling but matches the user's strict
  "no Phase N in code" rule. Rename to step-style or descriptive prose.
**A5-H2** `crates/strider-ir/src/graph_dot/render.rs:52,58,63,74,135,192`
  "Phase A/B/C" subsection markers inside `dump_as_dot`. Same — rename.

### Task 3 — stale / incorrect comments
**A5-H3** `opt/mem_walk.rs:5,6,14,33-34,43-46`
  References `StackStore`, `StackStorePhi` nodes that don't exist. Real names:
  `Store(VnSpace)` with stack-offset metadata in `Function::stack_offsets`.
  Also references `sp_expr::step_through_*` family that doesn't exist (only
  `step_through_store` singular).
**A5-M1** `opt/alias_split/mod.rs:58` "v1 scope" — no v2 ever shipped; rename
  to "Current scope" or drop.
**A5-M2** `strider-ir/src/lib.rs:31` — claims orchestrator drives FunctionBuilder
  directly from CFG; actual path goes through PerRegionDriver. Mild.

### Task 4 — README/CLAUDE.md drift
**A5-H4** `README.md:162,214,238` references `ControlState` NodeKind variant —
  actual name is `Region` (per `strider-ir/src/node/kind.rs:39`).
**A5-H5** `README.md:162,220` references `FunctionArg` NodeKind variant —
  doesn't exist (CLAUDE.md:411 explicitly states this). Real mechanism is
  `Function::arg_index_to_nodes` mapping arg-index → `InitialVar`/`Load`.
**A5-H6** `README.md:217` lists `StackStoreDetect` opt pass — doesn't exist.
  Work happens inside `AliasSplit`'s side-table population.
**A5-H7** `README.md:268` + `CLAUDE.md:329` claim `run() -> Result<Graph>`.
  Actual signature is `Result<Function>` (`orchestrator/mod.rs:155`).
**A5-H8** `CLAUDE.md:313-314` describes `FunctionArgDetect` as canonicalising
  reads "into `FunctionArg` nodes" — self-contradicts CLAUDE.md:411.
  Reword as "into the `Function::arg_index_to_nodes` side-table".

### Task 5 — skills
**A5-H9** `.claude/skills/strider-asm-to-pattern/SKILL.md`
  - References `StackStoreDetect` 3× (66,144,158) — doesn't exist.
  - Lines 168,201: explicit `[Sketch — second half]` placeholders never
    filled in.
  - Refs to non-existent sibling skills: strider-pattern-author,
    strider-debug-pattern, strider-fixture-author.
**A5-H10** `.claude/skills/strider-py-pattern/SKILL.md` — ALREADY covers
  "generate python patterns"; well-aligned with code. Only fix: drop the
  4 cross-refs to non-existent siblings (lines 19,463-466).
**A5-H11** `.claude/skills/strider-rewrite-rule-author/SKILL.md`
  - Line 58 references `ControlState` — should be `Region`.
  - Cross-refs to strider-opt-pass-author (10,21,219), strider-rewrite-
    rule-multinode-audit (20,94,223) — both non-existent.

**Cross-cutting:** 7 distinct sibling skill names referenced across the 3
SKILL.md files don't exist. Strip references (do NOT create stubs — the
user wants real skills only).

**Conclusion on "skill for generating python patterns":** the existing
`strider-py-pattern` skill already covers this. No new skill needed; just
fix the broken refs above.

---

## Agent A6 — Calling convention correctness

### High
**A6-H1** `crates/strider-analyze/src/orchestrator/mod.rs:858`
  `AnchorCallingContext::for_anchor`'s synthesized Return drops
  `cc.ret_val_regs_float`. On AArch64 q0/q1, MIPS f0/f2, PPC f1/f2, ARM d0/d1,
  x86_64 XMM0/XMM1 the synthesized Return-via-link-register has different
  arity than the naturally-lifted Return for the same function. Fix:
  iterate `ret_val_regs.iter().chain(ret_val_regs_float.iter())`.

**A6-H2** `crates/strider-analyze/src/opt/indirect_branch_resolve/inplace.rs:230`
  `apply_tail_call` ignores `no_memory_clobber`. The spliced Call always
  emits `Memory(None)` + wires it into the Return. For `x86_64_all_preserving`
  (e.g. `__fentry__` tail call) this breaks `LoadReadOnly`/`StackLoadForward`
  chains — opposite of all-preserving semantics. Also `orchestrator::apply_in_place_edit`
  (718-727) doesn't pass `no_memory_clobber` into `apply_tail_call`. Fix:
  parameterise + when true leave Call mem dangling, wire `memory_in` into Return.

### Medium
**A6-M1** Orchestrator's clobber projection (`mod.rs:843-849, 900-911`)
  duplicates `FunctionBuilder::build_call_with_cc::select_call_abi`
  (`strider-ir/src/builder/call.rs:142-147`). Centralise on a
  `BuiltCallingConvention::call_clobbered_for(variables)` helper.

**A6-M2** `strider-analyze/src/strider/insn/control.rs:293-298` handle_call_indirect
  doesn't apply per-address override at lift time (deferred to orchestrator).
  Correct but unpinned — add an integration test for "indirect resolves to
  intra-fn override target".

**A6-M3** `crates/strider-target/README.md:45,54` — documents
  `BuiltCallingConvention::positional_arg_layout()` method that doesn't exist
  (only `PositionalArgLayout::from_convention(&cc)` free fn).

**A6-M4** Validator gap: no graph-level check on Call/Return arity vs
  cc_metadata. Mismatched arity from A6-H1 lands silently.

**A6-M5** `crates/strider-py/src/cc.rs:1-100` — Python users cannot construct
  a **custom** CC; only the ~22 presets are exposed. Memory mandates we
  "treat custom CC correctly in all construction + optimization", but no
  Python construction path exists. Add `CallingConvention.custom(...)`
  builder + `try_new` validation surface.

### Low
**A6-L1** `mips_linux_syscall_n64` arg list comment ambiguity (cosmetic).
**A6-L2** `x86_64_all_preserving` with stack-arg override unintuitive — doc only.
**A6-L3** `sp_pass_cc.rs:42-58` minimal_cc bypasses try_new validation — OK
  because pub(crate) test shorthand. Doc comment acknowledges.

### Agent A6 verified clean
- BuiltCallingConvention::try_new validation suite.
- CallingConvention::build register-name resolution against SleighArch.
- LinkRegister consistency between IR + cfg level resolvers.
- PositionalArgLayout single source of truth (consumers actually use
  `from_convention` — 3 callers, no recomputation. The 4-consumer claim
  in CLAUDE.md is stale: StackStoreDetect doesn't exist, AliasSplit/SLF
  only need sp_vn).
- CallOtherAbi (implicit_reads/writes/mem_clobbers) fully wired.
- no_memory_clobber correctly suppresses mem advance in build_call_with_cc
  (function-default + per-call override) — exception is A6-H2 in apply_tail_call.
- Per-address CC override plumbing (4 consultation sites) all consistent.
- ret_val_regs_float upgrade-to-tracked-container via FunctionBuilder::new.

---

## Agent A7 — Dead code + panic/unwrap audit

### Confirmed unused (delete-safe)
**A7-H1** `crates/strider-analyze/src/pattern/matcher/mod.rs:499`
  `pub fn function_args_for(&self, index: u32) -> impl Iterator<...>` — 0 callers.
**A7-H2** `crates/strider-ir/src/ops/consts.rs:42`
  `pub fn float_const_val(&self, ...)` — 0 callers (sibling `int_const_val` used).
**A7-H3** `crates/strider-ir/src/ops/consts.rs:102`
  `pub fn make_bool_const(&mut self, val: bool)` — 0 callers (sibling
  `make_int_const` used).
**A7-H4** `crates/strider-ir/src/function.rs:114`
  `pub fn from_built_graph(graph, entry) -> Self` — 0 callers.
**A7-H5** `crates/strider-analyze/src/pattern/mod.rs:175-181`
  `#[allow(unused_imports)] pub(crate) use pat::{…}` — inline comment confirms
  re-exports are inert. Drop the whole block.
**A7-H6** `crates/strider-analyze/src/pattern/pat/traits.rs:106`
  `BuildOutcome::Skip` with `#[allow(dead_code)]` — fallback never used
  (`RewriteSkip` error sentinel used instead). Delete the variant.

### Suspected unused / over-public (demote)
**A7-M1** `strider-ir/src/lib.rs:67` `pub mod wide_const` — zero external
  consumers, all callers use `crate::wide_const::…`. Demote to `pub(crate)`.
**A7-M2** `strider-ir/src/wide_const.rs:58` `pub fn limbs` — only intra-module
  callers. Demote.
**A7-M3** `strider-ir/src/iterators.rs:100` `pub fn move_next` — only used
  by `replace_current_with` (same file). Demote.
**A7-M4** `strider-ir/src/builder/coerce.rs:68` `pub fn get_as_bool` — only
  intra-file. Demote.
**A7-M5** `strider-ir/src/graph/compact.rs:73,81` `pub fn output_old_to_new`
  + `input_old_to_new` — used only in test module. Demote to `pub(crate)`
  or `#[cfg(test)]`-gate.
**A7-M6** `strider-lift/src/cfg/builder/region_builder.rs:49`
  `pub enum ProcessInsnRes` — doc on 59 says "Created internally; not public
  API". Demote to `pub(super)`/`pub(crate)`.
**A7-M7** `crates/dot/src/lib.rs:361` `pub fn dot_node_count` — only used
  internally + tests. Demote.
**A7-M8** `strider-ir/src/graph_dot/label.rs:22` `pub fn vn_to_display_name`
  — only used by trait impl + tests. Module already `pub(crate)`. Shrink.
**A7-M9** `strider-analyze/src/pattern/pat/ctor/consts.rs:29`
  `pub type BuildValueFn<T>` — sibling fns already `pub(crate)`. Match.

### Trait-alias keep-decisions
- `graphwalk::TreePreOrder/PostOrder` aliases: no external callers but look
  like intentional stable API. Confirm with user; default keep.

### Examples / benches
**A7-L1** `crates/strider-analyze/examples/memory_demo.rs` — undocumented in
  CLAUDE.md (only orchestrator_demo + dump_arch_cmps documented). Keep or
  document.

### Panic/unwrap in production
**Production code is CLEAN.** Three `.expect(...)` sites all carry
`#[allow(clippy::expect_used)]` with `// SAFETY:` reasons that are
load-bearing:
- `crates/strider-ir/src/builder/vars.rs:73-76`
- `crates/strider-ir/src/builder/mod.rs:293,671`

All `panic!`/`unreachable!`/`todo!` workspace-wide are inside `tests/`,
`benches/`, `#[cfg(test)]` mods, or test-utility crate's allow-block.
**No action needed on the panic/expect/unwrap axis.**

---

## Agent A8 — Python API + tests + pattern coverage

### A. Broken Python tests (root: typed-error collapse + stub drift)
**A8-H1** `test_smoke.py:9-13` `test_error_hierarchy` references
  `errors.{LiftError,ReaderError,PatternError,RewriteError}` — all collapsed
  to `StriderError`. AttributeError on first reference.
**A8-H2** `test_typed_errors_e2e.py:35-269` — entire file references the
  removed subclasses + `UnresolvedIndirectBranchError` + `UnknownCallOtherError`.
  Module load fails.
**A8-H3** `test_symbol_size.py:35` `pytest.raises(errors.ReaderError)` — same.
**A8-H4** `strider/pattern.pyi:233-234` declares `stack_store()` and
  `stack_store_phi()` module fns — neither exists. `.pyi` lies; runtime
  AttributeError.
**A8-H5** `strider/__init__.pyi:80-86` missing `MemoryMap.set_endianness(str)`
  method that exists in `reader.rs:210`. Mypy error.
**A8-H6** `strider/__init__.pyi:144-151` `Strider` stub matches the cdylib's
  `PyStrider(arch, sleigh, cc)` but `strider/_api.py:161`'s `Strider` class
  has `(mem, arch, cc)` signature — name collision via overshadowing.
  Calls per `_api.py:174` doctest fail at runtime.
**A8-H7** `strider/opt.pyi:7-31` missing `FlagCmpCanonicalize` and
  `IfCondInversion` (both exist in `opt.rs:266-271, 407-408`).

### B. Rust opt/patterns missing from Python
**A8-H8** `AliasSplit` opt pass — **entirely absent** from Python (no
  `PyAliasSplit` in `opt.rs::register`, no enum variant in `PyOptPass`).
  Python users assembling custom pipelines silently degrade.
**A8-M1** `float_cmp(op, l, r)` — parametric cmp constructor not in Python
  (only `float_eq/lt/le/ne` + `float_cmp_any`).
**A8-M2** `int_unary(op, operand)` not in Python (only named ops + `int_un_any`).
**A8-M3** `bool_unary(op, operand)` not in Python (only `bool_not`).
**A8-M4** `float_unary(op, operand)` not in Python.
**A8-M5** No `int_const_wide(bytes)` / equivalent — matches against
  U256/U512 `IntConstWide` nodes are not expressible from Python pattern DSL.

### C. Missing docstrings on Python user API (~15 sites)
- `cfg.rs:25-32,74-89` `build_cfg`, `PyCfg.to_html/to_dot/html_str`
- `arch.rs:9-25` `PySleighArch` + 15 macro-emitted classmethods
- `cc.rs:9-76` `PyCallingConvention` + 21 macro-emitted preset classmethods
- `strider_cls.rs:42-91` `PyStrider.__new__`, `analyze_cfg`
- `matcher.rs:108-156` `PyMatch.{__getitem__, __contains__, uint, int_, bool_, float_bits, has}`
- `reader.rs:224-251` `PyMemoryMap.{add_region, region_count, read}`
- `sleigh.rs:69-86, 111-153, 185-209` PySleigh + PyVnSpace + PyVn
- `graph.rs:170-194` `PyGraph.{to_html, to_dot, html_str, node_count}`
- `pattern.rs:52-105` `PyCapture` ctor + dunder
- `opt.rs:198-241,266-271,286-307` PyOptimizerPipeline factory+add methods
  + all macro-emitted opt-pass classes
- `run.rs:26-69` `PyRunResult` getters + `run` pyfunction

PyO3 emits Python `__doc__` from `///` (not `//!`). Fix: add `///` per item.

---

## Agent A9 — Test parity feature/ai vs rewrite/ai

### Missing Rust tests (coverage gaps)
**A9-H1** `feature/ai:crates/opt/tests/pipeline_subsets.rs` — pass-membership
  tests for stable/destructive/default subsets (6 tests). New
  `optimizer_pipeline_subsets.rs` only checks counts. Suggested: add to
  `strider-analyze/tests/optimizer_pipeline_subsets.rs`.
**A9-H2** `feature/ai:crates/opt/tests/multi_pass.rs` — ~6 of 10 multi-pass
  cooperation cases not migrated. Extend
  `strider-analyze/tests/multi_pass_cooperation.rs`.
**A9-H3** `feature/ai:crates/opt/tests/pipeline_default.rs` — 5 end-to-end
  default-pipeline smoke tests. Add new
  `strider-analyze/tests/pipeline_default.rs`.
**A9-M1** `feature/ai:crates/opt/tests/pipeline_fixedpoint.rs` —
  `default_pipeline_idempotent` smoke missing.
**A9-M2** `feature/ai:crates/pattern/tests/get_vn_with_callother_clobber.rs`
  — 3 tests for `get_vn` on CallOther clobber slots (value-bearing vs no-value,
  override vs default). Extend `pattern_matching/matcher_api.rs`.
**A9-M3** `feature/ai:crates/pattern/tests/get_vn_with_call_override.rs::get_vn_indexes_override_list_for_overridden_call`.
**A9-M4** `feature/ai:crates/pattern/tests/pattern_next_mem_zero_when_multi_consumer.rs::next_mem_returns_no_match_when_no_consumer`
  (sibling present).
**A9-M5** `feature/ai:crates/ir/tests/call_other_classification.rs` +
  `call_other_modeled.rs` — 3 missing tests: `build_call_other_terminal_emits_ctrl_mem_only`,
  `build_call_other_modeled_with_empty_abi_no_clobbers`, `modeled_does_not_advance_memory_token`.
**A9-M6** `feature/ai:crates/ir/tests/build_validate_roundtrip.rs` —
  4 of 10 missing: const-then-return, every-int-cmp-op, extend/truncate,
  float-int conversions.
**A9-M7** `feature/ai:crates/ir/tests/walk_reachability.rs` —
  `diamond_join_via_phi_visits_all_arms` missing.
**A9-M8** `feature/ai:crates/ir/tests/proptest_graph_invariants.rs` —
  `walk_visits_each_node_at_most_once` and `dedup_determinism` missing.
**A9-M9** `feature/ai:crates/ir/tests/retain_reachable.rs::retain_reachable_preserves_asm_fingerprint_on_surviving_node`.
**A9-M10** `feature/ai:crates/pattern/tests/matching/stack.rs` — stack-phi
  offset suite (`stack_store_phi_*`, `stack_store_offset_any_*` — ~7 tests).
  New `load_store_stack_offset_capture.rs` covers ~11 cases but misses
  multi-offset stack-phi.

### Missing Python tests
**Zero.** All 54 Python test files preserved; 7 net-new.

### Likely-broken Python tests
Same root cause as A8: typed-error subclasses collapsed. **3 broken files** —
`test_smoke.py`, `test_typed_errors_e2e.py`, `test_symbol_size.py` — all
AttributeError on collection or specific assertions. The rest of the
Python suite is structurally intact.

---

## Agent A10 — Optimization + data structures

### High
**No O(n²) or recursion risks found.** topological_mem_order cycle fallback
already fixed; all walks iterative with DenseEntitySet+Worklist.

### Medium
**A10-M1** `opt/alias_split/mod.rs:296,299,368,369,472,617,881-883`
  uses `FxHashMap<NodeId/NodeOutputId, _>` for `addr_class`, `barriers`,
  `outgoing_heads`, `predecessors/successors/in_degree` — all
  EntityRef-keyed. Swap to `SecondaryMap`.
**A10-M2** `orchestrator/mod.rs:125` `RegionIndex.by_exit_control:
  FxHashMap<NodeOutputId, ExitVnToValue>` — swap to SecondaryMap.
**A10-M3** `strider-ir/src/graph_dot/mod.rs:155,162,192,195`
  `node_to_arg_indices`, `visited_node_id`, `virtual_nodes` — all EntityRef-keyed.
  (Lower priority — debug-only path.)
**A10-M4** `opt/sp_expr/decompose.rs:102` `SpExprMemo: FxHashMap<NodeOutputId, _>`
  — five passes reuse this memo. Hot. Swap to SecondaryMap.
**A10-M5** `pattern/pat/builders/function_arg.rs:96-101` —
  `FunctionArgPattern::kind_spec() = KindSpec::Any` forces wildcard scan
  over every reachable node. Add inverse map
  `arg_indices_by_node: SecondaryMap<NodeId, SmallVec<[u32; 1]>>` on Function.
**A10-M6** `strider-ir/src/function.rs:75` `arg_index_to_nodes:
  FxHashMap<u32, Vec<NodeId>>` — indices dense + small; use
  `Vec<SmallVec<[NodeId; 1]>>` (or paired with M5).
**A10-M7** `strider-ir/src/function.rs:54` `asm_fingerprints:
  SecondaryMap<NodeId, Vec<u64>>` — Vec heap-allocates per node.
  `SmallVec<[u64; 2]>` would inline single-contributor case (the common case).
  Same for `call_clobbered_overrides` (line 57),
  `call_stack_arg_offsets_overrides` (line 64).
**A10-M8** `opt/pipeline.rs:228-244` — every pass calls `ctx.preorder()`
  independently. Consider caching on Function with a structural-mutation
  counter (analog to `Graph::generation`). Worth profiling first.

### Low
**A10-L1** `opt/dead_branch/mod.rs:281` — seeds worklist with full preorder;
  use `seeded_kind(ctx, |k| matches!(k, NodeKind::If))` for symmetry.
**A10-L2** `strider-ir/src/graph/access.rs:211-219` `reachable_kind_iter`
  iterates full arena including zombies; iterate `reachable.iter()` instead.
**A10-L3** `opt/alias_split/mod.rs:889` `preds: Vec::new()` per-loop-iter.
  Hoist + `.clear()`.

### Things checked clean
- Memory-chain walks (mem_walk.rs:166+) iterative with DenseEntitySet cycle guard.
- find_largest_fitting_register fast/fallback paths.
- Pattern matcher kind-index (matcher/mod.rs:134-148) O(N) build + O(bucket)
  per find_all.
- decompose_sp depth cap (512).
- flatten_add_tree depth cap (32).
- CFG explore work-queue driven.
- region_membership_from_exit iterative.
- KnownBits already migrated from FxHashMap to SecondaryMap.
- Peephole driver single Worklist drain.
- validate use-list O(N+E) single sweep.

---

