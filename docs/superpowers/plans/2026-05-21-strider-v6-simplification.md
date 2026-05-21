# strider v6 simplification plan

**Goal:** Strip another ~1,140 production-Rust LOC + ~440 test LOC, drop
**~50 MB of dead snapshot/fixture state**, tighten ~60 `pub` items to
`pub(crate)`, and resolve one explicit owner decision on the deferred
A4 `<R: MemReader>` generic-collapse spike.

**Inputs:** Five parallel audits over the v5-final codebase — one
per-crate (strider-analyze deep dive), one public-API consumer audit,
one test/snapshot surface audit, one pattern + pattern-macros boundary
audit, one cross-cutting + A4 spike.  Reports at
`/tmp/claude-1000/.../tasks/a*.output`.

After five rounds of simplification, the obvious wins are gone.  V6's
character: **hygiene + contract-tightening + a few targeted structural
collapses + one owner decision on A4**.  LOC isn't the right metric
for most of this — disk cleanup, surface-area contraction, and
per-future-edit cost dominate.

**Skills for execution:** superpowers + subagent-driven-development +
test-driven + debugging + code-review + writing.  Push after every
commit.  Use entity-utils structures.  Keep O(n) / O(n·log n).  No
plan-identifier strings in code, doc comments, or commit messages.

---

## Reachable scope at a glance

| Tier | Description | Themes | Production LOC | Test LOC | Disk |
|---|---|---:|---:|---:|---:|
| **H — Hygiene** | Orphan snapshots + fixture | 3 | ~25 | n/a | **~50 MB** |
| **C — Surface dead code** | Zero-consumer pubs, redirect funcs | 13 | ~250 | ~150 | — |
| **P — Pub-tightening** | `pub` → `pub(crate)` contracts | 12 | 0 LOC removed | n/a | — |
| **T — Test consolidation** | 16-stamp macro collapses + table-driven | 3 | n/a | ~440 | — |
| **D — Per-file simplification** | Long-fn splits, helper extractions | 5 | ~150 | n/a | — |
| **G — Generalization** | Shared memory-walk + side-table | 5 | ~100 + perf | n/a | — |
| **A4 — Bench-gated** | Drop `<R: MemReader>` generic | 1 (decision) | ~50 + compile time | n/a | — |
| **F — Final review + tag** | Reviewer + v6-final | 1 | — | — | — |
| **Total** | | **43 themes** | **~575 LOC** + perf + compile + contract | **~440 LOC** | **~50 MB** |

---

# Tier H — Hygiene (the biggest single win, no behaviour change)

## H1.  Delete 4,323 orphan snapshot files (~40 MB)

**Finding:** `crates/strider-analyze/tests/snapshots/` contains 6,484
`.snap` files.  Only 2,161 (the `v3__*` prefix) are produced by a live
test (`v3_baseline.rs`).  The other 4,323 split as:
- ~2,161 unprefixed (e.g. `aarch64__abi__main.snap`) — orphans from
  the deleted `v1_baseline.rs`.
- ~2,161 `v2__*` prefixed — orphans from the deleted `v2_baseline.rs`.

Their headers still reference source files (`source: crates/strider/tests/v1_baseline.rs`)
that don't exist.  V4 deleted the producing tests but the snapshots
weren't swept.

**Cleanup:**
```bash
cd crates/strider-analyze/tests/snapshots/
ls | grep -v "^v3__" | grep -v "^control_c_sum_to_n_cross_arch" | xargs git rm
```

**Verify after:** `cargo test -p strider-analyze --test v3_baseline`
still passes.  `cargo insta` reports no stale snapshots.

**Disk impact:** ~40 MB removed from every clone of the repo.

## H2.  Delete orphan 9.5 MB FreeBSD kernel fixture

**File:** `fixtures/kernels/freebsd/arm64/11.1/kernel` — 9.5 MB ELF
binary, **zero references** anywhere (`grep -rn "kernels/freebsd"`
returns nothing).

**Cleanup:** `git rm -r fixtures/kernels/freebsd/`.  Possibly the
whole `fixtures/kernels/` directory (only one entry under it).

**Disk impact:** ~9.5 MB.

## H3.  Filter compiler-runtime symbols from v3_baseline

**Finding:** 21 of the `v3__*.snap` files are `LIFT_FAILED:...`
placeholders for compiler-runtime helpers (`__aeabi_ldiv0`, `__divsi3`,
`__udivsi3`, etc.) that the user did not write and that aren't the
unit-under-test.  Pinning their lift failure on every CI run adds noise.

