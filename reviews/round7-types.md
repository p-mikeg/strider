# Round 7 — Public Type Design Audit

Scope: end-user / pattern-author public surfaces across `pattern`, `ir`,
`target`, `strider`, `opt`, `reader`, `cfg`. Each rating is on a 1–5 scale
(see prompt definitions). Citations reference current `feature/ai` (HEAD).

---

## 1. `pattern::{Pat, Capture, Match, Matcher}`

Files: `crates/pattern/src/pat/mod.rs`, `crates/pattern/src/var.rs`,
`crates/pattern/src/matcher/{mod,match_result}.rs`,
`crates/pattern/src/lib.rs`.

### Invariants identified
- `Pat` wraps an `Arc<dyn Pattern>` (`crates/pattern/src/pat/mod.rs:90`).
  Construction is gated through crate-internal builders.
- `Capture` is a globally-unique `u32` allocated from a process-wide
  atomic counter (`crates/pattern/src/var.rs:17-43`).
- `Match` is logically immutable post-construction (only `Match::new_for_test`
  and `Matcher` produce them; `match_result.rs:32`).
- `Matcher<'g>` holds `OnceCell` lazy indices behind `&self`
  (`matcher/mod.rs:73-97`) — borrow-checker-enforced staleness.

### Ratings
- **Encapsulation: 4/5** — `Pat`'s inner `DynPat` is `pub(crate)`
  (`pat/mod.rs:95-102`); the only public state on `Capture` is the new
  constructor. `Match`'s fields are `pub(super)`. Good.
  Concern: `Matcher::options_for_test` (`matcher/mod.rs:189`) is `pub` even
  though the name says "for_test" — escapes intent into the public surface.
- **Invariant Expression: 4/5** — the unified `Capture` (replacing the
  prior typed `Var`/`NodeVar` split, see `var.rs` doc comment) cleanly
  expresses "captures bind a node-id and *optionally* an output". The
  `node`/`output` accessors in `Match` (`match_result.rs:46-57`) make
  the value vs. control-flow split explicit. Concern: nothing in the
  type prevents two `Match` objects from one matcher from being mixed
  with bindings from a different matcher's run; identity is purely by
  `Capture` integer.
- **Usefulness: 5/5** — typed extractors (`get_int`/`get_uint`/`get_bool`/
  `get_vn`/`get_*_op`/`stack_offset`/`asm_fingerprint`,
  `match_result.rs:60-305`) eliminate downstream `match graph.node_kind(...)`
  noise at every call-site. `find_all_requirements` (`matcher/mod.rs:435`)
  has a clear contract.
- **Invariant Enforcement: 4/5** — `Capture::new()` enforces global
  uniqueness; `Pat` cannot be mutated post-construction (Arc + `pub(crate)`
  field). The `find_all_requirements` post-condition that all shared
  captures bind the same node *across* the tuple is enforced via
  `prefix_agrees` (`matcher/mod.rs:676`). Concern: a `Capture` allocated
  in one process region can be silently reused against a `Match` from
  another graph — no graph-identity binding. Acceptable for usability.

### Concrete improvements
- Rename `Matcher::options_for_test` → `Matcher::options` and document
  that it returns the active config (it's already `pub` and documented
  for both diagnostics and tests; the misleading suffix should go).
- Consider tagging `Capture` with a `PhantomData<&'matcher ()>` *only*
  if the cross-matcher misuse becomes a real footgun (currently low
  priority — the `u32` design is what enables `find_all_requirements`'s
  cross-pattern join via shared captures).

---

## 2. `ir::{Graph, NodeOutputKind, NodeKind, NodeOutputType}`

Files: `crates/ir/src/graph/mod.rs`, `crates/ir/src/node/{kind,output_kind,output_type}.rs`.

### Invariants identified
- `Graph` carries multiple `SecondaryMap` side-tables keyed by `NodeId`
  (`graph/mod.rs:39-118`): `stack_phi_offsets`, `call_other_names`,
  `asm_fingerprints`, `call_clobbered_overrides`. Each has documented
  per-node defaults.
- The asm-fingerprint side-table is governed by a "superset-only"
  contract (`graph/mod.rs:78-99`).
- `NodeOutputType` variants are ordered to match an external `TYPE_INFO`
  lookup table (`node/output_type.rs:50-64`).
