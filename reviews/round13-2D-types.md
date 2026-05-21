# Round 13 — Type-design audit (2D)

Scope: every `pub struct` / `pub enum` / `pub trait` across
`crates/*/src/`. Round-12 fixes verified as landed (and **not**
re-flagged): R12-T-N, R12-T-P, R12-T-C, R12-T-A, R12-T-H, R12-T-G,
R12-T-Q.

8 R12 deferred items confirmed still applicable; 10 new findings —
most LOW (cosmetic), two MED (active cross-field-invariant risk).

---

### R13-T-1. `ResolvedTargets::Multiple(Vec<u64>)` still tuple-constructible
- **Severity:** MED
- **Where:** crates/opt/src/indirect_branch_resolve/mod.rs:75-112
- **Status:** carries over from R12-T-F
- **What's wrong:** `ResolvedTargets::multiple(targets) -> Option<Self>`
  documents a non-empty invariant ("empty `Multiple` would silently
  advertise zero runtime targets, making the dispatch site appear
  unreachable"). The variant `Multiple(Vec<u64>)` is still tuple-public,
  so any external caller bypasses the gate via
  `ResolvedTargets::Multiple(Vec::new())`. Today's five call sites are
  safe by construction; the invariant is enforced by convention only.
- **Migration cost estimate:** MED
- **Proposed shape:** `#[non_exhaustive]` on the variant + a
  `targets(&self) -> Option<&[u64]>` accessor; external construction
  now must go through `Self::multiple`. Pattern matches inside the
  crate retain field syntax.

---

### R13-T-2. `BuiltCallingConventionParts` not `#[non_exhaustive]`
- **Severity:** LOW
- **Where:** crates/target/src/calling_convention/mod.rs:126-148
- **Status:** carries over from R12-T-E
- **What's wrong:** 10-field public struct-literal bag fed to the
  validating `try_from_parts`. Adding a new field is breaking for every
  external struct-literal caller. The struct is purely an argument bag
  — exactly what `#[non_exhaustive]` is for.
- **Migration cost estimate:** SMALL
- **Proposed shape:** `#[non_exhaustive]` + a `new(required_fields…) ->
  Self` ctor plus `with_*` setters for optional fields
  (`ret_val_regs_float`, `syscall_number_vn`, `no_memory_clobber`).

---

### R13-T-3. `cfg::Region` cross-field invariant on `pub` fields
- **Severity:** MED
- **Where:** crates/cfg/src/cfg/types.rs:246-274
- **Status:** carries over from R12-T-B
- **What's wrong:** `Region { pub start_addr, pub insns, pub
  terminator }` carries the documented invariant "`insns` is empty
  only when `terminator == Branch`" (enforced inside
  `Builder::add_region`). External `Region { insns: vec![], terminator:
  Return }` skips the check. `contains_addr` silently fans to the
  wrong arm for illegal field combinations. Test call sites
  (`tests/builder_add_region.rs:28`, `tests/region.rs:55`, the
  `make_region` helper in `tests/common/synthetic.rs:110-120`) account
  for the 52 sites round-12 flagged.
- **Migration cost estimate:** MED (mechanical rewrite of 52 test sites)
- **Proposed shape:** fields → `pub(crate)` + `Region::try_new(start_addr,
  insns, terminator)` validating ctor + `#[doc(hidden)] for_test`
  helper analogous to `Cfg::from_parts_for_tests`.

---

