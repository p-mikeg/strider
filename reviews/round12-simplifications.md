# Round 12 — R5: simplifications

## Per-category totals

| Category | Entries | Net LOC delta |
|----------|---------|----------------|
| 1. Delete | 6 | -260 |
| 2. Merge | 4 | -30 |
| 3. Inline single-callsite helpers | 4 | -25 |
| 4. Stdlib-idiom replacements | 3 | -10 |
| 5. Visibility tightening | 8 | 0 |
| 6. Wrappers to drop | 3 | -50 |
| 7. Partial-state types to convert | 3 | -40 |
| **Total** | **31** | **~-415** |

Pre-implementation workspace LOC: ~53,600 (Rust crates/, post-round11)
Projected post-implementation: ~53,185
Net reduction: ~0.8%

**Headline: diminishing returns post-W12.**  Round 11 W9/W10/W12/W14
collectively landed -1,500+ LOC (deleted `IndirectBranchResolve`,
migrated to `Builder::for_arch`, collapsed `default_pipeline`,
tightened the H-6 CC fields, etc.).  This round's harvestable items
are smaller and more targeted: a handful of test-only stragglers from
the `Builder::new` deprecation, the `make_int_const` duplicate that
was deferred from R11 (S1.5 not landed), three `classify_anchor*`
delegators, four test-only shim files, and seven type-design quick
wins from audit 2D.  Several large structural simplifications listed
in round11-simplifications.md (S1.1 `IndirectBranchResolve` deletion,
S1.4 `Builder::new` migration, etc.) DID land — they appear in audit
1B's "no findings" verdict.

If no fresh code lands before round 13, recommend deferring further
sweeps until then.  The remaining items either (a) require multi-file
migrations across many test sites (cfg::Builder deprecation in tests,
`make_int_const` rename) or (b) are 5-10 minute visibility tightenings
with no LOC impact.

---

## Section 1 — Code to delete

### S1.1 — `ir::Graph::make_bool_const` / `make_float_const` and the duplication with `FunctionBuilder::build_*`

- **Where:** `crates/ir/src/ops/consts.rs:102-129`.
- **Why dead-ish (semantic duplicate):** `make_bool_const` and
  `make_float_const` mirror `FunctionBuilder::build_bool_const` and
  `build_float_const`.  Audit 2B (`crates/ir/src/ops/consts.rs:82`)
  flagged the `make_int_const` / `build_int_const` duplicate; the
  same pattern holds for the bool/float siblings.
- **Net LOC delta:** -30 (after R11 S1.5 lands; deferred again here).
- **Net cognitive delta:** one canonical constructor for IR
  constants.  Today the two paths drift independently — e.g. the
  wide-rejection error messages have slightly different wording at
  `consts.rs:78-82` vs `builder/nodes.rs:96-100`.
- **Migration:** keep `Graph::make_int_const` / `make_bool_const` /
  `make_float_const` and rename to `Graph::int_const` /
  `Graph::bool_const` / `Graph::float_const` (drop `make_` prefix);
  delete the duplicate masking + validation block in
  `FunctionBuilder::build_int_const` and have it forward to the
  graph method, with the fingerprint-side-table call layered on top.
- **Status:** flagged in R11 S1.5; NOT YET LANDED.  Still applicable.

### S1.2 — `strider::indirect_resolve::classify_anchor` and `classify_anchor_with_rom` (test-only delegating shims)

- **Where:** `crates/strider/src/indirect_resolve/classify.rs:20-43`
  (~24 LOC of two thin forwarders).
- **Why dead:** Both are one-line forwarders to `opt::classify_anchor`
  / `opt::classify_anchor_with_rom`.  Production strider's
  orchestrator uses `classify_anchor_with_rom_and_sp` (which passes a
  cached `KnownBitsMap`); only `crates/strider/src/indirect_resolve/classify.rs`'s
  inline tests and one integration test (`indirect_resolve_jump_table.rs`,
  `optimizer_pipeline_subsets.rs`) reach for the simpler signatures.
- **Net LOC delta:** -24 (shim functions) + ~50 LOC trimmed at test
  call sites that adopt the canonical `_with_rom_and_sp` form.
- **Net cognitive delta:** the strider classifier surface shrinks to
  one function.  Audit 2B (line 235) called the N²-naming explosion
  out as a maintainability anti-pattern.
- **Migration:** delete the two shims; tests adopt the same idiom
  the orchestrator uses (compute KB via `analyze_known_bits`, call
  `classify_anchor_with_rom_and_sp` directly).
- **Status:** flagged in R11 S1.11; NOT YET LANDED.

### S1.3 — `opt::classify_anchor` and `opt::classify_anchor_with_rom` (1-line delegators)

