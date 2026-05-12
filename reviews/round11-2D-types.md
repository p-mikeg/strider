# Round 11 — 2D: type-design audit

Branch reviewed: `feature/ai` working tree at `/mnt/c/Users/mikeg/Documents/strider`.

This audit walks every `pub struct` / `pub enum` / `pub trait` flagged by the
high-priority surfaces brief and the broader `grep` sweep. For each finding I
cite `file:line` and the construction-site / call-site cost picture from
`grep`. I distinguish "hazard worth fixing" from "cost the codebase pays for
ergonomics" by tagging severity HIGH / MED / LOW.

## Summary

| Category                                    | Count |
|---------------------------------------------|-------|
| Leaky encapsulation                         |   8   |
| Primitive obsession                         |   5   |
| Failed invariant expression                 |   4   |
| Partial-state types                         |   6   |
| Implementation detail leaked to API         |   5   |
| Total findings                              |  28   |

## Findings

### 1. `cfg::PcodeInsnAddr` — `pub` fields with mandated accessors

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/types.rs:60-100`
- **Type:** `pub struct PcodeInsnAddr { pub machine_addr: MachineInsnAddr, pub insn_index: u64 }`
- **What's wrong:** Both fields are `pub` while four canonical accessors
  (`machine_addr()`, `insn_index()`, `machine_addr_u64()`, `at_machine_start()`)
  are documented as the migration path. Field-tightening was deferred for
  multiple rounds: `grep` finds 14 direct `.insn_index` reads, 21 direct
  `.machine_addr.addr` chains, plus 7 struct-literal constructions outside of
  `types.rs` (`region_builder.rs:175,187`; `query.rs:149,153`;
  `builder/mod.rs:238`; orchestrator + tests). Migration is at most ~10 prod
  sites. Field ordering `machine_addr` first is load-bearing for the derived
  `Ord` (line 58), so any constructor change must preserve it.
- **Migration cost:** SMALL (≤ 10 prod sites; tests add another ~5)
- **Proposed shape:** Tighten both fields to `pub(crate)`. Add `pub fn new(addr,
  insn_index)` ctor next to `at_machine_start`. Production callers already
  accept the accessor surface.
- **Call-site impact:** Strider/Cfg builders/queries swap `.machine_addr.addr`
  → `.machine_addr_u64()` and `.insn_index` → `.insn_index()`. Tests in
  `cfg/tests/*` and the `synthetic.rs` helper wrap their literal builds in the
  new ctor.

### 2. `cfg::MachineInsnAddr` — `pub addr: u64` field with `as_u64`

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/types.rs:24-49`
- **Type:** `pub struct MachineInsnAddr { pub addr: u64 }`
- **What's wrong:** Same migration story as `PcodeInsnAddr`. Direct `.addr`
  reads are tracked in finding 1 (the typical pattern is the chained
  `.machine_addr.addr` access). The newtype's invariant is the *type
  distinction* itself (preventing a `u64` address from accidentally fulfilling
  a `MachineInsnAddr` slot), and `From<u64>` already gives that for free.
- **Migration cost:** SMALL — tightening the field to `pub(crate)` only
  invalidates the chained access pattern; callers swap to `.machine_addr_u64()`
  on `PcodeInsnAddr`.
- **Proposed shape:** Tighten `addr` to `pub(crate)`. Keep `as_u64()`,
  `From<u64>`, and `Into<u64>` accessors as the surface.
- **Call-site impact:** Mostly subsumed by finding 1's migration.

### 3. `pattern::RewriteCtxView` — `pub graph: &Graph, pub entry: NodeId`

- **Severity:** HIGH
- **Where:** `crates/pattern/src/rewrite.rs:215-229`
- **Type:** `#[derive(Clone, Copy)] pub struct RewriteCtxView<'g> { pub graph:
  &'g Graph, pub entry: NodeId }`
- **What's wrong:** The inline doc-comment itself spells out the hazard:
  "Re-binding the field (`view.graph = &other_graph`) silently redirects the
  view at distance" and "read-only by convention; mutating from external code
  redirects the view's anchor." External code can rebind `graph` to a
  different graph while keeping the old `entry: NodeId`, producing a view
  whose entry is illegal in the new graph. The `Deref<Target=Graph>` impl on
  the view further hides the field from idiomatic readers, so most callers
  never need direct field access at all.
- **Migration cost:** SMALL — the only callers using fields directly are
  inside `pattern` and `opt` (a handful), and `Deref` already makes
  `view.graph.x()` interchangeable with `view.x()`.
- **Proposed shape:** Make both fields `pub(crate)`. Public surface stays at
  `RewriteCtxView::new`, `From<&BuiltFunctionGraph>`, `From<&RewriteCtx>`, and
  `Deref<Target=Graph>`.
- **Call-site impact:** A few `view.graph` reads inside opt passes become
  bare `*view` / `view.x()`; if anyone really needs the underlying borrow,
  add a `pub fn graph(&self) -> &Graph` accessor.

### 4. `pattern::RewriteCtx` — `pub graph: &mut Graph, pub entry: NodeId`

- **Severity:** HIGH
- **Where:** `crates/pattern/src/rewrite.rs:154-157`
- **Type:** `pub struct RewriteCtx<'g> { pub graph: &'g mut Graph, pub entry:
  NodeId }`
- **What's wrong:** Same "rebind from outside" hazard as `RewriteCtxView`,
  but worse because `graph` is `&mut`. Code holding a `&mut RewriteCtx` could
  swap in an unrelated graph (or — once the field is exposed — pin a stale
  `entry` that the current `graph` no longer contains). The struct already
  exposes `Deref<Target=Graph>` + `DerefMut`, so the field's `pub` visibility
  isn't ergonomically necessary.
- **Migration cost:** SMALL — same argument as RewriteCtxView; the
  `as_view`/`for_built`/`new` ctors plus `Deref` cover everything callers
  actually do.
- **Proposed shape:** Tighten both fields to `pub(crate)`.
- **Call-site impact:** `ctx.graph.x()` becomes `ctx.x()` thanks to
  `DerefMut`. The handful of `ctx.entry` reads (4-5 sites in opt + strider)
  swap to a `pub fn entry(&self) -> NodeId` accessor.

### 5. `ir::BuiltFunctionGraph` — five `pub` fields with documented "do not mutate" caveats

- **Severity:** HIGH
- **Where:** `crates/ir/src/function.rs:45-109`
- **Type:** `pub struct BuiltFunctionGraph { pub graph: Graph, pub entry:
  NodeId, pub variables: PrimaryMap<VarId, rsleigh::Vn>, pub call_clobbered:
  Box<[rsleigh::Vn]>, pub ret_val_regs: Box<[rsleigh::Vn]>, pub
  call_other_clobbered: Box<[rsleigh::Vn]>, pub(crate) no_memory_clobber: bool
  }`
- **What's wrong:** Every CC-bearing field carries a "Caution: mutating
  desynchronises... silently breaks Match::get_vn" warning in its docstring.
  The desync is real: `Match::get_vn` (`matcher/match_result.rs:198,239`) does
  positional indexing into these arrays based on `Call`/`CallOther` output
  slot numbers. A `call_clobbered.push(extra)` after build silently shifts
  every existing slot's varnode. Read-only accessors `call_clobbered_regs()`,
  `ret_val_regs_as_slice()`, `call_other_clobbered_regs()`, `variables_map()`
  already exist and are documented as the migration path.
- **Migration cost:** LARGE — `grep` shows ~20+ direct field readers across
  `opt::function_args`, `opt::stack_store::call_args`, `pattern::matcher`, and
  `dot::*`. Most are read-only, so the migration is mostly mechanical:
  swap `.call_clobbered` → `.call_clobbered_regs()`, etc.
- **Proposed shape:** Tighten the five CC-bearing fields to `pub(crate)`.
  Keep `pub graph` / `pub entry` for now (they already deref through
  `Deref<Target=Graph>`, but downstream callers structurally rely on the
  through-path for arena writes; tightening is a separate, larger move).
- **Call-site impact:** ~20 read sites swap to the accessor names. No write
  sites should exist outside `FunctionBuilder::build`.

### 6. `ir::FunctionGraph` — internal partial-state ctor leaks via `pub`

- **Severity:** MED
- **Where:** `crates/ir/src/function.rs:13-23`
- **Type:** `pub struct FunctionGraph { pub graph: Graph, pub entry: NodeId,
  pub entry_control: NodeOutputId, pub entry_memory: NodeOutputId }`
- **What's wrong:** The constructor `new_invalid` (line 30) sets `entry`,
  `entry_control`, `entry_memory` to `NodeId::reserved_value()` /
  `NodeOutputId::reserved_value()` sentinels — a textbook partial-state. The
  doc-comment on `new_invalid` says: "consumers should never observe a partial
  graph" — and indeed `new_invalid` is `pub(crate)`, but the **fields**
  remain `pub`, so any external reader of `FunctionBuilder::body()` (returns
  `&FunctionGraph`) can observe the sentinels before
  `FunctionBuilder::build_entry` runs. The reserved sentinel is detected at
  `cranelift_entity` lookup time via panic, not via type-level distinction.
- **Migration cost:** SMALL — `body()` and `body_mut()` are only used inside
  `FunctionBuilder`'s own impls; one external caller (the strider region
  driver) reaches in to `builder.body().graph.output_definition(...)`.
- **Proposed shape:** `enum FunctionGraph { Invalid, Initialised { graph,
  entry, entry_control, entry_memory } }` — or, simpler, tighten the four
  fields to `pub(crate)` and add accessors that return `Result` until the
  entry has been registered.
- **Call-site impact:** `body().graph.output_definition(...)` becomes a
  fallible accessor; one strider call-site adds `?`.

### 7. `cfg::Cfg::start_addr_to_region_id` — `pub` map with "direct mutation desyncs" caveat

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/mod.rs:62-73`
- **Type:** `pub start_addr_to_region_id: BTreeMap<PcodeInsnAddr, NodeIndex>`
- **What's wrong:** The doc-comment explicitly says: "kept `pub` because
  `cfg/tests/cfg_query.rs` constructs `Cfg` via struct-literal syntax for
  hand-built petgraph fixtures... External readers should still go through
  `region_id_at_start` — the field accessor is the canonical path; direct
  mutation desyncs the index from `graph`." This is a leaky-encapsulation
  pattern justified entirely by test ergonomics. The graph-and-index pair
  must stay coherent; user code that does `cfg.graph.add_node(_)` without
  also updating the map silently breaks `region_id_at_start`.
- **Migration cost:** SMALL — only one test uses struct-literal syntax;
  exposing a `Cfg::from_parts_for_tests` ctor under `#[doc(hidden)]` covers
  the case.
- **Proposed shape:** Tighten the field to `pub(crate)`; expose
  `#[doc(hidden)] pub fn from_parts_for_tests(...)` for the cfg_query test.
- **Call-site impact:** One test rewrites a struct literal as a ctor call.

### 8. `cfg::Region` — fully `pub` fields with cross-field invariant

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/types.rs:216-227`
- **Type:** `pub struct Region { pub start_addr: PcodeInsnAddr, pub insns:
  Vec<RegionInstruction>, pub terminator: RegionTerminator }`
- **What's wrong:** The doc on `insns` says "Empty only when the terminator
  is `Branch` and arose from the single-instruction CondBranch-with-OOB-
  successor fold... Otherwise non-empty." That's a real cross-field
  invariant: `insns.is_empty() ⇒ matches!(terminator, RegionTerminator::Branch)`.
  `Builder::add_region` enforces it at construction time
  (`builder/mod.rs:204-211`), but external code mutating `insns` /
  `terminator` directly can violate it. `Region::contains_addr` already
  branches on the empty case (line 240); a malformed state would silently
  affect that result.
- **Migration cost:** LARGE — `grep` shows the strider lift driver iterates
  `region.insns` directly in many places (`pipeline.rs:366,386`,
  orchestrator helpers, plus tests). Read access is dominant; very few sites
  mutate.
- **Proposed shape:** Tighten the three fields to `pub(crate)` and expose
  read accessors (`insns(&self) -> &[RegionInstruction]` etc.).  Better
  long-term: model the empty-Branch case as a distinct variant
  (`enum RegionBody { Insns(Vec<...>), CondBranchFolded { /* edge only */ } }`)
  to make the empty/Branch correlation unrepresentable, but the migration
  cost is high.
- **Call-site impact:** ~10 read sites swap to `.insns()`; opt/strider tests
  unchanged.

### 9. `target::SleighArch` — `pub` fields with all canonical `()` accessors

- **Severity:** LOW
- **Where:** `crates/target/src/arch.rs:118-130`
- **Type:** `pub struct SleighArch { pub sla_spec, pub pspec, pub endianness,
  pub preset }`
- **What's wrong:** The four `()` accessors `sla_spec()`, `pspec()`,
  `endianness()`, `preset()` (lines 132-154) are present and documented as
  "the migration path that will eventually tighten the field to
  `pub(crate)`." `SleighArch` is `Copy`, and all 13 preset constructors
  produce well-formed instances — the only invariant is structural
  agreement between the four fields (e.g. `aarch64be` paired with
  `Endianness::Big`). External mutation could fabricate `(SLA_SPEC_X86_64,
  Endianness::Big)`, which would silently corrupt every byte-order-sensitive
  decode.
- **Migration cost:** SMALL — `grep` shows 5-6 prod readers; everything
  else uses preset constructors.
- **Proposed shape:** Tighten the four fields to `pub(crate)`. Strider's
  `arch.endianness` becomes `arch.endianness()` etc.
- **Call-site impact:** ~5-6 reads swap to the accessor.

### 10. `target::CallingConvention` — every field private but `&'static [&'static str]` representation

- **Severity:** LOW (intentional)
- **Where:** `crates/target/src/calling_convention/mod.rs:29-93`
- **Type:** `pub struct CallingConvention { … all pub(super) }`
- **What's wrong:** No leaky-encap issue (fields are private). However the
  representation `arg_passing_regs: &'static [&'static str]` is primitive
  obsession in spirit: register names are stringly-typed. The 17 preset
  constructors all pass typo-free names, but a future addition (or the
  `x86_linux_kernel`-style mutation pattern at line 808 that overwrites
  `arg_passing_regs`) is exactly where a typo would slip in. `build()`
  catches it at register-resolution time, so the cost is bounded — but the
  resolution failure surfaces deep inside the call stack rather than at
  type-check time.
- **Migration cost:** LARGE — would require generating a per-arch typed
  register-name enum or moving to `&'static [rsleigh::Vn]`.
