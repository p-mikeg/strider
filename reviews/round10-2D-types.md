# Round 10 — 2D Type-Design Audit

Workspace: `/mnt/c/Users/mikeg/Documents/strider`. All findings verified against
source.  Round-9 landmarks (`OptimizerOnBuilt::optimize_built` migration to
`&mut RewriteCtx`, `RewriteCtxView`, `AnchorAddr` accessors, `Capture::from_id`,
`from_graph_and_entry_for_rewrite` doc-hidden, accessor methods on
`PcodeInsnAddr` / `MachineInsnAddr` / `Cfg::start_addr_to_region_id` /
`SleighArch`) are explicitly out of scope.

The audit is grouped by anti-pattern.  Every finding cites file:line and notes
external-call-site count from `grep`-verified call graphs; the closing
**Visibility tightening table** lists items with zero external consumers.

---

## 1. Leaky encapsulation — `pub` fields whose docs spell out an invariant

### `opt::Kb` — `pub ones` / `pub zeros` with documented "ones & zeros == 0"
- **Severity:** HIGH
- **Where:** `crates/opt/src/known_bits/mod.rs:38-43`
- **Issue:** Doc on the struct says
  `> Both ones and zeros are masked to the output type's width and must never overlap (ones & zeros == 0).`
  Both fields are `pub`.  `Kb::merge` enforces the invariant on update, but
  any caller can write `Kb { ones: 0xFF, zeros: 0xFF }` directly — the merge
  contradiction-detection then silently swallows future merges (`ones &
  other.zeros == 0xFF & 0` is false, so no contradiction with a fresh `other`)
  and downstream `Kb::all_known` / `Kb::max_value` produce poisoned results.
- **Why it matters:** `analyze_known_bits` produces a `KnownBitsMap` consumed
  by the indirect-branch jump-table classifier (`jump_table.rs`,
  `stack_array.rs`).  A poisoned `Kb` can silently advertise a 4096-entry
  jump table where only 16 entries actually exist (or vice-versa), turning a
  pattern-query bug into a wrong-edge classification bug.
- **Proposed fix:** Make fields `pub(crate)`, add `Kb::from_ones_zeros(ones,
  zeros) -> Result<Self, _>` that rejects overlap, and `ones()` / `zeros()`
  accessors.  No external consumer of `Kb` exists in the workspace —
  `KnownBitsMap` is `pub` re-exported but threaded through internal-only opt
  classifier helpers, so the field-tightening is no-blast-radius.
- **Blast radius:** internal; ~6 sites in `opt/src/known_bits/mod.rs`
  construct `Kb { ones, zeros }` literals; convert each to a private
  `Kb::raw(ones, zeros)` ctor and the change is local.

### `cfg::Cfg::start_addr_to_region_id` — `pub` even though "direct mutation desyncs"
- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/mod.rs:66-73`
- **Issue:** Field doc:
  `> External readers should still go through Self::region_id_at_start — the
  field accessor is the canonical path; direct mutation desyncs the index
  from graph.`
  Round-9 wave 14 added the accessor but kept the field `pub` because
  `crates/cfg/tests/cfg_query.rs` uses struct-literal syntax for hand-built
  petgraph fixtures.  That justifies `pub(crate)` + a test-only `#[doc(hidden)]
  pub` builder, not unrestricted `pub`.
- **Why it matters:** A workspace consumer that pushes regions into `graph`
  but forgets the `BTreeMap` insert silently breaks `region_id_at_start` for
  exactly one region — and the bug surfaces only in the indirect-branch
  resolver's switch handler much later.
- **Proposed fix:** Tighten field to `pub(crate)`.  Add
  `#[doc(hidden)] pub fn from_parts_for_test(graph, entry, idx) -> Self` for
  the petgraph fixture path; add `add_region_with_idx(&mut self, …)` if any
  external mutator needs to extend post-construction.  External readers
  already use the accessor.
- **Blast radius:** ~1 call site (`cfg/tests/cfg_query.rs`); add a
  `Self::from_parts_for_test` ctor and migrate.

### `ir::BuiltFunctionGraph` — four `pub` fields (`graph`, `variables`, `call_clobbered`, `ret_val_regs`, `call_other_clobbered`)
- **Severity:** HIGH (round-9 deferred)
- **Where:** `crates/ir/src/function.rs:45-100`
- **Issue:** Each field carries an inline `**Caution:** mutating … silently
  breaks pattern queries` doc warning.  Round 9 V2 added accessor methods
  (`call_clobbered_regs`, `ret_val_regs_slice`, `call_other_clobbered_regs`,
  `variables_map`) but left the fields `pub` for back-compat with "~30+
  direct-field readers."  `graph` is also `pub` — any consumer can replace
  the entire `Graph` mid-analysis and unhook every cached `NodeId`.