- `NodeKind` is a closed sum; `is_cacheable` partitions kinds into
  cache-eligible vs. identity-bearing (`node/kind.rs:253-272`).

### Ratings
- **Encapsulation: 3/5** — `Graph`'s arena fields (`nodes`, `outputs`,
  `inputs`, `output_pool`, `input_pool`, `node_to_id`) are all
  `pub(crate)` not private. This is currently sound because the only
  cross-crate writers go through `create_node` and friends, but the
  side-tables are also `pub(crate)` (`graph/mod.rs:61, 77, 99, 117`)
  with read-only access via accessors. Good restriction. Concern:
  `BuiltFunctionGraph.graph` is `pub` (`function.rs:46`) — a downstream
  caller can `&mut graph.nodes` if they have `&mut BuiltFunctionGraph`.
- **Invariant Expression: 2/5** — **`NodeOutputType::info` indexes
  `TYPE_INFO[self as usize]`** (`node/output_type.rs:69`). The comment
  at `node/output_type.rs:50-51` claims a test
  `type_info_table_matches_variants` enforces variant-to-row alignment;
  searching the workspace turns up no such test (also flagged in
  `reviews/round7-ir.md:14`). Adding a new variant out-of-order silently
  misindexes every hot type predicate (`is_bool`, `is_integer`,
  `byte_size`). The comment is false advertising.
- **Usefulness: 4/5** — `NodeKind` and `NodeOutputKind` are clean,
  exhaustive enums; `NodeOutputKind::is_value`/`as_value`/
  `as_value_or_err` (`output_kind.rs:23-66`) are the right idiom.
  `NodeOutputType` predicates are useful but compromised by the
  `as usize` issue.
- **Invariant Enforcement: 3/5** — `Graph::create_node` mediates dedup
  + side-table writes; there is no public "raw write to side-table"
  surface. Asm-fingerprint helpers (`extend_asm_fingerprint`,
  `extend_asm_fingerprint_from`) are public but additive-only by
  contract. Concern: nothing structurally prevents a caller from
  shrinking a fingerprint via `set_asm_fingerprint(id, vec![])`. The
  superset contract is enforced only by docs.

### Concrete improvements
- **HIGH**: Replace `&TYPE_INFO[self as usize]` with a `match self`
  arm in `output_type.rs:67-70`, OR add a real `const _: () = assert!`
  block pinning each variant's discriminant. Either eliminates the
  invisible fragility. (Cited and re-flagged from `round7-ir.md`.)
- Make `BuiltFunctionGraph.graph` field private and expose `&Graph` /
  `&mut Graph` accessors. The `pub` field is the same kind of contract
  leak as `BuiltFunctionGraph::from_graph_and_entry` (see §5).
- Replace `set_asm_fingerprint(id, Vec<u64>)` with `merge_asm_fingerprint`
  (union-only) and downgrade `set_asm_fingerprint` to `pub(crate)` for
  the rare lift-time initial-population callsite.

---

## 3. `ir::FunctionBuilder`

File: `crates/ir/src/builder/mod.rs`.

### Invariants identified
- Tracked variables are deduplicated to "largest containing register"
  in the same fixed-offset space (`builder/mod.rs:284-299`).
- Argument-passing and ret-val regs are upgraded to the closest tracked
  variable in either direction (`builder/mod.rs:344-351`).
- `lift_addr: Option<u64>` toggles per-node fingerprint attribution
  (`builder/mod.rs:137`, `set_lift_addr` at 376).
- `build()` invokes `validate::validate` before returning
  (`builder/mod.rs:531`).

### Ratings
- **Encapsulation: 3/5** — most fields are `pub(crate)`, but
  `body_mut()` returns `&mut FunctionGraph` (`builder/mod.rs:148`) and
  `graph_mut()` returns `&mut Graph` (`builder/mod.rs:166`). Both let
  callers invalidate the SSA-style per-region variable map without the
  builder noticing. Documented as "primary entry point for in-place
  mutation"; the mutation is ungated.
- **Invariant Expression: 2/5** — the `lift_addr` funnel is purely
  convention: callers must remember to `set_lift_addr(Some(addr))`
  before each pcode insn and `set_lift_addr(None)` after. Forgetting
  to clear it attributes structural / region-setup nodes to the last
  insn's address; forgetting to set it leaves the fingerprint empty
  (silent loss of attribution). Layer-C validation catches the
  empty-fingerprint case only when explicitly opted-in via
  `validate_with_options(check_asm_fingerprints: true)`.