- **Proposed shape:** Keep as-is; document the cost. The validating
  `try_from_parts` constructor (line 213) catches structural ABI errors;
  the build path catches name typos. This is a deliberate ergonomics
  tradeoff (single-source-of-truth presets that read like spec text).

### 11. `target::BuiltCallingConventionParts` — fully `pub` fields used as a "validation skip" hatch

- **Severity:** MED
- **Where:** `crates/target/src/calling_convention/mod.rs:127-148`
- **Type:** `pub struct BuiltCallingConventionParts { pub arg_passing_regs,
  pub callee_saved_regs, pub ret_val_regs, … 10 fields all pub }`
- **What's wrong:** This is a builder-bag that's *only* used to feed
  `from_parts_unchecked` (line 165, `#[doc(hidden)]`) and the validating
  `try_from_parts` (line 213). The `unchecked` form's docs explicitly say:
  "Test-only escape hatch. A typo overlapping `arg_passing_regs` with
  `callee_saved_regs` ... silently miscompiles downstream pattern queries."
  Allowing all 10 fields `pub` plus a `#[doc(hidden)]` unchecked ctor leaves
  a path where production code might reach for the unchecked path and skip
  the invariant checks the docstring lists.
- **Migration cost:** SMALL — only test code uses these.
- **Proposed shape:** Make `BuiltCallingConventionParts` non-exhaustive
  (`#[non_exhaustive]`) and keep `try_from_parts` as the only path; deprecate
  / remove `from_parts_unchecked` (the `#[doc(hidden)]` already discourages
  it). Or, if the validation cost is unacceptable in test-only fixtures,
  rename to `BuiltCallingConventionPartsUnchecked` so the unsafety is
  flagged at the type level.