- **Why it matters:** The whole pattern API (`Match::get_vn`, `Matcher::new`)
  indexes `call_clobbered` by varnode position, and the `pattern` crate's
  cross-iteration cache pins `NodeId`s assuming `graph` is monotonically
  grown, never replaced.  The contract is uniformly "write only at build
  time."  Encoding that as `pub(crate)` is a one-pass migration.
- **Proposed fix:** Two-step: (a) migrate readers off field access to the
  round-9 accessors; (b) flip fields to `pub(crate)`.  The migration step is
  mechanical (find/replace `bfg.call_clobbered` → `bfg.call_clobbered_regs()`
  etc.).  Step (b) blocks external crates' direct-field reads but the
  workspace is the only consumer; strider-py exposes accessors only.
- **Blast radius:** ~30 internal sites for step (a); zero risk after step (b).

### `cfg::RegionTerminator::Switch` — `target_value: Option<ir::Value>` with documented "always None at cfg build, populated later"
- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/types.rs:165-182`
- **Issue:** This is **partial-state**: the cfg builder always emits
  `target_value: None`; the orchestrator's known-targets feedback path may
  set it to `Some(_)` on a later iteration.  The two states have different
  semantics ("re-read `target_vn` at region exit" vs. "use the pinned
  output").  The variant carries both modes in one struct.
- **Why it matters:** Strider's `handle_switch` does
  `if let Some(value) = target_value { use value } else { read target_vn }`
  — if a future caller forgets the `else` arm, the same `Switch` produces
  different IR depending on whether it came from a cfg rebuild or an
  in-place edit.  The variant's identity hides which mode is active.
- **Proposed fix:** Split into two variants: `Switch { target_vn, targets }`
  (cfg-time, no pinned output) and `SwitchPinned { target_value, targets }`
  (orchestrator-fed, no `target_vn` because it's redundant once the value is
  pinned).  Each consumer matches the variant explicitly; no mixed mode.
- **Blast radius:** internal; 1-2 call sites in `strider/strider/pipeline.rs`
  + 1 in `cfg::region_builder`.  Maybe defer until an actual incremental-
  rebuild round arrives (TODO comment in `RegionLiftHandles`).

### `pattern::error::NotBuildable(pub &'static str)` & `MissingBinding(pub &'static str)` — tuple struct with `pub` field
- **Severity:** LOW
- **Where:** `crates/pattern/src/error.rs:28-36`
- **Issue:** The string is documented as a "kind name" (e.g. `"uint"`,
  `"int_binary_op"`).  External tests downcast and read it
  (`pattern::MissingBinding("uint")`).  The `pub` is needed for that pattern
  match.  No invariant beyond "string is non-empty" is documented; it's
  fine.  Flagged only so reviewers see this was considered.
- **Proposed fix:** No change.  Acceptable wrapper-over-string.

---

## 2. Primitive obsession — missed newtype candidates

### `is_addr_tail_call(target: u64, start_addr: u64, fn_max_size: Option<u64>, allow_code_before_start_addr: bool)`
- **Severity:** HIGH
- **Where:** `crates/cfg/src/cfg/query.rs:25-48`; called by
  `crates/cfg/src/cfg/builder/region_builder.rs:223` and
  `crates/strider/src/orchestrator.rs:630`.
- **Issue:** Round-9 P2 added `cfg::FunctionBoundary` (sum type that makes
  the documented "ignored when bounded" coupling unrepresentable) and
  `Options::function_boundary()` accessor that returns it.  Yet the
  load-bearing `is_addr_tail_call` predicate still takes the *unsafe* primitive
  4-tuple.  `FunctionBoundary` is currently dead weight — no caller consumes
  it.  Both call sites of `is_addr_tail_call` re-derive the boundary from
  the same primitive fields the sum-type was meant to encapsulate.
- **Why it matters:** The "fn_max_size.is_some() implies allow_code_before_
  start_addr ignored" rule is enforced inside `is_addr_tail_call` (line 36-37),
  not in the type system.  Any future caller passing `fn_max_size = Some(_),
  allow_code_before_start_addr = true` thinking they're permissive will be
  silently overridden.  The sum-type fix already exists; the function just
  doesn't take it.
- **Proposed fix:** New signature
  `is_addr_tail_call(target: u64, start_addr: u64, boundary: FunctionBoundary)`.
  Migrate the two call sites; delete the primitive form.  This makes
  `FunctionBoundary` actually load-bearing.
- **Blast radius:** 2 call sites + 1 unit test; <1 hour.

### `MemRegion::start_addr: u64` — could be a `MachineInsnAddr` / virtual-addr newtype
- **Severity:** LOW
- **Where:** `crates/reader/src/lib.rs:113-134`
- **Issue:** `MemRegion` already encapsulates the "no-overflow" invariant via
  a private field, but the addr is a raw `u64`.  Mixing it with non-virtual-
  addr `u64`s (e.g. byte sizes, pcode insn indices) is silent.  The cfg
  crate has `MachineInsnAddr(u64)` for exactly this; reader could re-use it
  or define its own `VirtualAddr(u64)`.
- **Why it matters:** Mostly self-documenting.  Errors today are unlikely
  because the reader API is small.  Flagged for completeness.
- **Proposed fix:** Defer.  Cross-crate alignment isn't worth the churn
  unless reader gains a richer API.

### `RunConfig::start_addr: u64` — the function entry address
- **Severity:** LOW
- **Where:** `crates/strider/src/orchestrator.rs:67`
- **Issue:** Same as above — `cfg::MachineInsnAddr` exists and is the right
  type, but the orchestrator surface uses `u64`.  Round-9 added accessors
  on `MachineInsnAddr` so adoption is cheap.
- **Proposed fix:** `pub start_addr: cfg::MachineInsnAddr` with `From<u64>`
  already implemented.  Existing callers `start_addr: 0x1000` keep working
  via the `into()` shim.
- **Blast radius:** ~5 call sites in tests; internal API only.

### `AnchorAddr::machine_addr: u64` & `insn_index: u64` (round-9 H2 deferred)
- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:223-226`
- **Issue:** These are explicitly the packed bits of a `cfg::PcodeInsnAddr`,
  but opt cannot depend on cfg (round-9 wave 18 verified opt → cfg would be
  a cycle).  The current `from_packed`/`parts` accessors transit through
  `u64`s.  This is the right call until the dep graph allows it.
