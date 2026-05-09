# Round 8 — Simplification Proposals

**Branch:** `review/ai2` at `/mnt/c/Users/mikeg/Documents/strider`.
**Status:** Proposals only — no source files modified.
**Cross-references:** `round8-2B-naming.md`, `round8-2C-silent-failures.md`,
`round8-2D-types.md`, `round8-1C-opt.md`, `round8-3B-comments.md`,
`round8-16-perf.md`.

This document collects 30 concrete, actionable simplifications drawn from the
round-8 per-ask reports plus a fresh skim of `crates/`.  Items are grouped by
section and tagged with priority (HIGH / MED / LOW).  Every item cites
absolute file paths and line ranges.

---

## Section 1 — Code to delete

### 1.1 (MED) Dead always-true guard in `apply_link_register`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/inplace.rs:60-63`.
- **What:**
  ```rust
  let inputs = graph.node_inputs(placeholder);
  if inputs.len() > 2 && inputs.len() > ret_val_outputs.len() + 2 {
      graph.remove_node_input(placeholder, 2)?;
  }
  ```
- **Why deletable:** Cross-ref `round8-1C-opt.md`.  By the time control
  reaches the guard, the loop above has appended every `ret_val_outputs`
  entry to a node that started with exactly 3 inputs (control, memory,
  target_value).  Both halves of the guard reduce to `3 + N > 2` (always
  true) and `3 > 2` (always true).  The `remove_node_input` is therefore
  unconditional.  A future reader can mis-read this as conditional and
  conclude `target_value` removal is sometimes skipped.
- **Migration:** Replace the `if` with an unconditional
  `graph.remove_node_input(placeholder, 2)?;` and add a
  `debug_assert!(graph.node_inputs(placeholder).len() >= 3, "IndirectBranch must have ≥3 inputs");`
  above it.  No callers migrate.

### 1.2 (LOW) Stale `CallOtherElide` tombstone in `opt::lib.rs`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/lib.rs:148-151,181-183`
  and `/mnt/c/Users/mikeg/Documents/strider/crates/opt/README.md:89-91`.
- **What:** Doc-comments mention the deleted `CallOtherElide` pass.
- **Why preserve / how to delete:** Round 7's deletion left intentional
  breadcrumbs for grep continuity.  Cross-ref `round8-3B-comments.md` MED
  list — the recommendation is to KEEP these because they are
  rot-prevention.  Listed here only for completeness; do not delete.

### 1.3 (LOW) `crates/strider/src/indirect_resolve/classify.rs` shim is a thin wrapper

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/indirect_resolve/classify.rs:14-66`.
- **What:** Three thin delegators (`classify_anchor`,
  `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`) each
  call into `opt::classify_anchor*`.  The doc-comment at line 1-5 says
  "Retained as a strider-side entry point so the orchestrator and
  integration tests can call into the classifier under a stable strider
  path" — but the orchestrator at
  `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs`
  drives the `IndirectBranchResolve` pass directly via `optimize`,
  never through this shim.
- **Why deletable:** The shim's only consumer is its own test module
  (lines 67-449).  Tests should call `opt::classify_anchor*` directly.
  Cross-ref `round8-2C-silent-failures.md` M1 — the shim's
  `analyze_known_bits` failure path silently swallows the error,
  duplicating a bug from `opt`.
- **Migration:** Move the test module into `crates/opt/src/indirect_branch_resolve/classify_tests.rs`,
  delete `crates/strider/src/indirect_resolve/classify.rs`, drop the
  `pub mod classify;` and the strider-level `pub use` chain.

### 1.4 (MED) Reserved-but-uncalled `IndirectBranchResolve::new` defaults

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:199-221`.
- **What:** `IndirectBranchResolve::new()` constructs a pass with all
  fields defaulted (empty anchors, default `is_tail_call: |_| false`,
  None lr/sp_vn).  The constructor is `pub` and has a `Default` impl.
- **Why deletable:** Every production caller in
  `crates/strider/src/orchestrator.rs` pre-fills every field before
  invoking `optimize`.  A "fully-defaulted" instance has no analytical
  use — tests at `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs`
  also build with field-by-field initialisation.  Subsumed by the
  `RunConfigBuilder`-style fix proposed in `round8-2D-types.md` (see
  Section 6 below).
- **Migration:** Replace `pub fn new()` + `impl Default` with a
  `pub fn from_anchors(anchors, contexts, is_tail_call, lr_vn, sp_vn, rom)`
  that takes ownership.  Delete the `Default` derive.  ~2 orchestrator
  sites adjust their call shape.

### 1.5 (LOW) `M9` unreachable `unwrap_or(u64::MAX)` in `region_builder`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/region_builder.rs:165`.
- **What:**
  ```rust
  let pcode_count = u64::try_from(lift_res.insns.len()).unwrap_or(u64::MAX);
  ```
- **Why deletable:** `usize` → `u64` is infallible on every supported
  target (32/64-bit).  The fallback is unreachable.  Cross-ref
  `round8-2C-silent-failures.md` M9.
- **Migration:** Replace with `lift_res.insns.len() as u64` and add a
  `// usize -> u64 is infallible on supported targets` comment.

---

## Section 2 — Helper consolidation

### 2.1 (HIGH) Centralise the poisoned-mutex `map_err` boilerplate in `strider-py`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/graph.rs:58,67,80,92,105,115,136,183,197,213,229,245,262,277` (18+ sites);
  `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs:169,191,224,233,242,250` (6 sites);
  `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/reader.rs` and `pattern.rs` (more).  Total: ~50 sites.
