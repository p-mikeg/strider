# Graph compaction + per-callsite clobber overrides

Three related extensions to `strider::run` and the IR shape:

1. **Graph compaction.** After the destructive optimiser pipeline runs,
   the IR arena typically holds detached "zombie" nodes that
   `RedundantPhis` / `DeadBranchElimination` / `CallOtherElide` severed
   from the live graph (their inputs were dropped to `[]`, no consumers
   remain).  This spec adds a final compaction step that rebuilds the
   arena to retain only nodes reachable from `entry` via
   `ir::walk::walk_graph` (control-out + data-in).  Default-on knob on
   `RunConfig`.
2. **Per-address calling-convention overrides.**  Some call targets
   have an ABI that differs from the function-default convention.  The
   driving case is Linux-kernel `__fentry__` / `mcount`, which preserve
   every register and observe no arguments.  This spec adds a
   `HashMap<u64, CallingConvention>` field on `RunConfig` so the lifter
   and the indirect-branch in-place editor can swap in a per-target CC
   when emitting a `Call` whose target address matches.
3. **Conservative `CallOther` clobber default.**  Today
   `FunctionBuilder::build_call_other` emits no clobber output slots —
   the function's tracked variables retain their pre-CallOther values.
   This is wrong for opaque user-ops that clobber state (`syscall` is
   the canonical case: every userland register the syscall ABI doesn't
   preserve is gone after `int 0x80` / `syscall` / `svc 0`).  This spec
   changes the CallOther default to **clobber every tracked variable
   except the stack pointer** and rebind each cleared variable to the
   corresponding clobber slot, mirroring how `Call` already works.  The
   future, out-of-scope follow-up is a per-user-op CC override map
   analogous to feature 2's per-address map.

All three features are additive at the public API boundary
(`RunConfig` gains two fields with backward-compatible defaults;
`strider.run` in Python gains two keyword arguments).  Feature 2 and
feature 3 share the same per-Call clobber-list side-table on
`ir::Graph` (`call_clobbered_overrides`) and adjust
`pattern::Match::get_vn` to consult it for both `Call` and
`CallOther` nodes.

## Goals

1. After `strider::run`, every node in the returned `BuiltFunctionGraph`
   is reachable from `entry` (when `compact=true`, the default).
2. NodeId stability across compaction is **not** preserved — pre-
   compaction NodeIds become invalid.  This is documented and only
   matters for callers that snapshot NodeIds before `run` returns
   (Strider's internal loop is safe; Python `Match`/`Capture` are
   re-queried against the post-compaction graph).
3. A user can supply a per-target-address `CallingConvention` map.
   For every direct or in-place-edited `Call` whose target is in the
   map, the Call node is built using the override CC end-to-end:
   args, ret-vals, clobber set, and `ret_stack_pop` all replaced.
4. Pattern queries (`Match::get_vn` and friends) continue to recover
   the correct varnode for any clobber slot, regardless of whether
   the node is a `Call` (function-default or override CC) or a
   `CallOther` (function-default conservative-all set).
5. Every `CallOther` node emitted by `FunctionBuilder::build_call_other`
   clobbers every tracked variable (except the stack pointer) by
   default; downstream reads of those variables flow from the
   CallOther's corresponding clobber slot, not from the pre-CallOther
   producer.
6. Compaction (feature 1) and the new per-Call clobber side-table
   (features 2 + 3) are exposed from `strider-py` via keyword arguments
   on `strider.run` (compaction) and through the existing graph /
   match accessors (clobber side-table).

## Non-goals

* Symbol resolution.  The user supplies raw target addresses; Linux-
  kernel callers resolve `__fentry__` / `__gnu_mcount_nc` etc. from
  the ELF symbol table themselves.