- **Proposed fix:** No change.  Documented intentional.

### `RegionInstruction.addr: PcodeInsnAddr` is OK; `Region.start_addr: PcodeInsnAddr` is OK
- **Severity:** —
- **Where:** `crates/cfg/src/cfg/types.rs`
- These already use the right newtypes.

### `i64` SP offsets across `SpExpr`, `StackStore`, `StackStorePhi`, `stack_arg_offsets`
- **Severity:** LOW
- **Where:** `crates/opt/src/sp_expr.rs:30-35`,
  `crates/ir/src/node/kind.rs:117-128`, `crates/target/src/calling_convention/mod.rs:139`
- **Issue:** SP-relative offsets are uniformly typed as `i64` across passes.
  A `SpOffset(i64)` newtype would prevent accidentally adding a positional-
  index `i64` (e.g. `stack_arg_offsets[idx]`) to an SP offset without intent.
- **Why it matters:** Today the bug doesn't fire because `idx: usize` and
  the conversion is explicit; a future `i64`-flavored index could collide
  silently.  Aspirational improvement.
- **Proposed fix:** Defer until profiler shows mixing.

---

## 3. Partial-state types — built with sentinels in some paths and real values in others

### `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` (round-9 H1 deferred)
- **Severity:** HIGH (architectural; round-9 doc-hidden mitigation in place)
- **Where:** `crates/ir/src/function.rs:148-192`
- **Issue:** The doc explicitly calls this **partial-state**: returns a
  `BuiltFunctionGraph` with `variables: PrimaryMap::new()`, `call_clobbered:
  Box::new([])`, `ret_val_regs: Box::new([])`, `call_other_clobbered:
  Box::new([])`.  Any consumer reading those fields gets a meaningless empty
  view silently.  The `#[doc(hidden)]` is a band-aid.
- **Why it matters:** Round-9 wave 28 successfully migrated
  `OptimizerOnBuilt::optimize_built` to `&mut RewriteCtx<'_>` so the opt
  crate no longer needs this.  Today's only consumers are 4 pattern-test
  scaffolds (per round-9 notes) plus the `compact_tests` mod inside `function.rs`
  itself.  The pattern crate's `RewriteCtx::new(graph, entry)` is the
  type-correct alternative; the test scaffolds could migrate.  Once they
  do, `from_graph_and_entry_for_rewrite` can be deleted.
- **Proposed fix:** Migrate the 4 pattern-test scaffolds to
  `Matcher::for_graph(&graph, entry)` (the round-8 raw-graph entry point) +
  `RewriteCtx::new(&mut graph, entry)`.  Delete the partial-state ctor.
- **Blast radius:** ~5 test files in `crates/pattern/tests/`; ~1 hour.