- **Call-site impact:** Test fixtures that build CCs flip from
  `from_parts_unchecked` → `try_from_parts(...).unwrap()` (the validation is
  cheap enough to pay in tests).

### 12. `opt::Kb` — public `ones`/`zeros` fields with disjointness invariant

- **Severity:** HIGH
- **Where:** `crates/opt/src/known_bits/mod.rs:36-50`
- **Type:** `#[derive(Default)] pub struct Kb { pub ones: u64, pub zeros: u64 }`
- **What's wrong:** The doc explicitly calls out: "Both `ones` and `zeros`
  ... must never overlap (`ones & zeros == 0`)" and "External code
  constructing a `Kb` via struct-literal syntax (`Kb { ones: 0xFF, zeros:
  0xFF }`) silently violates it, producing nonsensical analysis results."
  `try_new` enforces the invariant — but it's bypassed by every internal
  struct-literal site in `known_bits/mod.rs` itself (lines 178, 182, 186,
  216, 228, 257, 269, 288, 298, 319, 341, 347, 353, 377). Internal sites
  can argue "the math here keeps the invariant by construction," but the
  field-level `pub` exposure means external callers can construct
  contradictions, and `merge` only catches them when two contradictory
  values combine.
- **Migration cost:** MED — internal sites stay (they're sound by
  construction); external callers (and the public re-export via
  `KnownBitsMap`) lose the struct-literal path.
