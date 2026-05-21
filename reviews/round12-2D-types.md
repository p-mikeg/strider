# Round 12 — 2D: type-design audit

Branch reviewed: `review/ai6` working tree at `/mnt/c/Users/mikeg/Documents/strider` (forked from `feature/ai` HEAD `008f530`).

This audit re-walks the Round 11 2D findings (28 items) to see what Round 11 W14 / W15 fixed and what remains, and then sweeps the focus surfaces from the round-12 prompt for any new leakiness. Tone is mostly "already-fixed" / "remaining-with-rationale"; the bulk of the actionable work was done in W14.

## Summary table — Round 11 2D items, current state

| # | Item (R11 finding) | Round 12 state | Note |
|---|--------------------|----------------|------|
| 1 | `cfg::PcodeInsnAddr` `pub` fields | ✅ FIXED in W14 (M-14): both fields `pub(crate)`, `new` ctor + accessors | |
| 2 | `cfg::MachineInsnAddr` `pub addr` | ✅ FIXED in W14 (M-14) | |
| 3 | `pattern::RewriteCtxView` `pub graph/entry` | ⚠️ PARTIAL (W14 added `graph_ref`/`entry()` accessors; fields stayed `pub` for opt-pass ergonomics) — risk acknowledged in doc | See R12-T-A |
| 4 | `pattern::RewriteCtx` `pub graph/entry` | ⚠️ PARTIAL — same as #3, accessors added but fields stayed `pub` | See R12-T-A |
| 5 | `ir::BuiltFunctionGraph` CC `pub` fields | ✅ FIXED in W14 (H-6): five CC fields `pub(crate)` with `*_for_test` setters; `pub graph`/`pub entry` kept | partial (graph/entry still pub — see R12-T-D) |
| 6 | `ir::FunctionGraph` partial-state `pub` fields | ❌ DEFERRED (M-15: 20+ external test sites) | See R12-T-D |
| 7 | `cfg::Cfg::start_addr_to_region_id` `pub` map | ✅ FIXED in W14 (M-16): `pub(crate)` + `from_parts_for_tests` ctor | |
| 8 | `cfg::Region` `pub` fields (cross-field invariant) | ❌ DEFERRED (M-17: 52 external test sites) | See R12-T-B |
| 9 | `target::SleighArch` `pub` fields | ❌ NOT FIXED — accessors exist but fields stayed `pub` | See R12-T-C |
| 10 | `target::CallingConvention` stringly-typed reg-names | LOW intentional — kept | |
| 11 | `target::BuiltCallingConventionParts` + `from_parts_unchecked` | ✅ FIXED — `from_parts_unchecked` deleted; only validating `try_from_parts` remains. (`#[non_exhaustive]` still not applied — needs `rsleigh::Vn::Default`, M-18) | partial — see R12-T-E |
| 12 | `opt::Kb` `pub ones/zeros` | ✅ FIXED in W14: `pub(crate)` + `try_new` + `ones()/zeros()` accessors | |
| 13 | `opt::IndirectBranchResolve` `pub` config | ✅ N/A — entire struct deleted by W9 S1.1 (-633 LOC) | |
| 14 | `opt::AnchorAddr` opaque-by-doc | ✅ N/A — also deleted by S1.1 | |
| 15 | `opt::ResolvedTargets::Multiple(Vec<u64>)` non-empty | ⚠️ PARTIAL — validating `multiple()` ctor exists, but tuple-construct `Multiple(vec)` form still public for pattern-matching | See R12-T-F (low) |
| 16 | `cfg::Options`/`OptionsBuilder` partial state | ⚠️ PARTIAL — `function_boundary()` accessor recovers the total enum, but the two setters on `OptionsBuilder` still let callers set `allow_code_before_start` after `set_function_max_size` and have it silently ignored | See R12-T-G |
| 17 | `strider::RunConfig` partial-state + raw `u64` start_addr | ❌ DEFERRED (M-21) — all 8 fields still `pub`, `start_addr: u64` unchanged, partial-state pair `(fn_max_size, allow_code_before_start_addr)` still flat | See R12-T-H |
| 18 | `strider::AnalyzeOptions` `Option<Vec<Vn>>` partial-state | ❌ NOT FIXED — `all_vns: Option<Vec<Vn>>` with same sorted-and-complete preconditions, all `pub` | See R12-T-I |
| 19 | `strider::RegionLiftHandles` 9 `pub` fields | ❌ DEFERRED (M-22) — all 9 fields still `pub`, cross-field invariants still doc-only | See R12-T-J |
| 20 | `strider::AnalyzeOutcome` `pub` bag | ❌ NOT FIXED — fields still `pub`, referential invariants doc-only | See R12-T-K |
| 21 | `opt::OptimizerPipeline::optimizer_names` `type_name` capture | ❌ NOT FIXED — still `Vec<&'static str>` captured at registration | See R12-T-L (low) |
| 22 | `cfg::ProcessInsnRes` boolean enum (`pub`) | ❌ NOT FIXED — still `pub` although only used internally in builder | See R12-T-M (low) |
| 23 | `pattern::Binding` `pub` fields w/ cross-field invariant | ❌ NOT FIXED — `Binding { pub node, pub output }` still public; `Bindings::bind_capture` is `pub(crate)` (M-23 done) but `Binding`'s OWN fields still leak | See R12-T-N |
| 24 | `ir::WideConstStorage` | OK, kept as-is | |
| 25 | `cfg::Builder` deprecated `new` / `with_endianness` | DEPRECATED ctors still present; not removed | partial (acceptable) |
| 26 | `target::Endianness` | OK | |
| 27 | `reader::ElfFileMemReader` `is_little_endian: bool` | ❌ NOT FIXED — still `bool` (private field, cosmetic only) | See R12-T-O (very low) |
| 28 | `reader::RelocationStats` 8 `pub` fields | OK pure-data return | |