### `FunctionGraph::new_invalid` (already `pub(crate)`)
- **Severity:** LOW (internal already)
- **Where:** `crates/ir/src/function.rs:30-37`
- **Issue:** `FunctionGraph` (internal — `mod function` is private) carries
  reserved `NodeId::reserved_value()` sentinels until the builder emits the
  entry node.  Doc says "consumers should never observe a partial graph".
  This is documented internal scaffolding for `FunctionBuilder`.
- **Proposed fix:** None needed — already encapsulated.

### `Strider::analyze_cfg` returns `AnalyzeOutcome { graph, unresolved_branches, region_handles }`
- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:56-72`
- **Issue:** All three fields are `pub`.  The struct is genuinely a
  product type, not partial state — every caller of `analyze_cfg`
  legitimately needs all three together (orchestrator) or just the graph
  (one-shot test).  Empty `unresolved_branches` is a valid steady state, not
  a sentinel.
- **Proposed fix:** None.

### `MatcherOptions { ignore_cast_mask, ignore_control_states }` is OK
- **Severity:** —
- Pure value-bag struct; `Default` is the safe state.

### `IndirectBranchResolve::unresolved_anchors` and `anchor_contexts` — co-keyed maps
- **Severity:** MED (round-9 V5 mitigation in place)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:172-184`
- **Issue:** Two `pub` fields documented as co-keyed: `unresolved_anchors:
  Vec<(AnchorAddr, NodeOutputId)>` paired with `anchor_contexts:
  HashMap<AnchorAddr, AnchorCallingContext>`.  Round-9 added
  `add_anchor(addr, output, ctx)` and `clear_anchors()` to populate them in
  lockstep, but the fields are still `pub` and direct mutation is still
  legal.  The `optimize` impl returns `Err` if the keys go out of sync —
  good defense, but the type permits the bad state.
- **Why it matters:** External callers (today only the strider orchestrator)
  may legitimately want to enumerate anchors, which requires field access.
  But the `pub` write access is the bug.
- **Proposed fix:** Tighten both fields to `pub(crate)`.  Expose
  `anchors() -> impl Iterator<Item = (&AnchorAddr, &NodeOutputId)>` and
  `anchor_context(&AnchorAddr) -> Option<&AnchorCallingContext>`.  Keep the
  builder ctor (`new`) plus `add_anchor` / `clear_anchors` as the only
  mutators.  External read-only consumption is preserved.
- **Blast radius:** internal; the strider orchestrator uses
  `pass.unresolved_anchors.push(...)` and `pass.anchor_contexts.insert(...)` in
  several places — round-9 added `add_anchor` but didn't migrate all sites.

### `IfRegionState { if_true_region: Option<NodeIndex>, if_false_region: Option<NodeIndex> }`
- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/query.rs:53-58`
- **Issue:** Two independent `Option`s.  A region with `region_if` returning
  `IfRegionState { if_true: None, if_false: None }` is suspicious — what
  does "If region with no successors" mean?  No invariant is documented.
- **Why it matters:** Reading `region_if` and matching `(true, false)` arm
  by arm makes the four states (TT, TF, FT, FF) all valid, but in practice
  most consumers expect TT (both successors).  A `Result<IfRegionState>` /
  enum form would clarify.
- **Proposed fix:** Defer.  Code paths surveyed don't depend on the four-
  state distinction; today's usage is "both must be Some, else error".

---

## 4. Types that fail to express invariants — public ctors that allow building bad values

### `BuiltCallingConvention::from_parts(parts) -> Self` (no validation)
- **Severity:** HIGH (round-9 V4 mitigation in place)
- **Where:** `crates/target/src/calling_convention/mod.rs:158-184`
- **Issue:** Two ctors:
  - `from_parts` — no validation; returns whatever you supply.
  - `try_from_parts` — round-9-added; validates disjointness, no SP in
    register lists, no duplicates, link-register-in-callee-saved, non-
    negative `ret_stack_pop`.

  The `from_parts` ctor is the back-compat path; tests use it.  `from_parts`
  is `#[must_use] pub` — any test can build a malformed CC and the rest of
  the pipeline silently mis-classifies clobbers.
- **Why it matters:** Tests in `crates/ir/tests/build_call_with_cc.rs` and
  `crates/pattern/tests/get_vn_with_call_override.rs` use `from_parts`; if
  they ever drop a register from `callee_saved_regs` that's also in
  `arg_passing_regs`, the test "passes" with wrong IR.  Production code
  should not have access to the unvalidated form.
- **Proposed fix:** Mark `from_parts` `#[doc(hidden)]` and rename to
  `from_parts_unchecked`; wire the ~3 test sites through `try_from_parts`
  (they already supply validated inputs).  Production code paths only use
  `CallingConvention::build`.