### R13-T-4. `ir::FunctionGraph` partial-state public-field bag
- **Severity:** LOW
- **Where:** crates/ir/src/function.rs:13-38
- **Status:** carries over from R12-T-D
- **What's wrong:** `FunctionGraph { pub graph, pub entry,
  pub entry_control, pub entry_memory }` has a `pub(crate)
  new_invalid` ctor returning reserved-sentinel ids ("not part of the
  public surface because consumers should never observe a partial
  graph"). But `pub` fields let external callers construct an
  invalid one anyway. No production caller does so today.
- **Migration cost estimate:** SMALL
- **Proposed shape:** four fields → `pub(crate)` + four read accessors
  mirroring the `BuiltFunctionGraph` strategy already landed via R12.

---

### R13-T-5. `AnalyzeOptions::all_vns: Option<Vec<rsleigh::Vn>>` partial state
- **Severity:** LOW
- **Where:** crates/strider/src/strider/pipeline.rs:89-116
- **Status:** carries over from R12-T-I
- **What's wrong:** the `Some(…)` arm requires "sorted by
  `pcode_lift::vn_sort_key` and includes every varnode any insn in
  `cfg` references". `pub` field permits misordered/under-tracked
  `Some(…)` silently. One real caller (`orchestrator.rs:983`).
- **Migration cost estimate:** SMALL-MED
- **Proposed shape:** `AnalyzeOptions::with_all_vns(impl IntoIterator<Item=Vn>)`
  setter that sorts internally, or an `AllVns` newtype with a
  validating ctor.

---

### R13-T-6. `RegionLiftHandles` + `AnalyzeOutcome` public-field consistency
- **Severity:** LOW
- **Where:** crates/strider/src/strider/pipeline.rs:21-72
- **Status:** carries over from R12-T-J + R12-T-K
- **What's wrong:** both structs are all-pub bags whose entries point
  into the same `Graph` produced by the same `FunctionBuilder` run.
  External rebinding of any one `NodeId`/`NodeOutputId` to a different
  graph silently corrupts the orchestrator's per-iteration index.
  Production callers consume these only via `analyze_cfg`; the public
  fields exist for the orchestrator's field-move.
- **Migration cost estimate:** MED
- **Proposed shape:** all fields → `pub(crate)`; expose the
  orchestrator-relevant subset (`exit_control`, `exit_vn_to_value`,
  `entry_var_phis`) via accessors.

---

### R13-T-7. `OptimizerPipeline::optimizer_names` captures `std::any::type_name`
- **Severity:** LOW
- **Where:** crates/opt/src/pipeline.rs:187-250
- **Status:** carries over from R12-T-L
- **What's wrong:** `type_name::<O>()` returns an implementation-defined
  string. Tests rely on substring matches like `"RedundantPhis"` —
  robust to renames but not to a rustc upgrade that reformats paths.
- **Migration cost estimate:** SMALL
- **Proposed shape:** add `fn name(&self) -> &'static str` to
  `OptimizerRaw` (defaulted to `type_name::<Self>()`); populate
  `optimizer_names` from `opt.name()`.

---

### R13-T-8. `ElfFileMemReader::is_little_endian: bool` should be `Endianness`
- **Severity:** LOW
- **Where:** crates/reader/src/elf.rs:255-258, 273-279, 309-343
- **Status:** carries over from R12-T-O
- **What's wrong:** workspace already has `target::Endianness` with
  `read_u64` / `read_u32` / `read_u16` helpers; this is the only
  bool-flavoured endianness left.
- **Migration cost estimate:** SMALL
- **Proposed shape:** swap `is_little_endian: bool` for `endianness:
  target::Endianness`; rewrite read body using `Endianness::read_u64`.

---

### R13-T-9. `ValidationErrors(pub Vec<ValidationError>)` exposes inner Vec mutably
- **Severity:** LOW
- **Where:** crates/ir/src/validate/mod.rs:152
- **Status:** new this round
- **What's wrong:** `pub struct ValidationErrors(pub Vec<ValidationError>);`
  — outside callers can append/remove errors after the validator
  produced them, breaking the "this is the validator's report" contract.
  Callers grep show only `.0.is_empty()` and `.0.iter()` patterns.
- **Migration cost estimate:** SMALL
- **Proposed shape:** `pub(crate)` inner + `errors(&self) ->
  &[ValidationError]` + `into_inner(self) -> Vec<…>` + `IntoIterator`.

---

