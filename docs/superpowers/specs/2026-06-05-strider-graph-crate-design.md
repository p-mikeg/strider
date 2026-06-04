# Generic `strider-graph` Crate Design

**Date:** 2026-06-05
**Status:** Approved (pending spec review)
**Scope:** One bite — extract the generic graph data structure into a
`strider-graph` crate, re-base `strider-ir`'s `Graph` onto it, migrate
`strider-pattern`'s `BiGraph` onto it, and move `wide_const_interner` to
`Function`. Gated by the full workspace suite per task.

## Goal

Make the sea-of-nodes graph layout a single, reusable SSoT: one
`Graph<N, V, C>` data structure, instantiated by `strider-ir` (with caching)
and by `strider-pattern` (without). Today the pattern/template crate hand-rolls
a parallel bipartite graph (`BiGraph` over `petgraph`) to *emulate* the IR
graph; this collapses the two into one. The IR's `Graph` sheds everything
strider-specific (wide consts, value normalization, control-flow semantics) so
it can be generic over node/value payload types.

## Why this isn't speculative genericity

`Graph` has a **real second consumer**: `strider-pattern`'s `BiGraph<N, O>`
(over `petgraph::StableDiGraph<BiVertex<N,O>, BiEdge>`). Both are bipartite
node/value graphs. We are collapsing two maintained graphs into one, not
inventing genericity for a hypothetical future user.

## Architecture

### `strider-graph::Graph<N, V, C: NodeCacheable<N, V>>` — pure layout

Storage = the IR's current cranelift-entity representation (NOT petgraph):
- `PrimaryMap<NodeId, Node<N>>`, `PrimaryMap<ValueId, ValueData<V>>` (each value
  knows its producer node + slot), `PrimaryMap<UseId, UseData>` (node-input
  edges), the `ListPool`s, and the **use-lists** that make `value_uses` /
  `replace_all_uses` cheap.
- `generation` counter.
- The cacher `C` (see below).

**Payload-agnostic:** `N` and `V` carry **no `Hash`/`Eq` bound at the struct
level** — the pattern payloads (`PatNode`/`PatValue`/`TmplNode`) hold
`Box<dyn Fn>` predicate/closure fields and are not hashable. The `Hash`/`Eq`
requirement is confined to the IR cacher's `impl` (below).

Surface (all generic over `N, V`):
- Node/edge: `create_node`, `add_node_input`, `remove_node_input`,
  `update_input`, `detach_node_inputs`, `replace_all_uses`.
- Access: `node_kind`, `value_kind`, `producer`, `value_definition`,
  `node_inputs`/`node_outputs` (+ `_exact`), `nth_input`, `node_input_id_at`,
  `value_of_use`, `next_node_id`, `has_node`, `node_id_from_u32`, `generation`.
- Use-lists: `value_uses`, `value_use_cursor`, `value_has_one_use`,
  `value_first_use_id`, `next_use`.
- Iteration: the **`Inputs` iterator** + `InputCursor` (moved in — pure
  linked-list navigation), `all_node_ids`, a structural **def→use
  reverse-postorder** (`reverse_postorder`) and a basic input-following walk.
- **Compaction:** `retain_reachable(roots) -> NodeIdRemap` (arena GC + id
  remap). The remap is returned so the IR `Function` can fix its side-tables;
  the generic graph itself only compacts nodes/values/uses.
- **petgraph views:** impl `GraphBase`/`Visitable`/`IntoNeighbors[Directed]`/
  `IntoNodeIdentifiers`/`NodeCount` over a bipartite
  `Vertex = Node(NodeId) | Value(ValueId)` abstraction (node→produced values,
  value→consuming nodes). This is the generalization of `BiVertex`/`BiEdge`,
  and it gives `petgraph::algo::toposort` / `DfsPostOrder` / `Reversed` for free
  — exactly what the pattern crate's `reachable_topo` uses today, carried over
  unchanged on cranelift-entity storage (no petgraph *storage*).

**Not in the generic graph:** anything that reads node/value *semantics* —
control-flow reachability, `ValueType` normalization, wide consts. Those stay in
`strider-ir` (below), built on this layout.

### `NodeCacheable<N, V>` — owns the dedup-or-create

```rust
pub trait NodeCacheable<N, V> {
    /// Either return an existing structurally-equal node or allocate a fresh
    /// one via `alloc`. The cacher owns its cache; the `Hash`/`Eq` bound (if
    /// any) lives in the impl, not on `Graph`.
    fn create(&mut self, storage: &mut RawStore<N, V>, kind: N,
              inputs: SmallVec<[ValueId; 4]>, outputs: SmallVec<[V; 4]>) -> NodeId;
}
```
- **`IrCacheable`** (in `strider-ir`): holds the dedup cache; on a cacheable
  kind it looks up / inserts, unioning asm-fingerprints on a hit; on a
  non-cacheable kind (e.g. `Region`/`Phi` build-identity) it allocates. Its
  `impl` is where `N, V: Hash + Eq` is required — satisfied by
  `NodeKind`/`ValueKind`.
- **`NeverCacheable`** (in `strider-graph`, a ZST): always allocates. No key,
  no `Hash`/`Eq`. Patterns/templates use this — dedup only pays off on the
  large IR graph; pattern/template graphs are small (a handful of nodes per
  rule), so caching them buys nothing and would only force a `Hash`/`Eq` bound
  their closure-bearing payloads can't satisfy.

