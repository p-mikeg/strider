# strider v5 simplification plan

**Goal:** Take the v4 codebase and remove another ~2,500–3,500 LOC, fix
two genuine correctness/perf bugs that survived four rounds of cleanup,
and collapse the three remaining architectural layers that exist for
historical reasons no longer load-bearing.

**Inputs:** Five parallel read-only audits — four crate-scoped (dead
surface + per-file simplification) plus one cross-cutting (deep
architectural collapse).  Reports stored under
`/tmp/claude-1000/.../tasks/a*.output`.

**Scope:** the deep collapses are the headline; surface cleanups and
generalizations are the support work.  No new features.  All work pinned
to `v3_baseline.rs` snapshot contract; the snapshots must not move
except where the work item explicitly is "regenerate after a verified
shape change".

**Skills required for execution:** superpowers + subagent-driven-development
+ test-driven + debugging + code-review + writing.  Push after every
commit.  Use entity-utils structures (`DenseEntitySet`, `Worklist`) over
std collections for entity-keyed bookkeeping.  Keep every code path O(n)
or O(n·log n).  No plan-identifier strings in code, doc comments, or
commit messages.

---

## Reachable scope at a glance

| Tier | Description | Theme count | Est. LOC | Risk |
|---|---|---:|---:|---|
| **A — Deep architectural collapse** | Multi-layer wrappers, trait stacks, type-system tax | 5 | ~900 | High |
| **B — Generalization (data tables, trait extraction)** | Hand-rolled tables → data; per-pass driver | 7 | ~900 | Medium |
| **C — Surface dead code** | Zero-caller pubs, redundant DTOs, doc rot | 15 | ~750 | Low |
| **D — Code-level simplification** | Long fn split, deep nesting, manual Iterator | 9 | ~400 | Low |
| **E — Correctness + perf fixes** | O(n²) loop walk, mis-classification heuristic | 2 | n/a | Medium |
| **F — Final review + tag** | Reviewer pass + v5-final tag | 1 | n/a | n/a |
| **Total** | | **39 themes** | **~2,950 LOC** | |

Each tier is described below.  Execution order recommendation at the
end.

---

# Tier A — Deep architectural collapse (the headline)

These are the **real simplifications**: redundant layers that exist
because of an earlier architectural decision no longer in force.

## A1.  Drop `BuiltFunctionGraph` — `Graph::entry: Option<NodeId>` + `Graph::cc_metadata: Option<CcMetadata>` already encode the invariant

**Status today.**  Phase 6.2 of v2 moved both `entry` and `cc_metadata`
into `Graph` directly so that `RewriteCtx::new(&mut Graph, NodeId)` can
work without a wrapper.  `BuiltFunctionGraph` was kept to mark "fully
built" through type signatures.  Today it's a `Deref<Target=Graph>`
wrapper whose every accessor expands to `self.inner.field.expect(...)`.

**Evidence the wrapper has lost its load-bearing role:**
- Every opt pass calls `bfg.graph_mut()` and works on `&mut Graph`.
- `RewriteCtx` / `RewriteCtxView` carry `&[mut] Graph + NodeId`, not
  `&[mut] BuiltFunctionGraph`.
- `from_graph_and_entry_for_rewrite` (now in `strider-ir-test-utils`
  per v4 Theme C7, but if not, in `function.rs:177-187`) exists to
  fabricate a `BuiltFunctionGraph` with empty CC just to satisfy the
  wrapper for tests that don't use CC at all — direct evidence the
  invariant is the wrapper's only contribution.
- `&BuiltFunctionGraph` derefs to `&Graph`; so do all the
  `preorder` / `preorder_kind` / `cfg_reachable` accessors.

**Cleanup:**
1. Delete `BuiltFunctionGraph` struct in `crates/strider-ir/src/function.rs`.
2. Replace returns of `BuiltFunctionGraph` with `Graph` (the rich one
   has `entry: Option<NodeId>` and `cc_metadata: Option<CcMetadata>`
   already).
3. Provide a `Graph::built(&self)` predicate that returns
   `self.entry.is_some() && self.cc_metadata.is_some()`; one
   `expect("graph not built")` per call site for the rare external API
   that previously asserted "this is built".
4. Update all `bfg.graph_mut()` call sites to take `&mut Graph` directly.
   Update `Strider::analyze_cfg` / `run()` return types from
   `BuiltFunctionGraph` to `Graph`.