- **Usefulness: 4/5** — the `new` / `empty` / `new_raw` triple is a
  clean factoring (the convention-aware path vs. the test-friendly
  raw path). `vn_of_var` / `var_of_vn` (`builder/mod.rs:484-493`) are
  exactly the inverse pair callers need.
- **Invariant Enforcement: 3/5** — `build()` validates; that's the
  one strong guard. Mid-construction the builder's invariants are
  doc-only.

### Concrete improvements
- Replace the bare `set_lift_addr(Option<u64>)` with a scope guard:
  `fn lift_at<R>(&mut self, addr: u64, f: impl FnOnce(&mut Self) -> R) -> R`.
  Drop-time clears `lift_addr`. The strider per-region driver becomes
  `builder.lift_at(machine_addr, |b| b.process_insn(...))`. Forgetting
  to clear becomes structurally impossible.
- Hide `body_mut` / `graph_mut` from outside `pub(crate)` use (or
  rename to `*_unchecked` and document the validity contract). Today
  the only legitimate caller is the orchestrator's optimizer pipeline,
  which already has its own `(graph, entry)` API.

---

## 4. `target::{CallingConvention, BuiltCallingConvention, SleighArch, ArchPreset, CallOtherAbi, CallOtherClass}`

Files: `crates/target/src/{arch,call_other_abi}.rs`,
`crates/target/src/calling_convention/mod.rs`.

### Invariants identified
- `CallingConvention` fields are *all private* (`calling_convention/mod.rs:33-81`)
  — only the `*_systemv_abi` / `*_aapcs64` / etc. presets construct one.
- `BuiltCallingConvention.stack_ptr_vn` is non-optional `rsleigh::Vn`
  (`calling_convention/mod.rs:110`).
- `CallOtherAbi` has three `&'static [&'static str]` fields plus a bool
  `memory_edge` (`call_other_abi.rs:11-30`).
- `CallOtherClass` separates `NoOp` / `NoReturn` / `Call(CallOtherAbi)`
  (`call_other_abi.rs:35-49`).
- Arch-independent `Call` entries must have empty register channels
  — pinned by the `arch_independent_call_entries_have_empty_register_channels`
  test (`call_other_abi.rs:587-648`).

### Ratings
- **Encapsulation: 5/5** for `CallingConvention` (all private fields,
  preset-only construction). **3/5** for `BuiltCallingConvention` —
  every field is `pub` (`calling_convention/mod.rs:88-140`); a caller
  could swap `arg_passing_regs` post-build, breaking the disjointness
  invariant (`call_passing_regs ∩ callee_saved_regs ∩ {sp} = ∅`)
  documented at `calling_convention/mod.rs:601-602`.
- **Invariant Expression: 5/5** for `BuiltCallingConvention.stack_ptr_vn:
  rsleigh::Vn` (NOT `Option<rsleigh::Vn>`) — every CC has one, and
  this is enforced at the type level. **5/5** for `SleighArch.preset:
  ArchPreset` — full granularity (one variant per preset constructor)
  lets `call_other_abi::classify` discriminate without re-deriving.
  **5/5** for `CallOtherClass`'s 3-arm split — `NoReturn` is a distinct
  classification because it terminates control flow at *cfg-build* time
  (region split), not at lift time, so collapsing it into `Call(empty)`
  would miscompile.
- **Usefulness: 4/5** — the single `classify(preset, name)` entry point
  with arch-specific-then-arch-independent fallback is exactly right.
  `PURE` / `PURE_WITH_MEM_EDGE` / `NO_OP` / `NO_RETURN` constants
  (`call_other_abi.rs:195-218`) keep the table per-line diffable.
- **Invariant Enforcement: 4/5** — the `arch_independent_call_entries_have_empty_register_channels`
  test enforces the "named registers ⇒ arch-specific table" rule
  syntactically at the call sites that use `PURE` / `PURE_WITH_MEM_EDGE`.
  Concern: the disjointness contract on `BuiltCallingConvention`
  (sp ∉ arg ∪ callee-saved, arg ∩ callee-saved = ∅) is doc-only and
  reliant on tests.

