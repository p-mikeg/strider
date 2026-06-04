# Builder-Trait Unification Design

**Date:** 2026-06-04
**Status:** Approved (pending spec review)

## Goal

Unify the three independent node-creation paths in the IR behind a single
creation trait in `strider-ir`, so that the editing context (today's
`strider-opt` `RewriteCtx`, renamed `EditFunction` and moved into
`strider-ir`) tracks liveness and stamps fingerprints **at the moment each
node is created** — rather than reconciling them in a retroactive pass after
the fact. This deletes the most bug-prone code in the optimizer.

## Background — why

Three places create IR nodes, and they share no machinery:

1. **`FunctionBuilder::create_node`** (`strider-ir/src/builder/mod.rs:355`) —
   lift path. Creates the node, then stamps the ambient `lift_addr`
   fingerprint. No liveness (lift only adds nodes, never kills).
2. **`RewriteCtx::create_node`** (`strider-opt/src/rewrite/mod.rs:769`) —
   optimizer path. Creates the node, then `track_created` to maintain the
   `live_nodes` / `roots` caches.
3. **`template::instantiate`** (`strider-pattern/src/template/mod.rs:195`) —
   rewrite-RHS path. Builds the RHS subtree straight onto
   `function.graph_mut().create_node`, with **no** context. The rewrite
   harness then runs a *retroactive* reconciliation
   (`rewrite/mod.rs:181` `absorb_fingerprints_into_fresh_subtree` +
   `:189` `track_fresh_subtree`) that re-walks the fresh subtree to
   back-fill fingerprints and liveness by diffing against a pre-build
   `NodeId` snapshot.

Path 3 is the fragile one. Both soundness bugs caught in the prior round (the
dead-cone liveness leak and the dedup-revived constant) lived in that
snapshot-diff reconciliation. It exists *only* because `instantiate` cannot
see the bookkeeping context — it holds a raw `&mut Function`, not a tracker.

## Architecture

### The `Builder` trait (in `strider-ir`)

A minimal, creation-only trait. Static dispatch only (no object safety
needed — `instantiate` is generic over it).

```rust
// crates/strider-ir/src/builder/  (new trait, alongside FunctionBuilder)
pub trait Builder {
    /// Create (or dedup to) a node, applying this builder's own
    /// fingerprint-attribution and bookkeeping policy.
    fn create_node<I, O>(&mut self, kind: NodeKind, inputs: I, outputs: O) -> NodeId
    where
        I: IntoIterator<Item = ValueId>,
        O: IntoIterator<Item = ValueKind>;

    /// Read access to the function under construction/edit. Used by
    /// `instantiate` to read a just-built node's outputs and to hand a
    /// `&Function` to dynamic template closures.
    fn function(&self) -> &Function;
}
```

The trait carries **only creation + read**. Liveness is *not* in the trait
contract — it is an implementation detail of the editing impl. This keeps the
lift path free of any tracking burden.

### Two implementors, symmetric

- **`FunctionBuilder: Builder`** — `create_node` is its existing body:
  create on the graph, stamp the ambient `lift_addr`. No liveness.
- **`EditFunction: Builder`** — `create_node`: create on the graph, stamp the
  ambient attribution source (see below), then `track_created`. Maintains
  `live_nodes` / `roots`.

The two builders become mirror images: each carries an *ambient fingerprint
source* that `create_node` consults.

### `EditFunction` — the moved editing context

`RewriteCtx` + `FunctionState` move from `strider-opt/src/rewrite/` into a new
`strider-ir/src/edit/` module, renamed `EditFunction`. Rationale: graph-editing
knowledge (cacheability, use-list maintenance, detach, liveness culling)
depends only on the graph and knows nothing about optimization — its natural
home is the IR crate. `compute_full`, `DenseEntitySet`, `Worklist` are all
already in / available to `strider-ir`, so no new dependency edges.

`EditFunction` keeps its current internals verbatim, plus one new field:

```rust
pub struct EditFunction<'g> {
    function: &'g mut Function,
    state: StateSlot<'g>,          // Borrowed | Owned, unchanged
    attribution: Option<NodeId>,   // NEW: ambient fingerprint source
}
```

`FunctionState` (`live_nodes`, `roots`, `queue`, `flags`) moves unchanged.
All edit verbs (`kill_node`, `replace_value`, `clean`, `update_input`,
`add_node_input`, `remove_node_input`, `redirect_input`, `make_int_const`,
`track_created`, the cached `postorder` / `reverse_postorder`) move unchanged.

### Ambient fingerprint attribution

`FunctionBuilder` already attributes from an ambient `lift_addr: Option<u64>`.
`EditFunction` gets the symmetric mechanism: an ambient `attribution:
Option<NodeId>` naming the source node whose fingerprint each freshly created
node should absorb. Set via a scoped helper so it can't leak:

```rust
impl EditFunction<'_> {
    fn with_attribution<R>(&mut self, src: NodeId, f: impl FnOnce(&mut Self) -> R) -> R {
        let prev = self.attribution.replace(src);
        let r = f(self);
        self.attribution = prev;
        r
    }
}

impl Builder for EditFunction<'_> {
    fn create_node<I, O>(&mut self, kind, inputs, outputs) -> NodeId {
        let node = self.function.graph_mut().create_node(kind, inputs, outputs);
        if let Some(src) = self.attribution {
            self.function.extend_asm_fingerprint_from(node, src); // superset-preserving union
        }
        self.track_created(node);
        node
    }
    fn function(&self) -> &Function { self.function }
}
```