- **Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:66-115`
  (~50 LOC; mostly doc, ~6 LOC of body each).
- **Why dead:** Both compute `analyze_known_bits` and forward to
  `classify_anchor_with_rom_and_sp`.  After S1.2 lands, the only
  callers are tests inside `opt`.
- **Net LOC delta:** -50.
- **Net cognitive delta:** classifier's public surface shrinks to one
  function.  Audit 2B (line 235) recommends collapsing to a
  `ClassifyAnchorOptions { link_register, rom, stack_ptr }` builder,
  but the simpler step is to delete the two shims and rename
  `classify_anchor_with_rom_and_sp` → `classify_anchor` at the
  single canonical name.
- **Migration:** delete the two shims; rename
  `classify_anchor_with_rom_and_sp` to `classify_anchor`.
- **Status:** flagged in R11 S1.12; NOT YET LANDED.

### S1.4 — `cfg::Builder::new` (deprecated) + `Builder::with_endianness` (deprecated)

- **Where:** `crates/cfg/src/cfg/builder/mod.rs:97-139` (two
  deprecated ctors).
- **Why dead-ish:** Production code path (`crates/strider/src/orchestrator.rs:954`)
  uses `Builder::for_arch`.  The deprecation has been live for several
  rounds.  Audit 1B (focus area 7) verified no production callers
  remain — only test files still use the deprecated ctors.  Test
  migration is mechanical: ~50 sites across `cfg/tests/`,
  `strider/tests/`, `cfg/examples/`.
- **Net LOC delta:** -45 (the two ctors + their docstrings) + ~50
  LOC adjusted at test sites (each test fixture has a known arch).
- **Net cognitive delta:** removes a footgun ctor that defaults
  `preset = X86_64` and silently misclassifies non-x86 CallOthers.
  Audit 3A (claim 17) flagged that `cfg/README.md` doesn't mention
  the deprecation; deleting the ctors closes that gap.
- **Migration:** mechanical — replace `Builder::new(sleigh, addr, opts)`
  → `Builder::for_arch(&arch, sleigh, addr, opts)` at every test
  site.  Each test fixture has a known arch (e.g. `target::SleighArch::x86_64()`
  in default test files).
- **Status:** flagged in R11 S1.4; the production migration LANDED
  but the test-side migration + the actual ctor deletion did NOT.

### S1.5 — Inline-test module `tests` inside `crates/strider/src/indirect_resolve/classify.rs` (455 LOC)

- **Where:** `crates/strider/src/indirect_resolve/classify.rs:73-454`.
- **Why suspect:** The file is 73 LOC of production code (three
  delegating shims) and ~380 LOC of inline-mod tests.  Once S1.2
  deletes the two simpler shims, the tests should migrate to call
  the canonical `_with_rom_and_sp` form directly — and since they're
  testing `opt::classify_anchor`'s behaviour through a thin strider
  re-export, they belong in `crates/opt/src/indirect_branch_resolve/classify.rs`'s
  tests (or in `crates/opt/tests/`) rather than under `strider`.
- **Net LOC delta:** 0 (relocation, not deletion) — listed here to
  flag the structural smell.  The file shape "73 LOC prod + 380 LOC
  tests of opt's classifier through a strider shim" is a code-shape
  red flag the auditor noticed.
- **Net cognitive delta:** strider tests should test strider; opt
  tests should test opt.  Removing the through-shim test layer
  clarifies which crate owns the contract.
- **Migration:** after S1.2/S1.3, move the tests into
  `crates/opt/src/indirect_branch_resolve/classify.rs`'s own
  `#[cfg(test)] mod tests`, or to a new integration test file under
  `crates/opt/tests/`.

### S1.6 — `from_graph_and_entry_for_rewrite` legacy ctor

- **Where:** `crates/ir/src/function.rs:215-240`.
- **Why dead-ish:** `#[deprecated]` + `#[doc(hidden)]`.  6 callers,
  all in pattern + ir tests (`tests/get_vn_with_callother_clobber.rs`,
  `tests/get_vn_with_call_override.rs`,
  `tests/matching/control_flow.rs`, and one self-use at
  `function.rs:322` inside an `impl Default` for tests).  The
  `pattern::RewriteCtx::new(&mut graph, entry)` ctor covers every
  use without requiring a `BuiltFunctionGraph` wrapper.
- **Net LOC delta:** -25 (method body + extensive doc).
- **Net cognitive delta:** removes the only reason 4 pattern test
  files have `#![allow(deprecated)]` headers.
- **Migration:** rewrite each pattern-test caller to construct a
  `pattern::RewriteCtx::new(&mut graph, entry)` instead.
- **Status:** flagged in R11 S1.9; NOT YET LANDED.

---

## Section 2 — Code to merge

### S2.1 — Rename `function: &mut pattern::RewriteCtx` → `ctx` across 8 opt passes