### Why `CallOtherClass::NoOp` is a separate variant and NOT
`Call(CallOtherAbi { reads:&[], writes:&[], memory_edge:false })`:
The two have different *consumers*. `NoOp` tells `IrStrider::handle_call_other`
to **emit no IR node at all** (truly invisible — Sleigh decoder context
markers like `setEndianState`/`setISAMode`). `Call(empty)` (e.g.
`Hint_Prefetch`) tells it to emit a bona-fide `CallOther` node so
patterns walking the memory chain or pattern-matching for the hint
can find it. Collapsing the two would lose the "emit vs. skip"
distinction. Keep as-is.

### Concrete improvements
- Make `BuiltCallingConvention` fields private and expose accessors
  (`stack_ptr_vn(&self)`, `arg_passing_regs(&self) -> &[rsleigh::Vn]`,
  etc.). The fluent pattern matches `CallingConvention`'s privacy
  policy and pins the disjointness invariant at construction.
- Add a debug-assert in `CallingConvention::build` that the disjointness
  contract holds (cheap, runs only in debug builds).

---

## 5. `strider::{RunConfig, AnalyzeOutcome, Strider, GraphRewriter, BuiltFunctionGraph, RegionLiftHandles}`

Files: `crates/strider/src/{lib,orchestrator,rewrite}.rs`,
`crates/strider/src/strider/{mod,pipeline}.rs`,
`crates/ir/src/function.rs` (for `BuiltFunctionGraph`).

### Invariants identified
- `RunConfig` owns the Sleigh handle, which is then **moved** into the
  loop and consumed/restored per iteration (`orchestrator.rs:60-106`).
- `AnalyzeOutcome` bundles `graph` + `unresolved_branches` + `region_handles`;
  consumers may use just `outcome.graph` (`strider/pipeline.rs:56-72`).
- `Decision { FixedPoint, StableOnly, Rebuild }` totally classifies
  one orchestrator loop step (`orchestrator.rs:194-205`).
- `RegionLiftHandles.exit_vn_to_value` is `Arc<HashMap>` — shared,
  not deep-cloned (`strider/pipeline.rs:45-46`).

### Ratings
- **Encapsulation: 2/5** for `BuiltFunctionGraph`: every field is
  `pub` (`ir/function.rs:46-79`), AND `from_graph_and_entry`
  (`ir/function.rs:94-103`) is `pub` — it constructs a graph with
  empty `variables` / `call_clobbered` / `ret_val_regs` / `call_other_clobbered`,
  documented as "not appropriate for downstream consumers that need
  that metadata". The contract leak is real: `GraphRewriter` and
  `opt::with_built` use it as a bridge for opt impls that want
  `&mut BuiltFunctionGraph` typing, but the entry point is fully
  public. (Also flagged in `reviews/round7-strider-target-reader.md:9`.)
- **Encapsulation: 4/5** for `RunConfig` (most fields are pub, but it's
  a builder-pattern config; the only invariant — that `sleigh` is moved
  in — is enforced by ownership). **5/5** for `Strider` (private fields,
  constructor-validated).
- **Invariant Expression: 5/5** for `Decision` — the three-way split
  is exactly the orchestrator's state space (no fixed-point / re-run
  stable subset / rebuild CFG), and the loop body's `match decision`
  (`orchestrator.rs:184-191`) makes this exhaustive at compile-time.
- **Invariant Expression: 2/5** for `BuiltFunctionGraph` —
  `from_graph_and_entry`'s "empty fields are safe because opt impls only
  read graph and entry" comment is exactly the kind of invariant that
  belongs in the type system, not the docstring.
- **Usefulness: 4/5** — `RunConfig` + `run()` is the right one-call
  entry point. `AnalyzeOutcome::Display` (`strider/pipeline.rs:74-83`)
  is a thoughtful debug aid.
- **Invariant Enforcement: 2/5** — see `from_graph_and_entry`. The
  fact that `GraphRewriter::apply_rule` and `opt::with_built` both have
  to comment "we promise we only touch graph + entry" is exactly the
  shape of an unenforced contract.

### Concrete improvements
- **HIGH (CRIT)**: Make `BuiltFunctionGraph::from_graph_and_entry`
  `pub(crate)`. Introduce a sibling `pub struct GraphRewriteCtx<'a> {
  graph: &'a mut Graph, entry: NodeId }` that opt passes / rewriters
  consume via `pattern::rewrite_rule(&mut ctx, ...)`. The dummy CC
  metadata never appears anywhere. The only consumers today
  (`opt::with_built` at `opt/src/pipeline.rs:94-104`,
  `GraphRewriter::apply_rule` at `strider/src/rewrite.rs:117-145`)
  literally do `mem::take(&mut graph)` to bypass the CC fields — they
  do not benefit from the empty fields and are paying the contract
  leak for no gain.