### R13-T-10. `LoadReadOnly<M>(pub M)` exposes the ROM
- **Severity:** LOW
- **Where:** crates/opt/src/load_readonly/mod.rs:47
- **Status:** new this round
- **What's wrong:** `pub struct LoadReadOnly<M>(pub M);` — newtype
  wrapper carrying a `ReadOnlyMemory` impl. Public inner field admits
  post-construction rom swap (and via boxed-trait-object,
  `pipeline.optimizers[i].0`). No invariant per se, but pub-field is
  gratuitous when the type is a newtype.
- **Migration cost estimate:** SMALL
- **Proposed shape:** `pub struct LoadReadOnly<M>(pub(crate) M);` +
  `new(rom: M)` + `rom(&self) -> &M`.

---

### R13-T-11. Convention-aware opt passes — all-pub fields with cross-field invariant
- **Severity:** LOW
- **Where:** crates/opt/src/stack_load_forward/mod.rs:29-36;
  crates/opt/src/stack_store/detect.rs:89-92;
  crates/opt/src/stack_store/call_args.rs:271-279;
  crates/opt/src/function_args/mod.rs:50-61
- **Status:** new this round
- **What's wrong:** every pass exposes the CC-derived data
  (`stack_ptr_vn`, `endianness`, `stack_arg_offsets`,
  `arg_passing_regs`) as `pub` fields. Each has a `from_convention`
  ctor that derives them coherently — the fields are
  mutually-consistent-by-construction. Direct field rebind can pair
  `stack_ptr_vn` with the wrong `endianness`; in `StackLoadForward`
  this inverts the bit-shift formula for partial-overlap reads. No
  external reader of these fields exists today.
- **Migration cost estimate:** SMALL
- **Proposed shape:** fields → `pub(crate)` on all four passes;
  retain `new` / `from_convention` ctors; add accessor methods.

---

### R13-T-12. `Kb` invariant — fix already complete
- **Severity:** LOW (info only)
- **Where:** crates/opt/src/known_bits/mod.rs:42-93
- **Status:** verifying-fix-complete
- **What's wrong:** R12 already tightened `ones` and `zeros` to
  `pub(crate)` and added `Kb::try_new` validating the `ones & zeros == 0`
  invariant. Accessors `ones()` / `zeros()` are public. No remaining
  gap — flagged here purely to confirm coverage.

---

### R13-T-13. `AnchorCallingContext` partial-state via `Default`
- **Severity:** MED
- **Where:** crates/opt/src/indirect_branch_resolve/mod.rs:119-136;
  callsite at crates/strider/src/orchestrator.rs:790
- **Status:** new this round
- **What's wrong:** orchestrator does `ctx =
  AnchorCallingContext::default()` (three empty Vecs) then pushes
  fields in three independent loops. A partial state where two of the
  three are filled and one is empty silently produces a Call node
  with the wrong number of inputs/outputs. The struct does not
  enforce "you must have populated all three before consuming me";
  the contract lives only in the orchestrator's discipline.
- **Migration cost estimate:** MED
- **Proposed shape:** introduce `AnchorCallingContextBuilder` with
  `push_arg_output` / `push_clobbered_kind` / `push_ret_val_output`
  + `build(self) -> AnchorCallingContext`. Final struct's fields
  become `pub(crate)` with read accessors. The orchestrator's
  three-loop accumulation maps one-to-one onto the builder.

---

### R13-T-14. `UnknownCallOtherError { pub name: String }` — typed-error pub field
- **Severity:** LOW
- **Where:** crates/ir/src/error.rs:22-24
- **Status:** new this round
- **What's wrong:** typed error struct with `pub name: String`.
  Downcast-and-mutate could alter the message the user already
  received. Cosmetic but eliminable.
- **Migration cost estimate:** SMALL
- **Proposed shape:** `pub(crate) name: String` + `pub fn name(&self)
  -> &str`.

---

### R13-T-15. `UnresolvedIndirectBranch { pub addr: PcodeInsnAddr }` same pattern
- **Severity:** LOW
- **Where:** crates/strider/src/errors.rs:39-41
- **Status:** new this round
- **What's wrong:** mirrors R13-T-14; even more cosmetic because
  `PcodeInsnAddr` is `Copy`.
- **Migration cost estimate:** SMALL
- **Proposed shape:** `pub(crate) addr: PcodeInsnAddr` + `addr(&self)
  -> PcodeInsnAddr`.

---

### R13-T-16. `pattern::NotBuildable(pub &'static str)` / `MissingBinding(pub &'static str)`
- **Severity:** LOW
- **Where:** crates/pattern/src/error.rs:28, 36
- **Status:** new this round
- **What's wrong:** typed error newtypes with `pub` `&'static str`.
  Same pattern as R13-T-14/15. Tests downcast by type, not by inner
  string content, so either an accessor or full unit-struct shape
  works.