- **Proposed shape:** Tighten `ones` and `zeros` to `pub(crate)`. Internal
  arithmetic sites in `known_bits/mod.rs` keep working; external callers
  use `try_new`. Add `pub fn ones(&self) -> u64` / `pub fn zeros(&self) ->
  u64` accessors for read paths (currently 2-3 external readers).
- **Call-site impact:** External readers of `kb.ones` / `kb.zeros` swap to
  accessors. The `KnownBitsMap = SecondaryMap<NodeOutputId, Kb>` re-export
  still works because `Default` is preserved.

### 13. `opt::IndirectBranchResolve` — every config field `pub` plus boxed predicate

- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:144-191`
- **Type:** `pub struct IndirectBranchResolve { pub link_register_vn:
  Option<Vn>, pub stack_ptr_vn: Option<Vn>, pub rom: Option<Arc<dyn
  ReadOnlyMemory>>, pub unresolved_anchors: Vec<(AnchorAddr, NodeOutputId)>,
  pub anchor_contexts: HashMap<AnchorAddr, AnchorCallingContext>, pub
  is_tail_call: Box<dyn Fn(...) -> bool> }`
- **What's wrong:** Two issues:
  1. The doc on `unresolved_anchors` says: "Precondition: each anchor must
     appear at most once... a duplicate would re-visit the same anchor, find
     the now-detached placeholder ... and silently no-op — masking the
     duplicate." Public field with a precondition only enforced at the
     producer (the orchestrator) is a contract leak.
  2. `anchor_contexts` is documented as "Defaulting to an empty map preserves
     back-compat with callers that haven't populated this field yet — the
     in-place editors emit a degenerate but well-typed Call/Return in that
     case." Producing a degenerate output for empty input is a partial-state
     pattern.
- **Migration cost:** SMALL — only the strider orchestrator and tests
  populate these fields.
- **Proposed shape:** Add a builder ctor `IndirectBranchResolve::new(...)`
  that takes the full set up front and tightens the fields to `pub(crate)`.
  Validate uniqueness of `unresolved_anchors` keys at construction time.
- **Call-site impact:** Orchestrator + tests swap to the ctor; the boxed
  predicate stays.

### 14. `opt::AnchorAddr` — opaque-by-doc but transparent representation

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:213-255`
- **Type:** `#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)] pub struct
  AnchorAddr { pub(crate) machine_addr: u64, pub(crate) insn_index: u64 }`
