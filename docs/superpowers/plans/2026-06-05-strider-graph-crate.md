# `strider-graph` Generic Crate — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Extract the sea-of-nodes graph layout into a generic `strider-graph::Graph<N, V, C: NodeCacheable<N, V>>` crate, re-base `strider-ir`'s `Graph` onto it (moving `wide_const_interner` to `Function`), and migrate `strider-pattern`'s `BiGraph` onto it — collapsing two hand-maintained graphs into one SSoT.

**Architecture:** A payload-agnostic cranelift-entity graph (nodes/values/uses + use-lists + `Inputs` iterator + structural walks + petgraph views over a bipartite `Vertex = Node|Value`). Caching is a `NodeCacheable<N,V>` policy that *owns the dedup-or-create*, so the `Hash`/`Eq` requirement lives only in the IR's `IrCacheable` impl; patterns use a `NeverCacheable` ZST. Strider-specific semantics (wide consts, value normalization, control-flow walks) stay in `strider-ir`/`Function`.

**Tech Stack:** Rust; `cranelift-entity`, `petgraph`, `smallvec`, `rustc-hash`; `proptest` (dev-dep) for property/stress tests. Crates: new `strider-graph`, plus `strider-ir`, `strider-pattern`, and the downstream `-opt`/`-orchestrator`/`-py`/`-lift`/`-ir-test-utils`.

**Working tree:** worktree `.worktrees/generic-graph`, branch `refactor/generic-graph-crate`. Push every commit: `git push origin refactor/generic-graph-crate`. Run the workspace gate (`cargo test --workspace`, `cargo clippy --workspace --all-targets`, and `uv run pytest` when strider-py is touched) at the end of each task — this refactor is behavior-preserving, so the existing suites are the regression net.

**Naming note:** the generic crate is `strider-graph` (lib name `strider_graph`).

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `crates/strider-graph/Cargo.toml` (new) | crate manifest (cranelift-entity, petgraph, smallvec, rustc-hash; proptest dev-dep) | 2 |
| `crates/strider-graph/src/lib.rs` (new) | crate root: re-export `Graph`, `NodeCacheable`, `NeverCacheable`, ids, `Inputs` | 2 |
| `crates/strider-graph/src/ids.rs` (new) | `NodeId`/`ValueId`/`UseId` entity refs + `Vertex` enum | 2 |
| `crates/strider-graph/src/storage.rs` (new) | `Node<N>`/`ValueData<V>`/`UseData` + the PrimaryMaps/pools (`RawStore`) | 2 |
| `crates/strider-graph/src/graph.rs` (new) | `Graph<N,V,C>` + structural verbs (create/edit/access/use-lists/compact) | 2 |
| `crates/strider-graph/src/cache.rs` (new) | `NodeCacheable<N,V>` trait + `NeverCacheable` ZST | 2 |
| `crates/strider-graph/src/iter.rs` (new) | `Inputs` iterator + `InputCursor` (moved from strider-ir) | 2 |
| `crates/strider-graph/src/walk.rs` (new) | structural def→use `reverse_postorder` + input-following walk | 2 |
| `crates/strider-graph/src/petgraph_view.rs` (new) | petgraph trait impls over bipartite `Vertex` | 2 |
| `crates/strider-graph/tests/proptest_invariants.rs` (new) | property/stress suite | 2 |
| `crates/strider-ir/src/function.rs` | gains `wide_const_interner` + its accessors + gc in `compact` | 1 |
| `crates/strider-ir/src/graph/mod.rs` etc. | `Graph` becomes `strider_graph::Graph<NodeKind, ValueKind, IrCacheable>`; structural code deleted | 3 |
| `crates/strider-ir/src/graph/cache.rs` (new) | `IrCacheable` (current Vec-keyed cache + fingerprint-union-on-hit) | 3 |
| `crates/strider-ir/src/walk/mod.rs` | control-aware walks (`cfg_reachable`, `GraphWalkInfo`) re-based onto the generic graph | 3 |
| `crates/strider-pattern/src/bigraph.rs` | **deleted**; `Pattern`/`Template` use `strider_graph::Graph<…, NeverCacheable>` | 4 |
| `crates/strider-pattern/src/matcher/*`, `template/*` | repointed to the generic graph API | 4 |

---

## Task 1: Move `wide_const_interner` from `Graph` to `Function`

Independent, behavior-preserving prep — does NOT touch the generic crate. De-risks Task 3 by getting the one strider-specific side-store off `Graph` first, on the current code.