- **What:** Identical `.map_err(|_| anyhow::anyhow!("X lock poisoned"))?`
  / `.map_err(|_| crate::errors::into_strider_err(anyhow!(...)))?` chain
  repeated across every `Mutex`/`RwLock` access in the FFI layer.  Each
  site discards `PoisonError::into_inner`.
- **Why consolidate:** Cross-ref `round8-2C-silent-failures.md` H2/H3/L12.
  The current pattern is verbose AND silently loses the panic-data the
  inner `PoisonError` carries.  A single helper would also let us add a
  one-line `eprintln` on first poison observed (cross-ref H2).
- **Migration:** Add `crates/strider-py/src/errors.rs` helpers:
  - `lock_poison_strider_err(target: &'static str) -> PyErr`
  - `lock_poison_anyhow(target: &'static str) -> anyhow::Error`
  - `unwrap_or_recover_poison<T>(result: LockResult<MutexGuard<'_, T>>) -> MutexGuard<'_, T>`
    (mirrors the `clear_graph_ptr` recovery at `pattern.rs:282-285`)
  Replace each `.map_err(|_| anyhow!("X lock poisoned"))` with
  `.map_err(|_| lock_poison_anyhow("X"))`.  Roughly 50 call sites trivially
  rewrite to a one-line invocation.  Cross-ref H3 — the predicate-callback
  path at `pattern.rs:289-298` should use the recovery helper.

### 2.2 (MED) Consolidate `analyze_known_bits → eprintln → None` into a single helper

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/classify.rs:54-64,88-94`
  and `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/indirect_resolve/classify.rs:49-56`.
- **What:** Three near-identical match arms wrap `analyze_known_bits`,
  `eprintln!` on `Err`, and convert the result to `None`.  Cross-ref
  `round8-2C-silent-failures.md` M1.
- **Why consolidate:** A `Kb::merge` contradiction is a real IR-level
  bug; the current shape silently surfaces as "anchor unresolved" and
  hides the underlying `Err`.  A single fallible helper that returns
  `Result<Option<ResolvedTargets>>` would propagate the error correctly.
- **Migration:** Convert `classify_anchor` and `classify_anchor_with_rom`
  to fallible (`-> Result<Option<ResolvedTargets>>`) and delete the
  duplicate strider shim per Section 1.3.  ~5 production callers; tests
  add `?` or `.expect("known-bits did not contradict")`.

### 2.3 (MED) Per-anchor `recompute_unresolved` helper duplicates filter logic

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:554-580` plus the
  per-anchor sweep at `apply_in_place_edits` (lines 512-549).
- **What:** Filter "drop entries whose placeholder Return was detached"
  is repeated in two flavours: orchestrator's `recompute_unresolved`
  walks the in-place edit list and the loop at line 530 walks
  `all_node_ids()` for `InitialVar` reuse.
- **Why consolidate:** Both walks consult the same per-iteration index;
  the second walk is also cross-referenced by `round8-16-perf.md`
  Finding F (zombie-inclusive scan).
- **Migration:** Lift the `InitialVar` index population into a
  `RegionIndex::collect_initial_var_index(graph) -> SecondaryMap<...>`
  helper that uses `graph.preorder()` instead of `all_node_ids()`.
  Replaces ~7 lines and makes the post-iteration filter a single
  `worklist.retain(...)` call.

### 2.4 (MED) Test-fixture helpers duplicated across `crates/*/tests/common/`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/tests/common/{assertions,real_binary,synthetic}.rs`,
  `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/mod.rs` (~100 lines),
  `/mnt/c/Users/mikeg/Documents/strider/crates/opt/tests/common/mod.rs`,
  `/mnt/c/Users/mikeg/Documents/strider/crates/ir/tests/common/mod.rs`.
- **What:** Each crate's `tests/common/` has its own
  `Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi())`
  / `cargo_arch_for(...)` boilerplate (verified — see
  `crates/strider/tests/common/mod.rs:108`,
  `crates/strider/tests/r1_placeholder.rs:69,112`,
  `crates/strider/tests/per_address_cc.rs:30`,
  `crates/strider/tests/jump_table_lifting.rs:88,233`,
  and many others).
- **Why consolidate:** Tens of tests build a `Strider` from one of
  three preset `(arch, regs, cc)` triples.  Promoting these into a
  feature-gated `strider::test_utils::strider_x86_64()` /
  `strider_aarch64()` constructor (mirroring the shape `ir::test_utils`
  already uses) eliminates ~10 lines of boilerplate per test.
- **Migration:** Add `pub fn x86_64_strider_with_arch() -> (SleighArch, Strider)`
  in a new `strider::test_utils` module gated by
  `#[cfg(any(test, feature = "test-utils"))]`.  Mirror for AArch64,
  ARM, MIPS.  ~30 test files contract by 5–10 lines each.

### 2.5 (LOW) `find_all_unique_vns` duplicate VN scan in `LoopState`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:258-263` (`vn_cache`)
  and `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:256-272`
  (`Strider::find_all_unique_vns`).
- **What:** Two separate VN-collection loops.  The orchestrator's
  caches incremental adds; the Strider's helper rescans from scratch.
- **Why consolidate:** The orchestrator's incremental cache exists
  precisely because the helper's full rescan is O(N) per Rebuild.
  TODO(Task17) marker at line 244 already calls this out.
- **Migration:** Plan-only — see
  `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md`.