`Graph::create_node` delegates to `C::create`. The graph exposes the raw
allocate-and-wire primitive (`RawStore`) the cacher calls; the generic graph
never hashes a payload.

### `strider-ir` re-base

`Graph` becomes `strider_graph::Graph<NodeKind, ValueKind, IrCacheable>`.
Everything strider-specific moves off the layout:
- **`wide_const_interner` → `Function`.** `NodeKind::IntConstWide(WideConstId)`
  carries the id as **opaque payload data**; the generic graph knows nothing of
  wide consts. `Function` owns the value-deduped interner (`value →
  WideConstId`); the builder interns first (in `Function`), then creates the
  node; `IntConstWide` nodes dedup by id via `IrCacheable` like any cacheable
  node; `Function::compact` gc's the interner using the `NodeIdRemap`.
- **`IntConst` normalization stays in the builder.** `build_int_const` already
  masks (`val & ty.bit_mask_u128()`) before `create_node`; the generic graph
  trusts its input, so this strider-specific normalization never enters the
  graph.
- **Control-aware walks stay in `strider-ir`.** `cfg_reachable`,
  `graph_walk_succs` (data-backward + control-forward), `GraphWalkInfo::
  compute_full` all branch on `ValueKind::is_control()` — strider semantics.
  They operate on the generic `Graph`'s public API + strider's `ValueKind`. The
  generic graph provides the structural primitives (`node_outputs`,
  `value_uses`, `value_kind`); strider-ir filters by `is_control`.
- The **validator** and the **builders** (`FunctionBuilder`, `EditFunction`,
  `IRBuilder`/`IRBuilderExt`) stay in `strider-ir`. `Function` keeps `entry`,
  `cc_metadata`, the side-tables, and now the wide-const interner.

### `strider-pattern` migration (the `BiGraph` dissolves)

`Pattern`/`Template` become
`strider_graph::Graph<PatNode, PatValue, NeverCacheable>` /
`Graph<TmplNode, TmplValue, NeverCacheable>`. The hand-rolled `BiGraph` +
`BiVertex`/`BiEdge` are deleted; their accessors (`node_weight`,
`consumed_inputs`, `produced_outputs`, `reachable_topo`) map onto the generic
`Graph` API + the petgraph views.

- **Captures need no special graph support.** A capture is already a leaf *node*
  producing a capture-marked value; the `Option<Capture>` rides on the
  `V`/`N` payload (`PatValue.capture` / `TmplNode::Capture`), opaque to the
  graph. The "every value has a producer" invariant holds uniformly — no
  producerless-leaf concept needed.
- **The typed safety is preserved.** `MatchPat`/`TemplatePat` are trait bounds
  *on top of* the graph (a wildcard impls `MatchPat` but not `TemplatePat`, so
  `rewrite_rule<L: MatchPat, T: TemplatePat>` rejects a wildcard RHS at compile
  time). They are orthogonal to the storage and carry over untouched.
- `template::instantiate`'s `reachable_topo` walk carries over via the petgraph
  views.

## Testing — first-class, stress the data structure

`strider-graph` ships with a dedicated suite (this is the SSoT data structure;
it must be hardened):
- **Property-based (`proptest`, dev-dep):** a random-valid-graph generator +
  asserted invariants — use-list bidirectional consistency (every input edge
  ↔ its value's use-list), `replace_all_uses` redirects exactly the right uses,
  `retain_reachable` preserves reachability and remaps ids consistently,
  petgraph `toposort` agrees with an independent walk, round-trip
  detach/re-add.
- **Edge-case units:** repeated operands (`Add(x, x)`), multi-output nodes,
  leaf values, empty graph, self-/cyclic-input handling, large fan-in/fan-out
  stress, `NeverCacheable` (always-distinct) vs a test `is_cacheable` impl
  (structural sharing), compaction with dangling/unreachable nodes.
- **Integration proof:** the full existing `strider-ir` + `strider-pattern`
  suites become the behavior-preservation gate for the re-base/migration.

## Follow-ups (NOT in this bite)

- Swap `IrCacheable`'s internal cache for the **hash-on-demand `NodeCache`**
  (`HashTable<NodeId>` + `SecondaryMap<NodeId, u32>` — stores no payload vecs,
  O(1) `remove` so killed nodes leave the cache, `Region`/`Phi` build-identity
  exclusion). Behavior-equivalent, memory + removal win. Reference impl on hand.

## Risks / notes

- **Big surface:** ~221 `Graph`-method call sites across the workspace
  (strider-opt 89, strider-ir 50, strider-py 37, strider-pattern 34, …); the IR
  core *and* the pattern core both move. Sequenced so each task leaves the
  workspace gate green.
- **Dedup placement:** moving dedup from always-on (graph) into `IrCacheable`
  is behavior-preserving for production (all IR construction already routes
  through the builders post-IRBuilder work); raw `graph.create_node` in test
  fixtures stops deduping (acceptable — those build explicit mock shapes).
- **Naming:** the crate is `strider-graph` (strider-internal substrate that is
  generic over node/value kinds, not a standalone general-purpose lib).
- **petgraph:** added as a dependency of `strider-graph` (for the algo reuse via
  the views). It's already a workspace dep (strider-lift's cfg).