- **What's wrong:** The doc says: "Opaque address tag carried alongside each
  anchor — opaque to opt, transparent to strider (which casts it back to its
  `cfg::PcodeInsnAddr`)." The `pub fn machine_addr() -> u64` and `pub fn
  insn_index() -> u64` accessors expose the internal layout, which prevents
  the type from ever growing into a different opaque shape (e.g. a 16-byte
  packed key) without breaking callers. The "opaque payload; opt does not
  interpret it" wording on the accessor docs is a hint that the layout
  shouldn't have leaked.
- **Migration cost:** SMALL — only one external test reads
  `AnchorAddr::from_packed` (`opt/tests/indirect_branch_resolve.rs:43`).
- **Proposed shape:** If the type is genuinely opaque, drop
  `machine_addr()` / `insn_index()` accessors and only keep `from_packed` /
  `parts() -> (u64, u64)` (which are symmetric, so they preserve a pure
  serialisation interface). Add `From<cfg::PcodeInsnAddr>` for
  ergonomics in strider.
- **Call-site impact:** Three test sites change to `addr.parts().0` /
  `.parts().1` (or to `cfg::PcodeInsnAddr::from(addr).machine_addr_u64()`).

### 15. `opt::ResolvedTargets::Multiple(Vec<u64>)` — non-empty invariant on a tuple variant

- **Severity:** MED
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:81-99`
- **Type:** `pub enum ResolvedTargets { LinkRegister, Single(u64),
  Multiple(Vec<u64>) }`
- **What's wrong:** Doc on `Multiple`: "Invariant: the inner `Vec` must be
  non-empty. An empty `Multiple` would silently advertise zero runtime
  targets, making the dispatch site appear unreachable. Use `Self::multiple`
  for the validating constructor that rejects empty input — the existing
  `Multiple(targets)` tuple-construct form is retained for pattern-matching
  and for callers that have already established non-emptiness." Tuple-variant
  pub access bypasses the invariant — a pattern-matching destructure is fine
  (read-only), but `ResolvedTargets::Multiple(Vec::new())` constructs a
  forbidden state. Pattern-matching needs the variant name + payload, but
  Rust enums can't make construction `pub(crate)` while keeping the variant
  publicly destructurable.
- **Migration cost:** SMALL — only the validating ctor `Self::multiple` is
  the production write path; tuple-construct sites should have been
  audited.
- **Proposed shape:** Wrap the `Vec` in a `NonEmptyVec<u64>` newtype (small
  bespoke wrapper around `(u64, Vec<u64>)`) so empty cannot be constructed.
  Variant becomes `Multiple(NonEmptyVec<u64>)`; pattern matching gets the
  same ergonomics.
- **Call-site impact:** `Self::multiple` becomes infallible; tuple-construct
  external sites (rare) get a compile error pointing them at
  `NonEmptyVec::new`.

### 16. `cfg::Options` / `cfg::OptionsBuilder` — partial-state representation choice

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/options.rs:59-95, 162-226`
- **Type:** `pub struct Options { (super) fn_max_size: Option<u64>, (super)
  allow_code_before_start_addr: bool, … }`
- **What's wrong:** This was *fixed* in round 9 — the implicit "ignored when
  bounded" coupling between `fn_max_size` and `allow_code_before_start_addr`
  is now unrepresentable via `Options::function_boundary() -> FunctionBoundary
  { Bounded { max_size }, Unbounded { allow_code_before_start } }`. **However**
  the *storage* still uses the two scalar fields, and the builder's two
  setter methods (`set_function_max_size`, `allow_code_before_start_addr`)
  let users call them in an order that produces a bounded `Options` whose
  `allow_code_before_start_addr` flag is silently ignored. This is partial-
  state at the builder level even though `function_boundary()` recovers a
  total enum.
- **Migration cost:** MED — touches `OptionsBuilder` API.
- **Proposed shape:** Replace the two setter methods with a single
  `set_function_boundary(FunctionBoundary)` (or a pair of named ctors:
  `bounded(max_size)`, `unbounded(allow_code_before_start: bool)`). Migrate
  `Options::fn_max_size` + `allow_code_before_start_addr` storage to a
  single `boundary: FunctionBoundary` field.
- **Call-site impact:** Strider's RunConfig and any test fixture builder
  swap to the new setter; ~5 sites.

### 17. `strider::RunConfig` — partial-state composition + raw `u64` start_addr

- **Severity:** MED
- **Where:** `crates/strider/src/orchestrator.rs:60-106`
- **Type:** `pub struct RunConfig<'a, R> { pub strider, pub start_addr: u64,
  pub sleigh: rsleigh::Sleigh<R>, pub rom, pub fn_max_size, pub
  allow_code_before_start_addr, pub compact, pub per_address_ccs }`
- **What's wrong:** Three issues, listed in priority:
  1. `start_addr: u64` is primitive obsession — every other site in the
     workspace has switched to `MachineInsnAddr`, but RunConfig still takes
     a raw `u64`. Mixing it with `fn_max_size: u64` allows a `(start_addr,
     fn_max_size)` argument swap.
  2. `(fn_max_size: Option<u64>, allow_code_before_start_addr: bool)` is the
     same partial-state pair as `cfg::Options` (finding 16). Users see two
     fields and don't know `allow_code_before_start_addr` is silently
     ignored when `fn_max_size.is_some()`.
  3. Every field is `pub`; no validation or builder. Eight fields with no
     defaults and a generic param give callers many ways to forget a knob.
- **Migration cost:** LARGE for fixing all three; SMALL for any single fix.
- **Proposed shape:**
  - Migrate `start_addr: u64 → MachineInsnAddr` (touches ~3 prod sites).
  - Replace `fn_max_size: Option<u64>` + `allow_code_before_start_addr:
    bool` with `boundary: FunctionBoundary` (the same fix as finding 16).
  - Add a `RunConfigBuilder` so the `RunConfig::new(strider, start_addr,
    sleigh).with_rom(...).with_boundary(...)` shape is the public ctor.
- **Call-site impact:** strider-py adapter, orchestrator tests, the example
  binary (~6 sites total).

### 18. `strider::AnalyzeOptions` — `Option<Vec<rsleigh::Vn>>` partial-state

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:89-116`
- **Type:** `pub struct AnalyzeOptions<'a> { pub all_vns: Option<Vec<Vn>>,
  pub per_address_ccs: &'a HashMap<u64, BuiltCallingConvention> }`