### 2.6 (LOW) `compact.rs` intermediate `HashMap<NodeId, NodeId>` is redundant

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/compact.rs:201-206`.
- **What:** Builds `old_to_new_pairs: HashMap<NodeId, NodeId>` from
  `remap.nodes[old_id]`, then iterates it.  The data is already in
  `remap.nodes: SecondaryMap<NodeId, Option<NodeId>>` — direct lookup
  avoids the alloc.  Cross-ref `round8-16-perf.md` Finding E.
- **Migration:** Replace the two-loop pattern with a single loop:
  ```rust
  for &old_id in &reachable {
      let Some(new_id) = remap.nodes[old_id] else { continue };
      // ... swap each side-table entry directly
  }
  ```

---

## Section 3 — Visibility tightening (`pub` → `pub(crate)`)

### 3.1 (HIGH) `BuiltFunctionGraph` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs:45-80`.
- **What:** Every field (`graph`, `entry`, `variables`, `call_clobbered`,
  `ret_val_regs`, `call_other_clobbered`) is `pub`.  Cross-ref
  `round8-2D-types.md` HIGH "BuiltFunctionGraph".
- **Why tighten:** `ret_val_regs` is documented as a strict prefix of
  `call_clobbered` (load-bearing in `Match::get_vn`); public mutation
  breaks that invariant.  `Deref<Target = Graph>` already provides
  ergonomic graph access — fields can move behind accessors without
  changing call-site syntax for graph reads.
- **Migration:** Change every field to `pub(crate)`; expose `graph()`,
  `graph_mut()`, `entry()`, `call_clobbered()`, `ret_val_regs()`,
  `call_other_clobbered()`, `variables()` accessors.  ~50 call sites
  in `pattern`/`opt`/`strider`/`strider-py` migrate to
  `bfg.call_clobbered()` etc.

### 3.2 (HIGH) `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` should be crate-private

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs:117`.
- **What:** Public constructor of the partial-state form documented as
  "rewrite-only".  Round 7 introduced `pattern::RewriteCtx` to carve
  off this concern; today the partial-state `BuiltFunctionGraph` is
  still publicly constructible.
- **Why tighten:** The partial-state shape (empty `variables` / clobber
  lists) is an internal contract for `opt::with_built` and
  `strider::rewrite::GraphRewriter`.  Public exposure invites callers
  to invoke `dot_dumper` on a CC-empty graph and see partial output.
- **Migration:** `pub fn from_graph_and_entry_for_rewrite` →
  `pub(crate) fn from_graph_and_entry_for_rewrite` + `#[doc(hidden)]`.
  External tests at `crates/pattern/tests/get_vn_with_call_override.rs:90`,
  `get_vn_with_callother_clobber.rs:34,69,112`,
  `matching/control_flow.rs:399` switch to
  `ir::FunctionBuilder::empty()` + builder-driven setup, OR migrate
  to the existing `pattern::RewriteCtx`.

### 3.3 (HIGH) `FunctionGraph` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/function.rs:14-23`.
- **What:** `graph`, `entry`, `entry_control`, `entry_memory` all public,
  populated to `reserved_value()` sentinels in `new_invalid()`.
- **Why tighten:** Cross-ref `round8-2D-types.md` HIGH "FunctionGraph".
  Sentinel-population is documented as transient but the public fields
  let callers observe the partial state.
- **Migration:** Make all four fields `pub(crate)`.  External
  consumers (none today) would go through `body()` / `entry()` accessors.

### 3.4 (HIGH) `cfg::Cfg<R>` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/mod.rs:37-67`.
- **What:** `sleigh`, `graph`, `entry`, `start_addr_to_region_id` all `pub`.
  `start_addr_to_region_id` is a *derived* index — mutating `graph`
  without updating it silently corrupts the invariant.  Cross-ref
  `round8-2D-types.md` HIGH "cfg::Cfg<R>".
- **Migration:** All fields `pub(crate)`; expose `sleigh()`, `graph()`,
  `entry()`, `region_id_at_start(addr)` accessors.  ~25 call sites in
  `strider/orchestrator.rs`, dot dumper, `strider-py/cfg.rs`,
  `cfg/tests/*` migrate.  Indirect-resolve's `add_region` /
  `compact` paths stay inside `cfg` so they can keep both fields in
  sync atomically.

### 3.5 (HIGH) `cfg::Region` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/types.rs:183-189`.
- **What:** `start_addr`, `insns`, `terminator` public.  `insns` is
  documented "Never empty" but `region.insns.clear()` from outside
  silently breaks `Region::contains_addr`.  Cross-ref `round8-2D-types.md`
  HIGH "cfg::Region".
- **Migration:** `pub(crate)` fields; expose `start_addr()`, `insns()`,
  `terminator()`, `terminator_mut()` (the indirect-branch resolver
  legitimately rewrites terminators).  `RegionInstruction` is a flat
  value-object — its inner `pub addr` / `pub insn` can stay public.

### 3.6 (HIGH) `cfg::PcodeInsnAddr` and `MachineInsnAddr` newtypes leak inner fields

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/types.rs:29-32,50-55`.
- **What:** Both are newtypes, but with `pub addr` / `pub machine_addr`
  / `pub insn_index` they're equivalent to raw `u64` / `(u64, u64)`
  pairs at any caller.  The doc explicitly warns "Do not reorder the
  fields" — pure discipline.  Cross-ref `round8-2D-types.md` MED.
- **Migration:** Fields `pub(crate)`; expose `addr() -> u64`,
  `machine_addr() -> MachineInsnAddr`, `insn_index() -> u64`.  ~30
  call sites (mostly `cfg`, `strider/orchestrator.rs`,
  `strider/indirect_resolve`, dot dumper, tests).  Construction stays
  through `PcodeInsnAddr::at_machine_start(addr)` plus a
  `pub(crate) fn new(machine_addr, insn_index)`.

### 3.7 (HIGH) `target::SleighArch` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/arch.rs:118`.
- **What:** `sla_spec`, `pspec`, `endianness`, `preset` public.
  `preset` is the discriminator that drives
  `target::call_other_abi::classify_arch_specific`; mismatched fields
  silently break ABI dispatch.  Cross-ref `round8-2D-types.md` HIGH.