**Files:**
- Modify: `crates/strider-ir/src/graph/mod.rs` (remove the field + `intern_wide_const`/`wide_const`/`wide_const_opt`), `crates/strider-ir/src/graph/compact.rs` (move `gc_wide_consts`), `crates/strider-ir/src/function.rs` (add the field + accessors + gc), `crates/strider-ir/src/builder/nodes.rs` (`build_int_const_wide` interns via `Function`).

- [ ] **Step 1: Add the interner to `Function`.** In `function.rs`, add field `pub(crate) wide_const_interner: entity_utils::EntityInterner<crate::wide_const::WideConstId, crate::wide_const::WideConstStorage>` and move the `intern_wide_const`/`wide_const`/`wide_const_opt` methods here verbatim (re-pointing `self.wide_const_interner`). Keep their signatures identical.

- [ ] **Step 2: Re-point `build_int_const_wide`.** In `builder/nodes.rs`, the wide-const builder currently calls `self.function_mut().graph_mut().intern_wide_const(...)`; change to `self.function_mut().intern_wide_const(...)` then `create_node(NodeKind::IntConstWide(id), …)`. The graph node just carries the id.

- [ ] **Step 3: Move the gc into `Function::compact`.** `Graph::retain_reachable` returns the `NodeIdRemap`. Move `gc_wide_consts` logic so `Function::compact` runs it after `retain_reachable`, rewriting surviving `IntConstWide` ids and rebuilding the interner over live ids. (Today `gc_wide_consts` lives on `Graph` and is called inside `retain_reachable`; relocate it to `Function::compact`, reading the remapped node kinds via the graph.) Remove `Graph::gc_wide_consts` + the `wide_const_interner` field from `Graph`.

- [ ] **Step 4: Fix callers.** `grep -rn "\.wide_const\b\|wide_const(\|wide_const_opt(\|intern_wide_const(" crates` — re-point every `graph.wide_const(...)` / `graph_mut().intern_wide_const(...)` to the `Function` accessor (callers that have a `Function` use `function.wide_const(...)`; the validator + dot renderer + py bindings). Build iteratively.

- [ ] **Step 5: Gate + commit.**
```
cargo test --workspace 2>&1 | tail -8
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A && git commit -m "refactor(strider-ir): move wide_const_interner from Graph to Function"
git push origin refactor/generic-graph-crate
```
Expected: full suite green (behavior-preserving — the interner is value-deduped identically, just on `Function`).

---

## Task 2: Scaffold the generic `strider-graph` crate

Build the payload-agnostic graph standalone, with its own property/stress tests using a *test* node/value/cacher. No strider dependency — this proves the data structure in isolation first.

**Files:** create the `crates/strider-graph/**` files from the table above. Register `crates/strider-graph` in the root `Cargo.toml` `[workspace] members` (glob `crates/*` likely already covers it) + add `strider-graph = { path = "crates/strider-graph" }` to `[workspace.dependencies]`.

The extraction source is `strider-ir/src/graph/{mod,store,uses,access,compact}.rs` + `iterators.rs` — port the STRUCTURAL machinery, made generic over `N` (node payload, replaces the strider `NodeKind` baked into `Node`) and `V` (value payload, replaces `ValueKind` in `ValueData`).

- [ ] **Step 1: `Cargo.toml` + ids.** Match an existing small crate's manifest style (`crates/graphwalk/Cargo.toml`) for edition/lints. Deps: `cranelift-entity`, `petgraph`, `smallvec`, `rustc-hash` (workspace refs); dev-dep `proptest`. In `ids.rs`, define `NodeId`/`ValueId`/`UseId` via `cranelift_entity::entity_impl!` (port from `strider-ir/src/node/ids.rs`), plus:
```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Vertex { Node(NodeId), Value(ValueId) }
```

- [ ] **Step 2: `cache.rs` — the policy.**
```rust
use crate::ids::{NodeId, ValueId};
use crate::storage::RawStore;
use smallvec::SmallVec;

/// Owns dedup-or-create. The `Hash`/`Eq` requirement (if any) lives in the
/// impl, never on `Graph`. `create` either returns an existing structurally-
/// equal node or allocates a fresh one via `store.alloc_node`.
pub trait NodeCacheable<N, V> {
    fn create(
        &mut self,
        store: &mut RawStore<N, V>,
        kind: N,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[V; 4]>,
    ) -> NodeId;
}

/// No caching — always allocate. A ZST; imposes no `Hash`/`Eq` bound. Used by
/// the pattern/template graphs (small, dedup buys nothing).
#[derive(Clone, Copy, Default)]
pub struct NeverCacheable;

impl<N, V> NodeCacheable<N, V> for NeverCacheable {
    fn create(&mut self, store: &mut RawStore<N, V>, kind: N,
              inputs: SmallVec<[ValueId; 4]>, outputs: SmallVec<[V; 4]>) -> NodeId {
        store.alloc_node(kind, inputs, outputs)
    }
}
```