- **Migration cost estimate:** SMALL
- **Proposed shape:** `pub(crate)` inner + `kind(&self) -> &'static
  str` accessor.

---

### R13-T-17. `IfRegionState` two-`Option` partial state
- **Severity:** LOW
- **Where:** crates/cfg/src/cfg/query.rs:53-58
- **Status:** new this round
- **What's wrong:** `IfRegionState { pub if_true_region:
  Option<NodeIndex>, pub if_false_region: Option<NodeIndex> }` — four
  combinations of (Some/None, Some/None) all carry distinct semantics.
  A sum type would express intent more directly. Pub fields also
  let consumers mutate after `region_if(…)` returned.
- **Migration cost estimate:** MED if rewritten as a sum type;
  SMALL if kept as storage shape.
- **Proposed shape:** keep storage shape; tighten to `pub(crate)` +
  add `true_region(&self) -> Option<NodeIndex>` /
  `false_region(&self) -> Option<NodeIndex>`. The sum-type rewrite
  is overkill for a query result.

---

### R13-T-18. `ValueLifter` (`pub builder`, `pub sleigh`, `pub endianness`)
- **Severity:** LOW
- **Where:** crates/pcode-lift/src/lib.rs:46-56
- **Status:** new this round
- **What's wrong:** all three fields are `pub`. The constructor takes
  them together, implicitly committing to "the endianness matches the
  arch whose Sleigh spec the `sleigh` was built from, and the builder
  was constructed against a CC compatible with the same arch's
  register table." Direct field rebind would corrupt the
  register-aliasing bit-shift formulas in `vn_io.rs` (which branch on
  `self.endianness`).
- **Migration cost estimate:** SMALL
- **Proposed shape:** all three fields → `pub(crate)`; expose
  `builder_mut()` / `sleigh()` / `endianness()` accessors. The `new`
  ctor stays the only construction path.

---

## Summary

- **R12 deferred items still standing (8):** R13-T-1 (was R12-T-F),
  R13-T-2 (R12-T-E), R13-T-3 (R12-T-B), R13-T-4 (R12-T-D),
  R13-T-5 (R12-T-I), R13-T-6 (R12-T-J + R12-T-K), R13-T-7 (R12-T-L),
  R13-T-8 (R12-T-O).
- **New this round (10):** R13-T-9 through R13-T-18.
- **Severity distribution:** 2 MED (R13-T-1, R13-T-3 → invariant
  violations enabled by tuple- / struct-literal construction;
  R13-T-13 = MED for partial-state risk), 16 LOW.
- **Recommended priority:**
  - MED-first: R13-T-1 + R13-T-3 + R13-T-13 (all guard active
    cross-field invariants that today's discipline carries but the
    type does not).
  - Cluster fix: R13-T-4 + R13-T-11 + R13-T-18 — same shape
    (`pub` → `pub(crate)` + accessors); can ship as one PR.
  - Forward-compat: R13-T-2 (`#[non_exhaustive]`) + R13-T-7 (Optimizer
    `name()` method).
  - Cosmetic cleanup last: R13-T-9, R13-T-10, R13-T-14, R13-T-15,
    R13-T-16, R13-T-17.
- **File written:** `/mnt/c/Users/mikeg/Documents/strider/reviews/round13-2D-types.md`