- Make `BuiltFunctionGraph` fields `pub(crate)` and expose `&Graph` /
  `entry()` / `variables()` / `call_clobbered()` accessors. The
  `pub graph: Graph` field is the load-bearing reason the empty-field
  shortcut survives.

---

## 6. `opt::{OptimizerPipeline, default_pipeline, stable_default_pipeline, destructive_default_pipeline}`

File: `crates/opt/src/pipeline.rs`, `crates/opt/src/lib.rs`.

### Invariants identified
- Stable subset: passes that *only rewrite operands* (don't detach phi
  / control-state nodes the orchestrator's `RegionIndex` pins by id).
  Lib doc comment, `lib.rs:70-104`.
- Destructive subset: passes that **remove** nodes (`RedundantPhis`,
  `DeadBranchElimination`). Safe only at fixed point.
- `OptimizerPipeline.run` validates the final graph
  (`pipeline.rs:290`).

### Ratings
- **Encapsulation: 4/5** — pipeline internals (`optimizers`,
  `optimizer_names`, `post_passes`) are all private (`pipeline.rs:181-191`).
  `add` / `add_post_pass` / `run` are the API.
- **Invariant Expression: 1/5** — **NOTHING in the type prevents
  `pipeline.add(RedundantPhis)` on a freshly-constructed pipeline
  intended to be the stable subset.** The "stable" / "destructive"
  invariant is enforced purely by which `default_pipeline()` /
  `stable_default_pipeline()` constructor you call — once you have
  an `OptimizerPipeline` value, `add` accepts any `Optimizer` impl.
  The CLAUDE.md doc says callers MUST run destructive passes only at
  the fixed point; this is documented, not type-enforced.
- **Usefulness: 5/5** — fluent `add` / `add_post_pass`, deterministic
  iteration order, fixed-point convergence cap (1024,
  `pipeline.rs:270-285`). The trait split (`Optimizer` vs.
  `OptimizerOnBuilt` blanket impl, `pipeline.rs:114-172`) is elegant.
- **Invariant Enforcement: 2/5** — the pipeline-shape tests
  (`pipeline.rs:317-385`) compare `optimizer_names` substrings; this
  catches accidental drift in the prebuilt pipelines but does not stop
  a downstream caller from mixing destructive passes into the stable
  subset.

### Concrete improvements
- Introduce phantom-typed wrappers: `OptimizerPipeline<Stable>`,
  `OptimizerPipeline<Destructive>`, `OptimizerPipeline<Full>`. The
  `add` method is then `OptimizerPipeline<Stable>::add<P: StableOptimizer>`
  vs `OptimizerPipeline<Destructive>::add<P: DestructiveOptimizer>`.
  Each pass impl marks itself with one (or both for the dual-purpose
  passes like `KnownBits`). `default_pipeline()` returns
  `OptimizerPipeline<Full>`; the orchestrator's loop body type-checks
  that it can't pass a `Full` pipeline to the iterating-fixed-point
  call. Cost: small marker trait per pass; benefit: invariant becomes
  unrepresentable at compile time. Worth doing — the bug is currently
  caught only by code review.

---

## 7. `reader::{MemReader, ReadOnlyMemory, MemRegion, MemRegionsLookupTable, ElfFileMemReader}`

File: `crates/reader/src/{lib,elf}.rs`.

### Invariants identified
- `MemRegion` private fields with `new()`-checked overflow guard
  (`lib.rs:113-133`) — strong, and explicitly cited in the docstring
  as "the no-overflow invariant".
- `ElfFileMemReader.is_little_endian: bool` is captured at construction
  from `obj.endianness()` (`elf.rs:255-279`).
- `ReadOnlyMemory` trait has no endianness in its signature
  (`lib.rs:77-82`): `fn read(&self, space: VnSpace, addr: u64, size: usize) -> Option<u64>`.
  The implementor (e.g. `ElfFileMemReader::read` at `elf.rs:316-343`) is
  responsible for endian-correct decoding.

### Ratings
- **Encapsulation: 5/5** for `MemRegion` — the textbook example of
  invariants enforced by privacy + constructor.
- **Encapsulation: 4/5** for `ElfFileMemReader` (private struct, factory
  constructors, no field leak). **3/5** for `MemRegionsLookupTable`
  (private `regions` map; only API is `read`).
- **Invariant Expression: 2/5** for the trait surface — endianness is
  *not* part of `ReadOnlyMemory`. An implementor that forgets to swap
  bytes for a big-endian target silently returns wrong-endian results
  for `LoadReadOnly`. (Linked to the `PyMemoryMap` always-LE bug
  flagged in `round7-py-support.md`.) The trait should encode "I return
  a `u64` matching what the target machine would observe" — that's a
  contract about the implementor's bytes-to-u64 conversion, which
  presupposes knowing the target endianness. Today the trait is silent.
- **Usefulness: 5/5** — `MemRegion::read` returning `Option<usize>`
  with documented partial-read semantics is precise and useful.
- **Invariant Enforcement: 4/5** — `MemRegion::new` checks overflow.
  `ElfFileMemReader::from_object` captures endianness from the parsed
  ELF. The trait-level endianness gap is the gap.

### Concrete improvements
- Add `fn endianness(&self) -> reader::Endianness;` to `ReadOnlyMemory`,
  OR rename the API from `read(...) -> Option<u64>` to a clearer
  `read_native_word(space, addr, size, endianness) -> Option<u64>` so
  the caller passes the target's endianness explicitly. The `LoadReadOnly`
  pass already knows the target (it's configured via
  `from_convention(_, &arch)` and the arch carries `endianness`); make
  it pass that in.
- Alternative (less invasive): add a typed wrapper
  `pub struct EndianBytes { is_little: bool, data: [u8; 8] }` returned
  from `read_bytes` that the caller decodes explicitly. Forces the
  endianness decision at the call site.

---

## 8. `cfg::{Cfg, RegionTerminator, SpecialTerm, RegionEdgeKind}`

Files: `crates/cfg/src/cfg/{mod,types}.rs`. Note `SpecialTerm` is
NOT a public type in `cfg`; it is private to `strider` at
`strider/src/strider/pipeline.rs:490` — see commentary below.

### Invariants identified
- Each region has exactly one `RegionTerminator` (`types.rs:88-175`).
- Variants like `Switch` reserve future expansion (doc comment,
  `types.rs:84-87`).
- `Region.insns` is "never empty" (`types.rs:188`).

### Ratings
- **Encapsulation: 3/5** for `Cfg` — `sleigh` and `graph` and `entry`
  are all `pub` (`cfg/mod.rs:37-60`). Borrows tend to be `&Cfg`, but
  nothing structurally prevents a caller from mutating `cfg.entry` to
  point at a non-existent `NodeIndex`. The sleigh field is `pub`
  because `strider::run` harvests it back out of the CFG between
  iterations — that's a real ergonomic constraint.
- **Encapsulation: 5/5** for `RegionTerminator` / `RegionEdgeKind` —
  closed sums, no inner-mutation.
- **Invariant Expression: 5/5** for `RegionTerminator` — the variants
  precisely cover the cfg builder's classification space (Fallthrough,
  Branch, CondBranch, Return, NoReturn, TailCall, Switch,
  UnresolvedIndirectBranch). The `Switch.target_value: Option<ir::Value>`
  field signals "OPTIONAL pinned" via type, not docs.