- **Where:** `crates/opt/src/constant_fold/mod.rs:62`,
  `flag_cmp_canonicalize/mod.rs:69,115`, `if_cond_inversion/mod.rs:58`,
  `known_bits/mod.rs:427,467`, `load_readonly/mod.rs:50`,
  `redundant_phis/mod.rs:12,32,172`, `stack_store/detect.rs:110`,
  `worklist.rs:49`, plus the trait declaration at `pipeline.rs:41`
  (which already uses `ctx`).
- **Three-occurrence threshold:** YES, 11 sites in the trait body
  vs 4 already-renamed sites — pattern is the majority.
- **Load-bearing-different vs accidentally-different:** the trait
  signature `fn optimize(&self, ctx: &mut RewriteCtx<'_>)` already
  uses `ctx`; the impls inherited the older `function` name from
  when the parameter was a `FunctionGraph`.  Audit 2B (line 132-147)
  flagged this.  All 11 renames are mechanical, no semantic change.
- **Net LOC delta:** 0 (rename only).
- **Net cognitive delta:** the impl signature matches the trait's
  signature; readers stop wondering why the param name shifted.
- **Migration:** mechanical sed-rename across the 11 sites.
- **Status:** flagged in R11 W15 (partial); NOT FULLY LANDED.

### S2.2 — `WideConstStorage::to_le_bytes` U256/U512 arms via shared `limbs()`

- **Where:** `crates/ir/src/wide_const.rs:60-65`.
- **Three-occurrence threshold:** N/A (2 arms).
- **Load-bearing-different:** identical body, different inner array
  size.
- **Net LOC delta:** -3 (collapse to single arm via a shared
  `limbs() -> &[u64]` projection).
- **Net cognitive delta:** marginal.
- **Migration:** add `fn limbs(&self) -> &[u64]` and rewrite
  `to_le_bytes` as `self.limbs().iter().flat_map(|l| l.to_le_bytes()).collect()`.
- **Status:** flagged in R11 S2.8; NOT YET LANDED.

### S2.3 — `set_lift_addr(Some(addr))…work…set_lift_addr(None)` paired pattern

- **Where:** `crates/strider/src/strider/insn/mod.rs:43-47` (one
  invocation) and `crates/strider/src/strider/pipeline.rs:388-412`
  (terminator path, ~20 LOC apart).
- **Three-occurrence threshold:** 2 documented sites + the
  `lift_at` API at `crates/ir/src/builder/mod.rs:411` which already
  uses RAII via a `Drop` guard but isn't usable due to the inner
  mutable-borrow conflict.
- **Load-bearing-different:** each call site brackets a different
  body; the body is genuinely different.
- **Net LOC delta:** -4 (each site drops 2 lines for the bracketing).
- **Net cognitive delta:** the panic-leak hazard (called out in
  audit 1A by implication and in `insn/mod.rs:36-42` comment) is
  mitigated.  `lift_at` already has the RAII shape; this would
  unify the two call patterns.
- **Migration:** factor a `set_lift_addr_scoped<F>` that takes a
  closure with `&mut self`, mirroring `lift_at`.  Where the
  mutable-borrow constraint blocks direct use, the closure-form is
  still resolvable.
- **Status:** flagged in R11 S2.7; NOT YET LANDED.

### S2.4 — `Cfg::sleigh` + `Cfg::graph` sibling-field pattern

- **Where:** `crates/cfg/src/cfg/mod.rs:54-57`.
- **Three-occurrence threshold:** Not a merge; structural pattern
  flag.  The W14 `start_addr_to_region_id` tightening made the cfg's
  three storage fields heterogeneous — one is now `pub(crate)` with
  invariant docs, two remain fully `pub`.
- **Load-bearing-different vs accidentally-different:** the three
  fields conceptually form the cfg's storage; tightening one while
  leaving the others open creates an inconsistent reader experience.
- **Net LOC delta:** -15 (consolidate accessors / move sleigh to
  `into_parts()` since the orchestrator harvests it post-run).
- **Net cognitive delta:** see audit 2D R12-T-Q.  Anyone with `&mut Cfg`
  can still call `cfg.graph.remove_node(_)` and produce the same
  index-desync the W14 fix tried to prevent.
- **Migration:** `cfg.graph → pub(crate)` + `pub fn petgraph(&self)`;
  `cfg.sleigh` stays for now but document the ownership-transfer
  intent.  Pairs naturally with S5.7.

---

## Section 3 — Single-callsite helpers to inline

### S3.1 — `Strider::arch()` accessor is single-callsite

- **Where:** `crates/strider/src/strider/pipeline.rs:175-177`.
- **Body length:** 1 line.
- **Call sites:** 1 — `crates/strider/src/orchestrator.rs:954`
  (`Builder::for_arch(opts.strider.arch(), ...)`).
- **Net LOC delta:** -3 (delete accessor; the call site reads the
  `pub(super)` field directly).