* "Function profile" support beyond the CC override (e.g. "this
  function never returns", "this function takes its own snapshot of
  caller state").  Out of scope.
* NodeId-stable compaction (a `removed: bool` flag + iterator
  filtering).  Considered and rejected — it would not reclaim memory
  and would clutter every iterator.
* Per-caller (rather than per-target) CC dispatch.  Out of scope; the
  override is keyed on the call target only.
* Per-user-op `CallOther` calling-convention overrides.  Feature 3
  introduces only the conservative default; a future spec adds a
  `RunConfig::per_user_op_ccs: HashMap<u64, CallingConvention>`
  parallel to `per_address_ccs`, keyed on `CallOther::user_op_id`,
  that lets a syscall ABI be expressed precisely (which registers
  the kernel preserves, which it clobbers, which carry the syscall
  number).  The infrastructure put in place by features 2 + 3 (the
  shared `Graph::call_clobbered_overrides` side-table and the
  override-aware `pattern::Match::get_vn`) make that follow-up cheap.

## Architectural facts

* `ir::Graph` stores nodes / inputs / outputs in
  `cranelift_entity::PrimaryMap`s, which do not support deletion.
  Real compaction means rebuilding the arena.
* Node side-tables (`asm_fingerprints`, `stack_phi_offsets`,
  `call_other_names`) are `SecondaryMap`s keyed by `NodeId`.  After a
  compaction that re-numbers `NodeId`s, side-table entries must be
  remapped through the old→new translation table.
* The dedup cache `node_to_id` keys on `(NodeKind, Vec<NodeOutputId>,
  Vec<NodeOutputKind>)`.  Both the values (`NodeId`) and the
  `NodeOutputId` keys re-number across compaction, so the cache must
  be rebuilt.
* `BuiltFunctionGraph::call_clobbered: Box<[rsleigh::Vn]>` is
  documented today as "the same for all calls" and is the index that
  `pattern::Match::get_vn` uses to recover varnodes from a Call's
  clobber output slots.  Per-call CC breaks the same-for-all-calls
  invariant; we add a per-Call override side-table and adjust
  `get_vn` to consult it.
* `FunctionBuilder::build_call` ([`crates/ir/src/builder/call.rs`])
  reads `arg_passing_vars` and `call_clobbered_variables` directly off
  the builder, both populated once at `FunctionBuilder::new`.  To
  support a per-call override, we add a sibling
  `build_call_with_cc(addr, override_cc)` that overrides those two
  lists for a single Call (and adjusts the post-call SP-pop using
  the override's `ret_stack_pop`).
* The orchestrator's in-place tail-call edit
  ([`crates/strider/src/orchestrator.rs`]) materialises a Call node
  via `apply_tail_call`, also using the function-default CC.  The
  override map must reach that path too.
* `FunctionBuilder::build_call_other`
  ([`crates/ir/src/builder/call.rs`]) currently emits outputs
  `[Control, Memory]` and (when `output_ty.is_some()`)
  `[Control, Memory, value_output]` — no clobber slots.  The
  validator's signature already permits a variadic value-output tail
  (`out_tail: ANY_VAL` in
  [`crates/ir/src/node_signature.rs`]), so extending CallOther to
  emit clobber outputs is signature-compatible without any validator
  change.

## Design

### Feature 1 — Graph compaction

#### `Graph::retain_reachable(entry)`

New method on `ir::Graph` (in `crates/ir/src/graph/store.rs`).
Signature:

```rust
pub fn retain_reachable(&mut self, entry: NodeId) -> NodeIdRemap {
    // 1. reachable: NodeIdSet = walk::walk_graph(self, entry).collect()
    // 2. allocate fresh nodes / inputs / outputs PrimaryMaps
    // 3. for each reachable node in primary-map order:
    //      - copy the node into the fresh nodes map (returns new NodeId)
    //      - copy each output into the fresh outputs map (returns new NodeOutputId)
    //      - copy each input into the fresh inputs map (returns new NodeInputId)
    //      - record old→new for nodes / outputs / inputs in NodeIdRemap
    // 4. rewrite each copied node's input.output_id and input.target_node
    //    through the remap (uses the new NodeOutputId / NodeId values)
    // 5. rebuild link_input_to_output_list for every fresh input
    // 6. rebuild node_to_id dedup cache from scratch by walking the
    //    fresh nodes and re-keying every cacheable kind
    // 7. remap side-tables (asm_fingerprints, stack_phi_offsets,
    //    call_other_names, and the new call_clobbered_overrides)
    //    by iterating the old SecondaryMap and writing into a fresh
    //    SecondaryMap keyed on the new NodeId
    // 8. swap fresh maps into self
}
```

`NodeIdRemap` is a typed three-field struct exposing `old_to_new` for
`NodeId` / `NodeOutputId` / `NodeInputId` (sparse vec or hash map —
sparse vec is fine since old IDs are dense before compaction).
Returned for callers that hold raw IDs externally; `Graph` itself
internally uses the remap to fix up its own state.

The implementation is mechanical but must respect the Graph's
internal invariants.  Specifically:
- Use-list pointers in `NodeOutput` (head of input list) and
  `NodeInput` (next-input-of-this-output) must be rebuilt by
  `link_input_to_output_list`, exactly as `create_node` does today.
  Do not try to remap the existing pointer values — easier to clear
  and re-link.

#### `BuiltFunctionGraph::compact()`

Wraps `Graph::retain_reachable(self.entry)` and remaps `self.entry`
through the returned `NodeIdRemap`.  No other `NodeId` field exists
on `BuiltFunctionGraph` today (it holds `Box<[Vn]>` + `Box<[Vn]>` for
arg-passing / clobbered varnodes — both vn-keyed, not NodeId-keyed).

Audit step (catch future drift): scan `BuiltFunctionGraph` for any
`NodeId`-typed field at implementation time; if a future field
appears, it MUST be remapped here.

#### `RunConfig::compact: bool`

New field on `crates/strider/src/orchestrator.rs::RunConfig`,
defaulting to `true` (constructor unchanged for downstream code is
not idiomatic Rust — instead, the field is documented as
`compact: true` recommended and existing call-sites are updated to
explicitly set it).

`LoopState::finalize` calls `graph.compact()` after the destructive
pipeline returns when `self.opts.compact` is true.

#### Python: `strider.run(..., compact=True)`

Add a `compact: bool = True` keyword argument on `strider.run`
([`crates/strider-py/src/run.rs`]).  Plumbs into `RunConfig.compact`
on the orchestrator path.  On the custom-pipeline path
(`run_with_custom_pipeline`), expose the same flag — when `True`,
call `BuiltFunctionGraph::compact()` after the user's pipeline
finishes.  Default-on for both paths.

#### Validator interaction

Layer A is already reachability-scoped via `walk_graph`, so
compaction is correctness-neutral for the validator.  The Layer-C
asm-fingerprint check (when enabled via `validate_with_options`) also
continues to pass: surviving nodes' fingerprints are preserved
verbatim across compaction.

### Feature 2 — Per-address calling-convention override

#### `RunConfig::per_address_ccs`

New field on `crates/strider/src/orchestrator.rs::RunConfig`:

```rust
pub per_address_ccs: HashMap<u64, target::CallingConvention>,
```

Default empty.  Held by `LoopState` for the lifetime of `run`.

#### Pre-resolve once at `LoopState::new`

```rust
let per_address_built_ccs: HashMap<u64, target::BuiltCallingConvention> =
    config.per_address_ccs
        .iter()
        .map(|(addr, cc)| Ok((*addr, cc.build(&sleigh_regs)?)))
        .collect::<Result<_>>()?;
```

The probe Sleigh handle (`config.sleigh.regs()`) is reused for
resolution.  Errors propagate as the existing `anyhow::Error`.

#### `Strider::analyze_cfg_with_overrides`

New method:

```rust
pub fn analyze_cfg_with_vns_and_overrides<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
    all_vns: Vec<rsleigh::Vn>,
    per_address_built_ccs: &HashMap<u64, target::BuiltCallingConvention>,
) -> Result<AnalyzeOutcome>;
```

The existing `analyze_cfg` and `analyze_cfg_with_vns` stay; both
delegate to the new variant with an empty map.

`IrStrider` gains a borrowed reference to the overrides map for the
lifetime of the lift.

#### `IrStrider::process_call_insn` direct-call dispatch

When the call insn's target is an `IntConst(K)`, look up `K` in the
overrides map.  If present, call `builder.build_call_with_cc(addr,
Some(&built_cc))`; otherwise the existing `builder.build_call(addr)`
path (which becomes a thin shim for `build_call_with_cc(addr, None)`).

Indirect calls (target is a non-constant value) cannot match an
override at lift time — they may be resolved later by the orchestrator
to `Single(K)` and then routed via the in-place edit path
(below) which DOES consult the override map.

#### `FunctionBuilder::build_call_with_cc`

New method on `crates/ir/src/builder/call.rs`:

```rust
pub fn build_call_with_cc(
    &mut self,
    call_address: NodeOutputId,
    override_cc: Option<&BuiltCallingConvention>,
) -> Result<NodeId>;  // returns the fresh Call node id
```

`build_call(addr)` becomes `build_call_with_cc(addr, None)` and
preserves the existing `Result<()>` shape (drops the returned id).

When `override_cc` is `None`, behaviour is identical to today.

When `override_cc` is `Some(cc)`:
- arg_passing_vars source = `cc.arg_passing_regs` (resolved against
  the function's tracked-variable table via the same
  `upgrade_to_tracked_for` shape `FunctionBuilder::new` already uses;
  varnodes that the override declares but the function never reads
  are filtered out — they would otherwise produce a `VariableNotFound`
  error from `read_variable`).
- callee_saved set = `cc.callee_saved_regs`
- clobber set = function's tracked variables MINUS callee_saved MINUS
  stack_ptr_vn.  Computed by re-running the existing
  `is_clobbered` filter against the override's callee_saved set.
- `ret_stack_pop` source = `cc.ret_stack_pop` (drives the post-call
  SP-add constant).
- The fresh Call's clobber list is recorded in the new
  `Graph::call_clobbered_overrides` SecondaryMap (see below).

The `arg_passing_vars` filter for the override needs a small helper
on `FunctionBuilder` exposing the existing tracked-variable set so
the override-time resolution can mirror lift-time resolution.  The
helper is private; only `build_call_with_cc` calls it.

The function-default `call_clobbered_variables` field stays
populated as today and is consumed by `build_call_with_cc(addr,
None)`.

#### `Graph::call_clobbered_overrides` side-table

```rust
pub call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>;
```

`None` (the default) means "use the function-default
`BuiltFunctionGraph::call_clobbered`."  `Some(list)` means "this Call
emitted with an override; index its clobber output slots through
this list."

Asm-fingerprint comparison: this side-table is purely additive,
indexed by NodeId, treated by `retain_reachable` exactly as the
other three side-tables (remap through old→new, drop unreachable
entries).

#### `pattern::Match::get_vn`

Update to consult the per-Call override first:

```rust
if matches!(graph.graph.node_kind(node), NodeKind::Call) && slot >= 2 {
    let idx = (slot - 2) as usize;
    if let Some(override_list) = graph.graph.call_clobbered_overrides.get(node).and_then(|opt| opt.as_ref()) {
        return override_list.get(idx).copied();
    }
    return graph.call_clobbered.get(idx).copied();
}
```

The doc comment on `get_vn` is updated to mention the override.

#### Orchestrator in-place tail-call edits

The orchestrator's `apply_in_place_edit` for `Single(K)` calls
`build_anchor_calling_context` to materialise arg-passing /
clobber / ret-val outputs in the function-default convention, then
hands them to `apply_tail_call`.

For per-address override support:

1. `build_anchor_calling_context` gains a `target_address: u64`
   parameter.  When `target_address` is in the orchestrator's
   `per_address_built_ccs` map, the function uses the override CC
   instead of `strider.calling_convention()` for `arg_passing_regs`,
   `ret_val_regs`, and the clobber-kinds list.
2. `apply_tail_call` receives the same per-call clobber list and
   records it on the new `Graph::call_clobbered_overrides`
   side-table for the freshly-created Call node.
3. `LinkRegister` resolutions don't materialise a Call (they
   produce a Return), so no change there.

The orchestrator passes its `per_address_built_ccs` map to
`Strider::analyze_cfg_with_vns_and_overrides` and reads the same map
for in-place edits.

#### Python: `strider.run(..., per_address_ccs={})`

Add a `per_address_ccs: dict[int, CallingConvention] = {}` keyword
argument on `strider.run`.  At call time, iterate the dict and
unwrap each `PyCallingConvention.inner` into the `RunConfig`'s
`HashMap<u64, target::CallingConvention>`.

Errors from `cc.build(&sleigh_regs)` (unresolved register names)
surface as `LiftError`.

For the custom-pipeline path: ignore `per_address_ccs` (custom-
pipeline is a single-iteration analyze with no orchestrator).
Surface a warning in the Python docstring.  No-op silently is fine
because no orchestrator-driven Call resolution happens on that path.

### Feature 3 — Conservative `CallOther` clobber default

#### `BuiltFunctionGraph::call_other_clobbered`

New field on `crates/ir/src/function.rs`:

```rust
pub call_other_clobbered: Box<[rsleigh::Vn]>,
```

Populated by `FunctionBuilder::build()` from the function's tracked
variables (the `variables` PrimaryMap), filtered to exclude the
stack pointer.  Order is the tracked-variable order
(`variables.values()` iteration), which is deterministic across runs
because variables are inserted in `pcode_lift::vn_sort_key` order at
`FunctionBuilder::new` time.

This is the function-default clobber list for every CallOther node.
A per-CallOther override (Future work — feature 3's follow-up) would
populate `Graph::call_clobbered_overrides[callother_node]` to shadow
this default for a specific user-op.

#### `FunctionBuilder::build_call_other`

Update the existing method to emit a clobber slot per entry of the
function's tracked-variable set (excluding SP) AND rebind each
clobbered variable to its corresponding output, mirroring how
`build_call` handles `call_clobbered_variables`.

Output layout becomes:

```
slot 0: Control
slot 1: Memory
slot 2: Value output (if `output_ty.is_some()`)
slot N..: Clobber outputs, one per `BuiltFunctionGraph::call_other_clobbered` entry
         where N = 2 if no value output, 3 if value output present
```

The variable-rebind loop runs after `advance_cur_region_ctrl` /
`advance_cur_region_memory` / the optional value-output emission;
each clobber output's variable mapping is updated via
`write_variable`.

The signature returned to callers stays
`Result<(NodeId, Option<NodeOutputId>)>` — `node_id` is the new
CallOther; `Option<NodeOutputId>` is the value output (when
`output_ty.is_some()`).  Existing callers (`strider/src/strider/insn/mod.rs`)
are unaffected at the call site; the IR shape downstream changes,
which existing tests assert on and must be updated.

#### `pattern::Match::get_vn` for `CallOther`

Today `get_vn` recognises `Call` clobber slots only (`slot >= 2`).
Update to also recognise `CallOther` clobber slots:

* For `Call`: clobber starts at slot 2 (unchanged).
* For `CallOther`: clobber starts at slot 2 (no value output) or
  slot 3 (with value output).  Determine which by inspecting the
  node's outputs slice — if any non-Control / non-Memory output
  appears at slot 2 with a non-clobber kind (specifically: the
  CallOther's value output kind from the user-op's source insn),
  shift by one.  Simpler and more robust: query
  `node.outputs.len()` against `BuiltFunctionGraph::call_other_clobbered.len()`
  to derive the clobber start.

For the lookup itself: `Graph::call_clobbered_override(node)` returns
`Some(list)` when a per-CallOther override was set; otherwise fall
back to `BuiltFunctionGraph::call_other_clobbered[idx]`.

The shared `call_clobbered_overrides` SecondaryMap (introduced by
feature 2 for `Call`) is reused here for `CallOther` — same key
type, same value type, same semantics ("`None` means use the
function-default for this node's kind").

### Tests

#### Feature 1 — compaction

* `crates/ir/tests/retain_reachable.rs`:
  - Synthetic graph: entry + live chain + detached zombie subgraph
    (created by detaching inputs of an interior node).  After
    `retain_reachable`, the zombie nodes are absent from
    `all_node_ids` and the live chain is intact (input/output
    queries return the same value-typed edges).
  - Side-table preservation: place an asm-fingerprint on a surviving
    node; assert it's preserved bit-exactly post-compaction.  Place
    one on a doomed node; assert it's no longer queryable
    post-compaction.
  - Dedup cache rebuilt: after compaction, `create_node(kind, inputs,
    outputs)` for a kind+inputs that match a surviving node returns
    that surviving node's new ID (no duplicate created).
  - NodeIdRemap correctness: every old NodeId in the reachable set
    maps to the surviving node with identical kind / input shapes;
    every dropped NodeId has no entry in `old_to_new` (returns
    `None`).
* `crates/strider/tests/compact.rs`:
  - Build a small fixture binary, run `strider::run(compact=true)`
    and `strider::run(compact=false)`; assert the compact graph has
    strictly fewer node ids than the non-compact graph (at least one
    redundant phi or dead branch in the chosen fixture).
  - Run the same handful of pattern queries against both graphs;
    assert the same number of matches in both.  This pins the
    semantic-equivalence guarantee.
* `crates/strider-py/tests/python/test_compact.py`:
  - Mirror of the above end-to-end check from Python.  `strider.run`
    with `compact=True` (the default) yields a graph with strictly
    fewer node ids than the same call with `compact=False`.

#### Feature 2 — per-address CC

* `crates/ir/tests/build_call_with_cc.rs`:
  - Synthetic builder test: `build_call_with_cc(addr, Some(&all_preserving_cc))`
    produces a Call with no arg inputs, no clobber outputs, and
    populates `Graph::call_clobbered_overrides[node] = Some(vec![])`.
  - `build_call_with_cc(addr, None)` produces an identical Call to
    `build_call(addr)` — pinned by output kinds + input list.
* `crates/strider/tests/per_address_cc.rs`:
  - Synthesise an x86_64 binary with two calls, one targeting an
    address designated as "fentry" (all-preserving CC), one
    targeting a normal SystemV function.  After `strider::run` with
    `per_address_ccs = {fentry_addr: all_preserving_cc}`, assert the
    fentry Call has 0 clobber outputs and 0 arg inputs while the
    other has the full SystemV shape.
  - Pattern query: `call().at(fentry_addr).arg(0, …)` matches zero
    times; `call().at(other_addr).arg(0, …)` matches once.
* `crates/strider/tests/per_address_cc_indirect.rs`:
  - Indirect branch resolves to `Single(fentry_addr)` as a tail
    call.  In-place edit must respect the override — the resulting
    Call has 0 clobber outputs.
* `crates/strider-py/tests/python/test_per_address_cc.py`:
  - Build an "all-preserving" CC in Python (every register
    callee-saved).  Pass `per_address_ccs={fentry_addr:
    all_preserving_cc}` to `strider.run`.  Assert the fentry Call
    has zero clobber outputs.

#### Feature 3 — conservative `CallOther` clobber

* `crates/ir/src/builder/tests.rs` (or a new `crates/ir/tests/`
  file, mirroring `build_call_with_cc`):
  - `build_call_other` with no value output and a tracked-variable
    set `[rax, rbx, rsp]` produces a CallOther with output count
    `2 (Control + Memory) + 2 (rax, rbx)` = 4.  SP is excluded.
  - `build_call_other` with `output_ty = Some(U32)` and the same
    tracked set produces output count `3 (Control + Memory + value)
    + 2 (rax, rbx)` = 5.
  - After `build_call_other`, `read_variable(&rax)` returns a
    `NodeOutputId` whose producer is the freshly-created CallOther
    (i.e. the variable was rebound).  Pre-CallOther reads of `rax`
    are unaffected (no retroactive rewrite).
  - `BuiltFunctionGraph::call_other_clobbered` after
    `FunctionBuilder::build()` lists `[rax, rbx]` in tracked-
    variable order (SP excluded).
* `crates/pattern/tests/get_vn_with_callother.rs`:
  - Synthetic graph with one CallOther whose clobber output 0 is
    bound to a capture; assert
    `Match::get_vn(c, &graph)` returns the corresponding entry from
    `BuiltFunctionGraph::call_other_clobbered`.
  - Same with a per-CallOther override set on
    `Graph::call_clobbered_overrides`; assert `get_vn` returns the
    override entry instead.
* Existing tests that build CallOther nodes and assert specific
  output counts (search for `build_call_other` in test files) are
  updated to match the new conservative-clobber shape.  Tests that
  detect a CallOther via `preorder_kind(NodeKind::CallOther…)`
  continue to work without change.
* End-to-end: a fixture that contains a `syscall` instruction (e.g.
  a tiny x86_64 hand-encoded `mov rax, 1; syscall; ret`) — assert
  that after `strider::run`, a `Return` does NOT see the
  pre-`syscall` value of `rax` flow through; instead it sees the
  CallOther's clobber slot for `rax`.  Pinned by walking the
  Return's input chain back through the IR.

## Decisions

* **Compaction at finalize, not as a pass.**  Compaction is a global
  rebuild that re-numbers NodeIds; running it inside the optimiser's
  fixed-point loop would invalidate any pass's cached per-iteration
  state.  Putting it at finalize, after the destructive pass, sidesteps
  this entirely.
* **NodeId remap returned, not silently dropped.**  Even though
  internal callers don't need it (the orchestrator releases its
  NodeId state at finalize), surfacing the remap leaves the door open
  for callers that DO want stable references across compaction.  Cost
  is minimal (one sparse vec).
* **Per-Call clobber side-table on `Graph`, not on `BuiltFunctionGraph`.**
  The override list is conceptually per-node, so it lives where the
  node lives.  Keeps `BuiltFunctionGraph::call_clobbered`'s existing
  meaning (function-default).
* **Override is per-target only.**  Per-caller dispatch is more
  general but unmotivated by the kernel-fentry use case.  Caller-
  side conventions can still be expressed via the function-default
  CC on `Strider::new`.
* **`build_call_with_cc(addr, None)` as the unified path.**
  `build_call(addr)` becomes a one-line shim.  Avoids duplicating
  the Call-emission logic between two methods.
* **Override CC is resolved (via `cc.build()`) once at `LoopState::new`,
  not per-Call.**  Resolution is mechanical but allocates and might
  surface unresolved-register-name errors; doing it once up front
  surfaces those errors before iteration starts.
* **`CallOther` defaults to clobbering everything (except SP).**  The
  current "preserves all" default is unsound for opaque user-ops like
  `syscall` (which clobbers every userland register the syscall ABI
  doesn't preserve).  Conservative-clobber is the correctness floor;
  per-user-op CC overrides (deferred follow-up) can later relax this
  for specific user-ops whose ABI we know precisely.  SP is excluded
  because (a) the `syscall` instruction itself does not push to the
  stack on any supported arch, and (b) flagging SP as clobbered would
  break the SP-rebinding invariant the rest of the pipeline relies
  on.
* **`call_other_clobbered` lives on `BuiltFunctionGraph`, distinct
  from `call_clobbered`.**  The two have different semantics:
  `call_clobbered` is "caller-clobbered registers per the function-
  default CC" (excludes callee-saved AND SP); `call_other_clobbered`
  is "all tracked variables except SP" (excludes only SP).  Storing
  them as separate fields keeps each one's index meaning unambiguous
  for `pattern::Match::get_vn`.

## Out-of-scope follow-ups

1. NodeId-stable compaction (a `removed: bool` flag) — only worth
   considering if a future user pattern emerges where external
   long-lived NodeId references must survive a `run` call.
2. Symbol-table-driven override map (resolve `__fentry__` symbol
   automatically from the ELF).  A thin Python helper on top of
   `pyelftools` (or similar) is more appropriate than baking it into
   `strider`.
3. Per-caller CC dispatch.
4. CC override for `CallOther` (user-op) nodes.  Today user-ops
   carry no clobber list, so there's nothing to override; if/when
   user-op clobber semantics get richer, this might become relevant.