**Cleanup:** `crates/strider-analyze/tests/v3_baseline.rs:99` already
filters zero-size symbols; add a name-filter that skips symbols
starting with `__` (compiler-runtime convention) or maintain an
explicit `RUNTIME_HELPERS` skip list.

**LOC:** +5 (filter), −21 snapshot files.

**Tier H total:** ~50 MB disk + ~25 LOC, zero behaviour change.

---

# Tier C — Surface dead code (low-risk parallel batches)

Each theme deletes or downgrades code with **zero workspace consumers**.
Verified by grep.  Parallel-safe — dispatch in batches of 4–6 subagents.

## C1.  Delete `classify_anchor` + `classify_anchor_with_rom`; rename `_with_rom_and_sp` → `classify_anchor`

**Where:** `crates/strider-analyze/src/opt/indirect_branch_resolve/classify.rs:66, 100`.

**Status:** Both are tests-only convenience wrappers around
`classify_anchor_with_rom_and_sp`.  Production (orchestrator) goes
directly to `_with_rom_and_sp`.  After deletion, the surviving function
can drop the verbose suffix.

**LOC:** −60 (+ doc).

## C2.  Collapse `GraphRewriter::re_optimize` (50 LOC doc, zero production callers)

**Where:** `crates/strider-analyze/src/rewrite.rs:197`.

**Body is literally:** `pipeline.run(&mut *self.graph, self.entry)`.
The 50-LOC doc-block explains destructive-vs-stable etc. but the method
itself has zero production callers — only 2 test sites use it.

**Cleanup:** delete the method.  Update the 2 test sites to call
`pipeline.run(...)` directly.

**LOC:** −50.

## C3.  Eliminate `RunOpts` shadow-copy of `Config`

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:114-126` (struct) + the 7-field copy at `LoopState::new` (~lines 357-364).

**Status:** `RunOpts<'a>` duplicates every field of `Config<'a, R>` except
`sleigh` + `per_address_ccs`.  The `<R>` parameter still infects `LoopState`'s
impl block regardless, so the split doesn't buy a layer of `<R>`-free code.

**Cleanup:** delete `RunOpts`.  `LoopState` holds `&'a Config<'a, R>` (by
reference) plus its own `Option<Sleigh<R>>`.

**LOC:** −25.

## C4.  Delete three trivial smoke tests

**Files:** `crates/strider-analyze/tests/smoke.rs`, `crates/strider-ir/tests/smoke.rs`, `crates/strider-lift/tests/smoke.rs`.

Each is `#[test] fn crate_compiles() {}` — 2 LOC of zero signal.  If the
crate doesn't compile, every other test also fails to run.

**LOC:** −6.

## C5.  Delete 7 of 9 dead `common::strider_builders` fns

**Where:** `crates/strider-analyze/tests/common/strider_builders.rs:32-92`.

Only `strider_x86_64` (10 sites) + `strider_aarch64` (1 site) are used.
The other 7 (`strider_for_arch`, `strider_x86`, `strider_arm`,
`strider_mips_o32`, `strider_mips_o32_be`, `strider_ppc32`,
`strider_ppc64le`, `strider_ppc64be`) have zero callers.  Plus
`common::strider_x86_64()` in `mod.rs:138` duplicates the one in
`strider_builders.rs`.

**Cleanup:** delete the 7 dead fns; inline `strider_x86_64` +
`strider_aarch64` directly into `common/mod.rs`; delete
`strider_builders.rs`.

**LOC:** −70.

## C6.  Delete `build_int_const_target_scenario` (unimplemented stub)

**Where:** `crates/strider-analyze/tests/common/indirect_resolve_helpers/classify.rs:56-62`.

Body is `unimplemented!("...")` with a doc explaining why it can't be
implemented.  A code comment is the right home for that, not a function.

**LOC:** −10.

## C7.  Delete 2 permanently-ignored "diagnostic printer" tests

**Where:** `crates/strider-target/src/calling_convention/tests.rs:495-511` + a similar one nearby.

`dump_mips_register_names` etc. are `#[ignore = "diagnostic — uncomment locally"]`-marked println-only tests.  A `cargo run --example` is the right shape for that.

**LOC:** −30.

## C8.  Delete `common_smoke.rs` (duplicates control.rs coverage)