- [ ] **Step 3: `storage.rs` — `RawStore`.** Port `Node`, `ValueData`, `UseData` (from `strider-ir/src/node/data.rs` / `graph/mod.rs`) made generic: `Node<N> { kind: N, outputs: EntityList<ValueId>, inputs: EntityList<UseId> }` (adapt to the real field set), `ValueData<V> { kind: V, producer: NodeId, slot: u32, first_use: PackedOption<UseId> }`, `UseData { value: ValueId, consumer: NodeId, slot: u32, next_use: PackedOption<UseId> }` (match the real layout). `RawStore<N, V>` holds the `PrimaryMap`s + `ListPool`s and exposes the **structural primitives** the cacher + graph need: `alloc_node(kind, inputs, outputs) -> NodeId` (allocates + links use-lists), raw read accessors (`node_kind`, `node_inputs`, `node_outputs`, `value_kind`, `producer`), and the use-list link/unlink. This is the part the cacher's equality check reads.

- [ ] **Step 4: `graph.rs` — `Graph<N,V,C>`.**
```rust
pub struct Graph<N, V, C: NodeCacheable<N, V>> {
    store: RawStore<N, V>,
    cacher: C,
    generation: u64,
}
```
`create_node(&mut self, kind, inputs, outputs) -> NodeId` collects inputs/outputs into `SmallVec` and delegates to `self.cacher.create(&mut self.store, …)`. Port the remaining structural verbs from `strider-ir/src/graph/{uses,access,rewrite,compact}.rs` onto `Graph`/`RawStore`, generic over `N,V`: `add_node_input`/`remove_node_input`/`update_input`/`detach_node_inputs`, `replace_all_uses`, `value_uses`/`value_use_cursor`/`value_has_one_use`/`value_first_use_id`/`next_use`, `node_kind`/`value_kind`/`producer`/`value_definition`/`node_inputs`(+exact)/`node_outputs`(+exact)/`nth_input`/`node_input_id_at`/`value_of_use`/`next_node_id`/`has_node`/`node_id_from_u32`/`generation`/`all_node_ids`, and `retain_reachable(roots) -> NodeIdRemap`. NOTE: the strider-specific cacheable-gating on `add/remove_node_input` (`is_cacheable` guard) does NOT belong here — instead, document that mutating a node that a cacher has cached is the *cacher's* invariant; the generic `add_node_input` just mutates. (IR's `IrCacheable` will not cache `Region`/`Phi`, the only mutated kinds — same effect.)