- **Blast radius:** internal; ~3 test sites + the body of `build` (which
  goes through `from_parts` today — change to `try_from_parts` and propagate
  the error).

### `ResolvedTargets::Multiple(pub Vec<u64>)` — non-empty invariant only enforced by sibling ctor
- **Severity:** MED (round-9 P5 mitigation in place)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:81-99`
- **Issue:** Doc says "**Invariant:** the inner `Vec` must be **non-empty**".
  Round-9 P5 added `ResolvedTargets::multiple(targets) -> Result<Self>` that
  validates.  But the tuple variant is still constructible directly:
  `ResolvedTargets::Multiple(vec![])` compiles and any consumer's
  `match` arm sees the bad variant.
- **Why it matters:** Doc explicitly says "An empty Multiple would silently
  advertise zero runtime targets, making the dispatch site appear
  unreachable."  The classifier arms today check `is_empty()` before
  constructing, so production never violates it — but a future arm doesn't
  have type help.
- **Proposed fix:** One of:
  - Make the field private: `Multiple(Vec<u64>)` → `Multiple(NonEmptyVec<u64>)`
    (or a private struct field).  Keeps `match` ergonomics with destructuring
    via accessor.
  - Or keep current shape and document that future arms must use
    `ResolvedTargets::multiple` (the round-9 ctor).  Static analyzers won't
    catch new bypasses.
- **Blast radius:** ~3 internal call sites + 4 pattern-match destructurings
  if going the `NonEmptyVec` route.  Defer unless a real arm regression hits.

### `Match::new_for_test(root, bindings) -> Self` — bypass for tests
- **Severity:** LOW
- **Where:** `crates/pattern/src/matcher/match_result.rs:32-34`
- **Issue:** `pub fn new_for_test` is documented "Test-only entry point";
  exposes the private `Bindings` ctor for synthesising matches.  Used by
  ~4 integration tests.  The `_for_test` suffix is the convention.
- **Proposed fix:** No change.  Convention is followed.

### `Bindings { entries: Vec<(Capture, Binding)> }` — `bind_capture` is `pub`, `mark`/`restore` are `pub(crate)`
- **Severity:** LOW
- **Where:** `crates/pattern/src/matcher/bindings.rs:55-90`
- **Issue:** `bind_capture` is `pub` and external callers can use it via
  `Match::new_for_test`'s bindings argument.  `mark`/`restore` (the
  speculative-attempt journal) are `pub(crate)` to prevent a tester from
  forgetting to restore on failure.  The split is correct.
- **Proposed fix:** No change.

---

## 5. Sealed traits / sealed enums — extensibility documented but not enforced

### `BinaryOpKind` is sealed; `BinaryOpPat<Op>` is `pub` with `#[allow(private_bounds)]`
- **Severity:** —
- **Where:** `crates/pattern/src/pat/builders/binary_op.rs:31-89`
- **Issue:** `pub(crate) trait BinaryOpKind` is properly sealed; only
  `IntBinaryOp` / `BoolBinaryOp` / `FloatBinaryOp` impl it.  External code
  cannot extend the trait or construct a non-aliased `BinaryOpPat<MyOp>`.
  Correct.
- **Proposed fix:** No change.  Reference example for "how to seal."

### `Optimizer` / `OptimizerOnBuilt` traits — `Send + Sync` but not sealed
- **Severity:** LOW
- **Where:** `crates/opt/src/pipeline.rs:102-160`
- **Issue:** Both traits are `pub` and unsealed.  `OptimizerPipeline::add<O:
  Optimizer + 'static>` accepts any `O` — strider-py implements
  `Optimizer` for its custom-pipeline path, and the doc-tests in
  `LoadReadOnly` show external impls.  Intentional extensibility.
- **Proposed fix:** No change.  Open trait is correct here.

### `ReadOnlyMemory` trait — open by design
- **Severity:** —
- **Where:** `crates/reader/src/lib.rs:77-99`
- Open trait; strider-py implements it for callback-style readers.  Correct.

### `BinaryOpPat<Op: BinaryOpKind>` & sealed family pattern is good
- already covered above

### `NodeKind` enum — `pub`, no `#[non_exhaustive]`
- **Severity:** MED
- **Where:** `crates/ir/src/node/kind.rs:27-243`
- **Issue:** External `match graph.node_kind(n) { ... }` exhaustiveness
  pins on the closed enum.  Adding a new `NodeKind` variant is a breaking
  change for every external `match`.  Strider-py and pattern's matcher both
  have exhaustive matches.  No `#[non_exhaustive]` attribute.