**Where:** `crates/strider-analyze/tests/common_smoke.rs`.

The 4 tests pin `count_return_paths >= 2` on `early_return` for x64 +
ppc64le.  `control.rs:27`'s `per_arch_test!` covers the same assertion on
all 16 arches.

**LOC:** −51.

## C9.  Delete the `make_sp_fn` meta-test in `strider-ir-test-utils`

**Where:** `crates/strider-ir-test-utils/src/lib.rs:134-152`.

A `#[cfg(test)]` block tests that `make_sp_fn` emits an `InitialVar` node.
With 44 callers across the workspace, if the helper were broken every
consumer would fail.

**LOC:** −18.

## C10.  Delete `Matcher::options_for_test` (zero callers)

**Where:** `crates/strider-analyze/src/pattern/matcher/mod.rs:206`.

**LOC:** −5.

## C11.  Delete back-compat `strider_reader::ReadOnlyMemory` re-export

**Where:** `crates/strider-reader/src/lib.rs:77` — `pub use strider_ir::ReadOnlyMemory`.

Zero non-comment external consumers; callers use
`strider_ir::ReadOnlyMemory` directly.

**LOC:** −5.

## C12.  Collapse 4 binop macros + 3 unop macros in `strider-py/src/pattern.rs`

**Where:** `crates/strider-py/src/pattern.rs:845-1062`.

5 `macro_rules!` (`int_binop!`, `bool_binop!`, `float_binop!`,
`float_cmp_op!`, plus `int_unop!`, `float_unop!`, `conv_op!`) all share
identical bodies modulo arity.  Two unified macros (`binary!` + `unary!`)
absorb the 7 originals.

**LOC:** −30.

## C13.  Refresh `EMISSION_SPEC.md` for `node_phrase` attribute + drop `V2` example

**Where:** `crates/strider-pattern-macros/EMISSION_SPEC.md`.

Two doc drifts: the macro accepts a `node_phrase = "..."` crate attribute
that isn't documented; the title example says `PyStackStorePatV2` but the
real symbol is `PyStackStorePat` (the V2 suffix was dropped during
migration as the spec itself anticipated).

**LOC:** ~15 (doc-only).

**Tier C total:** ~400 LOC (250 production + 150 test).

---

# Tier P — Pub-tightening (no LOC removed, but contracts shrink)

These are pure visibility changes.  Sweep callers of each item; no
external compilation unit names it directly.  Each commit is one item,
the verification is `cargo check --workspace --tests` (the strider-py
consumers and strider-analyze integration tests are the only external
compilation units that might break).

## P1.  `strider-analyze::opt::indirect_branch_resolve` 8-item re-export → `pub(crate)`

**Where:** `crates/strider-analyze/src/opt/mod.rs:63-67`.

The 8 items (`AnchorCallingContext`, `ResolvedTargets`,
`apply_link_register`, `apply_tail_call`, `classify_anchor`,
`classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`,
`find_placeholder_return_for_anchor`) have **zero non-comment external
consumers**.  Internal use is via the `indirect_resolver` module wrapper.

After C1 + C2's cleanup, this set shrinks further; the remainder become
`pub(crate)`.

## P2.  `strider-analyze::opt::{Kb, KnownBitsMap, analyze_known_bits}` → `pub(crate)`

**Where:** `crates/strider-analyze/src/opt/mod.rs:68`.

The user-facing entry is the `KnownBits` pass struct.  The bit-lattice
types are implementation details.  Zero external consumers.

## P3.  `strider-analyze::opt::Optimizer` trait → `pub(crate)`

**Where:** `crates/strider-analyze/src/opt/pipeline.rs:149` + re-export.

strider-py implements `OptimizerRaw` only (`strider-py/src/opt.rs:35`);
no external `impl Optimizer for ...`.  `OptimizerRaw` stays `pub` (Python
type-erasure boundary, verified in v4).

## P4.  Delete `OptimizerPipeline::{optimizer_count, optimizer_names}`

**Where:** `crates/strider-analyze/src/opt/pipeline.rs:230, 244`.

Zero callers anywhere — not even strider-py.

**LOC:** −10.

## P5.  14 typed Pat builders → `pub(crate)`

**Where:** `crates/strider-analyze/src/pattern/mod.rs:162-166`.