- [ ] **Step 5: `iter.rs` + `walk.rs`.** Move `Inputs`/`InputCursor` (from `strider-ir/src/graph/iterators.rs`) verbatim (generic — they're pure list navigation). In `walk.rs`, port the structural **def→use `reverse_postorder`** (follows input producers; no control semantics) + a basic input-following preorder. (Control-aware walks stay in strider-ir — do NOT move them.)

- [ ] **Step 6: `petgraph_view.rs`.** Impl petgraph's `GraphBase` (`NodeId = Vertex`), `IntoNeighbors`/`IntoNeighborsDirected` (Node→its produced values; Value→its consuming nodes; `Reversed` flips), `IntoNodeIdentifiers`, `NodeCount`, `Visitable` over the bipartite `Vertex`. This makes `petgraph::algo::toposort` / `DfsPostOrder` work — the pattern crate's `reachable_topo` will use these in Task 4. Test that `toposort` on a small hand-built graph matches a known order.

- [ ] **Step 7: property + edge-case tests** — `tests/proptest_invariants.rs`. Define a test payload (`enum TestKind { Const(i64), Add, Region }`, `enum TestVal { Int, Ctrl }`) + a test cacher (one that caches `Add`/`Const`, never `Region`). Write:
  - **proptest**: generate a random sequence of `create_node`/`add_input`/`replace_all_uses` ops building a valid DAG; assert invariants after each — (a) every `UseData` appears in exactly one value's use-list and vice-versa (bidirectional consistency); (b) `value_uses(v)` count == number of input edges pointing at `v`; (c) after `replace_all_uses(a,b)`, no use references `a` and `b` gained exactly `a`'s former uses; (d) `retain_reachable(roots)` keeps exactly the reachable set and the returned remap is a bijection on survivors; (e) petgraph `toposort` yields producers before consumers.
  - **edge-case units**: `Add(x,x)` (repeated operand — both use slots present, `value_uses(x)`==2), multi-output node, a value with zero uses, empty graph (`retain_reachable` no-op), a cacheable kind dedups (same kind+inputs → same id) while `Region` (non-cacheable) always distinct, `detach_node_inputs` then re-add, large stress (10k nodes, dedup-heavy, assert node count bounded).
  - Run: `cargo test -p strider-graph` → all pass. `cargo clippy -p strider-graph --all-targets` → 0.

- [ ] **Step 8: Commit.**
```
git add -A && git commit -m "feat(strider-graph): generic bipartite Graph<N,V,C> + NeverCacheable + property/stress tests"
git push origin refactor/generic-graph-crate
```

---

## Task 3: Re-base `strider-ir::Graph` onto `strider-graph`

Make strider-ir's `Graph` an instantiation of the generic one; the structural code in `graph/` is deleted (it now lives in strider-graph). `IrCacheable` carries the current Vec-keyed cache. Control-aware walks re-base onto the generic graph. Behavior-preserving — the full IR + downstream suites are the gate.

**Files:** `crates/strider-ir/Cargo.toml` (+`strider-graph` dep), `crates/strider-ir/src/graph/*` (gut the structural code; keep/rename strider-specific bits), new `crates/strider-ir/src/graph/cache.rs`, `crates/strider-ir/src/walk/mod.rs`, `crates/strider-ir/src/lib.rs`, plus the ~140 strider-ir + downstream call sites.

- [ ] **Step 1: `IrCacheable`** — `crates/strider-ir/src/graph/cache.rs`. Port the current dedup logic (the `node_to_id: HashMap<(Node, Vec<ValueId>, Vec<ValueKind>), NodeId>` keyed cache from old `graph/store.rs`) into a struct implementing `strider_graph::NodeCacheable<NodeKind, ValueKind>`. Its `create` builds the key (NodeKind + inputs + output kinds), looks up; on hit returns the existing id (and unions the asm-fingerprint — but fingerprints live on `Function`, so the fingerprint-union actually already happens in the builder via `create_node_attributed`; verify the dedup path itself only needs to return the existing id and the builder handles fingerprints); on miss `store.alloc_node` + cache insert. Skip caching for `NodeKind::is_cacheable()==false` (Region/Phi/etc.) → `alloc_node`. `NodeKind`/`ValueKind` are `Hash+Eq` so the key works.

- [ ] **Step 2: Re-define `Graph`.** In `strider-ir/src/graph/mod.rs`, replace the struct with:
```rust
pub type Graph = strider_graph::Graph<crate::node::NodeKind, crate::node::ValueKind, crate::graph::cache::IrCacheable>;
```
Delete the structural method impls now provided by the generic crate (store.rs/uses.rs/access.rs/rewrite.rs/iterators.rs/compact.rs structural parts). Re-export the generic `NodeId`/`ValueId`/`UseId`/`Inputs` from strider-graph at the strider-ir paths downstream expects (`strider_ir::node::NodeId`, `strider_ir::Inputs`, etc.) so downstream code is undisturbed.

Handle the strider-specific `Graph` methods that can't live on a type alias:
- `kind_of_value(value)` is **structural** (`node_kind(producer(value))`) → keep it as a generic method ON `strider_graph::Graph` (returns `&N`); the alias inherits it, callers unchanged.
- Genuinely semantic helpers (e.g. `memory_output_of`, which scans for a `ValueKind::Memory` output) move to an **extension trait** `IrGraphExt` with a blanket `impl IrGraphExt for Graph` in strider-ir, so call sites keep `graph.memory_output_of(...)` syntax by adding `use crate::...::IrGraphExt;` (the same pattern as `IRBuilderExt`). Count + repoint those callers.

- [ ] **Step 3: Re-base control-aware walks.** In `strider-ir/src/walk/mod.rs`, `cfg_reachable`/`graph_walk_succs`/`cfg_outputs`/`cfg_succs`/`GraphWalkInfo::compute_full` now take/operate on `&strider_graph::Graph<…>` via its public API + `ValueKind::is_control()`. They stay in strider-ir (control semantics). The structural `reverse_postorder` comes from the generic crate (re-export or call `graph.reverse_postorder`).

- [ ] **Step 4: `Function::compact` + side-table remap.** `Function::compact` calls `graph.retain_reachable(roots)` (generic), then remaps its side-tables (incl. the wide-const gc from Task 1) with the returned `NodeIdRemap`. Confirm the remap type/shape matches what the generic crate returns.

- [ ] **Step 5: Fix call sites.** `cargo build --workspace 2>&1 | tail -40` — the API names are preserved, so most callers are untouched; the breakage is (a) anything that constructed `Graph { … }` directly (now `Graph::new()`/`Default`), (b) the wide-const accesses (done in Task 1), (c) `is_cacheable`-gated mutation (now unconditional). Fix iteratively until clean.

- [ ] **Step 6: Gate + commit.**
```
cargo test --workspace 2>&1 | tail -12
cargo clippy --workspace --all-targets 2>&1 | tail -3
cargo build -p strider-py 2>&1 | tail -2
git add -A && git commit -m "refactor(strider-ir): re-base Graph onto strider-graph; IrCacheable holds the dedup cache"
git push origin refactor/generic-graph-crate
```
Expected: full suite green (behavior-preserving). The 8 `track_*` tests + validator + every opt pass exercise the re-based graph end-to-end.

---

## Task 4: Migrate `strider-pattern`'s `BiGraph` onto `strider-graph`

Delete the hand-rolled `BiGraph`; `Pattern`/`Template` become generic-graph instances with `NeverCacheable`. The typed `MatchPat`/`TemplatePat` builders + capture handling stay (trait-based safety is orthogonal to storage).

**Files:** `crates/strider-pattern/Cargo.toml` (+`strider-graph` dep), delete `crates/strider-pattern/src/bigraph.rs`, modify `matcher/graph.rs`, `template/graph.rs`, `template/mod.rs`, `matcher/walk.rs`, the builders.

- [ ] **Step 1: Re-point `Pattern`/`Template`.** `Pattern { graph: strider_graph::Graph<PatNode, PatValue, NeverCacheable>, cast_mask }`, `Template { graph: strider_graph::Graph<TmplNode, TmplValue, NeverCacheable> }`. The bipartite `Node`/`Output` vertices map to the generic graph's node-vertices (carrying `PatNode`/`TmplNode`) producing value-vertices (carrying `PatValue`/`TmplValue`). The capture `Option<Capture>` stays a field on `PatNode`/`PatValue`/`TmplValue` (opaque payload).

- [ ] **Step 2: Map the `BiGraph` accessors to the generic API.** Replace `node_weight`/`output_weight`→`node_kind`/`value_kind`; `consumed_inputs`→`node_inputs` (+ slot from use order); `produced_outputs`→`node_outputs`; `add_node`→`create_node` (via `NeverCacheable`, so always fresh); `node_weights`/`output_weights`→`all_node_ids`+`node_kind` / value iteration. `reachable_topo` → `petgraph::algo::toposort(Reversed(&graph), …)` via the petgraph views from Task 2 Step 6.

- [ ] **Step 3: `instantiate`** (`template/mod.rs`) walks the generic graph with `reachable_topo` (petgraph view) exactly as today, calling `builder.create_node_attributed(...)` — unchanged except the graph type.

- [ ] **Step 4: Delete `bigraph.rs`.** Confirm no remaining `BiGraph`/`BiVertex`/`BiEdge` references: `grep -rn "BiGraph\|BiVertex\|BiEdge" crates/strider-pattern` empty.

- [ ] **Step 5: Gate + commit.**
```
cargo test -p strider-pattern 2>&1 | tail -8
cargo test --workspace 2>&1 | tail -12
cargo clippy --workspace --all-targets 2>&1 | tail -3
git add -A && git commit -m "refactor(strider-pattern): migrate BiGraph onto strider-graph::Graph<…, NeverCacheable>; delete bigraph"
git push origin refactor/generic-graph-crate
```
Expected: pattern suite (293+ tests) + the rewrite/template tests + full workspace green. The `MatchPat`/`TemplatePat` compile-time guards still reject a wildcard RHS (unchanged trait bounds).

---

## Final: review + merge

- [ ] Holistic review over `develop..HEAD` (focus: the generic `Graph<N,V,C>` soundness + the property/stress suite, the `NodeCacheable`-owns-dedup correctness, behavior preservation across the IR re-base and the pattern migration, no `Hash`/`Eq` bound leaked onto the pattern payloads, the wide-const→`Function` move, petgraph views correct, no dependency cycles — `strider-graph` is a leaf depending only on cranelift-entity/petgraph/smallvec).
- [ ] Confirm `grep -rn "BiGraph" crates` empty; `strider-graph` has no strider dep; `cargo build -p strider-graph` compiles standalone.
- [ ] Merge `--no-ff` into `develop`, push, remove worktree + branch (after user confirmation).

## Non-goals (deferred)
- The hash-on-demand `NodeCache` (no stored vecs, O(1) removal, Region/Phi build-identity exclusion) — swap into `IrCacheable` as a follow-up; this plan keeps the current Vec-keyed cache there, behavior-preserving.