- **Migration:** All four fields `pub(crate)`; expose accessors.  ~15
  call sites: `strider/orchestrator.rs`, `cfg::Builder::for_arch`,
  `target::call_other_abi::classify_arch_specific`, the example, and
  `strider-py`.

### 3.8 (HIGH) `target::BuiltCallingConventionParts` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs:127`.
- **What:** All-public-fields "test bag" duplicating
  `BuiltCallingConvention`'s shape with a constructor that bypasses
  invariant checks (`arg_passing_regs ∩ callee_saved_regs == ∅`).
  Cross-ref `round8-2D-types.md` HIGH.
- **Migration:** Convert to a builder that runs disjointness checks
  in `build()`.  ~5 test sites adjust to `parts.build()?` style.

### 3.9 (MED) `pattern::RewriteCtx` fields all-public

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/rewrite.rs:109`.
- **What:** `pub graph: &'g mut Graph, pub entry: NodeId`.  Cross-ref
  `round8-2D-types.md` MED.
- **Migration:** `pub(crate) graph` + `pub(crate) entry`; expose
  `graph()`, `graph_mut()`, `entry()`.  ~6 call sites
  (`rewrite_rule`, `apply_rules_in_order`).

### 3.10 (MED) `pattern::MatchCtx` / `BuildCtx` mixed visibility — seal `Pattern`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/pat/traits.rs:30,118`.
- **What:** `MatchCtx` has `pub graph` but `pub(crate) matcher`;
  `BuildCtx` has all-public fields including a mutable graph reference.
  Every `Pattern` impl lives in-crate.  Cross-ref `round8-2D-types.md` MED.
- **Migration:** Add a sealed-marker (`mod sealed { pub trait Sealed {} }`
  + `Pattern: Sealed`).  Then `MatchCtx.matcher` and `BuildCtx.*` can
  all become `pub(crate)`.  ~3 sites add `impl sealed::Sealed for X {}`.

### 3.11 (LOW) `pattern::Match::new_for_test` is a public test-only constructor

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/match_result.rs:32`.
- **What:** `pub fn new_for_test` lets external code synthesise
  arbitrary matches without going through the matcher.  Cross-ref
  `round8-2D-types.md` LOW.
- **Migration:** Gate on `#[cfg(any(test, feature = "test-utils"))]`
  or rename to `pub(crate) fn new_unchecked`.

### 3.12 (LOW) `ir::ValidationErrors(pub Vec<ValidationError>)`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/mod.rs:145`.
- **What:** Public `Vec` field invites external `errs.0.clear()`.
  Cross-ref `round8-2D-types.md` LOW.
- **Migration:** `pub(crate)` field; expose `iter() -> impl Iterator`
  and `len()`.  ~5 sites in `ir/src/validate/tests.rs`.

### 3.13 (LOW) `pattern::matcher::bindings::BindingsMark(usize)` type-level pub

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:64`.
- **What:** `pub struct BindingsMark(usize)` — `mark()`/`restore()` are
  `pub(crate)`, so external code can never obtain or use a `BindingsMark`.
  Cross-ref `round8-2D-types.md` LOW.
- **Migration:** Drop the `pub` from the type — `pub(crate) struct BindingsMark(usize)`.

---

## Section 4 — Stale-comment / doc cleanups

(Cross-ref `round8-3B-comments.md`.)

### 4.1 (HIGH) `Graph::asm_fingerprints` doc lists ghost `IfCase`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/mod.rs:80-98`.
- **What:** Doc claims `(\`ControlState\`, phis, \`Entry\`, \`InitialMemory\`,
  \`InitialVar\`, \`FunctionArg\`, \`IfCase\`)` are exempt.
- **Reality:** `IfCase` is not a `NodeKind`.  Real exempt list is in
  `crates/ir/src/validate/layer_c.rs:178-191`: `Entry | InitialMemory |
  InitialVar(_) | FunctionArg{..} | ControlState | MemPhi | VarPhi(_) |
  ValuePhi | StackStorePhi{..}`.
- **Fix:** Replace parenthetical with the exact `asm_fingerprint_exempt`
  set.

### 4.2 (HIGH) IR README cites non-existent `IfCase`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/README.md:43-46`.
- **What:** "control flow is encoded by `ControlState` / `If` / `IfCase`".
- **Reality:** `IfCase` is a CFG-level edge label, not a NodeKind.
- **Fix:** Drop `IfCase` from the list; describe `If`'s two `Control`
  outputs as the branch arms.

### 4.3 (HIGH) `build_optimizer_pipeline` and `build_stable_optimizer_pipeline` doclists incomplete

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:178-188,208-218`.
- **What:** `build_optimizer_pipeline`'s step (1) lists 4 of 6
  `default_pipeline()` passes (omits `FlagCmpCanonicalize` +
  `IfCondInversion`); `StackLoadForward` (added at line 195) is missing
  from the numbered list entirely.  `build_stable_optimizer_pipeline`'s
  prose lists the same passes minus those two.
- **Fix:** Update both doclists to enumerate the actual six default
  passes; add a step for `StackLoadForward`.

### 4.4 (HIGH) `pcode-lift` lib doc says cfg integration is "(planned)"

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/lib.rs:23-25`.
- **What:** "`cfg`, which uses it (planned) to build a stand-alone
  single-block mini-IR".
- **Reality:** cfg's `indirect_resolve_test_api::resolve_indirect_target_for_test`
  and the internal `crates/cfg/src/cfg/builder/indirect_resolve.rs:148,162`
  actively use `pcode_lift::ValueLifter`.  CLAUDE.md confirms the wiring.
- **Fix:** Drop `(planned)`; describe cfg as the working second consumer.