- **Why it matters:** Today the workspace is the only consumer, so closed
  enum semantics are fine.  Once strider-py is published or external
  pattern crates appear, every `NodeKind` addition will be a SemVer bump.
- **Proposed fix:** Two options:
  - Keep closed (current) — accept that `NodeKind` is the IR surface and
    every addition is a major version bump.  Documented in CLAUDE.md as
    "the closed enum of every operation/role."
  - Add `#[non_exhaustive]` and require external matches to add `_ =>` arms
    — turns every IR addition into a non-breaking change but loses
    exhaustiveness checks for internal opt passes.
- **Proposed fix:** Defer until external crates appear.  Closed is the right
  choice for a tightly-coupled workspace.

### `RegionTerminator`, `RegionEdgeKind`, `ResolvedTargets` — all closed enums, same story
- **Proposed fix:** Same as `NodeKind` — defer.

---

## 6. Lifetime-leaking views

### `RewriteCtx<'g>` and `RewriteCtxView<'g>` — proper lifetime, `Deref<Target=Graph>`
- **Severity:** —
- **Where:** `crates/pattern/src/rewrite.rs:154-258`
- **Issue:** Both types correctly carry `'g`.  `RewriteCtx<'g>` has `pub graph:
  &'g mut Graph` and `pub entry: NodeId`; `RewriteCtxView<'g>` is `Copy` with
  `pub graph: &'g Graph`.  Deref impls expose `Graph` ergonomically.
- **Proposed fix:** None — round-9 design is good.  Note that the inner
  `pub graph` and `pub entry` fields permit external modification of the
  pair: `ctx.graph = some_other_mut_graph` flips the borrow target.  In
  practice the borrow checker prevents most abuse, but a `pub(crate)` on
  the fields + `graph()` / `entry()` accessors would be tighter.

### `CallingConvention` register-name slices use `&'static str` — intentional
- **Severity:** —
- **Where:** `crates/target/src/calling_convention/mod.rs:34-80`
- The fields hold `&'static [&'static str]` for built-in presets so the
  preset structs are `Copy + Clone`.  Intentional and safe — strings are
  baked into the binary.

### `SortedVns(Vec<rsleigh::Vn>)` — `#[allow(dead_code)]` newtype
- **Severity:** MED
- **Where:** `crates/strider/src/strider/pipeline.rs:97-143`
- **Issue:** Newtype documenting `Vec<Vn>` as sorted by
  `pcode_lift::vn_sort_key`.  Round-9 P3 added it for forward migration but
  `AnalyzeOptions::all_vns` still takes raw `Option<Vec<rsleigh::Vn>>`
  (line 163) "for back-compat".  The newtype carries `#[allow(dead_code)]`
  attributes — no production caller uses it yet.
- **Why it matters:** Half-finished migration.  The newtype is dead weight
  unless someone migrates `AnalyzeOptions::all_vns: Option<SortedVns>`.
- **Proposed fix:** Either complete the migration (flip `all_vns` field
  type, update orchestrator) or delete `SortedVns`.  Dead `pub` types in a
  released library accrete API surface.
- **Blast radius:** ~5 internal sites if completing; zero if deleting.

---

## 7. Visibility tightening candidates

See dedicated table at the end.

---

## 8. Wrappers that add no invariant

### `LoadReadOnly<M>(pub M)` — fine, just a function-arg wrapper
- **Severity:** LOW
- **Where:** `crates/opt/src/load_readonly/mod.rs:47`
- The tuple form is the standard "wrap a function-argument as an
  optimizer pass struct" pattern; there's no invariant to enforce on `M`.
  Reference example.

### `MemReadError(pub anyhow::Error)` — bridges `anyhow::Error` to `std::error::Error`
- **Severity:** LOW
- **Where:** `crates/reader/src/lib.rs:41`
- Pure adapter wrapper.  `pub` field allows the bridge to work; no
  invariant beyond "wraps an anyhow".  Fine.

### `RewriteSkip` — unit struct sentinel
- Unit struct used for `downcast_ref`.  Standard sentinel pattern.

---

## 9. Strider/pattern/opt — types whose invariants depend on side-tables

### `NodeKind::StackStorePhi { space }` + `Graph::stack_phi_offsets` (`SecondaryMap<NodeId, Vec<i64>>`)
- **Severity:** MED
- **Where:** `crates/ir/src/graph/mod.rs:60`,
  `crates/ir/src/node/kind.rs:120-128`,
  `crates/opt/src/sp_expr.rs:144-157` (correct conservative MayAlias)