- **What's wrong:** `all_vns` is None or Some<sorted, complete-set Vec>.
  The "must be sorted by `pcode_lift::vn_sort_key`" precondition is
  documented but unenforced; the "must include every varnode any
  instruction in `cfg` references" precondition is also unenforced. The
  "Some" path is exactly the orchestrator's reuse pattern; the "None" path
  is the convenience-default that triggers a fresh scan. This is a
  contractual partial-state.
- **Migration cost:** SMALL — only the orchestrator and tests build it.
- **Proposed shape:** `enum VarnodeSet { Compute, Cached(Vec<Vn>) }` (or a
  newtype `SortedVns(Vec<Vn>)` whose ctor verifies sortedness and
  completeness against a Cfg). The `Cached` variant carries the ordered
  invariant explicitly.
- **Call-site impact:** Two sites — the orchestrator's caching logic and
  `analyze_cfg`/`analyze_cfg_with`.

### 19. `strider::RegionLiftHandles` — every field `pub` with cross-field invariants

- **Severity:** MED
- **Where:** `crates/strider/src/strider/pipeline.rs:21-47`
- **Type:** `pub struct RegionLiftHandles { pub start_addr, pub
  entry_control_state, pub entry_mem_phi, pub entry_control, pub
  entry_memory, pub exit_control, pub exit_memory, pub entry_var_phis, pub
  exit_vn_to_value: Arc<HashMap<Vn, NodeOutputId>> }`
- **What's wrong:** All nine fields `pub`. Multiple cross-field invariants
  exist:
  - `entry_control` is the `output_index 0` of the node identified by
    `entry_control_state`.
  - `entry_memory` is the memory output of `entry_mem_phi`.
  - `exit_vn_to_value` keys must be the same `rsleigh::Vn` set as the
    function's tracked variables.
  External mutation could rewrite `entry_control` to point at any other
  output, breaking the orchestrator's per-iteration `RegionIndex` lookup.
- **Migration cost:** SMALL — only the strider lift driver writes; only the
  orchestrator reads.
- **Proposed shape:** Add a `pub(crate) fn new(...)` ctor that validates the
  cross-field invariants (e.g. checks that `output_definition(entry_control).0
  == entry_control_state`); tighten fields to `pub(crate)`. Add accessors as
  needed.
- **Call-site impact:** Strider's `analyze_cfg_with` builder swaps the
  struct literal for a ctor call; orchestrator's `RegionIndex::from_handles`
  uses the existing field names through accessors.