5. Verify `strider-py` doesn't expose `BuiltFunctionGraph` as a `pyclass`
   surface that consumers depend on (open question #1 below).

**Counter-argument:** `BuiltFunctionGraph` at function signatures
documents "I expect a built graph".  Replacing with `&Graph` loses this
hint at the boundary.  In practice the hint is already absent for
internal opt-pass callers (they take `&mut Graph`); only the orchestrator
+ Python boundary surfaces the wrapper.  Accept the doc loss for the
~150 LOC structural win.

**Dependencies:** A1 must land before A2 (RewriteCtx unification depends
on having one graph type).

**LOC:** ~150–250 net.

**Spike-then-commit:** start with a single call site rewrite to confirm
nothing in `strider-py`'s `.pyi` surface depends on the named type;
commit only after `v3_baseline` snapshot stays byte-identical.

---

## A2.  Unify `RewriteCtx` + `RewriteCtxView` — one `RewriteCtx<'g, M: Mutability>` (after A1)

**Status today.**  `RewriteCtx<'g> { graph: &'g mut Graph, entry: NodeId }`
and `RewriteCtxView<'g> { graph: &'g Graph, entry: NodeId }` are
structurally identical modulo `&mut` vs `&`.  Each carries its own
`preorder` / `preorder_kind` / `graph_ref` / `entry` accessors
(~50 LOC duplicated in `pattern/rewrite.rs:164-272`).  24 callsites use
`RewriteCtxView<'_>` (read-only opt API: `analyze_known_bits`,
`classify_anchor*`, `find_unique_if`, `count`, etc.).

**Why it survives:** the read-only view was extracted so the indirect-
branch classifier path (which needs `&Graph + entry` only) and the
rewriting path (which needs `&mut Graph + entry`) could share accessors
without mutability rebinding.  This is a real design need but expressible
with a single parameterized type.

**Cleanup:**
- Add a sealed `pub trait Mutability { ... }` with `Yes` / `No` impls.
- One struct: `pub struct RewriteCtx<'g, M: Mutability = Yes>` where the
  graph reference is `M::Ref<'g, Graph>` (associated type GAT, or use
  `MaybeMut<'g, T, M>` newtype).
- Read-only accessors via blanket impl; mutating accessors only on
  `RewriteCtx<'_, Yes>`.
- The 24 `&RewriteCtxView` signatures become
  `RewriteCtx<'_, No>` (or just `RewriteCtxView<'_>` as an alias).

**Counter-argument:** Mutability-marker GAT dance adds one type-level
indirection; a developer reading the trait signature pays for it.  But
the duplication today is real and the marker pattern is well-known.

**Dependencies:** lands after A1 (one graph type to parameterize over).

**LOC:** ~100.

---

## A3.  Replace `IndirectTargetResolver<R>` trait with `Arc<dyn Fn(...)>` callback (or function pointer)

**Status today.**
- `crates/strider-lift/src/cfg/builder/indirect_resolver.rs` defines a
  generic-over-`R` callback trait.
- Exactly one implementor: the zero-state `MiniIrIndirectResolver` in
  `crates/strider-analyze/src/indirect_resolver.rs`.
- Orchestrator allocates `Arc::new(MiniIrIndirectResolver)` per CFG
  rebuild even though the resolver is stateless.
- The trait exists because of an earlier cycle (`cfg → opt`).  The
  cycle is gone — `strider-analyze` depends on `strider-lift` cleanly.

**Cleanup options (pick one):**

**A3a (minimal, lowest risk):** keep the `Arc<dyn ...>` machinery but
replace the trait with a function-pointer type alias:
```rust
pub type IndirectResolverFn<R> = Arc<dyn Fn(
    &[RegionInstruction],
    Vn,
    &Sleigh<R>,
    Option<Vn>,
    Option<&dyn ReadOnlyMemory>,
    Endianness,
) -> Result<Option<ResolvedTargets>> + Send + Sync>;
```
The orchestrator constructs `Arc::new(resolve_indirect_target)`.

**A3b (aggressive):** combined with A4 (drop the `<R>` generic), the
function pointer simplifies further to a non-generic `fn(...) -> ...`.
No `Arc`, no `dyn`, no allocation per rebuild.

**LOC:** ~80–100.

---

## A4.  Drop `<R: rsleigh::MemReader>` from the strider-lift / orchestrator type stack

**Status today.**  `Cfg<R>`, `Builder<R>`, `ValueLifter<'a, R>`,
`RegionBuilder<'a, R>`, `IndirectTargetResolver<R>`, `Config<'a, R>`,
`LoopState<'a, R>` thread the generic through ~15 type signatures.
Concrete impl sites of `rsleigh::MemReader`: `ElfFileMemReader`,
`PyMemReaderAdapter`, `PyMemoryMapReader`, `AnyMemReader`.  Three of
those exist solely to feed `AnyMemReader` — Python production goes
through one `enum AnyMemReader { Map, Cb }` whose explicit comment is
"without monomorphising the entire downstream type stack".

So at the strider-lift level there are effectively two `R` types live:
`AnyMemReader` (production) and per-test concrete readers.  The
monomorphisation tax compounds across the workspace.

**Cleanup:**
- Replace `<R: rsleigh::MemReader>` everywhere with a workspace-level
  trait object: `pub type BinaryReader = Box<dyn rsleigh::MemReader>`
  (or a strider-side enum dispatch type matching `AnyMemReader`'s shape
  but in strider-lift).
- The pcode-lift hot path calls `MemReader::read` once per byte fetched
  — vtable hop is real but mitigated by Sleigh's decode caches.

**Caveats:**
- rsleigh is a path dep; if `Sleigh<R>` itself can't accept a
  trait-object `R`, the collapse stops at the strider-lift boundary
  (Sleigh stays generic, but everything downstream uses one boxed `R`).
- Need a perf measurement: bench `cross_arch_shape` before/after on
  a representative ELF.  The memory note `project-phase6-perf-reality`
  records v2 was 1.1–1.6× slower than v1 — we have perf budget for the
  trade-off.

**Counter-argument:** monomorphisation is the textbook Rust idiom; some
test cases may depend on per-type specialization in inlining.  Mitigation:
if the bench shows ≥5% regression, abort.

**Dependencies:** A3a/A3b benefit from this; can also stand alone.

**LOC:** ~200–400 (mostly trait-bound clauses and signature changes).

**Risk:** highest of the Tier A items.  Spike-first; abandon-if-bench-regresses.

---

## A5.  Collapse the `Decision { FixedPoint, StableOnly, Rebuild }` machine + the stable/destructive pipeline split

**Status today.**  Orchestrator has a 3-variant decision machine plus
`build_stable_optimizer_pipeline()` and `build_destructive_optimizer_pipeline()`.
The stable/destructive split exists to preserve `NodeId`s across
iterations of the indirect-branch fixed-point loop — destructive passes
(`RedundantPhis` / `DeadBranchElimination`) remove nodes that the
per-iteration `RegionIndex` pins.

**Why it's collapsible:**
- `RegionIndex` rebuilds per iteration anyway; the pin is a single-
  iteration concern.
- Once `RegionLiftHandles` is trimmed to its two live fields (Tier C
  item below), the index can rebuild from `exit_control` alone — no
  reliance on `NodeId` survival of destructive passes.
- The salsa orchestrator that motivated the per-iteration pinning
  never shipped (memory note `project-phase6-perf-reality`).

**Cleanup:**
1. Trim `RegionLiftHandles` to two fields (Tier C item C9 below) —
   prerequisite.
2. Replace `RegionIndex.by_exit_control: HashMap<NodeOutputId, Arc<HashMap<Vn, NodeOutputId>>>`
   with a per-call lookup from the placeholder's `Control` predecessor
   chain to its `ControlState` (or a stable address-keyed map populated
   once on CFG build).
3. Collapse `Decision { FixedPoint, StableOnly, Rebuild }` to
   `Decision { Continue, FixedPoint }` (or `bool`).
4. Delete `build_stable_optimizer_pipeline()` and
   `build_destructive_optimizer_pipeline()`; keep one
   `build_pipeline()` (already the public Rust + Python entry).
5. Demote `opt::stable_default_pipeline` + `opt::destructive_default_pipeline`
   to `pub(crate)`.

**Counter-argument:** the pipeline comment at `pipeline.rs:213`
explicitly says "destructive passes ... remove nodes that the
orchestrator's per-iteration index pins."  If the index outlives a
single iteration in any branch, the collapse is unsafe.  Spike: read
every `RegionIndex` user and confirm no consumer holds an index across
iterations.

**LOC:** ~250.

**Dependencies:** depends on C9 (`RegionLiftHandles` trim) landing first.

---

# Tier B — Generalization (data tables, trait extraction)

## B1.  Calling-convention presets → `&[CcPresetRow]` data table

Same as v4 plan's Theme D1; v5 audits across two crates flag it as the
biggest single LOC win remaining (~500 → ~150 LOC).  14 hand-rolled
zero-arg factories at
`crates/strider-target/src/calling_convention/mod.rs:381-962`.
Each ~30 LOC struct literal.  `*_linux_syscall` derived variants use
override-on-userland pattern; encode as per-row `arg_passing_regs_override`,
etc.  The test file already uses a `Case`-row table — direct evidence the
shape works.

Spike on `x86_64_systemv` first; convert the rest in batches of 3–4 per
commit.  Keep the named zero-arg fns as thin wrappers around `lookup(name)`
for back-compat.

**LOC:** ~400 net.

---

## B2.  `call_other_abi::classify_arch_specific` → `&'static [(ArchPresetSet, &str, CallOtherClass)]` table

Same as v4 plan's Theme D2.  16-arm `match (preset, name)` at
`crates/strider-target/src/call_other_abi.rs:76-263`; entries are
already shaped like data.  Linear scan over ~16 rows is fine — no `phf`
dep needed.

**LOC:** ~150–190.

---

## B3.  `PeepholePass` trait + generic driver — 7 of 11 opt passes share one shape

**Pattern:** 7 of the 11 `impl Optimizer` sites in
`crates/strider-analyze/src/opt/*` follow:
1. Seed a worklist with reachable nodes of some `NodeKind` set.
2. Per-node: extract inputs, check shape, mutate / replace.
3. On change, enqueue consumers and propagate.

**The 7 that fit:** `constant_fold`, `flag_cmp_canonicalize`,
`if_cond_inversion`, `load_readonly`, `stack_store/detect`,
`stack_load_forward`, `function_args`.

**The 4 that don't:** `known_bits` (full-graph analysis then rewrite),
`redundant_phis` (kind-filtered + structural), `dead_branch`
(kind-filtered + collect_dead_subgraph), `indirect_branch_resolve`
(classifier not a peephole pass).

**Cleanup:**
```rust
pub trait PeepholePass {
    fn name(&self) -> &'static str;
    fn root_kinds(&self) -> &'static [NodeKindDiscriminant];
    fn try_rewrite(&self, ctx: &mut RewriteCtx<'_>, root: NodeId)
        -> Result<OptimizationResult>;
}
pub fn run_peephole<P: PeepholePass>(pass: &P, ctx: &mut RewriteCtx<'_>)
    -> Result<OptimizationResult> {
    let mut work: Worklist<NodeId> = ctx.preorder_kind(|k| {
        pass.root_kinds().iter().any(|d| d.matches(k))
    }).collect();
    let mut overall = OptimizationResult::NoChange;
    while let Some(node) = work.dequeue() {
        match pass.try_rewrite(ctx, node)? {
            OptimizationResult::Changed => {
                overall = OptimizationResult::Changed;
                // enqueue consumers via ctx.graph_ref().output_uses(...)
                for consumer in /* ... */ { work.enqueue(consumer); }
            }
            OptimizationResult::NoChange => {}
        }
    }
    Ok(overall)
}
```

Each pass shrinks from ~30 LOC of `impl Optimizer` boilerplate to a
`PeepholePass` impl whose only body is `try_rewrite`.  Future passes
become ~10 LOC each.

**Spike-first**: convert `ConstantFold` (smallest, well-tested), confirm
parity, then batch the other 6.

**LOC:** ~80 net immediate + ~20 LOC saved per new pass.

---

## B4.  Pcode-lift opcode dispatch — ~28 trivial 1-line arms → `&[(Opcode, IntBinaryOp)]` table

`crates/strider-lift/src/pcode_lift/value/mod.rs:32-129`.  Of the 41
`Opcode::*` arms, ~28 are 1-line
`lifter.process_int_binary_op(insn, IntBinaryOp::Add)?;` patterns
across int/bool/float categories.  Three small slices:
```rust
static OPCODE_TO_INT_BINARY: &[(Opcode, IntBinaryOp)] = &[ ... ];
static OPCODE_TO_BOOL_BINARY: &[(Opcode, BoolBinaryOp)] = &[ ... ];
static OPCODE_TO_FLOAT_BINARY: &[(Opcode, FloatBinaryOp)] = &[ ... ];
```
plus a generic dispatcher `if let Some(op) = lookup(opcode, &TABLE)`.
Custom-logic arms (cast, piece, extract, insert, ptr_add, lowered-at-lift
shapes) stay.

**LOC:** ~70 net.

---

## B5.  `NodeOutputType` 12 flat variants → `Bool / Int(IntWidth) / Float(FloatKind)` (after spike)

**Status today.**  `crates/strider-ir/src/node/output_type.rs:8-69`
defines 12 flat enum variants + a parallel `TYPE_INFO` table + a 12-arm
`info(self)` match whose only purpose is "rather than `&TYPE_INFO[self as usize]`"
(per the doc-comment — variant-order-must-stay-synced concerns).

**Cleanup:**
```rust
pub enum NodeOutputType {
    Bool,
    Int(IntWidth),  // W8, W16, W32, W64, W80, W128, W256, W512
    Float(FloatKind),  // F32, F64, F80
}
```
Derive `byte_size` / `bit_width` / `is_integer` / `is_float` from
the variant directly.  Drop `TYPE_INFO` and the variant-position
assertion test.

**Caveats:** 83 external uses of width accessors across `strider-analyze`.
Most go through `byte_size()` / `is_*()`; those signatures stay.  The
`matches!(ty, U8 | U16 | U32 | U64)` shapes (validator, opt passes) need
updating to `matches!(ty, NodeOutputType::Int(IntWidth::W8 | IntWidth::W16 | ...))`.

**Spike-first:** confirm the cost of the match-arm updates is bounded
before mass conversion.

**Confidence:** ~70% per audit.  If the call-site sweep is messy, defer.

**LOC:** ~80 inside the file + ~50 LOC of call-site updates.

---

## B6.  6 `check_graph_invariants_*` free fns → `trait ValidatorPass` + registry

`crates/strider-ir/src/validate/graph_invariants.rs` (6 functions:
`uniqueness`, `control_state`, `phis`, `function_arg_uniqueness`,
`wide_consts`, `asm_fingerprints`).  Every signature except `uniqueness`
is `(graph, &reachable, &mut errs)`.

**Cleanup:**
```rust
pub trait ValidatorPass {
    fn name(&self) -> &'static str;
    fn check(&self, graph: &Graph, reachable: &NodeIdSet, errs: &mut Vec<ValidationError>);
}
static PASSES: &[&dyn ValidatorPass] = &[
    &ControlStatePass, &PhisPass, &FunctionArgUniquePass,
    &WideConstsPass, &AsmFingerprintsPass,
];
// validate() body:
let reachable = compute_reachable(graph, entry);
uniqueness::check(graph, &reachable, errs);  // special-case
for p in PASSES { p.check(graph, &reachable, errs); }
```
Adding a new invariant is a single-line registry append.

**LOC:** ~40 net.

---

## B7.  8 `*_op_name` helpers in `matcher.rs` reimplement `Debug`

`crates/strider-py/src/matcher.rs:256-345`.  Each helper is a
`match op { Add => "Add", Mul => "Mul", ... }`.  Every enum derives
`Debug` and the variant names match exactly.

**Cleanup:**
```rust
fn op_name<T: std::fmt::Debug>(op: T) -> String { format!("{op:?}") }
```
Or implement `Display` once per enum (single source).

**LOC:** ~90 net.

---

# Tier C — Surface dead code (parallel-safe deletions)

These are low-risk individual cleanups.  Dispatch in parallel batches
of 4–6 subagents.  Verify zero callers by grep before deleting.

## C1.  `from_graph_and_entry_for_rewrite` (strider-ir)
Inline its three lines into the one test that uses it; delete the
`#[doc(hidden)] pub` method + 25 LOC of doc.  ~35 LOC.

## C2.  `FunctionBuilder::lift_at` RAII scope-guard
Only tests use it (4 sites).  Production uses bare `set_lift_addr`
pairs.  Delete the method + `Guard` struct (~30 LOC); inline the bare
pair into the 4 test sites.

## C3.  `Outputs<'a>` newtype with no invariants
`crates/strider-ir/src/iterators.rs:9-50` — slice wrapper, all impls
forward to the slice.  Change `Graph::node_outputs` to return
`&[NodeOutputId]`.  Keep `Inputs<'a>` (genuine `&Graph` indirection).
~57 LOC.

## C4.  `UnknownCallOtherError` move to strider-analyze
Constructed only at `strider-analyze/src/strider/insn/mod.rs:123`; lives
in `strider-ir/src/error.rs`.  Move to `strider-analyze::errors`.  ~30 LOC.

## C5.  `BuiltCallingConvention` 10 `pub(crate)` fields + 10 trivial accessors
Type is immutable post-`try_new`; flip fields to `pub` and delete the
accessors.  ~70 LOC.

## C6.  `MachineInsnAddr` / `PcodeInsnAddr` triple constructor
3 constructors (`From<u64>` + `::new` + struct-literal) + 2 accessors
(`as_u64()` + `.addr`).  Pick the `From<u64>` form (idiomatic), drop
`::new`, drop `as_u64()` accessor.  ~20 LOC.

## C7.  `syscall_number_vn` write-only field plumbing
`crates/strider-target/src/calling_convention/mod.rs:74-80, 116, 297-306`
plus 7 `*_linux_syscall` setters.  Field exists, gets populated, never
read in production (only `tests/linux_cc_presets.rs`).  Either delete
(if syscall ABI is documented elsewhere) or wire a real consumer.
Spike to confirm before deleting.  ~30 LOC.

## C8.  `indirect_resolve/` shim layer
v4 plan flagged this, not executed.  ~50 LOC of shim forwarding into
`crate::opt::*`; ~480 LOC of inline tests that belong next to the impl
in `opt/indirect_branch_resolve/`.  Delete shim files; move tests.

## C9.  `RegionLiftHandles` trim to 2 fields
9 `pub` fields, only 2 read (`exit_control`, `exit_vn_to_value`).
Delete the 7 unread fields + their population in `analyze_cfg_with`.
~60 LOC.  **Prerequisite for A5.**

## C10.  `opt::indirect_resolver` re-export of `crate::indirect_resolver`
One-line re-export creating a split-brain `crate::opt::indirect_resolver`
vs `crate::indirect_resolver` path.  Pick one canonical, sweep call sites.

## C11.  `pat_builder_finalise!` macro of one
Invoked exactly once for `PyFunctionArgPat`.  Inline + delete the macro.
~30 LOC.

## C12.  `RomInput` / `ReaderInput` / `ReaderInputClone` → one `MemInput`
Three same-shape enums with three identical `FromPyObject` blocks.
Collapse into one enum + per-call materialisers.  ~50–70 LOC.

## C13.  25 hand-stamped classmethod presets on `PySleighArch` / `PyCallingConvention`
`forall_preset!(self_ty, inner_ty, [...])` macro.  ~140 LOC.

## C14.  `PyOptPass` 6 dead variant payloads
`Bound<>` kept only for `FromPyObject` dispatch; zero-sized passes
ignore it.  Replace with a `FromPyObject` impl that only keeps `Bound<>`
for the 4 stateful passes.

## C15.  `dot::GraphDot::with_name` / `as_svg` / `as_html_from_svg` + `HTML_SVG_TEMPLATE`
Zero callers.  System-`dot` shell-out path is dead; only the WASM
`as_html_from_dot` path is used.  ~75 LOC.

**Tier C total:** ~750 LOC.

---

# Tier D — Code-level simplification

Per-file cleanups identified by the audits.  Dispatch a
`code-simplifier` subagent per file.

## D1.  `FunctionBuilder::new_raw` 110 LOC → 3 named helpers
`crates/strider-ir/src/builder/mod.rs:280-380` has three independent
phases (overlap-largest filter, call-clobbered list construction,
arg/ret upgrade).  Extract `dedup_overlapping_largest` and
`build_call_clobbered_list`; `new_raw` becomes 3 named transforms.

## D2.  5 `get_as_*` helpers in `coerce.rs` → unified `const_value`
`crates/strider-ir/src/builder/coerce.rs:33-144`.  Five sites share
shape `let output_type = self.get_output_type(...)?; match graph.kind_of_output(...) { IntConst(v) if ... => ..., BoolConst(v) if ... => ..., _ => Ok(None) }`.
Collapse into one `pub fn const_value(...) -> Result<Option<ConstValue>>`
returning an `enum ConstValue`.  ~40 LOC.

## D3.  4 manual `Iterator` structs in `iterators.rs` → `impl Iterator`
Three of the four (`OutputIter`, `OutputUsageIter`, `InputIter`) can be
expressed as `impl Iterator<Item = ...> + '_` returned from the accessor
methods.  Keep `InputCursor` (mutation-capable).  ~50 LOC.

## D4.  `wide_consts` validation's 4-deep nesting → `?`-chain helper
`crates/strider-ir/src/validate/graph_invariants.rs:266-319`.  Extract
`fn wide_const_byte_size(graph, node) -> Result<(usize, NodeOutputType), ValidationError>`.
Body drops to ~20 LOC of one early-return loop.  ~30 LOC.

## D5.  `process_new_insn` 173 LOC + 4-deep nesting → per-opcode helpers
`crates/strider-lift/src/cfg/builder/region_builder.rs:238-421`.
Extract `process_branch`, `process_cond_branch`, `process_call_other` as
private helpers.  `process_new_insn` shrinks to a 25-line dispatch
table.  ~100 LOC saved.

## D6.  `opt/sp_expr.rs` 978 LOC → 3-4 focused modules
`crates/strider-analyze/src/opt/sp_expr.rs` mixes range arithmetic,
step-through walkers, constant peeling, and the SP decomposer.  Split
into `sp_expr/{decompose,ranges,walk,mod}.rs`.  Zero semantic change.

## D7.  Constant-peeling helpers shared between `stack_array.rs` + `jump_table.rs`
`crates/strider-analyze/src/opt/indirect_branch_resolve/{stack_array,jump_table}.rs`
both have `flatten_add_tree`, `extract_idx_and_stride`, `peel_to_u64_const`.
Extract to `pattern/walk_through.rs` (existing module).  ~80 LOC dedup.

## D8.  `Box<dyn Iterator>` heap alloc → `SmallVec`
`orchestrator/mod.rs:809-815` — `build_anchor_calling_context`
allocates a boxed iterator just to type-unify two branches.  Swap to
`SmallVec<[&Vn; 16]>`.  Tiny but on a hot path.

## D9.  `strider-reader/src/elf.rs` 1018 LOC → split
Three responsibilities (sections, reader, relocations).  Split into
`sections.rs`, `reader.rs`, `relocations.rs`.  After the split, demote
`apply_elf_relocations` to `pub(crate)` (already in v4 finding but
deferred because in-crate integration tests consume it — move those
tests into the new file's module).

**Tier D total:** ~400 LOC.

---

# Tier E — Correctness + perf fixes

These are not LOC wins — they are bugs.

## E1.  `count_loop_headers` is O(n²) and uses `HashSet`

**File:** `crates/strider-py/src/graph.rs:139-180`.

**Bug:** for each `ControlState` node N, runs a fresh DFS from each
predecessor of N to find back-edges.  Worst case
`O(|ControlState| × |graph|)`.  Plus uses `HashSet<NodeId>` for visited
— violates the project memory directives `feedback-scalability-thousands-of-nodes`
("O(n) or O(n·log n), never O(n²)") and `project-prefer-entity-utils`
("use `DenseEntitySet<NodeId>`").

**Fix:** preorder once, mark each node's order index, then a back-edge
is any control input from a node whose pre-order index ≥ the current
node's.  Single linear pass.

**Tests:** add a proptest generating CFGs of 100, 500, 1000 nodes;
assert `count_loop_headers` completes in <10ms.  Plus the existing
behavioural tests pin the correct count.

## E2.  `LIFT_MARKERS` substring heuristic for error classification

**File:** `crates/strider-py/src/errors.rs:58-67`.

**Bug:** classifies an `anyhow::Error` as `LiftError` by scanning the
formatted error chain for the substrings `"lift" | "sleigh" | "decode" |
"pcode" | "unsupported instruction"`.  Any `ReaderError` whose message
mentions `"decode"` (e.g. ELF parse "failed to decode section") will be
misclassified as `LiftError` — a real correctness hazard.

**Fix:** expose a typed `LiftError` in strider-lift / strider-analyze
that the Python boundary can downcast against.  Replace substring
matching with a `error.downcast::<strider_analyze::errors::LiftError>().is_ok()`
check.

**Spike-then-refactor:**
1. Add `#[derive(thiserror::Error)] enum LiftError { ... }` in
   strider-analyze (or strider-lift if more appropriate).
2. Update lift call sites to return the typed error.
3. Update strider-py classifier to downcast.
4. Delete `LIFT_MARKERS`.

**Tests:** integration test loading an ELF whose decoder produces a
`"failed to decode section"` error and asserting Python catches
`ReaderError`, not `LiftError`.

---

# Tier F — Final review + tag

## F1.  Workspace-wide reviewer pass

After all themes execute, dispatch `pr-review-toolkit:code-reviewer`
against `v4-final..HEAD` checking:
- v3_baseline byte-identical (production oracle)
- cross_arch_shape green
- per-crate suites all green
- no new plan-identifier strings in code or commit messages
- no new `HashSet<NodeId>` introduced
- clippy clean (the existing pre-v3 `expect_used` carryover is a known
  follow-up; v5 should fix or escalate)
- worklist + entity-utils usage as adopted in v4 still intact

If APPROVED: tag `v5-final` and push.

---

# Open questions for the owner (deep audit flagged these)

These are decision-level uncertainties; please answer before executing
the corresponding theme.

1. **A1 (BuiltFunctionGraph drop): does `strider-py` expose
   `BuiltFunctionGraph` as a pyclass surface that Python consumers
   depend on?** If yes, A1 needs a `pyclass` shim or stays as a
   typestate-marker.  Grep `crates/strider-py/src/` to confirm.

2. **A4 (`<R>` generic drop): does benchmark `cross_arch_shape` regress
   ≥5% under `Box<dyn MemReader>` dispatch?** If yes, the static
   monomorphisation pays off and A4 stops.

3. **A5 (Decision + RegionIndex collapse): does the orchestrator's
   `RegionIndex` survive across iterations in any branch?** Spike: read
   every `RegionIndex` user and confirm one-iteration scope before
   collapsing.

4. **B5 (`NodeOutputType` reshape): is the call-site impact bounded?**
   Spike conversion of the validator's `matches!` arms first; if the
   sweep is messy, defer.

5. **`syscall_number_vn` field (C7): delete or wire?** If the syscall
   ABI is documented separately and the field is genuinely vestigial,
   delete.  If there's a future consumer planned (Python syscall
   inspection), wire a producer.

6. **B3 (`PeepholePass` driver): is the 7-of-11-passes shape really
   uniform?** Spike on `ConstantFold`, confirm parity, then batch.

---

# Execution order

**Phase 1 — Surface (parallelizable, ~750 LOC):**
Dispatch Tier C themes in parallel batches of 4–6 subagents.  Each
verifies zero callers via grep before deleting.  No cross-theme
dependencies.

**Phase 2 — Code-level (parallelizable, ~400 LOC):**
Tier D themes — one `code-simplifier` subagent per file (D1, D2, D3,
D4, D5, D6, D7, D8, D9).  No cross-theme dependencies.

**Phase 3 — Generalization (~900 LOC, mostly parallel):**
- B1, B2 (calling-convention + call_other data tables) — independent.
- B4 (pcode-lift opcode table) — independent.
- B6 (ValidatorPass trait) — independent.
- B7 (op-name helpers → Debug) — independent.
- B3 (PeepholePass driver) — depends on a spike commit; once spiked,
  batch the remaining 6 passes.
- B5 (`NodeOutputType` reshape) — depends on spike; can run last.

**Phase 4 — Deep architecture (~900 LOC, serial chain):**
1. A1 (drop `BuiltFunctionGraph`) — solo, then verify.
2. A2 (`RewriteCtx` + view unification) — depends on A1.
3. C9 must land (RegionLiftHandles trim) — Tier C; dependency for A5.
4. A5 (Decision + pipeline collapse) — depends on C9.
5. A3 + A4 (resolver trait collapse + `<R>` generic drop) — can
   parallelize.  A4 has a bench-gated abort condition.

**Phase 5 — Correctness/perf (~2 commits, independent):**
- E1 (`count_loop_headers` O(n²) → O(n)) — proptest-gated.
- E2 (LIFT_MARKERS heuristic → typed error) — TDD-gated.

**Phase 6 — Final review + tag.**

**Estimated total wall-clock at v4 cadence:** 5–8 days.

---

# Risk callouts

1. **A1 (BuiltFunctionGraph drop)** — touches every consumer of the
   wrapper.  If `strider-py`'s `.pyi` surface exposes the named type,
   the rewrite is bigger than expected.  Spike-first.

2. **A4 (`<R>` generic drop)** — perf-sensitive.  Bench-gated abort.

3. **A5 (pipeline collapse)** — destructive passes may have
   non-`RegionIndex` reasons to defer.  Spike-first.

4. **B5 (`NodeOutputType` reshape)** — 83 call sites; mass conversion
   risk.  Spike-first.

5. **C7 (syscall_number_vn delete vs wire)** — decision-level open
   question; default to "delete + add a follow-up issue" unless the
   owner has a planned consumer.

6. **B3 (PeepholePass driver)** — risk that 1–2 of the 7 "fits" don't
   really fit and need per-pass escape hatches.  Spike on
   `ConstantFold` first.

7. **E2 (typed LiftError)** — cross-crate refactor; the error type's
   home (strider-lift vs strider-analyze) is itself a small decision.

---

# What this plan deliberately does NOT do

- Re-add salsa, egg, or any deleted v2/v3 machinery.
- Split `strider-analyze` back into multiple crates.
- Rename any project crate.
- Touch snapshot baselines without explicit regeneration review.
- Extend the proc-macro to cover the Rust-side pattern builders too
  (open question — flagged for owner; might be done OR might be a
  rollback to a hand-written shape).
- Move `Worklist` (kept per the v4 directive; usage now genuine across
  8 opt passes + cfg_reachable).

---

# Notes on memory directives

Per saved user feedback:
- **Use entity-utils structures** for entity-keyed bookkeeping.  E1
  fixes a `HashSet<NodeId>` violation; all new code in B3 / B6 / D-tier
  must follow.
- **O(n) / O(n·log n) only.**  E1 fixes an existing O(n²) violation.
- **No plan identifiers** (Phase X / Theme Y / Bug-N / v3 / v4 / v5)
  in code, doc comments, or commit messages.  Reviewer enforces.
- **Push after every commit.**
- **All v1 test functionality preserved.**  v3_baseline +
  cross_arch_shape + per-crate suites are the regression oracle.