- **Issue:** `StackStorePhi` carries only the `space` in the variant; the
  per-predecessor offsets live off-side in `Graph::stack_phi_offsets`.  The
  side-table defaults to empty `Vec<i64>` for any unset entry.  Pattern
  matchers must check `slice.is_empty()` to distinguish "real phi with
  no offsets recorded" from "unpopulated synthesised node".
  `Match::stack_phi_offsets` (round-9-era) collapses empty slices to `None`
  to surface this.
- **Why it matters:** A `StackStorePhi` node without its side-table entry
  is a malformed graph in production — `StackStoreDetect` always populates
  it.  But the type signature of `NodeKind` doesn't say so, and a test that
  hand-builds a `StackStorePhi` without populating the side-table compiles.
  The `step_through_stack_store_phi` checker correctly returns `MayAlias` in
  this case (line 144), pinned by a regression test.
- **Proposed fix:** No structural change feasible — the offsets list is
  variable-length, can't go in a `Copy` `NodeKind`.  Document the
  side-table population contract in `NodeKind::StackStorePhi`'s rustdoc and
  add a Layer C validator check that every reachable `StackStorePhi` has a
  non-empty `stack_phi_offsets` entry (mirrors the existing
  `check_layer_c_phis` arity check).  Production would never trigger the
  new error; mock tests that need the empty form would explicitly opt out.
- **Blast radius:** add a layer-C check + ~1 mock test fix.

### `NodeKind::CallOther { user_op_id }` + `Graph::call_other_names` side-table
- **Severity:** LOW
- **Where:** `crates/ir/src/graph/mod.rs:76`
- Same shape as `StackStorePhi`: variant carries an id, name is off-side.
  The doc says "Not all `CallOther` nodes are guaranteed to have an entry —
  e.g. nodes synthesised by tests that don't go through the analyzer."  This
  is documented "no invariant" — fine.
- **Proposed fix:** None.

### `Graph::call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>`
- **Severity:** LOW
- Same family.  `None` (default) is a meaningful value distinct from
  `Some(empty)`.  No invariant violation.

---

