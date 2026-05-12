# `graphwalk` — generic graph traversal

Pre-order and post-order DFS over any graph that can enumerate its successors,
with pluggable visit-tracking. Used by [`ir`](../ir)'s validator and walk
helpers (`ir::walk::walk_graph`) and by [`opt`](../opt)'s fixed-point passes.

## Public surface

- `GraphRef` — implement to expose successors. Required: `try_successors(node, f)`
  short-circuiting on `ControlFlow::Break`. `successors(node, f)` is the
  unconditional helper.
- `PredGraphRef` — extension of `GraphRef` adding `try_predecessors` /
  `predecessors`.
- `WalkPhase` — `Pre` | `Post`, reported by post-order walks that observe both
  enter- and leave-events.
- `VisitTracker<N>` — tracks visited nodes. `is_visited` / `mark_visited`.
  Built-in impls: `NopTracker` (never remembers — for tree walks), and
  `entity_utils::set::DenseEntitySet<N>` for `N: EntityRef`.
- `PreOrderContext<N>` / `PostOrderContext<N>` — reusable stack-based traversal
  state. `next(graph, visited)` pops the next node; `PostOrderContext::next_event`
  exposes both Pre and Post events.
- `PreOrder<G, V>` / `PostOrder<G, V>` — `Iterator` adapters over the contexts.
  `entity_preorder(graph, roots)` and `entity_postorder(graph, roots)` are
  convenience constructors that pick `DenseEntitySet` as the tracker.
- `TreePreOrder<G>` / `TreePostOrder<G>` — type aliases using `NopTracker`.

## Architecture

A single-file library (`src/lib.rs`). The split between "context" types
(`PreOrderContext`, `PostOrderContext`) and "iterator" types (`PreOrder`,
`PostOrder`) lets callers either drive the walk inline (with their own loop)
or treat it as an `Iterator`. The contexts are reusable: `reset(roots)` clears
and re-seeds the internal stack without reallocating.

`PostOrderContext` deliberately pushes roots in source order so that any
reverse-post-order derived from the walk preserves source order across
unrelated roots. `PreOrderContext` pushes in the same source order, which due
to LIFO stack semantics means roots are *visited* in reverse source order —
this asymmetry is documented on `PreOrderContext::reset`.

The crate is `no_std`; it depends only on `cranelift-entity` and
[`entity-utils`](../entity-utils) for `DenseEntitySet`.

## Key invariants

- Each node is yielded at most once per walk (when using a real `VisitTracker`;
  `NopTracker` yields a node every time it's pushed).
- `try_successors` / `try_predecessors` short-circuit on `ControlFlow::Break`,
  so callers can abandon a sub-traversal early.
- For post-order walks: a node's `Post` event always follows the `Post` events
  of all its (unvisited-at-pre-time) successors.
- `PostOrderContext::reset` push order guarantees source-order preservation
  in any derived RPO.

## Tests

Inline tests in `src/lib.rs`; integration tests in `crates/graphwalk/tests/`.

```
cargo test --package graphwalk
```

## Gotchas

- `PreOrderContext::reset` and `PostOrderContext::reset` have *opposite* root
  visit orders for the multi-root case. See the doc comment on
  `PreOrderContext::reset` if you need forward source-order from a pre-order
  walk over multiple roots — reverse the iterator yourself.
- `NopTracker` is correct only for genuine trees (or DAGs the caller knows are
  walked from a single root with no shared subgraphs). For the general case,
  pass `DenseEntitySet<N>` or a custom `VisitTracker`.
- Both `&G: GraphRef` and `&G: PredGraphRef` are blanket-implemented, so you can
  pass a borrowed graph reference everywhere a graph is required.