### 20. `strider::AnalyzeOutcome` — fully-`pub` bag with no invariants spelled

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:56-83`
- **Type:** `pub struct AnalyzeOutcome { pub graph, pub unresolved_branches:
  Vec<(PcodeInsnAddr, Value)>, pub region_handles: Vec<RegionLiftHandles> }`
- **What's wrong:** Every `Value` (= `NodeOutputId`) in
  `unresolved_branches` must reference a node alive in `graph`. Every
  `RegionLiftHandles.entry_*`/`.exit_*` `NodeOutputId` must reference a
  node alive in `graph`. Cross-field referential invariant; `pub` fields
  let consumers swap one without the others. No accessors, just the
  `Display` impl.
- **Migration cost:** SMALL — only the orchestrator's `LoopState` reads;
  only `analyze_cfg_with` writes.
- **Proposed shape:** Tighten fields to `pub(crate)` and add accessors.
  This is a low-risk find — the only consumer is internal.
- **Call-site impact:** ~3 read sites in orchestrator change to accessors.

### 21. `opt::OptimizerPipeline` — all fields private but `optimizer_names` is a debug-only leak

- **Severity:** LOW
- **Where:** `crates/opt/src/pipeline.rs:170-181`
- **Type:** `pub struct OptimizerPipeline { optimizers: Vec<Box<dyn>>,
  optimizer_names: Vec<&'static str>, post_passes: Vec<Box<dyn>> }`
- **What's wrong:** `optimizer_names` is captured via
  `std::any::type_name::<O>()` at registration time and exposed via
  `optimizer_names()` "so the fixed-point orchestrator's pipeline-shape tests
  can confirm the stable-vs-destructive subset partition without inspecting
  trait objects." This is an implementation-detail leak: type-name strings
  are documented as "implementation-defined but stable enough for substring
  matching." The orchestrator's pipeline-shape tests are reaching across an
  abstraction boundary.
- **Migration cost:** MED — would require giving each pass a stable
  identifier (`fn name(&self) -> &'static str` on the Optimizer trait, or
  a separate `OptimizerKind` enum).
- **Proposed shape:** Add `fn pass_id(&self) -> &'static str` to the
  `Optimizer` trait (returning a stable, declared-by-the-pass name) and
  drop the `type_name` capture. `optimizer_names()` then returns the
  declared ids.
- **Call-site impact:** Each opt pass adds one tiny method; the
  orchestrator's pipeline-shape tests keep working with stable strings.

### 22. `cfg::ProcessInsnRes` — boolean enum (low-value)

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:48-55`
- **Type:** `pub enum ProcessInsnRes { FinishedProcessing,
  DidntFinishProcessing }`
- **What's wrong:** This is a 2-variant enum used only inside the cfg
  builder. The `pub` visibility leaks to crate consumers but no external
  code uses it. Functionally equivalent to a `bool`, with the upside of
  named variants. Not really a hazard — just a classic "should this be
  `pub(crate)` instead?" question.
- **Migration cost:** SMALL.
- **Proposed shape:** Tighten to `pub(crate)`.
- **Call-site impact:** None outside `cfg`.

### 23. `pattern::Binding` — `pub` fields with cross-field invariant

- **Severity:** MED
- **Where:** `crates/pattern/src/matcher/bindings.rs:17-27`
- **Type:** `pub struct Binding { pub node: NodeId, pub output:
  Option<NodeOutputId> }`
- **What's wrong:** When `output` is `Some`, the contract is
  "graph.output_definition(output).0 == node". `Binding::new` doesn't
  validate. External code constructing a `Binding { node: n, output:
  Some(o) }` where `o` belongs to a different node silently miscompiles
  every downstream `Match::get_vn`, `get_uint`, etc. (those use `output` to
  find the node and assume the relationship).
- **Migration cost:** SMALL — `Binding` is only constructed inside
  `pattern` (matcher/bindings.rs:24, 81-89) and there `node` and `output`
  are derived from a single match site.
- **Proposed shape:** Tighten the fields to `pub(crate)`. Add accessors.
  `Binding::new(node, output)` keeps its current signature; production
  callers within `pattern` do their construction via the matcher.
- **Call-site impact:** Bindings reads in `Match` already go through the
  module; opaque to external code.

### 24. `ir::WideConstStorage` — `enum { U256([u64;4]), U512([u64;8]) }` is fine, but byte_size() returns u32 implicitly

- **Severity:** LOW
- **Where:** `crates/ir/src/wide_const.rs:39-44`
- **Type:** `pub enum WideConstStorage { U256([u64; 4]), U512([u64; 8]) }`
- **What's wrong:** This type is well-designed (variants encode the width
  invariant; ctors validate length). Tuple variants are `pub` so external
  destructure is fine and round-trips through `to_le_bytes` /
  `from_le_bytes_*` are total. Only nit: `byte_size() -> usize` returns a
  value that's always 32 or 64; expressing as a `BitWidth` enum would close
  the gap, but the value is already used as a slice index downstream so
  `usize` is justified.
- **Migration cost:** N/A.
- **Proposed shape:** Keep as-is.

### 25. `cfg::Builder` — `pub(super)` fields with `endianness` / `preset` defaults

- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/builder/mod.rs:51-85`
- **Type:** `pub struct Builder<R> { pub(super) sleigh, … pub(super)
  endianness: target::Endianness, pub(super) preset: target::ArchPreset,
  … }`
- **What's wrong:** This isn't a leaky-encap on the `pub` field axis (all
  `pub(super)`), but a partial-state issue. The doc on `endianness`/`preset`
  is explicit: "Defaults to `target::ArchPreset::X86_64` when constructed
  via `Self::new` / `Self::with_endianness`; callers that analyse non-x86
  binaries should use `Self::for_arch`..." So `Builder::new` and
  `with_endianness` produce a Builder whose `preset` field is silently
  wrong on non-x86 binaries — the kind of partial state where a "default"
  is dangerous unless you know about the deprecated path.
  This was *fixed* by deprecating the two ctors (`#[deprecated]` on
  `new` and `with_endianness`), but the type still represents "constructed
  via the deprecated path" and "constructed via `for_arch`" identically;
  there's no compile-time signal at the *type* level. The deprecation
  warning is the only guard.
- **Migration cost:** LARGE — would require splitting `Builder` into two
  types (`BuilderX86Default` and `BuilderForArch`), or deleting the
  deprecated constructors entirely (still some external test usage).
- **Proposed shape:** Once external callers have migrated, delete `new` and
  `with_endianness`. The deprecation already sets up the migration path.
  No type-level fix needed if the deprecated path is removed.

### 26. `target::Endianness` — clean enum (no issue)

- **Severity:** N/A
- **Where:** `crates/target/src/arch.rs:1-8`
- **Type:** `pub enum Endianness { Little, Big }`
- **What's wrong:** Nothing. This is a textbook good design — a 2-variant
  enum that replaces the `bool is_little_endian` pattern (see
  `reader::ElfFileMemReader` line 257 which still has the older `bool
  is_little_endian: bool` form, finding 27 below).

### 27. `reader::ElfFileMemReader` — `is_little_endian: bool` should be `Endianness`

- **Severity:** LOW
- **Where:** `crates/reader/src/elf.rs:255-258`
- **Type:** `pub struct ElfFileMemReader { lookup:
  MemRegionsLookupTable, is_little_endian: bool }`
- **What's wrong:** Primitive obsession. `target::Endianness` exists and
  is widely used elsewhere; `is_little_endian: bool` is an older
  representation that this reader hasn't been migrated to. Field is
  private so it's pure code-smell, not a hazard.
- **Migration cost:** SMALL — internal only.
- **Proposed shape:** Replace with `endianness: target::Endianness` and use
  `endianness.read_u64(buf)` instead of branching on the bool.
- **Call-site impact:** Internal to `elf.rs`; no public surface change.

### 28. `reader::RelocationStats` — eight `pub` fields, no construction invariants

- **Severity:** LOW
- **Where:** `crates/reader/src/elf.rs:368-411`
- **Type:** `#[derive(Default)] pub struct RelocationStats { pub seen, pub
  applied, pub skipped_unresolved_target, pub skipped_malformed_target, pub
  skipped_unsupported_kind, pub skipped_no_region, pub unsupported_r_types:
  Vec<u32>, pub autoload_section_parse_failures }`
- **What's wrong:** No mutation hazard since the struct is data-out only,
  but invariant `applied + skipped_* + skipped_no_region <= seen` is
  unenforced. External code constructing a `RelocationStats` with
  inconsistent counts is a degenerate state.
- **Migration cost:** N/A — pure-data return type, no real fix needed.
- **Proposed shape:** Keep as-is. At most: tighten fields to `pub(crate)`
  with accessors so the doc-noted "non-zero count means malformed ELF" can
  be enforced via a `is_clean(&self) -> bool` accessor that captures
  intent.

## High-priority follow-ups (recommended order)

The 6 highest-leverage fixes that pay back in proportion to their cost:

1. **Finding 5** (`BuiltFunctionGraph` CC fields) — HIGH severity, LARGE
   cost but the doc-comment migration is mechanical and the silent
   miscompile risk via `Match::get_vn` is real.
2. **Findings 3 & 4** (`RewriteCtxView` / `RewriteCtx`) — HIGH severity,
   SMALL cost; rebinding hazard is one mistyped struct-literal away.
3. **Finding 12** (`Kb { ones, zeros }`) — HIGH severity, MED cost; the
   `Kb { ones: 0xff, zeros: 0xff }` mistake produces wrong analysis silently.
4. **Findings 1 & 2** (`PcodeInsnAddr` / `MachineInsnAddr`) — MED severity,
   SMALL cost; the migration tooling already exists and the `()` accessors
   are documented as "the canonical path."
5. **Findings 16 & 17** (`Options`/`OptionsBuilder` partial state +
   `RunConfig`'s `start_addr: u64`) — MED severity, LARGE cost; these are
   the main user-facing API and the bound-vs-unbound + raw-address pair
   has bitten contributors before.
6. **Finding 11** (`BuiltCallingConventionParts` + `from_parts_unchecked`)
   — MED severity, SMALL cost; cleanly removable once the test fixtures
   migrate.

Findings 6 (FunctionGraph), 7 (Cfg::start_addr_to_region_id), 19
(RegionLiftHandles), 13 (IndirectBranchResolve), and 23 (Binding) round out
the "encapsulation hardening" tier — none of them are urgent but each is a
small change that removes a class of silent miscompile risk.

## Notes on what I deliberately did *not* flag

- **`NodeOutputType` / `NodeOutputKind` / `NodeKind`** — these are the IR's
  closed-set discriminators. They're large enums but the design pattern
  (each variant carries its own minimal payload, side-tables for ancillary
  data on `Graph`) is correct.
- **`Capture` (u32 newtype)** — the `from_id`/`id` round-trip is documented
  as a deliberate cross-FFI escape hatch, not a hazard.
- **`MemRegion`** — fields are private, ctor validates, accessors are
  total. Textbook good encapsulation.
- **`Bindings` / `BindingsMark`** — `mark`/`restore` is `pub(crate)`, so
  the journal is properly hidden.
- **`CallOtherAbi` / `CallOtherClass`** — pure-data static lookup; the
  `&'static [&'static str]` representation is a deliberate ergonomics
  tradeoff (single-source-of-truth name table read at lift time).
- **`SleighArch::probe_regs`** — error-returning, validates Sleigh
  compatibility before producing a `SleighRegs`. Sound.

The bulk of the "leaky-encapsulation" findings are the round-9-and-earlier
deferred field-tightening migrations: each was acknowledged in a doc-
comment with a "tighten to `pub(crate)` later" plan, but the migration has
not yet been run. Most of those are SMALL cost and worth picking up.