The 14 (`LoadPat`, `StorePat`, `PhiPat`, etc.) are never **named** by
external callers; strider-pattern-macros emits `::strider_analyze::pattern::<base_builder>()` calls (e.g. `phi()`, `load()`),
which route through the **free functions**.  Free functions stay `pub`;
typed builder structs become `pub(crate)`.

**Caveat:** Rust permits returning a `pub(crate)` type from a `pub fn`
only when no external caller binds the named type.  All external sites
flow through `IntoPat` — safe.

## P6.  `pattern::{BuildCtx, first_value_input_type, *_const_with_fn}` → `pub(crate)`; remove `#[macro_export]` from 6 macros

**Where:** `crates/strider-analyze/src/pattern/macros.rs` + `pattern/pat/ctor/consts.rs`.

The 6 macros (`int_const_with!`, `bool_const_with!`, `float_const_with!`,
plus `decl_pat_binary_ops!`, `decl_pat_unary_ops!`, `decl_pat_cmp_ops!`)
are marked `#[macro_export]` but have **zero external callers**.
Downgrading to crate-local removes the need to export
`BuildCtx`/`first_value_input_type`/`*_with_fn` publicly.

## P7.  Pattern internals → `pub(crate)`

`Binding`, `MatcherOptions`, `skip`, `MissingBinding`, `NotBuildable`,
`RewriteCtx`, `RewriteCtxView`, `apply_rules_in_order` — zero external
consumers.  `RewriteCtx`/`RewriteCtxView` exist for opt-pass plumbing
internal to strider-analyze.

## P8.  Drop redundant strider-ir top-level re-exports

**Where:** `crates/strider-ir/src/lib.rs:68, 77`.

External callers use canonical paths (e.g.
`strider_ir::wide_const::WideConstId`), not the top-level re-exports.
Drop `WideConstId`, `WideConstStorage`, `VarId`, `ValidationError`,
`ValidationErrors`, `CcMetadata` from the top-level re-export block.

**LOC:** −6.

## P9.  `strider-lift::cfg::{DecodeCache, IfRegionState, RegionInstruction}` → `pub(crate)`

**Where:** `crates/strider-lift/src/cfg/mod.rs:29, 32, 33`.

Zero non-comment external consumers.

## P10.  `strider-target::{CcPresetRow, CC_PRESETS}` → `pub(crate)`

**Where:** `crates/strider-target/src/calling_convention/mod.rs:305, 320`.

Internal-only; used by `CallingConvention::*` preset constructors only.

## P11.  `strider-analyze::strider::RegionLiftHandles` → `pub(crate)`

**Where:** `crates/strider-analyze/src/lib.rs:27`.

Zero external consumers post-v5.  CLAUDE.md already documents this is
"being phased out".

## P12.  Struct field tightening

- `dot::DotStyle.{graph, node, edge}` → `pub(crate)` (callers use constructors only).
- `strider-ir::CcMetadata.*` (5 fields) → `pub(crate)`.
- `graphwalk::PreOrder.{graph, visited}` + `PostOrder.{graph, visited}` → `pub(crate)`.

**Tier P total:** zero LOC removed but ~60 items contract-tightened
(future-edit cost: changes to internals don't bump the crate's
public-API SemVer).

---

# Tier T — Test infrastructure consolidation

## T1.  16 `__scan_ignore_<arch>` macros → one proc-macro driver

**Where:** `crates/strider-analyze/tests/common/mod.rs:493-900`.

16 byte-identical-except-arch-name 22-LOC blocks.  An existing comment
defends "would deepen macro recursion" but a proc-macro under the
existing `strider-pattern-macros` (or a `paste!`-driven single
`macro_rules!`) collapses them.  Adding a new arch is currently a
24-line stamp; after this it's one row.

**LOC:** −350.

## T2.  16 `arch_smoke.rs` identical `*_preset_resolves` tests → table-driven

**Where:** `crates/strider-target/tests/arch_smoke.rs:23-150`.

16 `#[test]` fns with identical bodies modulo arch name.  Table-driven
shape or `for_each_arch!` macro.

**LOC:** −80.

## T3.  12 `linux_cc_presets.rs` tests → table-driven

**Where:** `crates/strider-target/tests/linux_cc_presets.rs:49-174`.

All 12 fns share `assert_eq!(arg_reg_names(CC::<preset>(), &regs), vec![<names>])`.
Convert to `for &(cc_fn, expected) in CASES { ... }`.

**LOC:** −60.