### 4.5 (HIGH) `strider::indirect_resolve` doc references non-existent path

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/indirect_resolve/mod.rs:1-3`.
- **What:** "`cfg::indirect_resolve` couldn't classify".
- **Reality:** `cfg::indirect_resolve` is private.  Public re-export is
  `cfg::indirect_resolve_test_api`.
- **Fix:** "the cfg-time mini-graph resolver inside the `cfg` builder
  couldn't classify".

### 4.6 (HIGH) Strider example fixture path is invalid

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/examples/strider.rs:11,30-32`.
- **What:** `binary_path = "fixtures/out/x86/test.elf"` (does not exist);
  `.symbol_by_name("struct_test").ok_or("'fib' symbol not found in binary")`.
- **Reality:** `fixtures/out/x86/` contains `abi.elf`, `arithmetic.elf`,
  `calls.elf`, `memory.elf`, etc.  Error message names a different
  symbol from the lookup.  CLAUDE.md's bash invocation
  `cargo run -p strider --example strider` will fail.
- **Fix:** Repoint to a real fixture (e.g. `memory.elf` + `array_sum`)
  and align the error message.

### 4.7 (HIGH) Python `PhiPat` docstring overstates scope

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:580-583`.
- **What:** "Typed builder for `VarPhi` / `MemPhi` / `ValuePhi` patterns."
- **Reality:** Only constructs `VarPhi`.  Separate Python classes
  `PyMemPhiPat` (line 649) and `PyValuePhiPat` (line 693) cover the
  others.
- **Fix:** "Typed builder for VarPhi patterns. (For MemPhi / ValuePhi,
  use `mem_phi()` / `value_phi()`.)"

### 4.8 (HIGH) Python `match.vn(c)` docstring overstates supported kinds

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/matcher.rs:183-187`.
- **What:** Promises Vn for `InitialVar` / `VarPhi` / `FunctionArg`.
- **Reality:** `Match::get_vn` only returns Some for `InitialVar` and
  `Call`/`CallOther` clobber slots — no `VarPhi` or `FunctionArg` arm.
- **Fix:** Match the Rust doc on `Match::get_vn`.

### 4.9 (MED) `is_commutative_int_cmp_op` doc names non-existent variants

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/commutativity.rs:20-26`.
- **What:** `Less / LessEqual / Sless / SlessEqual` listed as if all
  were `IntCmpOp` variants.
- **Reality:** Only `Equal`, `Sless`, `Less`, `Carry`, `Scarry`,
  `Sborrow` exist.  `LessEqual`/`SlessEqual` are lift-time-lowered.
- **Fix:** `Less / Sless` only.

### 4.10 (MED) `SpecialTerm::PendingIndirect` doc says "lifts to `Return(target_value)`"

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:498-500`.
- **What:** Variant doc claims the handler emits `Return(target_value)`.
- **Reality:** `handle_unresolved_indirect_branch` calls
  `self.builder.build_indirect_branch(target_value)` which produces
  `NodeKind::IndirectBranch`.  Cross-ref `round8-2B-naming.md` HIGH.
- **Fix:** "lifts to `IndirectBranch(target_value)`".