## 10. `RegionLiftHandles` — broad `pub` field surface, `Arc`-shared map
- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:21-47`
- **Issue:** Every field is `pub`.  `exit_vn_to_value: Arc<HashMap<...>>` is
  shared with `RegionIndex::ExitVnToValue`.  No documented invariants
  beyond "captured snapshot."  Trivial product type.
- **Proposed fix:** No change.

---

## Visibility tightening table

Items with **no external workspace consumer** outside the defining crate
(verified via `grep -rn 'crate::TypeName' crates/*/src crates/*/tests`).
Tightening to `pub(crate)` (or `pub(super)`) has zero blast radius.

| Item | File:line | Current | Suggested | Notes |
| --- | --- | --- | --- | --- |
| `ir::FunctionGraph` (struct + fields) | `crates/ir/src/function.rs:14-23` | `pub` (`mod function` is private) | `pub(crate)` | Module is `mod function;` — not re-exported.  Effectively pub(crate) already, but mark explicitly so an accidental `pub use` re-export can't leak it. |
| `ir::FunctionGraph::new_invalid` | `crates/ir/src/function.rs:30` | `pub(crate)` | keep | Already correct. |
| `ir::Outputs<'a>` / `ir::Inputs<'a>` / `ir::OutputIter` / `ir::InputIter` / `ir::OutputUsageIter` / `ir::InputCursor` | `crates/ir/src/iterators.rs:9-152` | `pub` | `pub(crate)` | `mod iterators;` is private at `lib.rs`.  No external consumer; iterators are returned from `Graph` methods that have their own re-exports (the iterators are typed only via the methods' return types).  Tighten field visibility too (`Inputs::graph`, `Inputs::use_list` are `pub(crate)` — consistent). |
| `ir::dot::GraphDotDumperState` | `crates/ir/src/dot/mod.rs:137` | `pub` | `pub(crate)` | Used only by `render.rs` inside the `dot` module.  No external workspace consumer; not re-exported through `ir::lib.rs`. |
| `pattern::error::__missing_binding` | `crates/pattern/src/error.rs:65` | `#[doc(hidden)] pub` | keep | Macro-only entry point; correct. |
| `pattern::Bindings::iter` | `crates/pattern/src/matcher/bindings.rs:127` | `pub` | keep | Used by `Matcher::find_all_requirements` — it's in the same crate but the future-proofing for Python bindings keeps it `pub`. |
| `strider::IrStrider` | `crates/strider/src/strider/mod.rs:14` | `pub` (`mod strider/mod.rs` private) | `pub(crate)` | Not re-exported from `lib.rs`; effectively pub(crate) already.  Tighten explicitly. |
| `cfg::Cfg::start_addr_to_region_id` | `crates/cfg/src/cfg/mod.rs:72` | `pub` | `pub(crate)` + test-only ctor | See Section 1. |
| `cfg::FunctionBoundary` (entire enum) | `crates/cfg/src/cfg/options.rs:27-48` | `pub` | re-evaluate | Currently dead weight — no caller consumes it.  Either complete the migration (Section 2: `is_addr_tail_call(boundary: FunctionBoundary)`) or `pub(crate)`-tighten. |
| `strider::SortedVns` | `crates/strider/src/strider/pipeline.rs:99` | `pub` `#[allow(dead_code)]` | delete OR finish migration | See Section 6. |
| `opt::AnchorAddr.machine_addr` / `.insn_index` (round-9-tightened to `pub(crate)`) | `crates/opt/src/indirect_branch_resolve/mod.rs:224-225` | `pub(crate)` | keep | Already correct. |
| `opt::indirect_branch_resolve::jump_table_tests::TableRom` / `RecordingRom` / `PartialRom` | `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:25,55,70` | `pub` | `pub(super)` or move to test-utils | Currently `pub` inside a `#[cfg(test)] mod`, but the module is `pub use`d via `dot_test_api`-style hidden re-export.  Strider's integration tests duplicate these (`crates/strider/tests/indirect_resolve_jump_table.rs` defines its own `TableRom`, `PartialRom`).  Either lift to a shared `test_utils` module or `pub(super)`-tighten. |
| `cfg::region_builder::ProcessInsnRes` | `crates/cfg/src/cfg/builder/region_builder.rs:49` | `pub` | `pub(crate)` | Used only by the region-builder's internal loop.  No external consumer. |
| `pattern::matcher::FunctionArgIndex` (private struct) | `crates/pattern/src/matcher/mod.rs:31` | private | keep | Already correctly private. |
| `pattern::Matcher::options_for_test` | `crates/pattern/src/matcher/mod.rs:202` | `pub` | `#[doc(hidden)] pub` or `pub(crate)` test-only | Method exists for tests inspecting `MatcherOptions`.  Mark `#[doc(hidden)]` or extract to a `Matcher::options_for_test` cfg-test impl. |
| `pattern::Match::new_for_test` | `crates/pattern/src/matcher/match_result.rs:31-34` | `pub` | `#[doc(hidden)] pub` | Already documented test-only. |
| `pattern::Bindings::bind_capture` | `crates/pattern/src/matcher/bindings.rs:81` | `pub` | keep | Needed by `Match::new_for_test` callers. |
| `cfg::DecodeCache::insert` / `get` | `crates/cfg/src/cfg/decode_cache.rs:59,65` | `pub` | keep | Used by `cfg::Builder::with_decode_cache` and `RegionBuilder::lift_one_cached`; the cache is shared across `strider::run` iterations.  Public surface is correct. |
| `target::call_other_abi::CallOtherClass` (variants) | `crates/target/src/call_other_abi.rs:35` | `pub` | keep | Re-exported through `target::lib.rs` and consumed by both cfg's region builder and strider's CallOther dispatch.  Public surface is correct. |
| `target::CallOtherAbi` (struct) | `crates/target/src/call_other_abi.rs:12` | `pub` | keep | Same — public ABI. |

---

## Cross-cutting recommendations

1. **Migrate the four pattern-test scaffolds off
   `BuiltFunctionGraph::from_graph_and_entry_for_rewrite`** so the
   partial-state ctor can be deleted in round 11.  The migration target
   already exists (`Matcher::for_graph(&graph, entry)`).

2. **Complete the `SortedVns` and `FunctionBoundary` migrations** that round-9
   started.  Both newtypes/sum-types are dead until their consumer signatures
   adopt them.  This audit is the prompt to either finish or delete.

3. **Visibility audit pass for `pub` items in private modules**
   (`mod function`, `mod iterators`, `mod strider/mod.rs`, etc.).  Several
   types are `pub` inside privately-scoped modules — effectively pub(crate)
   today, but explicit `pub(crate)` prevents an accidental future
   `pub mod foo` or `pub use crate::foo::Bar` from leaking the type without
   intent.

4. **Add a Layer C validator check for `StackStorePhi` side-table population**
   so a malformed `StackStorePhi` (empty `stack_phi_offsets`) surfaces at
   validation time instead of silently `MayAlias`-ing through downstream
   passes.  Production never violates the contract; mock tests that need
   the empty form already explicitly check for it.

5. **`Kb { ones, zeros }` field privatization** — small effort, removes a
   real silent-corruption path in the indirect-branch jump-table classifier.