**Tier T total:** ~440 LOC of test code.

---

# Tier D — Per-file simplifications

## D1.  Split `build_lift_stable` (130 LOC) into 3 named helpers

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:936-1054`.

Mixed concerns: cfg building, vn-cache delta scan, lifter wrap-error
logic, stable opt.  Extract `build_cfg(sleigh, opts, known_targets, decode_cache)`
+ `scan_new_vns(cfg, vn_cache, region_count)`.  `build_lift_stable`
becomes a 25-LOC sequencer.

**LOC delta:** 130 → ~115 with named helpers; readability win.

## D2.  Extract `AnchorCallingContext::for_anchor` helper

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:673-738` + `:784-842`.

Both `apply_in_place_edit` (78 LOC) and `build_anchor_calling_context`
(58 LOC) share the override-vs-default CC fork (`override_cc.unwrap_or_else(...)` +
`if let Some(cc) = override_cc { override_clobber_vars(...) } else {
call_clobbered_regs() }`).  Move the selection logic into a single
`AnchorCallingContext::for_anchor(graph, placeholder, strider, override_cc)`.

**LOC:** ~40 saved.

## D3.  11 `RewriteCtx::for_built` constructions → `with_rewrite_ctx(|ctx| ...)` callback

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:693, 710`; `opt/indirect_branch_resolve/inplace.rs:241, 252, 260, 292, 340, 360, 390, 420, 459`.

Add `impl Graph { pub fn with_rewrite_ctx<F, R>(&mut self, f: F) -> R where F: FnOnce(&mut RewriteCtx<'_>) -> R }` in strider-ir (or
strider-analyze).  Each call site shrinks from a 2-line construction +
borrow dance to one `bfg.with_rewrite_ctx(|ctx| apply_link_register(ctx, ...))`.

**LOC:** ~22 saved.

## D4.  `LoopState::graph: Option<Graph>` → non-Option-after-init

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:247-310`.

The `Option<Graph>` is non-`None` after `build_initial_iteration`
returns; 5 `ok_or_else(...)` consumer sites pay for an `Option` that's
always `Some`.

**Cleanup:** initialise from a cheap dummy `Graph::new()` in
`LoopState::new`, overwrite in `build_initial_iteration`.  Save the
`Option<Sleigh>` (genuine take-rebuild trick).

**LOC:** ~6 sites of `ok_or_else` removed.

## D5.  Move misplaced `walk_through.rs` tests

**Where:** `crates/strider-analyze/src/pattern/matcher/walk_through.rs:64-133`.

70 LOC of `#[cfg(test)] mod tests` testing `cast_mask::cast_mask_of`, not
`try_walk_through_control_state`.  Move tests next to the function they
test.

**LOC:** 0 net (file move).

**Tier D total:** ~150 LOC.

---

# Tier G — Generalization (the substantive structural wins)

## G1.  Extract `walk_mem_chain<S>(...)` — `mem_chain_is_dirty` + `probe` share the skeleton

**Where:**
- `crates/strider-analyze/src/opt/function_args/mod.rs:416-561` (`mem_chain_is_dirty`, ~145 LOC iterative walk).
- `crates/strider-analyze/src/opt/stack_load_forward/mod.rs:184-347` (`probe`, ~160 LOC).

Both walk the memory chain backward from `mem`, fold MemPhi via a Frame
stack, delegate `StackStore` / `StackStorePhi` / `Store` to
`step_through_*` from `sp_expr`.  Frame discipline, cycle guard
(`seen`), malformed-MemPhi error, malformed-Call guard — all duplicated
near-verbatim.

**Cleanup:**
```rust
pub(crate) fn walk_mem_chain<S, F>(
    initial_mem: NodeOutputId,
    sp_vn: Vn,
    /* ... ctx args ... */,
    classify_step: F,
) -> Result<S, MemChainError>
where
    S: Default + ...,
    F: FnMut(/* per-step args */) -> StepVerdict<S>,
```

Both passes parameterise their verdict type (`bool` for dirty-check;
`ResolveShape` for forwarding) and pass their per-step closure.  The
walker owns the work-stack, cycle bookkeeping, MemPhi-fold, and
malformed-shape errors.

**LOC:** ~80 saved.  **Bigger correctness win** — one place to fix MemPhi
handling instead of two.

## G2.  `initial_var_index` → `Graph::initial_var_for(vn)` (perf + tighter encapsulation)

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:592-608` (`initial_var_index` builder).

Per-orchestrator-iteration the orchestrator walks the entire
`preorder()` once to map `Vn → NodeOutputId` for every `InitialVar`.
This index could live as a `SecondaryMap<NodeId, ...>` on `Graph` (or
`FunctionBuilder`), populated at lift time and updated when new
`InitialVar`s are inserted.

For a 5-iteration resolve loop on a 10k-node graph that's 50k extra
node visits per call to `apply_in_place_edits` eliminated.

**Cleanup:**
- Add `Graph::initial_var_for(&self, vn: Vn) -> Option<NodeId>` (O(1) via secondary map).
- Maintain the side-table in `FunctionBuilder::insert_initial_var`.
- Delete the orchestrator's per-iteration rebuild.

**LOC:** ~20 + per-iteration perf win.

## G3.  `RegionIndex::from_handles` Arc::clone → move

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:144-150`.

Currently `from_handles(&[RegionLiftHandles])` `Arc::clone`s each
entry's `exit_vn_to_value`.  The Vec is read-once-and-discarded; signature
becomes `from_handles(Vec<RegionLiftHandles>)` + `into_iter()`.  Save
one `Arc` bump per region per iteration.

**LOC:** ~5 net + alloc bumps.

## G4.  Route 6 sites through `OptimizationResult::after_replace`

**Where:** `crates/strider-analyze/src/opt/pipeline.rs:46-57` (the
helper); consumers in `function_args/mod.rs:208-209`,
`stack_load_forward/mod.rs:127-128`, etc.

The helper already absorbs `extend_asm_fingerprint_from + replace_all_uses + |=`
but ~6 sites do the 3-step dance inline.  Uniformity win.

**LOC:** ~15.

## G5.  Cache `apply_stall_guard`'s edge set across iterations

**Where:** `crates/strider-analyze/src/orchestrator/mod.rs:419-463`.

Two `edge_set_of(...)` calls per `LoopState::step`, each producing a
fresh `BTreeSet`.  Cache one on `self.cached_edges`; the next iteration
compares against it.

**LOC:** ~10 + one allocation per iteration removed.

**Tier G total:** ~130 LOC + per-iteration perf wins.

---

# Tier A4 — Bench-gated structural collapse (owner decision)

## A4.  Drop `<R: rsleigh::MemReader>` generic from 22 type signatures

**Two audits disagreed; the owner must choose.**

### Cross-cutting + A4-spike agent: APPROVE with bench gate
- 22 trait-bound declarations + 37 instantiation sites; `<R>` propagates without ever calling `R::read`.
- Production has **one** concrete `R` (`AnyMemReader`); tests have **one** (`BufMemReader<Vec<u8>>`).
- Object-safety: `rsleigh::MemReader` has an associated `type Err`, so requires pinning to `dyn MemReader<Err = MemReadError>`.  All production readers already use `MemReadError`; test reader needs a thin wrapper.
- Perf ceiling: ~100-200ns of vtable overhead per analysis run vs ~100+μs per Sleigh `lift_one` = **~0.001% regression** (3 orders of magnitude under the 5% gate).
- `Arc<dyn ReadOnlyMemory>` already used in the same lift path — precedent.
- LOC: ~50 in signatures + plausible 5-15% incremental rebuild speedup for `strider-lift` from monomorphisation removal.
- Migration: ~26 files mechanical.

### Strider-analyze deep-dive agent: SKIP
- "Replacing with `AnyMemReader` would require either boxing the Sleigh handle or restructuring the orchestrator to drop the cross-iteration Sleigh reuse — both are larger surgeries than the LOC saving justifies."
- The `<R>` propagation is structural via `Sleigh<R>`; replacing requires touching the orchestrator's Sleigh-take-rebuild dance.

### Owner decision needed

The spike agent's analysis is more detailed on the technical feasibility
(object-safety pinning, perf measurement against the `lift_one` cost
denominator, precedent from `ReadOnlyMemory`).  The deep-dive agent
flagged a real concern about the orchestrator restructuring cost.

**Recommendation:** **approve with a real benchmark gate** —
1. Spike on `crates/strider-lift/src/cfg/mod.rs` first.  Convert just
   `Cfg<R>` → `Cfg` with `Sleigh<BoxedReader>` field.  Measure.
2. If the bench (`benches/scaling.rs`) shows ≤5% regression, continue
   the conversion through `Builder<R>`, `ValueLifter<'a, R>`, etc.
3. The orchestrator restructuring concern is real but the
   `LoopState`'s Sleigh-take-rebuild dance works with any `Sleigh`
   type — the trick is the `Option<Sleigh>` field, not the `<R>`
   parameter.  Spike will confirm.

**If declined:** add a memory note documenting the decision so v7
doesn't re-propose.

**LOC:** ~50 (if executed) + compile-time win + cleaner API
(`IndirectResolverFn<R>` loses its `<R>`).

---

# Tier F — Final review + tag

Standard: `pr-review-toolkit:code-reviewer` over `v5-final..HEAD`.
Gates: v3_baseline byte-identical, cross_arch_shape green, per-crate
suites green, no new HashSet<NodeId>, clippy clean (the pre-v3
`expect_used` carryover documented).  If APPROVED: tag `v6-final`.

---

# Execution order

**Phase 1 — Hygiene (parallel-safe, ~50 MB disk):**
- H1 + H2 + H3 in one batch.

**Phase 2 — Surface dead code (parallel, ~400 LOC):**
- 4 subagents covering C1-C13 grouped by crate locality.

**Phase 3 — Pub-tightening (parallel, contracts only):**
- One subagent per crate sweeping P1-P12.  Each pushes `cargo check --workspace` after each change.

**Phase 4 — Test consolidation (parallel, ~440 LOC test):**
- T1 + T2 + T3 in parallel; each is its own file/crate.

**Phase 5 — Per-file simplification (parallel, ~150 LOC):**
- D1-D5 — one subagent per file mostly.

**Phase 6 — Generalization (mostly parallel, ~130 LOC + perf):**
- G1 + G2 in separate commits (different concerns); G3 + G4 + G5 can ride along.

**Phase 7 — A4 spike OR decline (owner decision before dispatch):**
- If approved: solo subagent on `Cfg<R>` first; bench-gate; expand or revert.
- If declined: save memory note, skip.

**Phase 8 — Final review + tag.**

**Estimated wall-clock at v5 cadence:** 3–5 days (smaller than v5;
many themes are 1-commit-pub-tightening with no test churn).

---

# Risk callouts

1. **H1 (orphan snapshot delete):** safe — they're never read by any
   live test.  But `git rm` 4,323 files in one commit balloons the
   commit diff.  Consider splitting H1 into batches of ~1,000 files
   for diff-review tractability.

2. **P5 (typed Pat builders → `pub(crate)`):** strider-pattern-macros'
   `base_builder = "..."` emits `pattern::<name>()` free-function
   calls, not typed-struct references.  But the macro's emitted code
   may reference the typed struct as a **return type** in the user's
   namespace; if so, `pub(crate)` breaks compilation.  Spike on one
   pattern first.

3. **A4 (`<R>` generic drop):** see the two-agent disagreement above.
   Owner decision required; benchmark gate is the safety valve.

4. **T1 (proc-macro for `per_arch_test`):** moves test-only dispatch
   into the workspace proc-macro crate.  If `strider-pattern-macros`'
   build-time grows substantially, defer.

5. **C9 (delete `make_sp_fn` meta-test):** the meta-test does protect
   against a future refactor of `make_sp_fn` going wrong.  If the test
   has saved a real bug at any point in history (`git log -p
   crates/strider-ir-test-utils/src/lib.rs` to check), keep it.

---

# What this plan deliberately does NOT do

- Re-propose the three-source-of-truth proc-macro absorb (Rust builders
  carry domain invariants — pattern audit confirmed).
- Re-propose the `Decision::StableOnly` collapse (load-bearing per
  saved memory).
- Re-propose the `NodeOutputType` reshape (deferred per saved memory).
- Split `strider-pattern-macros/src/lib.rs` (no LOC benefit, navigation
  fine).
- Tighten `graphwalk`'s 11 unused-but-public items — they're part of
  the generic-utility crate's intentional surface.

---

# Notes on memory directives

- **No plan identifiers** in code, doc comments, or commit messages.
- **Push after every commit.**
- **Entity-utils** structures for any new entity-keyed bookkeeping.
- **O(n) / O(n·log n)** throughout — G2's `initial_var_index` move
  to `Graph` specifically maintains the O(1) lookup.
- **Memory notes:** save A4's outcome (whether approved or declined)
  as a project memory so v7 has it.