### `template::instantiate` becomes generic

```rust
pub fn instantiate<B: Builder>(
    template: &Template,
    builder: &mut B,
    bindings: &Bindings,
    lhs_root: NodeId,
    root_ty: ValueType,
) -> anyhow::Result<ValueId>
```

Inside, `function.graph_mut().create_node(...)` becomes `builder.create_node(...)`;
the read sites (`node_outputs`, `first_value_output`) and the dynamic-closure
`TemplateCtx { function: builder.function(), .. }` use the read accessor.

`rewrite_rule` wraps the call:

```rust
let new_value = ctx.with_attribution(matched_root, |b| {
    template::instantiate(&template, b, &bindings, matched_root, root_ty)
})?;
ctx.replace_value(root_value, new_value)?;   // unchanged
```

### What this deletes

`absorb_fingerprints_into_fresh_subtree` and `track_fresh_subtree` (and the
pre-build `NodeId` snapshot threading) are **removed entirely**. With
stamp-and-track at creation, dedup-revival stops being a special case:

- A created node that dedups to an already-live node → `extend_asm_fingerprint_from`
  unions the fingerprint (superset-preserving) and `track_created` is an
  idempotent set insert.
- A created node that dedups to a culled-but-still-present node → `track_created`
  re-inserts it into `live_nodes` / `roots`.

No snapshot, no diff, no walk.

## Crate boundaries — what moves vs. stays

| Item | From | To |
|---|---|---|
| `Builder` trait | (new) | `strider-ir` (builder module) |
| `EditFunction` (was `RewriteCtx`) + `StateSlot` | `strider-opt/src/rewrite/mod.rs` | `strider-ir/src/edit/` |
| `FunctionState` + `NodeFlags` | `strider-opt/src/rewrite/function_state.rs` | `strider-ir/src/edit/` |
| edit verbs + cached walks | `strider-opt` | `strider-ir/src/edit/` |
| `rewrite_rule` / `rewrite_rule_impl` / `GraphRewriter` / `check_capture_coverage` / `boxed_rule` | `strider-opt/src/rewrite/` | **stays in `strider-opt`** |
| `OptCtx` | `strider-opt` | **stays in `strider-opt`** |
| passes / `OptimizerPipeline` | `strider-opt` | **stays in `strider-opt`** |
| `template::instantiate` | `strider-pattern` | **stays in `strider-pattern`** (made generic) |

`rewrite_rule` glues `strider-pattern` (Matcher/Template) to `strider-ir`
(`EditFunction`); it stays in `strider-opt` so that **no rewrite logic returns
to `strider-pattern`** (the prior round's explicit constraint). `strider-pattern`
already depends on `strider-ir`, so `instantiate`'s `B: Builder` bound needs no
new edge. The `Optimizer::apply` and `OptimizerPipeline::run` signatures change
only in that `RewriteCtx` is now `strider_ir::EditFunction`.

## Testing strategy (TDD)

- **`strider-ir` (new):** unit tests for the `Builder` trait and `EditFunction`
  edit verbs that need no patterns — `create_node` tracks into `live_nodes`,
  attribution stamps at creation, `create_node` that dedups to a live node
  unions the fingerprint without double-tracking, `create_node` that revives a
  culled node re-tracks it, `kill_node` / `replace_value` / `clean` invariants.
  Graphs built via `FunctionBuilder` (the crate cannot use
  `strider-ir-test-utils` for its own types — dev-dep double-compile).
- **`strider-opt` (kept + extended):** the 8 existing tracking tests stay (they
  exercise the full rewrite through `rewrite_rule` / `GraphRewriter` and need
  `strider-pattern`). The post-condition invariant
  `cached live_nodes == compute_full(entry)` must still hold after each rewrite,
  now with the retroactive pass gone. Add tests asserting the deleted pass's
  guarantees are preserved by stamp-at-creation: multi-output template interior
  fingerprints, RHS dedup-reviving a culled const.
- **Gate:** `cargo test --workspace` + `cargo clippy --workspace --all-targets`
  + `uv run pytest` all green before merge.

## Non-goals (deferred)

- **Graph crate split / generic `Graph<N, V>`** — deferred; discuss after this
  lands. (Single-consumer; would need trait plumbing for `is_cacheable` and
  `IntConst` payload normalization.)
- **Moving `wide_const_interner` onto `Function`** — deferred with the graph
  discussion.
- **Routing matcher/template *match-graphs* through this object** — the user's
  "later we can replace the template / matcher graphs to this object as well";
  a follow-up once the `Builder` trait exists.

## Sequencing (detailed in the plan)

1. Introduce `Builder` trait in `strider-ir`; `FunctionBuilder` implements it
   (refactor its inherent `create_node` into the trait impl). No behavior
   change.
2. Move `FunctionState` + `EditFunction` (renamed from `RewriteCtx`) into
   `strider-ir/src/edit/`, implementing `Builder`. Update `strider-opt`
   (`rewrite_rule`, `OptCtx`, passes, pipeline, `Optimizer` trait) to reference
   `strider_ir::EditFunction`. Pure move + rename; tests green.
3. Add ambient attribution to `EditFunction`; make `template::instantiate`
   generic over `B: Builder`; delete `absorb_fingerprints_into_fresh_subtree` +
   `track_fresh_subtree`; route `rewrite_rule` through `with_attribution`.
   Behavior-equivalent simplification; tracking tests stay green; add the new
   tests above.