Newly observed in Round 12 (not in R11):

| # | Item | Severity |
|---|------|----------|
| R12-T-P | `reader::MemReadError(pub anyhow::Error)` — newtype wrapper exposes inner `anyhow::Error` field | LOW |
| R12-T-Q | `cfg::Cfg::sleigh` + `cfg::Cfg::graph` still `pub` (sibling fields of the `pub(crate)`-tightened `start_addr_to_region_id`) | MED |

## Per-finding details (remaining items only)

### R12-T-A. `pattern::RewriteCtx{,View}` — `pub graph/entry` fields kept for opt ergonomics

- **Severity:** HIGH (carries over from R11 #3/#4 unchanged)
- **Where:** `crates/pattern/src/rewrite.rs:161-164` (RewriteCtx) and `:239-253` (RewriteCtxView)
- **What changed in W14 / W15:** Accessors `graph_ref()`, `graph_mut()` (RewriteCtx only), `entry()` were added. `Deref<Target=Graph>` + `DerefMut` exist for ergonomic call-site compatibility. The doc-comment on each field now spells out the hazard explicitly ("Re-binding the field … silently redirects the view at distance").
- **Why still pub:** The W14 commit message (M-15 sibling) cites "opt passes access `function.graph` directly across hundreds of call sites." Round 11 W15 renamed `fg → ctx` across passes but did not change the field-access pattern.
- **Concrete risk:** External code holding `&mut RewriteCtx` can rebind `ctx.graph = &mut other_graph` while keeping the old `entry: NodeId`. With a fresh graph the entry node id is almost certainly invalid (different `cranelift_entity` arena) and the next `walk_graph(ctx.graph, ctx.entry)` panics on a `cranelift_entity` lookup or — worse — silently dereferences a different node at the same packed id.
- **Migration cost (re-estimated):** Now SMALL: accessors are in place, the W15 rename verified no `ctx.graph = ...` rebinding sites exist in prod, and external Python bindings don't reach in. The remaining work is to flip fields to `pub(crate)` and let the workspace's `ctx.graph.x()` calls go through `Deref` automatically (no source changes needed because of `Deref<Target=Graph>`).
- **Proposed shape:** Tighten both fields on both types to `pub(crate)`. The `Deref{,Mut}` impls keep `ctx.x()` working. No accessor changes needed at call sites.

### R12-T-B. `cfg::Region` — `pub` fields with cross-field invariant (R11 #8 deferred)

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/types.rs:246-256`
- **Status in W14:** Deferred (M-17) — "52 external reader sites in test files alone".
- **Restated invariant:** `insns.is_empty() ⇒ matches!(terminator, RegionTerminator::Branch)`. `Builder::add_region` enforces it at construction; external mutation of `region.insns` after the fact can violate it. Strider's lift driver iterates `region.insns` heavily (`pipeline.rs:366,386`), but every prod site is read-only.
- **Path forward in two phases:**
  1. (cheap, ~10 prod sites) Add `pub fn insns(&self) -> &[RegionInstruction]` / `pub fn terminator(&self) -> &RegionTerminator` accessors and migrate the 10 prod sites. Tests stay on field access.
  2. (later) Tighten fields to `pub(crate)`; tests migrate as a separate, larger sweep.
- **Recommendation for round 12:** Phase 1 only (accessor surface + prod migration). Phase 2 deferred again pending the 52-test sweep.

### R12-T-C. `target::SleighArch` — `pub` fields with `()` accessors (R11 #9)

- **Severity:** LOW
- **Where:** `crates/target/src/arch.rs:118-156`
- **Status:** Accessors `sla_spec() / pspec() / endianness() / preset()` exist with doc-comments calling out the migration intent. Fields stayed `pub` through W14/W15.
- **Migration cost:** SMALL — ~5-6 prod readers.
- **Recommendation:** Tighten the four fields to `pub(crate)`. `SleighArch` is `Copy` and constructed only via the 13 preset constructors, so external code already goes through one of those.

### R12-T-D. `ir::FunctionGraph` + `ir::BuiltFunctionGraph::{graph, entry}` — partial-state ctor leaks (R11 #6 / #5 partial)

- **Severity:** MED for `FunctionGraph`; LOW for `BFG::graph`/`entry`
- **Where:** `crates/ir/src/function.rs:13-23` (`FunctionGraph`); `:47-49` (`BuiltFunctionGraph::graph` / `entry`)
- **Status:**
  - W14 fixed the CC-bearing five fields on `BuiltFunctionGraph`; kept `pub graph` / `pub entry` "downstream callers structurally rely on the through-path for arena writes" (R11 #5 footnote).
  - W14 deferred `FunctionGraph`'s four `pub` fields (M-15: 20+ external test sites). `FunctionGraph::new_invalid` is still `pub(crate)` and uses `cranelift_entity` reserved-value sentinels for `entry / entry_control / entry_memory`, but the **fields themselves remain pub** so external callers of `FunctionBuilder::body()` can observe sentinels during the brief window before `build_entry` runs.
- **Concrete risk for FunctionGraph:** Reading `body().entry` before `build_entry` produces a `NodeId::reserved_value()` whose use immediately panics inside cranelift_entity. Reading `body().graph.output_definition(entry_control)` likewise. The doc says "consumers should never observe a partial graph" but the type system doesn't enforce that.
- **Migration cost:** SMALL (only `body()` / `body_mut()` are used externally; one strider call-site reaches in).
- **Proposed shape:** Tighten the four `pub` fields on `FunctionGraph` to `pub(crate)`. Add accessors that return `Result<NodeId, BuilderError::EntryNotYetBuilt>` (or `Option`). `BuiltFunctionGraph::{graph, entry}` is a separate question (downstream callers want arena access via `Deref`); recommend keeping graph/entry pub for now and revisiting after FunctionGraph is tightened.

### R12-T-E. `target::BuiltCallingConventionParts` — `#[non_exhaustive]` blocked by no Default (R11 #11 partial)

- **Severity:** LOW (downgraded — `from_parts_unchecked` is gone)
- **Where:** `crates/target/src/calling_convention/mod.rs:127-148`
- **Status in W14:** `from_parts_unchecked` was deleted; only the validating `try_from_parts` remains. The struct is fully `pub` but every consumer goes through `try_from_parts`. M-18 (`#[non_exhaustive]`) deferred because rsleigh::Vn has no Default impl, so adding a field would force every existing literal site to update.
- **Remaining risk:** Future field addition (e.g. a new register class) forces a breaking change at all 6 struct-literal sites. `#[non_exhaustive]` would push that to a non-breaking addition.
- **Recommendation:** Defer until a future field addition makes it cheap; meanwhile every consumer is centrally `try_from_parts` so the cost of an additive change is bounded (mechanical edit of 6 literals).

### R12-T-F. `opt::ResolvedTargets::Multiple(Vec<u64>)` — non-empty invariant still expressible via tuple-construct (R11 #15)

- **Severity:** LOW (down from MED — every prod construction site is statically sound per W14 audit)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:76-117`
- **Status in W14:** Verified all `Multiple(...)` construction sites — every one passes a non-empty literal or a guaranteed-non-empty source. `#[non_exhaustive]` not applied because it would break many match-destructure sites.
- **Remaining risk:** A future arm constructs `Multiple(Vec::new())` and the dispatch site silently appears unreachable. The validating `Self::multiple` ctor catches it but doesn't prevent the tuple-construct path.
- **Recommendation:** Keep as-is. Document the constructor preference in the variant docstring and rely on future contributor diligence. Wrapping in a `NonEmptyVec` newtype is purer but pattern-match ergonomics on `Multiple(targets) =>` are load-bearing in the orchestrator.

### R12-T-G. `cfg::OptionsBuilder` — two-setter partial-state at the builder level (R11 #16)

- **Severity:** LOW (partial improvement in W12 simplifications via `function_boundary()` accessor on `Options`)
- **Where:** `crates/cfg/src/cfg/options.rs:162-230`
- **What's still wrong:** `OptionsBuilder::set_function_max_size(max_size)` followed by `.allow_code_before_start_addr()` produces an `Options` whose boundary is `Bounded { max_size }` but whose `allow_code_before_start_addr: true` flag is silently ignored at every consumer (per `Options::function_boundary()`).
- **Migration cost:** SMALL — replace the two setters with `set_function_boundary(FunctionBoundary)` or two named ctors (`bounded(max_size)` / `unbounded(allow_code_before_start: bool)`). Touches the example, strider's RunConfig adapter, ~3 test fixtures.
- **Recommendation:** Replace the two setters with one `set_function_boundary` setter that takes the total `FunctionBoundary` enum. Migrate `Options`'s storage from two scalars to a single `boundary: FunctionBoundary` field. The change is bigger than the simple `function_boundary()` accessor but eliminates the partial-state class entirely at both the builder and the storage.

### R12-T-H. `strider::RunConfig` — 8 `pub` fields, raw `u64` start_addr, flat partial-state (R11 #17, M-21 deferred)

- **Severity:** MED (carries over from R11)
- **Where:** `crates/strider/src/orchestrator.rs:60-106`
- **Three deficiencies (re-stated for round 12):**
  1. `start_addr: u64` while every other site in the workspace uses `MachineInsnAddr`. Permits `(start_addr, fn_max_size)` argument swap at a literal-construction site.
  2. `(fn_max_size: Option<u64>, allow_code_before_start_addr: bool)` is the same partial-state pair as `cfg::OptionsBuilder` — the W12 `function_boundary()` fix on the cfg side did not propagate up.
  3. All 8 fields `pub`, no defaults or builder. With a generic param `R: MemReader` the user-facing literal construction is brittle.
- **Migration cost:** SMALL per individual fix; MED in aggregate.
- **Recommendation:**
  - Migrate `start_addr: u64 → MachineInsnAddr` (~3 prod sites).
  - Replace `(fn_max_size, allow_code_before_start_addr)` with `boundary: FunctionBoundary` (paired with R12-T-G).
  - Add a `RunConfigBuilder` (or `RunConfig::new(strider, start_addr, sleigh).with_rom(...)` chain) so the literal-construction surface contracts to the ctor.

### R12-T-I. `strider::AnalyzeOptions` — `Option<Vec<Vn>>` partial-state (R11 #18)

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:89-116`
- **Restated:** `all_vns: Option<Vec<Vn>>` doc-comments two preconditions (sorted by `vn_sort_key`; complete superset of every varnode any instruction in `cfg` references). Both unenforced. `None` triggers a fresh scan; `Some(vec)` is the orchestrator's cache-reuse path.
- **Migration cost:** SMALL (only orchestrator + tests build it).
- **Recommendation:** `enum VarnodeSet { Compute, Cached(SortedVns) }` where `SortedVns` is a newtype whose ctor verifies sortedness. Tighten `pub all_vns` to `pub(crate)` and expose the enum through the public API. Lower priority than R12-T-H.

### R12-T-J. `strider::RegionLiftHandles` — 9 `pub` fields, cross-field invariants (R11 #19, M-22 deferred)

- **Severity:** MED
- **Where:** `crates/strider/src/strider/pipeline.rs:21-47`
- **Status in W14:** Deferred — "orchestrator-internal struct; tightening would force a per-region lift-driver constructor API."
- **Cross-field invariants (un-enforced):**
  - `entry_control == output_definition(entry_control_state).0`
  - `entry_memory == memory_output(entry_mem_phi)`
  - `exit_vn_to_value` keys match the function's tracked-variables set
  - All entry_* / exit_* `NodeOutputId`s reference live nodes in the graph the orchestrator sees
- **Migration cost:** SMALL — only the strider lift driver writes; only the orchestrator reads. The "would force a per-region lift-driver constructor API" objection is real but the constructor is one new function with 9 parameters or one builder.
- **Recommendation:** Add `pub(crate) fn new(...) -> Result<Self>` ctor that validates the `output_definition(entry_control).0 == entry_control_state` cross-field check (and analogous for memory). Tighten fields to `pub(crate)`. Add `pub fn start_addr(&self)`, `pub fn entry_control(&self)`, etc., accessors as the orchestrator demands.

### R12-T-K. `strider::AnalyzeOutcome` — fully-`pub` bag (R11 #20)

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:56-83`
- **Status:** No change in W14. Every `Value` (= `NodeOutputId`) in `unresolved_branches` must reference a node alive in `graph`. Every `RegionLiftHandles.entry_*/exit_*` `NodeOutputId` likewise. Cross-field referential invariants; `pub` lets consumers swap one without the others. The only consumer is internal so the risk is theoretical.
- **Recommendation:** Defer with R12-T-J. They share the same orchestrator-internal scope and can be tightened together.

### R12-T-L. `opt::OptimizerPipeline::optimizer_names` — `type_name` capture (R11 #21)

- **Severity:** LOW
- **Where:** `crates/opt/src/pipeline.rs:175-186`
- **What's wrong:** Stores `std::any::type_name::<O>()` at registration time. Documentation acknowledges "implementation-defined but stable enough for substring matching." The orchestrator's pipeline-shape tests reach across an abstraction boundary using these strings.
- **Migration cost:** MED — would require a `fn pass_id(&self) -> &'static str` on the `Optimizer` / `OptimizerRaw` trait and a one-line addition per pass impl.
- **Recommendation:** Defer. Low-priority hygiene; the test-only consumer can stay on `type_name` until a future pass refactor makes the trait-method addition cheap to batch.

### R12-T-M. `cfg::ProcessInsnRes` — `pub` 2-variant boolean enum (R11 #22)

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:48-55`
- **Status:** Still `pub`; no external user.
- **Recommendation:** Tighten to `pub(crate)`. One-line change; zero call-site impact.

### R12-T-N. `pattern::Binding` — `pub` fields with cross-field invariant (R11 #23 partial)

- **Severity:** MED (W14 M-23 fixed `Bindings::bind_capture` → `pub(crate)` but missed `Binding`'s own field visibility)
- **Where:** `crates/pattern/src/matcher/bindings.rs:17-27`
- **What's wrong:** `pub struct Binding { pub node: NodeId, pub output: Option<NodeOutputId> }`. Cross-field contract: when `output` is `Some(o)`, `graph.output_definition(o).0 == node`. External callers can build `Binding { node: n, output: Some(o_from_different_node) }` and the resulting `Match` silently miscompiles `get_vn`, `get_uint`, etc.
- **Migration cost:** SMALL — `Binding` is constructed inside `pattern` (matcher/bindings.rs:24, 81-89). The W14 `bind_capture_for_test` setter takes a `Binding`, which is why the fields stayed `pub`. Tightening fields to `pub(crate)` requires `Binding::new(node, output)` to be `pub` (already is) so the `_for_test` path stays open.
- **Proposed shape:** Tighten both fields to `pub(crate)`. `Binding::new(node, output)` is already public and is the only construction path needed.

### R12-T-O. `reader::ElfFileMemReader::is_little_endian: bool` (R11 #27)

- **Severity:** Very low (private field, cosmetic only)
- **Where:** `crates/reader/src/elf.rs:255-258`
- **Recommendation:** Defer. Internal `bool` is awkward in a codebase that has `target::Endianness`, but the field is private so it's pure code-smell with no leak.

### R12-T-P. `reader::MemReadError(pub anyhow::Error)` — tuple-struct exposes inner field (new in round 12)

- **Severity:** LOW
- **Where:** `crates/reader/src/lib.rs:41`
- **What's wrong:** `pub struct MemReadError(pub anyhow::Error)`. External callers can rebind the inner error: `mem_err.0 = anyhow!("totally different cause")`. The `Display` / `Error` impls already expose the message and the `source()`, so the `pub` on the inner field is redundant. Also `From<anyhow::Error>` is already provided as the canonical construction path.
- **Migration cost:** SMALL — make the field `pub(crate)` and remove the (unused) public rebinding path. The `From<anyhow::Error>` ctor and `Display`/`Error` impls cover every legitimate use.
- **Recommendation:** Tighten the inner field to `pub(crate)`.

### R12-T-Q. `cfg::Cfg::sleigh` + `cfg::Cfg::graph` — sibling `pub` fields of the W14-tightened map (new in round 12)

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/mod.rs:54-57`
- **What's wrong:** W14 tightened `start_addr_to_region_id` to `pub(crate)` and added `from_parts_for_tests`. But the sibling fields `pub sleigh: Sleigh<R>` and `pub graph: RegionGraph` remained `pub`. The doc on the W14-tightened field says "External readers should still go through `region_id_at_start` … direct mutation desyncs the index from `graph`" — but anyone with `&mut Cfg` can still call `cfg.graph.remove_node(_)` directly and produce exactly the same desync, since the petgraph handle is still public.
- **Concrete risk:** strider's per-iteration loop holds `&Cfg` and reads the graph. If a future caller takes `&mut Cfg` and mutates the petgraph through the `pub` field, the W14 invariant fix is bypassed.
- **Migration cost:** MED — `cfg.graph` is read in several visualization + orchestrator paths (`grep` shows ~12 prod sites). Tightening to `pub(crate)` and exposing a `petgraph()` accessor is mechanical.
- **Recommendation:** Tighten `graph` to `pub(crate)` and expose `pub fn petgraph(&self) -> &RegionGraph`. `sleigh` is owned by `Cfg` because `strider::run` harvests it between iterations — that legitimately needs ownership transfer, so a `pub fn into_parts(self) -> (Sleigh<R>, ...)` would be the cleaner shape than a `pub` field.

## Recommended round-12 work order

Quick-win batch (SMALL cost, MED severity each, no test sweep needed):

1. **R12-T-A** — Tighten `RewriteCtx{,View}::{graph, entry}` to `pub(crate)`. With `Deref{,Mut}<Target=Graph>` in place, no call-site changes needed. (~10 minutes; high-severity hazard removed.)
2. **R12-T-N** — Tighten `pattern::Binding::{node, output}` to `pub(crate)`. `Binding::new` is the only construction path. (~5 minutes.)
3. **R12-T-M** — Tighten `cfg::ProcessInsnRes` to `pub(crate)`. (~2 minutes.)
4. **R12-T-P** — Tighten `MemReadError`'s inner field to `pub(crate)`. (~5 minutes.)

Bigger items worth picking up (each is the kind of self-contained fix R11 W14 has already proven cost-effective):

5. **R12-T-C** — Tighten `SleighArch` 4 fields to `pub(crate)`. ~5-6 read sites swap to the accessors. (~20 minutes.)
6. **R12-T-Q** — Tighten `Cfg::graph` to `pub(crate)` + `petgraph()` accessor + `into_parts()` for the Sleigh ownership transfer. (~30 minutes; ~12 prod sites.)
7. **R12-T-G** — Replace `OptionsBuilder` two-setter pair with `set_function_boundary`. Carries through to **R12-T-H** as the natural sub-step. (~45 minutes for the cfg side alone.)

Deferred (cost > benefit this round):

- R12-T-B (`Region` fields — 52 test sites; phase 1 accessor surface is the cheap part if anyone wants the half-step)
- R12-T-D (`FunctionGraph` partial-state — 20+ external test sites)
- R12-T-H (`RunConfig` triple-deficiency — see R12-T-G for the partial-state sub-step; the others remain MED)
- R12-T-I (`AnalyzeOptions` newtype — low priority)
- R12-T-J + R12-T-K (`RegionLiftHandles` + `AnalyzeOutcome` cross-field invariants — orchestrator-internal, theoretical risk only)
- R12-T-L (`type_name` capture — wait for a `pass_id` trait-method opportunity)
- R12-T-O (`is_little_endian: bool` — private field, cosmetic)
- R12-T-E (`#[non_exhaustive]` on `BuiltCallingConventionParts` — blocked by no `Vn::Default`)
- R12-T-F (`Multiple(Vec)` non-empty — every prod site sound by construction)

## What I deliberately did *not* flag

- `ir::Graph` — every field `pub(crate)`. Accessors total. Side-tables (asm fingerprints, stack-phi offsets, wide consts, call-other names, call-clobbered overrides) follow the established `SecondaryMap` pattern with O(1) lookup. No leaks.
- `pattern::Pat` / `Pat::from_dyn` / `Pat::as_dyn` — opaque wrapper, `pub(crate)` ctors, `Arc`-shared inner. Textbook good.
- `pattern::Capture(u32)` — newtype with `from_id`/`id` round-trip explicitly documented as a cross-FFI escape hatch.
- `pattern::Match` — `pub(super)` fields, accessors total.
- `pattern::Bindings` — `bind_capture` is `pub(crate)`; only `bind_capture_for_test` reaches outside. M-23 done.
- `target::CallingConvention` — every field private; the 17 preset constructors plus a validating `from_parts` form the only construction surface. Stringly-typed register names is a deliberate ergonomics tradeoff.
- `target::Endianness` / `target::ArchPreset` — pure-data closed enums. Clean.
- `ir::NodeKind` / `NodeOutputKind` / `NodeOutputType` / `WideConstStorage` — closed-set discriminators with single-source-of-truth `expected_signature`. Pattern is correct.
- `cfg::DecodeCache` — internal cache type with private storage and a single-source-of-truth accessor.
- `reader::MemRegion` — private fields, constructor validates, accessors total.
- `opt::Optimizer` / `OptimizerRaw` — clean trait design; the blanket impl is the right boundary between the two.
- `opt::AnchorCallingContext` — pure-data bag carrying a per-anchor snapshot; fields `pub` but it's a one-way orchestrator-to-resolver carrier and the consumer reads each field exactly once.

The Round 11 W14 batch was the biggest single-round encapsulation improvement to date (H-6, M-10, M-14, M-16, M-23 in one commit). The residual items are mostly either (a) high blast-radius migrations the W14 commit deliberately deferred (M-15 / M-17 / M-21 / M-22), or (b) low-severity hygiene items where the cost-vs-benefit hasn't crossed a "do it now" threshold (most of R12-T-{F,I,K,L,M,O,P}).