- **Net cognitive delta:** marginal.
- **Migration:** demote `Strider::arch` from method to direct
  `pub(super)` field read at the orchestrator.
- **Status:** flagged in R11 S3.2; NOT YET LANDED.  Note: the
  comparable `Strider::calling_convention()` is used at 5 sites in
  orchestrator and stays.

### S3.2 — `FunctionBuilder::body()` accessor at 18+ test sites

- **Where:** `crates/ir/src/builder/mod.rs:152-159`.
- **Body length:** 1 line each (`body()` / `body_mut()`).
- **Call sites:** the production callers go through `body().graph`
  (so `body()` doesn't add invariant); tests use it at 18+ sites for
  fingerprint inspection (`builder/tests.rs:601, 604, 615, 637, 643,
  646, 1433, 1441, 1444, 1459, ...`).
- **Note:** the `body()` accessor names a `FunctionGraph` internal,
  which (per audit 2D R12-T-D) is a partial-state type whose `entry`
  field is `NodeId::reserved_value()` until `build_entry` runs.
  Audit 2B (line 156-178) flagged that the accessor name doesn't
  match the field's type.
- **Net LOC delta:** -7 (delete the two methods) but each call site
  gets longer (`b.function.graph.X` rather than `b.body().graph.X`).
- **Verdict:** SKIP — the call sites benefit from the accessor's
  documentation more than they suffer from the hop.  If S7.1
  (`FunctionGraph` enum migration) lands, the accessor would need
  rewriting anyway.

### S3.3 — `OptimizationResult::after_replace` migration-history doc

- **Where:** `crates/opt/src/pipeline.rs:32-39`.
- **Note:** Audit 1C #2 (and R11 S3.9) flagged lines 36-39 as
  "migration-history prose that has nothing to do with the function's
  contract."  Pure-doc cleanup.
- **Net LOC delta:** -4 (doc-comment lines).
- **Net cognitive delta:** doc explains how to call the function
  instead of how it got renamed.
- **Migration:** drop the migration-history sentences.
- **Status:** flagged in R11 S3.9; NOT YET LANDED.

### S3.4 — `strider-py::dot_style_for` is the entire `dot.rs` file (15 LOC)

- **Where:** `crates/strider-py/src/dot.rs:1-15`.
- **Body length:** 8 lines of `match`.
- **Call sites:** 4 — `cfg.rs:65, 76`, `graph.rs:96, 123`.
- **Net LOC delta:** -10 (inline the match at each call site, drop
  the file).
- **Net cognitive delta:** mixed.  R11 S6.3 / S3.1 flagged a similar
  pattern (the `dump_*` wrappers); those were DELETED in W12.  This
  leftover `dot_style_for` helper survives.  Audit 2C K21 noted that
  the "unknown style name → dark" fallback is a soft-failure surface
  worth keeping centralised.
- **Verdict:** SKIP — the 4-site reuse benefit edges out the
  inline-clarity win.  The 8-line body is small but the centralised
  unknown-style fallback is real.

---

## Section 4 — Bespoke patterns to replace with stdlib idioms

### S4.1 — `as u8` truncation in EXTRACT/INSERT operands

- **Where:** `crates/pcode-lift/src/value/cast.rs:113-114, 161-162`.
- **Pattern:** `let lsb = insn.inputs[1].addr_off as u8;` (silent
  truncation of u64 to u8).
- **Stdlib idiom:** `u8::try_from(insn.inputs[1].addr_off).map_err(|_| anyhow!("..."))?`.
- **Net LOC delta:** +8 (more verbose).
- **Net cognitive delta:** correctness — surface-level error instead
  of silent truncation.  Sleigh contract guarantees `addr_off < 256`
  for EXTRACT/INSERT operands, but the IR contract should still
  enforce it.
- **Verdict:** CORRECTNESS-improving; LOC +8 but worth it.
- **Status:** flagged in R11 S4.7; NOT YET LANDED.

### S4.2 — `if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer)` swallow-Err pattern

- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:431-435`
  (and similar at lines noted by audit 2C K9).
- **Pattern:** swallow `Err`, treat shape mismatch as "not the right
  node".
- **Stdlib idiom:** the `if let Ok(...)` is correct here
  (signature-guaranteed shape), but audit 2C F-5 (R11) flagged that
  this should have an inline comment naming the rationale.
- **Net LOC delta:** 0.
- **Net cognitive delta:** the audit recommends a one-line
  "by IndirectBranch signature" comment.
- **Migration:** add comment.
- **Status:** flagged in R11 S4.6; NOT YET LANDED.

### S4.3 — `ResolvedTargets::Multiple(vec![K])` singleton form in tests

- **Where:** `crates/strider/src/indirect_resolve/classify.rs:368`
  (test pattern `Multiple(vec![42])`); audit 2D R12-T-F flagged that
  the validating `Self::multiple` ctor exists but tuple-construct
  `Multiple(vec)` form is still tuple-pattern-able.
- **Pattern:** `Multiple(Vec)` non-empty invariant bypassable via
  tuple-construct.
- **Stdlib idiom:** wrap in `NonEmptyVec<u64>` newtype OR keep tuple
  shape for pattern-matching but make the field private to the
  variant via the validating ctor.
- **Net LOC delta:** ~+8 (newtype impl).
- **Net cognitive delta:** the variant's documented non-empty
  contract becomes type-enforced.
- **Verdict:** R12-T-F downgrades severity to LOW — every prod
  construction site is statically sound.  Listed here for
  visibility; SKIP this round unless paired with a broader sweep.

---

## Section 5 — Visibility to tighten

### S5.1 — `pattern::RewriteCtx.graph` and `.entry` → `pub(crate)` (HIGH severity)

- **Where:** `crates/pattern/src/rewrite.rs:162-163`.
- **Current:** `pub graph: &'g mut Graph, pub entry: NodeId`.
- **Proposed:** `pub(crate)` (audit 2D R12-T-A — HIGH severity).
- **Migration cost:** SMALL — `Deref<Target=Graph>` + `DerefMut`
  already cover ergonomics; no call-site changes needed.  The W15
  rename verified no `ctx.graph = ...` rebinding sites exist.
- **Net LOC delta:** 0.
- **Net cognitive delta:** removes the rebind footgun (a caller
  could rebind `ctx.graph = &mut other_graph` while keeping
  `entry: NodeId` from the original graph; the next
  `walk_graph(ctx.graph, ctx.entry)` panics on a cranelift-entity
  lookup or silently dereferences a stale id).
- **Status:** flagged in R11 S5.4; NOT YET LANDED.

### S5.2 — `pattern::RewriteCtxView.graph` and `.entry` → `pub(crate)` (HIGH severity)

- **Where:** `crates/pattern/src/rewrite.rs:247-252`.
- **Current:** `pub graph: &'g Graph, pub entry: NodeId`.
- **Proposed:** `pub(crate)` (audit 2D R12-T-A — HIGH severity,
  companion to S5.1).
- **Net LOC delta:** 0.
- **Net cognitive delta:** same rebinding hazard as S5.1.
- **Status:** flagged in R11 S5.3; NOT YET LANDED.

### S5.3 — `pattern::Binding.node` and `.output` → `pub(crate)` (MED severity)

- **Where:** `crates/pattern/src/matcher/bindings.rs:18-19`.
- **Current:** `pub node: NodeId, pub output: Option<NodeOutputId>`.
- **Proposed:** `pub(crate)` (audit 2D R12-T-N — MED severity, new
  in R12 since W14 fixed `bind_capture` but missed `Binding`'s own
  fields).
- **Concrete risk:** External callers can build
  `Binding { node: n, output: Some(o_from_different_node) }` and
  the resulting `Match` silently miscompiles `get_vn` / `get_uint`
  etc.  Cross-field invariant: when `output.is_some()`,
  `graph.output_definition(o).0 == node`.
- **Net LOC delta:** 0.
- **Migration cost:** SMALL — `Binding::new(node, output)` is
  already public.

### S5.4 — `cfg::ProcessInsnRes` → `pub(crate)` (LOW severity)

- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:48-55`.
- **Current:** `pub enum`; only used inside cfg crate (and exposed
  via `test_api`).
- **Proposed:** `pub(crate)` and expose only through `test_api`
  (audit 2D R12-T-M).
- **Net LOC delta:** 0.
- **Migration cost:** trivial — `test_api` re-export already
  isolates the test surface.

### S5.5 — `target::SleighArch` four `pub` fields → `pub(crate)` (LOW severity)

- **Where:** `crates/target/src/arch.rs:121-129`.
- **Current:** all 4 `pub` (`sla_spec`, `pspec`, `endianness`,
  `preset`).
- **Proposed:** `pub(crate)` (audit 2D R12-T-C, low cost).
- **Net LOC delta:** 0.
- **Net cognitive delta:** kills any remaining `(R9-...)`
  breadcrumb doc strings (audit 3B finding F-N et al.).  Accessors
  `sla_spec()` / `pspec()` / `endianness()` / `preset()` already
  exist.
- **Migration:** ~5-6 prod readers swap from field-read to
  accessor-call.
- **Status:** flagged in R11 S5.6; NOT YET LANDED.

### S5.6 — `reader::MemReadError(pub anyhow::Error)` inner field → `pub(crate)` (LOW severity)

- **Where:** `crates/reader/src/lib.rs:41`.
- **Current:** `pub struct MemReadError(pub anyhow::Error)`.
- **Proposed:** `pub(crate)` (audit 2D R12-T-P, new in R12).
- **Net LOC delta:** 0.
- **Net cognitive delta:** removes the unused rebinding path.
  `From<anyhow::Error>` ctor + `Display`/`Error` impls cover every
  legitimate use.  Field-rebinding via `mem_err.0 = anyhow!(...)`
  is a silent-confusion vector.

### S5.7 — `cfg::Cfg::graph` and `cfg::Cfg::sleigh` → tightened (MED severity)

- **Where:** `crates/cfg/src/cfg/mod.rs:54-57`.
- **Current:** `pub graph: RegionGraph, pub sleigh: rsleigh::Sleigh<R>`.
- **Proposed:** `graph` → `pub(crate)` + `pub fn petgraph(&self)`
  accessor; `sleigh` → consider `pub fn into_parts(self) -> (Sleigh<R>, ...)`
  for the orchestrator's ownership-transfer pattern.  Audit 2D
  R12-T-Q (new in R12, MED severity).
- **Concrete risk:** anyone with `&mut Cfg` can mutate the petgraph
  through `cfg.graph` and desynchronise `start_addr_to_region_id`
  (which W14 explicitly fixed).
- **Net LOC delta:** ~+10 (accessor + `into_parts`) - some adjustments.
- **Migration cost:** MED — ~12 prod sites read `cfg.graph`.

### S5.8 — `ir::Graph::create_node` accept-anything `IntConst` payloads (audit 1A MED)

- **Where:** `crates/ir/src/graph/store.rs:224-299`.
- **Current:** `Graph::create_node(NodeKind::IntConst(0x1FF), [], [OutputType(U8)])`
  succeeds without masking 0x1FF to 0xFF, breaking dedup-cache
  invariant.
- **Proposed:** demote `Graph::create_node` to `pub(crate)` for
  `IntConst` kind (force callers through the typed builders) OR
  fold masking into `create_node` itself when `kind == IntConst(_)`.
- **Net LOC delta:** ~+10 (extra branch in `create_node` for
  IntConst/IntConstWide validation).
- **Net cognitive delta:** the dedup-cache invariant becomes
  type-enforced.  Currently the only safeguard is convention — every
  prod-side IntConst caller routes through `make_int_const` or
  `build_int_const`.
- **Status:** audit 1A MED finding; NEW in R12.

---

## Section 6 — Wrappers to drop

### S6.1 — `cfg::test_api` flat re-export module

- **Where:** `crates/cfg/src/test_api.rs:1-22`.
- **Wrapper-ness:** four `pub use` star-imports / item-imports that
  mirror what `crates/cfg/src/cfg/mod.rs:11-22` already exposes via
  `pub use builder::test_api`, `pub use dot::test_api as dot_test_api`,
  etc.  Audit-2D R12-T-M flagged `ProcessInsnRes` (in
  `region_builder_test_api`); this is the same pattern.
- **Net LOC delta:** -22 (entire file).
- **Net cognitive delta:** one fewer module on the
  `cfg::test_api::*` path.  Tests can use either `cfg::test_api::Foo`
  or `cfg::region_builder_test_api::Foo` — the flat file collapses
  the two paths.
- **Migration:** delete `crates/cfg/src/test_api.rs`; integration
  tests already work through the direct paths.
- **Status:** flagged in R11 S6.1 / S1.7 / S1.10; NOT YET LANDED.
  W14 deferred this.

### S6.2 — `dot_test_api`, `indirect_resolve_test_api`, `region_builder_test_api` triple-pub re-exports

- **Where:** `crates/cfg/src/cfg/mod.rs:12-22`.
- **Wrapper-ness:** three `#[doc(hidden)] pub use` re-exports, each
  going via a sub-module's `test_api` named module that itself only
  re-exports a struct or two.  After S6.1, these can collapse into
  one canonical `cfg::test_api` path.
- **Net LOC delta:** -10 (three re-export lines + their inline
  targets if consolidated).
- **Net cognitive delta:** the `cfg` crate's public-but-doc-hidden
  surface shrinks from 4 paths to 1.
- **Migration:** combine with S6.1.

### S6.3 — Three `classify_anchor*` delegators are a wrapper chain (subsumed by S1.2 + S1.3)

- **Where:** `crates/strider/src/indirect_resolve/classify.rs:20-72`
  + `crates/opt/src/indirect_branch_resolve/classify.rs:66-115`.
- **Wrapper-ness:** strider's three forwarders to opt's three
  classifiers; opt's two simpler classifiers forward to the
  canonical `classify_anchor_with_rom_and_sp`.
- **Net LOC delta:** subsumed by S1.2 + S1.3 (-74 total).
- **Migration:** strider tests use the canonical opt entry
  directly.

---

## Section 7 — Partial-state types to convert

### S7.1 — `ir::FunctionGraph::new_invalid` / sentinel-state ctor (R11 S7.4 deferred)

- **Where:** `crates/ir/src/function.rs:13-38`.
- **Current:** `entry`, `entry_control`, `entry_memory` set to
  `NodeId::reserved_value()` until `build_entry` runs.  Fields `pub`,
  so `body().entry` is reachable before initialization.
- **Proposed:** Either (a) make `FunctionGraph`'s ctor produce a
  `Result<Self, BuilderError::EntryNotYetBuilt>`-style total type,
  or (b) tighten the four `pub` fields to `pub(crate)` and add
  accessors that return `Option<NodeId>`.
- **Net LOC delta:** -15 (after accessors land + visibility tighten).
- **Net cognitive delta:** invalid states unrepresentable.  Audit
  2D R12-T-D restated this — "Reading `body().entry` before
  `build_entry` produces a `NodeId::reserved_value()` whose use
  immediately panics inside cranelift_entity."
- **Migration cost:** SMALL — only `body()` / `body_mut()` are used
  externally; one strider call-site reaches in.
- **Status:** flagged in R11 S7.4 / S5.7-adjacent; NOT YET LANDED.

### S7.2 — `cfg::Options` + `cfg::OptionsBuilder` partial-state pair (audit 2D R12-T-G)

- **Where:** `crates/cfg/src/cfg/options.rs:59-230`.
- **Current:** Two scalar fields `fn_max_size: Option<u64>` +
  `allow_code_before_start_addr: bool` where the latter is silently
  ignored when the former is `Some`.  The W12 `function_boundary()`
  accessor recovers the total enum at *read time* but doesn't
  prevent the *write-time* partial state.
- **Proposed:** Replace the two setters with one
  `set_function_boundary(FunctionBoundary)` setter; migrate
  storage to a single `boundary: FunctionBoundary` field.
- **Net LOC delta:** -10 (per-callsite "did the user mean this?"
  comments removed; enum eliminates the conditional silence).
- **Migration cost:** SMALL — example, strider's RunConfig adapter,
  ~3 test fixtures.
- **Status:** flagged in R11 S7.1; NOT YET LANDED.

### S7.3 — `strider::RunConfig` triple-deficiency (R11 S7.2 / audit 2D R12-T-H)

- **Where:** `crates/strider/src/orchestrator.rs:60-106`.
- **Three problems:**
  1. `start_addr: u64` while every other site in the workspace uses
     `MachineInsnAddr` — permits `(start_addr, fn_max_size)`
     argument swap at a literal-construction site.
  2. Same partial-state pair as S7.2 replicated at the strider
     boundary.
  3. All 8 fields `pub`, no defaults or builder.
- **Proposed:** Migrate `start_addr: u64 → MachineInsnAddr`;
  replace `(fn_max_size, allow_code_before_start_addr)` with
  `boundary: FunctionBoundary` (paired with S7.2); add a
  `RunConfigBuilder` or new-with-chain ctor.
- **Net LOC delta:** -15 in aggregate.
- **Migration cost:** SMALL per individual fix; MED in aggregate.
- **Status:** flagged in R11 S7.2; NOT YET LANDED.

---

## Top-5 cognitive-load assessment per category

### Section 1 — Code to delete

1. **S1.4** (`cfg::Builder::new` + `with_endianness` ctors) —
   the largest single LOC win remaining (~95 LOC) and the only one
   that closes a real footgun (silent X86_64 preset default on
   non-x86 binaries).
2. **S1.2** + **S1.3** (the `classify_anchor*` shim chain) — kills
   the N²-naming explosion called out in audit 2B line 235.
3. **S1.5** (relocate the 380-LOC inline-test mod from strider to
   opt) — clarifies which crate owns the contract.
4. **S1.1** (`Graph::make_bool_const` / `make_float_const`
   duplicates) — paired with R11 S1.5's `make_int_const` migration.
5. **S1.6** (`from_graph_and_entry_for_rewrite` legacy ctor) — kills
   the `#![allow(deprecated)]` headers in 4 pattern test files.

### Section 2 — Code to merge

1. **S2.1** (`function: &mut RewriteCtx` → `ctx` rename across 11
   opt-pass sites) — pure mechanical alignment; biggest readability
   win.
2. **S2.4** (`Cfg::sleigh` + `Cfg::graph` sibling consolidation) —
   closes the W14 desync hazard that `start_addr_to_region_id`
   tightening tried to prevent.
3. **S2.3** (`set_lift_addr` RAII pair) — matches the existing
   `lift_at` RAII shape; reduces panic-leak surface.
4. **S2.2** (`WideConstStorage::to_le_bytes` arms) — trivial.

### Section 3 — Single-callsite helpers to inline

1. **S3.3** (`OptimizationResult::after_replace` migration-history
   doc) — pure doc cleanup; readers stop being confused by
   historical references.
2. **S3.1** (`Strider::arch()` single-callsite accessor) — trivial
   field promotion.
3. **S3.2** (`body()` / `body_mut()`) — SKIP unless paired with
   S7.1's `FunctionGraph` migration.
4. **S3.4** (`dot_style_for`) — SKIP, real centralisation benefit.

### Section 4 — Stdlib-idiom replacements

1. **S4.1** (EXTRACT/INSERT `as u8` truncation) — correctness;
   surface-level error instead of silent truncation.
2. **S4.2** (`if let Ok([_, _, val])` comment annotation) — pure
   doc; clarifies intent.
3. **S4.3** (`ResolvedTargets::Multiple` non-empty) — SKIP unless
   paired with a broader newtype sweep.

### Section 5 — Visibility to tighten

1. **S5.1** + **S5.2** (`RewriteCtx{,View}.graph`/`.entry`) — HIGH
   severity; rebinding hazard.  Top priority quick wins from audit 2D.
2. **S5.7** (`Cfg::graph` + `Cfg::sleigh`) — MED severity; new in
   R12 (audit 2D R12-T-Q).  Closes the W14 desync hazard.
3. **S5.8** (`Graph::create_node` IntConst-payload check) — MED
   severity (audit 1A); new in R12.
4. **S5.3** (`pattern::Binding.{node,output}`) — MED severity (audit
   2D R12-T-N); new in R12.
5. **S5.4** + **S5.5** + **S5.6** (`ProcessInsnRes` + `SleighArch` +
   `MemReadError`) — LOW severity batch; ~30 minutes total.

### Section 6 — Wrappers to drop

1. **S6.1** + **S6.2** (`cfg::test_api` flat re-export + triple
   sub-module re-exports) — biggest single deletion; collapses 4
   `cfg`-crate doc-hidden surfaces to 1.
2. **S6.3** (the `classify_anchor` wrapper chain) — subsumed by
   S1.2 + S1.3.

### Section 7 — Partial-state types

1. **S7.2** + **S7.3** (`cfg::Options` / `strider::RunConfig`
   partial-state pair) — high user-facing visibility.  Eliminates
   "user knob silently ignored" trap from R11.
2. **S7.1** (`FunctionGraph::new_invalid`) — invalid states observable
   through `pub` fields; sum type closes the gap (R11 S7.4 deferred).

---

## What's been already-harvested since round 11

Confirmed via audits 1A-1F + own grep scans (round 11 wave verification):

- **S1.1 (R11)** — `opt::IndirectBranchResolve` struct + tests
  DELETED (-700 LOC, audit 1C confirms public API no longer lists
  the type).
- **S1.2 (R11)** — `opt::AnchorAddr` newtype DELETED (no hits in
  source; README still mentions it — see audit 1C HIGH finding).
- **S1.3 (R11)** — `entity-utils::DenseEntitySet::test_and_set`
  DELETED (no hits in source; audit 1F F-9 confirms).
- **S1.4 (R11)** — `cfg::Builder::for_arch` migration LANDED for
  production code (audit 1B focus area 7 verified); deprecated ctors
  still present for tests (see S1.4 above).
- **S1.6 (R11)** — `WideConstStorage::from_le_bytes_u{256,512}`
  DELETED (no hits in source).
- **S2.5 (R11)** — `default_pipeline` composed from
  `stable + destructive` LANDED (audit 1C focus area 9 + audit 3A
  claim 8).
- **S2.6 (R11)** — `Outputs/Inputs::is_empty` cleanup LANDED
  (`crates/ir/src/iterators.rs:16, 80`).
- **S4.1 (R11)** — `Worklist::enqueue` single-pass LANDED
  (`crates/entity-utils/src/worklist.rs:55-59`, audit 1F).
- **S5.1 (R11)** — `cfg::PcodeInsnAddr` / `MachineInsnAddr` field
  tightening LANDED (W14 M-14; audit 2D #1, #2 confirmed FIXED).
- **S5.5 (R11)** — `cfg::ProcessInsnRes` still `pub` (S5.4 above
  carries forward).
- **S5.7 (R11)** — `BuiltFunctionGraph` CC fields tightened to
  `pub(crate)` (W14 H-6; audit 1A finding line 95-99 notes the doc
  is now stale and needs follow-up).
- **S5.8 (R11)** — `cfg::Cfg::start_addr_to_region_id` tightened
  (W14 M-16; audit 2D #7 confirmed FIXED).  See S5.7 above for the
  remaining sibling fields.
- **S1.13 (R11)** — `BuiltCallingConvention::from_parts_unchecked`
  DELETED (audit 2D #11 confirmed; only `try_from_parts` remains).
- **R11 wave** test-name nitpicks (`simple_graph`, `diamond`,
  `loop_graph`, `ranges_disjoint_basic`, `forward_basic`) renamed
  (audit 2B "What's clean since round 11").

The strict trust model precludes reading round 11's own
simplifications report's "verdict" annotations; this list was
re-verified from current-tree evidence only.