- **Usefulness: 5/5** — `MachineInsnAddr` and `PcodeInsnAddr` are
  newtype-wrapped `u64`s (`types.rs:29-67`) preventing accidental
  mixing with raw addresses. Lexicographic ordering on `PcodeInsnAddr`
  is enforced by field declaration order (with explicit "do not
  reorder" warning at `types.rs:46-48`).
- **Invariant Enforcement: 4/5** — `Region.insns` "never empty" is
  doc-only; `Region::contains_addr` handles the zero-insn case
  defensively (`types.rs:194-205`). Could be strengthened by replacing
  `Vec<RegionInstruction>` with a `NonEmpty<RegionInstruction>` or a
  custom newtype.

`SpecialTerm` (in strider, `strider/src/strider/pipeline.rs:490`) is
correctly a *private* enum — it's a lift-time projection of the cfg
terminator that picks out the four cases strider handles specially
(Unresolved, Switch, TailCall, NoReturn). Keep private.

### Concrete improvements
- Make `Cfg.entry` accessor-only (`pub fn entry(&self) -> NodeIndex`),
  hide the field. The "harvest sleigh" pattern can keep a public method
  `take_sleigh(&mut self) -> Option<rsleigh::Sleigh<R>>` that explicitly
  represents the move-semantics ownership transfer.
- Consider replacing `insns: Vec<RegionInstruction>` with a typed
  `NonEmptyVec`. The `contains_addr` defensive `match self.insns.last()`
  is the smell.

---

## Top 5 Highest-Impact Improvements

1. **Make `BuiltFunctionGraph::from_graph_and_entry` `pub(crate)`; introduce
   a `GraphRewriteCtx { graph, entry }` newtype for the opt / rewriter
   bridge.**
   *Files:* `crates/ir/src/function.rs:82-103`, `crates/opt/src/pipeline.rs:94-104`,
   `crates/strider/src/rewrite.rs:117-145`.
   *Why:* Both consumers do `mem::take(graph)` to construct the empty-CC
   `BuiltFunctionGraph`; a dedicated newtype makes the empty-fields contract
   leak vanish without churn at the call sites. (CRIT — already flagged in
   round7 cross-crate review.)

2. **Replace `NodeOutputType::info`'s `&TYPE_INFO[self as usize]` with
   either an explicit `match` or a `const _: () = assert!` block pinning
   variant discriminants.**
   *File:* `crates/ir/src/node/output_type.rs:50-70`.
   *Why:* The doc claim of "asserted by the test" is false; the load-bearing
   ordering is implicit and silently breaks on out-of-order variant additions.
   Hot path for every type-system predicate (`is_bool`, `byte_size`,
   `is_integer`, `is_float`).

3. **Phantom-type the `OptimizerPipeline` as `OptimizerPipeline<Stable>` /
   `<Destructive>` / `<Full>`, with `add<P: $StableOptimizer>` per state.**
   *File:* `crates/opt/src/pipeline.rs:180-220`, `crates/opt/src/lib.rs:106-194`.
   *Why:* The "destructive passes only at fixed point" rule is currently
   doc-only; running them mid-iteration silently invalidates the orchestrator's
   pinned `NodeId`s. Encoding this in the type system makes it a compile error.

4. **Make `BuiltCallingConvention`'s fields private and expose accessors.**
   *File:* `crates/target/src/calling_convention/mod.rs:88-140`.
   *Why:* `CallingConvention` already enforces this (all-private + preset
   constructors). The built version's invariants — disjointness of
   arg-passing / callee-saved / sp, the int-vs-float ret-vals split — are
   doc-only and can be invalidated by a single mutating call. Add a
   debug-assert in `build()` for the disjointness contract.

5. **Replace `FunctionBuilder::set_lift_addr(Option<u64>)` with a scope
   guard `lift_at(addr, |b| ...)`.**
   *File:* `crates/ir/src/builder/mod.rs:137, 376-386`, `crates/strider/src/strider/insn/`.
   *Why:* The asm-fingerprint contract relies on every lift-time `create_node`
   call being bracketed by `set_lift_addr(Some(addr))` / `set_lift_addr(None)`.
   This is convention; a forgotten clear contaminates structural nodes;
   a forgotten set silently drops attribution. The Layer-C check is
   opt-in and won't catch the contamination case at all. A scope guard
   makes the contract structurally enforceable.

---

## Summary

The codebase shows a strong design culture for **closed sums** (every
enum in scope is well-shaped — `NodeKind`, `NodeOutputKind`,
`NodeOutputType`, `RegionTerminator`, `RegionEdgeKind`, `Decision`,
`CallOtherClass`) and **newtype identifiers** (`MachineInsnAddr`,
`PcodeInsnAddr`, `Capture`, `VarId`). The recurring weakness is
**public fields on aggregate types whose invariants are documented
but not enforced** — `BuiltFunctionGraph`, `BuiltCallingConvention`,
`Cfg`, `Graph`'s side-tables (visible via `pub graph: Graph` on
`BuiltFunctionGraph`). The contract-leak `BuiltFunctionGraph::from_graph_and_entry`
is the highest-impact instance — it leaks an "empty-fields-are-safe"
contract used by exactly two internal call sites that don't even need
those fields. Two further patterns (`NodeOutputType` `as usize`
indexing, optimizer-pipeline destructive-passes-everywhere) are
correctness-critical invariants that exist purely in docs and are
worth promoting to type-level guarantees.