### 4.11 (MED) `ir/lib.rs` rustdoc-link `[\`node::Graph\`]` does not resolve

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/lib.rs:18,31`.
- **What:** Two doclinks to `node::Graph`.
- **Reality:** `Graph` is at `crate::graph::Graph` (re-exported as
  `ir::Graph`).
- **Fix:** `[\`Graph\`]` or `[\`graph::Graph\`]`.

### 4.12 (MED) `ir::FunctionBuilder::new` doc-arity in graph_creator example

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/examples/graph_creator.rs:31-34`.
- **What:** Comment says "FunctionBuilder::new takes ..." but the code
  calls `FunctionBuilder::new_raw(...)`.
- **Fix:** Align the comment with `new_raw`.

### 4.13 (MED) `ir/README.md` `NodeOutputType` enum claim omits `U80` / `F80`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/README.md:50`.
- **What:** "Bool / U8–U256 / F32 / F64".
- **Reality:** `crates/ir/src/node/output_type.rs:19-38` includes
  `U80`/`F80` for x87 80-bit extended precision.
- **Fix:** Mention `U80`/`F80`.

### 4.14 (LOW) "the analyzer" pre-rename references

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/tests/common/real_binary.rs:10`,
  `crates/ir/src/node_signature.rs:336`, `crates/ir/src/builder/coerce.rs:261`,
  `crates/ir/src/builder/mod.rs:223`, `crates/ir/src/builder/tests.rs:756,999,1219,1237`,
  `crates/ir/src/dot/label.rs:271`, `crates/ir/src/graph/mod.rs:68`.
- **What:** "the analyzer" pre-dates the `strider` rename.
- **Fix:** Replace "the analyzer" with "strider" / "the lifter" wherever
  it appears (~10 hits).

---

## Section 5 — Naming consolidation

(Cross-ref `round8-2B-naming.md`.)

### 5.1 (MED) Stale `R1`/`R2`/`R3` / `Tier 1`/`Tier 2` milestone labels

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/r1_placeholder.rs:32,48,105,107`,
  `crates/strider/tests/indirect_resolve_classify.rs:10,30,88,127,157,260`,
  `crates/strider/tests/indirect_resolve_jump_table.rs:1,17,18,41,95`,
  `crates/strider/tests/indirect_branch.rs:1,16,18,107,182,190`,
  `crates/strider/tests/indirect_resolve_in_place_edits.rs:76`,
  `crates/strider/tests/abi.rs:17`,
  `crates/cfg/tests/region_terminator.rs:278,281`,
  `crates/cfg/tests/known_targets.rs:3`,
  `crates/cfg/tests/indirect_dispatch.rs:182,311,334`,
  `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:655`,
  `crates/strider/tests/common/indirect_resolve_helpers/classify.rs:39,47,59,70,128,349,436,811,859`.
- **What:** Opaque milestone labels from internal design spec
  `2026-04-27-indirect-branch-fixedpoint.md`.  Most critical:
  `r1_placeholder.rs:105` ("Tier 2 (R2) walks this table") and
  `indirect_resolve_classify.rs:10` ("the orchestrator (R3) will
  hand to the classifier").
- **Fix:** Replace milestone labels with descriptive prose:
  - `R1`, `R1.3`, `R1.4` → "the placeholder lift" / "the cfg-time
    classifier" / "the cfg-time deferral path"
  - `R2` / `Tier 2` → "the IR-level classifier"
  - `R3` → "the orchestrator's fixed-point loop"
  - `R4` / `R5` → drop or "subsequent iterations"

### 5.2 (MED) `r1_placeholder.rs` filename

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/r1_placeholder.rs`.
- **What:** "r1" prefix is a stale internal milestone label.
- **Fix:** Rename to `indirect_branch_lift_placeholder.rs`.

### 5.3 (MED) `x86_64_systemv_abi` `_abi` suffix outlier

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/mod.rs:278`,
  `crates/strider-py/src/cc.rs:17`,
  `crates/strider-py/strider/__init__.pyi:51`,
  ~20 callers across the workspace (verified via grep).
- **What:** Inconsistent suffix — every other CC preset omits `_abi`:
  `x86_cdecl`, `arm_aapcs`, `aarch64_aapcs64`, `mips_o32`, `mips_n64`,
  `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`.
- **Fix:** Rename to `x86_64_systemv` with a `#[deprecated]` alias for
  one release.  Public API symbol — needs deprecation period.

### 5.4 (MED) Stale `analyze_cfg_with_unresolved` ref in test docs

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/indirect_resolve_classify.rs:4`,
  `crates/strider/tests/indirect_resolve_jump_table.rs:16`.
- **What:** Module-level doc comments reference `analyze_cfg_with_unresolved`
  by name.
- **Reality:** That symbol does not exist in the codebase.
- **Fix:** Reference `analyze_cfg` / `analyze_cfg_with`.

### 5.5 (LOW) `lift_and_seat` non-standard jargon

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:342`.
- **What:** Private method name "seat the resulting graph and region
  index onto `self`".
- **Fix:** Rename to `store_results` (or `absorb_results`) — the doc
  comment becomes a one-liner that no longer needs to explain "seat".

### 5.6 (LOW) `analyze_cfg_with`'s "legacy" label is misleading

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:86`.
- **What:** "match the legacy `analyze_cfg(cfg)` behaviour".
- **Reality:** `analyze_cfg` is the primary public API.  "Legacy"
  implies superseded.
- **Fix:** "match the `analyze_cfg(cfg)` default behaviour".

---

## Section 6 — Performance migrations

(Cross-ref `round8-16-perf.md`.)

### 6.1 (MED) `WorkSet::queued: FxHashSet<NodeId>` → `DenseEntitySet<NodeId>`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/worklist.rs:11,21,52-55,85`.
- **What:** Hashes 32-bit dense entity index per push/pop in every
  pass's worklist.  At 10k+ nodes the table holds 10k+ entries each
  with hash + bucket lookup.
- **Why migrate:** `entity_utils::DenseEntitySet<NodeId>` is a flat
  bit-vector indexed by raw u32; O(1) bitset ops with no hashing,
  better cache locality.  Cross-ref `round8-16-perf.md` Finding A.
- **Migration:** Replace `FxHashSet<NodeId>` with
  `DenseEntitySet<NodeId>` in `WorkSet::queued`.
  `detach_unreachable_nodes`'s local `reachable: FxHashSet<NodeId>`
  has the same issue — reuse the walker's `.visited`
  (already a `DenseEntitySet`).  Estimated: 15-30% reduction in
  per-pass iteration cost at 10k+ nodes.

### 6.2 (MED) `KnownBits::analyze` returns `FxHashMap<NodeOutputId, Kb>` → `SecondaryMap`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs:1,367,379-392,106`,
  callers at `crates/opt/src/indirect_branch_resolve/{stack_array,classify,jump_table}.rs`.
- **What:** Hot-path probe in tight per-node loop; 20k+ probes per
  propagation at 10k nodes.
- **Why migrate:** `cranelift_entity::SecondaryMap<NodeOutputId, Kb>`
  with `Kb::default()` (`{ones:0, zeros:0}`) as absent-entry sentinel
  is semantically equivalent ("no info").  `known.get(&x).copied().unwrap_or_default()`
  becomes `known[x]` with same semantics, O(1) array access.  Cross-ref
  `round8-16-perf.md` Finding B.
- **Migration:** Change return type and the four caller signatures.
  Estimated: 10-20% reduction in `KnownBits` pass time.

### 6.3 (MED) `validate` Layer C scans full arena including zombies

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/validate/layer_c.rs:27,61`
  (`check_layer_c_uniqueness` + `check_layer_c_control_state`).
- **What:** Both iterate `graph.nodes.keys()` — full arena including
  detached zombies.  Half the scan is wasted at 50% zombie ratio.
- **Why migrate:** Cross-ref `round8-16-perf.md` Finding C.
  `validate_with_options` already computes `reachable`; pass it
  through and reachable-scope each iteration.
- **Migration:** Add `reachable: &NodeIdSet` parameter to both layer-C
  helpers.  Estimated: 20-40% reduction in `validate` time on
  zombie-heavy graphs.

### 6.4 (LOW-HIGH) `find_all_requirements` cross-product lacks selectivity ordering

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/mod.rs:469-485`.
- **What:** O(N₁·N₂·…·N_M) cross-product.  Only pruning is
  `acc.is_empty()` — no per-tuple short-circuit beyond that.
- **Why migrate:** Sorting by ascending match count before expansion
  → most selective pattern first → maximum chance of `acc.is_empty()`
  short-circuit.  `prefix_agrees` already returns false on first
  disagreement; gain is purely from outer ordering.  Cross-ref
  `round8-16-perf.md` Finding D.
- **Migration:** Compute `len_per_pattern` once, sort the input
  patterns by ascending `len_per_pattern`, expand in that order.
  Estimated: O(N_min/N_max) for unbalanced match counts.

### 6.5 (LOW) `apply_in_place_edits` uses `all_node_ids()` instead of `preorder()`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:529-535`.
- **What:** Pre-built per-iteration index walks `graph.graph.all_node_ids()`
  including zombies.
- **Why migrate:** `graph.preorder()` is reachable-only.  Live
  `InitialVar` nodes are always reachable through their consumers.
  Cross-ref `round8-16-perf.md` Finding F + Section 2.3 above.
- **Migration:** One-line change: `for nid in graph.graph.preorder()`
  inside the `let mut initial_var_index` build loop.

### 6.6 (LOW) `compact.rs` intermediate `HashMap<NodeId, NodeId>`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/graph/compact.rs:201-206`.
- **What:** Builds intermediate map from data already in
  `remap.nodes: SecondaryMap<NodeId, Option<NodeId>>`.
- **Why migrate:** Direct lookup avoids the alloc.  Cross-ref
  `round8-16-perf.md` Finding E + Section 2.6 above.

### 6.7 (LOW) `LoopState::vn_cache: HashSet<rsleigh::Vn>` could be `FxHashSet`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:258`.
- **What:** Uses default `std::collections::HashSet` (DoS-resistant
  SipHash) for an internal cache.  Other crates use `FxHashSet` /
  `rustc_hash` for perf.
- **Why migrate:** Bounded by binary's distinct-varnode count
  (hundreds), so impact is small, but consistency with the rest of
  the workspace and the FxHash perf advantage are both wins.
- **Migration:** `use rustc_hash::FxHashSet`; rename type at the field
  declaration + the four usage sites.

### 6.8 (LOW) `Strider::find_all_unique_vns` uses `std::collections::HashSet`

- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/pipeline.rs:260-261`.
- **What:** Same pattern as 6.7.
- **Migration:** Same — switch to `FxHashSet`.

---

## Summary

| # | Section | Severity | Title | File:line anchor |
|---|---------|----------|-------|------------------|
| 1.1 | Code-delete | MED | Dead always-true guard in `apply_link_register` | `opt/src/indirect_branch_resolve/inplace.rs:60-63` |
| 1.3 | Code-delete | LOW | Strider classify shim duplicates opt | `strider/src/indirect_resolve/classify.rs:14-66` |
| 1.4 | Code-delete | MED | `IndirectBranchResolve::new` defaults dead | `opt/src/indirect_branch_resolve/mod.rs:199-221` |
| 1.5 | Code-delete | LOW | Unreachable `unwrap_or(u64::MAX)` | `cfg/src/cfg/builder/region_builder.rs:165` |
| 2.1 | Helper | HIGH | Centralise poisoned-mutex `map_err` boilerplate | `strider-py/src/{graph,opt,reader,pattern}.rs` (50+ sites) |
| 2.2 | Helper | MED | Consolidate `analyze_known_bits → eprintln → None` | `opt/src/.../classify.rs`, `strider/src/.../classify.rs` |
| 2.3 | Helper | MED | Lift `InitialVar`/region-index walks into a helper | `strider/src/orchestrator.rs:512-580` |
| 2.4 | Helper | MED | Promote test fixtures to `strider::test_utils` | `crates/*/tests/common/` (~30 test files) |
| 2.5 | Helper | LOW | Unify `find_all_unique_vns` paths (TODO Task17) | `strider/src/orchestrator.rs:258` + `pipeline.rs:256` |
| 2.6 | Helper | LOW | Drop intermediate `HashMap` in `compact` | `ir/src/graph/compact.rs:201-206` |
| 3.1 | Visibility | HIGH | `BuiltFunctionGraph` fields `pub(crate)` | `ir/src/function.rs:45-80` |
| 3.2 | Visibility | HIGH | `from_graph_and_entry_for_rewrite` `pub(crate)` | `ir/src/function.rs:117` |
| 3.3 | Visibility | HIGH | `FunctionGraph` fields `pub(crate)` | `ir/src/function.rs:14-23` |
| 3.4 | Visibility | HIGH | `cfg::Cfg<R>` fields `pub(crate)` | `cfg/src/cfg/mod.rs:37-67` |
| 3.5 | Visibility | HIGH | `cfg::Region` fields `pub(crate)` | `cfg/src/cfg/types.rs:183-189` |
| 3.6 | Visibility | HIGH | `PcodeInsnAddr`/`MachineInsnAddr` fields `pub(crate)` | `cfg/src/cfg/types.rs:29-32,50-55` |
| 3.7 | Visibility | HIGH | `target::SleighArch` fields `pub(crate)` | `target/src/arch.rs:118` |
| 3.8 | Visibility | HIGH | `target::BuiltCallingConventionParts` → builder | `target/src/calling_convention/mod.rs:127` |
| 3.9 | Visibility | MED | `pattern::RewriteCtx` fields `pub(crate)` | `pattern/src/rewrite.rs:109` |
| 3.10 | Visibility | MED | Seal `Pattern` trait → `MatchCtx`/`BuildCtx` `pub(crate)` | `pattern/src/pat/traits.rs:30,118` |
| 3.11 | Visibility | LOW | `Match::new_for_test` gate or rename | `pattern/src/matcher/match_result.rs:32` |
| 3.12 | Visibility | LOW | `ValidationErrors(Vec)` field `pub(crate)` | `ir/src/validate/mod.rs:145` |
| 3.13 | Visibility | LOW | `BindingsMark` type `pub(crate)` | `pattern/src/matcher/bindings.rs:64` |
| 4.1 | Doc | HIGH | `Graph::asm_fingerprints` ghost `IfCase` | `ir/src/graph/mod.rs:80-98` |
| 4.2 | Doc | HIGH | IR README cites non-existent `IfCase` | `ir/README.md:43-46` |
| 4.3 | Doc | HIGH | `build_optimizer_pipeline` doc omits passes | `strider/src/strider/pipeline.rs:178-188,208-218` |
| 4.4 | Doc | HIGH | `pcode-lift` cfg integration "(planned)" | `pcode-lift/src/lib.rs:23-25` |
| 4.5 | Doc | HIGH | `cfg::indirect_resolve` private path | `strider/src/indirect_resolve/mod.rs:1-3` |
| 4.6 | Doc | HIGH | Strider example fixture path invalid | `strider/examples/strider.rs:11,30-32` |
| 4.7 | Doc | HIGH | Python `PhiPat` scope overstated | `strider-py/src/pattern.rs:580-583` |
| 4.8 | Doc | HIGH | Python `match.vn(c)` scope overstated | `strider-py/src/matcher.rs:183-187` |
| 4.9 | Doc | MED | `is_commutative_int_cmp_op` ghost variants | `pattern/src/matcher/commutativity.rs:20-26` |
| 4.10 | Doc | MED | `SpecialTerm::PendingIndirect` doc says `Return` | `strider/src/strider/pipeline.rs:498-500` |
| 4.11 | Doc | MED | `ir/lib.rs` broken `node::Graph` doclink | `ir/src/lib.rs:18,31` |
| 4.12 | Doc | MED | graph_creator example wrong fn name | `ir/examples/graph_creator.rs:31-34` |
| 4.13 | Doc | MED | `ir/README.md` omits `U80`/`F80` | `ir/README.md:50` |
| 4.14 | Doc | LOW | "the analyzer" pre-rename references (~10 hits) | various |
| 5.1 | Naming | MED | `R1`/`R2`/`R3` / `Tier 1`/`Tier 2` milestone labels | many tests + indirect_resolve helpers |
| 5.2 | Naming | MED | Rename `r1_placeholder.rs` test file | `crates/strider/tests/r1_placeholder.rs` |
| 5.3 | Naming | MED | Rename `x86_64_systemv_abi` → `x86_64_systemv` | `target/src/calling_convention/mod.rs:278` + ~20 callers |
| 5.4 | Naming | MED | Stale `analyze_cfg_with_unresolved` ref in test docs | `strider/tests/indirect_resolve_*.rs` |
| 5.5 | Naming | LOW | Rename `lift_and_seat` → `store_results` | `strider/src/orchestrator.rs:342` |
| 5.6 | Naming | LOW | Drop "legacy" label on `analyze_cfg` | `strider/src/strider/pipeline.rs:86` |
| 6.1 | Perf | MED | `WorkSet` `FxHashSet` → `DenseEntitySet` | `opt/src/worklist.rs:11,21,52-55,85` |
| 6.2 | Perf | MED | `KnownBits::analyze` `FxHashMap` → `SecondaryMap` | `opt/src/known_bits/mod.rs:367,379-392` |
| 6.3 | Perf | MED | `validate` Layer C reachable-scope | `ir/src/validate/layer_c.rs:27,61` |
| 6.4 | Perf | LOW-HIGH | `find_all_requirements` selectivity sort | `pattern/src/matcher/mod.rs:469-485` |
| 6.5 | Perf | LOW | `apply_in_place_edits` use `preorder()` | `strider/src/orchestrator.rs:529-535` |
| 6.6 | Perf | LOW | `compact` intermediate HashMap | `ir/src/graph/compact.rs:201-206` |
| 6.7 | Perf | LOW | `LoopState::vn_cache` switch to `FxHashSet` | `strider/src/orchestrator.rs:258` |
| 6.8 | Perf | LOW | `Strider::find_all_unique_vns` switch to `FxHashSet` | `strider/src/strider/pipeline.rs:260-261` |

---

## Highest-leverage proposals

If a single PR can land just three items, the highest leverage are:

1. **3.1 + 3.4 + 3.6** — Tighten `BuiltFunctionGraph`, `cfg::Cfg<R>`,
   and the `Pcode/MachineInsnAddr` newtypes.  These are the broadest
   "pub field with hidden invariant" leaks; one focused PR retires
   them all.  Cross-ref `round8-2D-types.md` HIGH cluster.

2. **2.1** — Centralise the poison-mutex error boilerplate in
   `strider-py`.  Shrinks 50+ near-identical error-wrap sites to a
   single helper invocation, AND retires the silent-fallback paths
   identified in `round8-2C-silent-failures.md` H2/H3.

3. **6.1 + 6.2** — Migrate `WorkSet` to `DenseEntitySet` and
   `KnownBits` to `SecondaryMap`.  Both inner-loop-of-every-pass; the
   only quantitative-impact items in the perf review.  Cross-ref
   `round8-16-perf.md` Findings A and B.

The doc fixes in Section 4 (especially 4.1, 4.3, 4.6, 4.7, 4.8) are
all one-line edits that should land opportunistically alongside any
PR touching the surrounding code — the strider example (4.6) is the
single most user-hostile bug since it breaks the canonical "first run"
in CLAUDE.md.
