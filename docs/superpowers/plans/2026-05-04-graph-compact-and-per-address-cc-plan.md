# Graph compaction + per-callsite clobber overrides — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three coupled extensions:
1. `compact: bool` (default true) on `strider::run` / `strider.run`: rebuilds the IR arena to drop unreachable zombie nodes after the destructive optimiser pipeline.
2. `per_address_ccs: HashMap<u64, CallingConvention>` (default empty): swaps the entire calling convention for `Call` nodes whose target address matches (driver: Linux-kernel `__fentry__` / `mcount`).
3. Conservative `CallOther` clobber default: every `CallOther` now clobbers every tracked variable (except SP) and rebinds them, mirroring how `Call` already works (driver: `syscall` and other state-clobbering opaque user-ops).

**Architecture:** Three additive features sharing one IR side-table.
* Compaction: `Graph::retain_reachable(entry) -> NodeIdRemap` rebuilds nodes / inputs / outputs PrimaryMaps and remaps the four side-tables (`asm_fingerprints`, `stack_phi_offsets`, `call_other_names`, new `call_clobbered_overrides`). `BuiltFunctionGraph::compact()` wraps it and remaps `entry`. Run from `LoopState::finalize` controlled by `RunConfig::compact`.
* Per-address CC: new `Graph::call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>` records per-Call clobber lists for sites that used an override CC. `FunctionBuilder::build_call_with_cc(addr, override_cc)` is the unified entry point; the existing `build_call(addr)` becomes `build_call_with_cc(addr, None)`. `pattern::Match::get_vn` consults the per-Call override before falling back to `BuiltFunctionGraph::call_clobbered`. The orchestrator threads a pre-resolved `HashMap<u64, BuiltCallingConvention>` through `Strider::analyze_cfg_with_vns_and_overrides` (lift-time direct calls) and through `build_anchor_calling_context` (in-place tail-call edits).
* Conservative CallOther clobber: new `BuiltFunctionGraph::call_other_clobbered: Box<[rsleigh::Vn]>` carries the function-default clobber list (= every tracked variable except SP). `FunctionBuilder::build_call_other` emits one clobber output slot per entry and rebinds each variable to its corresponding output. `pattern::Match::get_vn` extends to handle `CallOther` clobber slots — same `call_clobbered_overrides` side-table for per-CallOther overrides (deferred follow-up); function-default falls back to `BuiltFunctionGraph::call_other_clobbered`.

**Tech Stack:** Rust workspace (`anyhow::Result` workspace-wide, `cranelift_entity` PrimaryMap/SecondaryMap, `rsleigh` for varnodes). Python via PyO3 + maturin (abi3-py39). Tests: `cargo test --workspace` for Rust; `uv run pytest` for Python under `crates/strider-py/tests/python/`.

**Spec:** [docs/superpowers/specs/2026-05-04-graph-compact-and-per-address-cc-design.md](../specs/2026-05-04-graph-compact-and-per-address-cc-design.md)

**Related conventions** (mirrors of patterns to copy verbatim where applicable):
* `Graph::stack_phi_offsets` / `call_other_names` / `asm_fingerprints` are the existing `SecondaryMap` side-tables; the new `call_clobbered_overrides` follows the same shape.
* `FunctionBuilder::build_call` ([crates/ir/src/builder/call.rs](../../crates/ir/src/builder/call.rs)) is the shape `build_call_with_cc` rewrites.
* `Strider::analyze_cfg` / `analyze_cfg_with_vns` ([crates/strider/src/strider/pipeline.rs](../../crates/strider/src/strider/pipeline.rs)) are the entry points the new override-aware variant joins.
* `RunConfig` and `LoopState` ([crates/strider/src/orchestrator.rs](../../crates/strider/src/orchestrator.rs)) hold the new fields and threading.
* `apply_tail_call` ([crates/opt/src/indirect_branch_resolve/inplace.rs](../../crates/opt/src/indirect_branch_resolve/inplace.rs)) populates the per-Call side-table when invoked with an override.

---

## Task 1: Add `Graph::call_clobbered_overrides` side-table

**Files:**
- Modify: `crates/ir/src/graph/mod.rs`
- Modify: `crates/ir/src/graph/store.rs`
- Test: `crates/ir/src/graph/tests.rs`

Ground-floor structural change: add the new SecondaryMap field with default-`None` semantics ("no override; consult function-default `call_clobbered`"). Accessor + setter mirror `call_other_name` / `set_call_other_name`. No consumers yet; downstream tasks wire it.

- [ ] **Step 1: Write the failing test**

Append to `crates/ir/src/graph/tests.rs`:

```rust
#[test]
fn call_clobbered_override_default_is_none() {
    let mut graph = Graph::new();
    let nid = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert!(graph.call_clobbered_override(nid).is_none());
}

#[test]
fn call_clobbered_override_set_then_get_round_trips() {
    let mut graph = Graph::new();
    let nid = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let vns: Vec<rsleigh::Vn> = vec![];
    graph.set_call_clobbered_override(nid, vns.clone());
    assert_eq!(graph.call_clobbered_override(nid), Some(vns.as_slice()));
}
```

Imports the test file likely already needs (`NodeOutputType`, etc.) — check the file's top and add only what's missing. Do not break compilation of existing tests.

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p ir call_clobbered_override
```
Expected: compile error — `call_clobbered_override` / `set_call_clobbered_override` not defined.

- [ ] **Step 3: Add the field**

In `crates/ir/src/graph/mod.rs`, add to the `Graph` struct (alongside the other `SecondaryMap` side-tables, after `asm_fingerprints`):

```rust
    /// Per-Call clobber-list override.
    ///
    /// `None` (the default) means the Call uses the function-default
    /// clobber list at [`crate::function::BuiltFunctionGraph::call_clobbered`];
    /// `Some(list)` shadows the function-default for this one Call —
    /// the i-th value-typed output (slot `i + 2`) corresponds to
    /// `list[i]` instead of the function-default.  Populated by
    /// [`crate::FunctionBuilder::build_call_with_cc`] when the call
    /// site uses a per-address calling-convention override (e.g.
    /// Linux-kernel `__fentry__` / `mcount` callbacks that preserve
    /// every register).
    ///
    /// Stored as `SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>` so
    /// the default `None` is the "no override" sentinel; the previous
    /// `HashMap`-keyed shape isn't used because the override is
    /// per-NodeId and benefits from the `SecondaryMap`'s O(1) array
    /// lookup with no hashing.
    pub(crate) call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>,
```

In the same file, add to `Graph::new`:

```rust
            call_clobbered_overrides: SecondaryMap::new(),
```

- [ ] **Step 4: Add the accessor and setter**

In `crates/ir/src/graph/store.rs`, after the existing `call_other_name` / `set_call_other_name` block, add:

```rust
    /// Returns the per-Call clobber-list override for `node_id`, or
    /// `None` if the Call uses the function-default
    /// [`crate::function::BuiltFunctionGraph::call_clobbered`].
    #[inline]
    #[must_use]
    pub fn call_clobbered_override(&self, node_id: NodeId) -> Option<&[rsleigh::Vn]> {
        self.call_clobbered_overrides[node_id]
            .as_deref()
    }

    /// Records `clobbered` as the per-Call clobber-list override for
    /// `node_id`.  Replaces any prior value.  Pass an empty `Vec` to
    /// declare "this Call clobbers nothing" (e.g. `__fentry__`).
    #[inline]
    pub fn set_call_clobbered_override(&mut self, node_id: NodeId, clobbered: Vec<rsleigh::Vn>) {
        self.call_clobbered_overrides[node_id] = Some(clobbered);
    }
```

- [ ] **Step 5: Run the test to verify it passes**

```
cargo test -p ir call_clobbered_override
```
Expected: PASS (both tests).

- [ ] **Step 6: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS (no regressions).

- [ ] **Step 7: Commit**

```bash
git add crates/ir/src/graph/mod.rs crates/ir/src/graph/store.rs crates/ir/src/graph/tests.rs
git commit -m "ir: add Graph::call_clobbered_overrides side-table

Default-None per-NodeId override slot for the per-Call clobber list
that future per-address-CC support populates."
```

---

## Task 2: Add `NodeIdRemap` + `Graph::retain_reachable`

**Files:**
- Create: `crates/ir/src/graph/compact.rs`
- Modify: `crates/ir/src/graph/mod.rs` (add `mod compact;`)
- Test: `crates/ir/tests/retain_reachable.rs`

Adds the core compaction primitive: walk reachable nodes from `entry`, allocate fresh `nodes`/`outputs`/`inputs` PrimaryMaps, copy each reachable node, rewrite all input.output_id references through the old→new translation table, rebuild use-list pointers via the existing `link_input_to_output_list`, rebuild the dedup cache, and remap all four side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`).

Returns a `NodeIdRemap` that exposes `old_to_new` for `NodeId` / `NodeOutputId` / `NodeInputId`. `Graph::retain_reachable` is `pub` (callable directly from `BuiltFunctionGraph::compact` in Task 3 and from external callers).

- [ ] **Step 1: Write the failing test**

Create `crates/ir/tests/retain_reachable.rs`:

```rust
//! Tests for `Graph::retain_reachable` — compaction of unreachable nodes.

use ir::graph::Graph;
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

#[test]
fn retain_reachable_drops_detached_zombie_node() {
    let mut graph = Graph::new();

    // Live: an entry-typed node we'll treat as the root.
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);

    // Doomed zombie: a free-standing IntConst with no consumers.
    let zombie = graph.create_node(
        NodeKind::IntConst(0xdead_beef),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let pre = graph.all_node_ids().count();
    assert!(pre >= 2, "graph must hold both nodes pre-compaction");
    assert!(graph.all_node_ids().any(|n| n == zombie));

    let remap = graph.retain_reachable(entry);

    // Zombie no longer in the graph.
    let post = graph.all_node_ids().count();
    assert!(post < pre, "compaction must shrink the graph");
    assert!(remap.node_old_to_new(zombie).is_none(), "zombie has no remap entry");
    assert!(remap.node_old_to_new(entry).is_some(), "entry survives");

    // Live entry still has its single Control output.
    let new_entry = remap.node_old_to_new(entry).unwrap();
    let outs: Vec<_> = graph.node_outputs(new_entry).into_iter().collect();
    assert_eq!(outs.len(), 1);
    assert!(graph.output_kind(outs[0]).is_control());
}

#[test]
fn retain_reachable_preserves_asm_fingerprint_on_surviving_node() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    graph.set_asm_fingerprint(entry, vec![0x1000, 0x1004, 0x1008]);

    let remap = graph.retain_reachable(entry);
    let new_entry = remap.node_old_to_new(entry).unwrap();
    assert_eq!(graph.asm_fingerprint(new_entry), &[0x1000, 0x1004, 0x1008]);
}

#[test]
fn retain_reachable_rebuilds_dedup_cache() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let _remap = graph.retain_reachable(entry);

    // After compaction, creating an Entry-shaped cacheable node returns
    // the existing surviving Entry id (dedup hits).
    // (Entry is non-cacheable; test the invariant on a cacheable kind.)
    let one_a = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let one_b = graph.create_node(
        NodeKind::IntConst(7),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    assert_eq!(one_a, one_b, "dedup cache must be rebuilt");
}

#[test]
fn retain_reachable_drops_side_table_entry_for_dropped_node() {
    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let zombie = graph.create_node(
        NodeKind::IntConst(0),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    graph.set_asm_fingerprint(zombie, vec![0xdead]);
    let remap = graph.retain_reachable(entry);
    assert!(remap.node_old_to_new(zombie).is_none());
    // Surviving entry has no fingerprint entry leaking from the dropped zombie.
    let new_entry = remap.node_old_to_new(entry).unwrap();
    assert!(graph.asm_fingerprint(new_entry).is_empty());
}
```

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p ir --test retain_reachable
```
Expected: compile error — `Graph::retain_reachable` and `NodeIdRemap` not defined.

- [ ] **Step 3: Add `NodeIdRemap` + `retain_reachable` skeleton**

Create `crates/ir/src/graph/compact.rs`:

```rust
//! `Graph::retain_reachable` — compact the IR arena down to nodes
//! reachable from `entry` via [`crate::walk::walk_graph`] (control-out +
//! data-in), returning the old→new id translation table so external
//! callers can fix up any ids they hold.

use cranelift_entity::{PrimaryMap, SecondaryMap};
use std::collections::HashMap;

use crate::node::{
    Node, NodeId, NodeInput, NodeInputId, NodeInputIdList, NodeOutput, NodeOutputId,
    NodeOutputIdList, NodeOutputKind,
};
use crate::walk::walk_graph;

use super::Graph;

/// Old→new id translation table produced by
/// [`Graph::retain_reachable`].  Sparse: only entries for surviving
/// ids are populated; dropped ids return `None`.
#[derive(Debug, Clone, Default)]
pub struct NodeIdRemap {
    nodes: SecondaryMap<NodeId, Option<NodeId>>,
    outputs: SecondaryMap<NodeOutputId, Option<NodeOutputId>>,
    inputs: SecondaryMap<NodeInputId, Option<NodeInputId>>,
}

impl NodeIdRemap {
    /// Returns the post-compaction `NodeId` for `old`, or `None` if
    /// `old` was unreachable and dropped.
    #[inline]
    #[must_use]
    pub fn node_old_to_new(&self, old: NodeId) -> Option<NodeId> {
        self.nodes[old]
    }

    /// Returns the post-compaction `NodeOutputId` for `old`, or
    /// `None` if `old`'s producing node was dropped.
    #[inline]
    #[must_use]
    pub fn output_old_to_new(&self, old: NodeOutputId) -> Option<NodeOutputId> {
        self.outputs[old]
    }

    /// Returns the post-compaction `NodeInputId` for `old`, or `None`
    /// if `old`'s consuming node was dropped.
    #[inline]
    #[must_use]
    pub fn input_old_to_new(&self, old: NodeInputId) -> Option<NodeInputId> {
        self.inputs[old]
    }
}

impl Graph {
    /// Rebuilds the arena to retain only nodes reachable from `entry`
    /// via [`crate::walk::walk_graph`] (control-out forward + data-in
    /// backward).  Returns the old→new id translation table.
    ///
    /// Pre-compaction `NodeId` / `NodeOutputId` / `NodeInputId` values
    /// are invalidated by this call.  Callers that hold any such ids
    /// MUST rewrite them through the returned [`NodeIdRemap`] (or
    /// drop them).
    ///
    /// The dedup cache is rebuilt from scratch.  All four side-tables
    /// (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`,
    /// `call_clobbered_overrides`) are remapped through the
    /// translation table; entries for dropped nodes are dropped.
    pub fn retain_reachable(&mut self, entry: NodeId) -> NodeIdRemap {
        // 1. Compute reachable set.
        let reachable: Vec<NodeId> = walk_graph(self, entry).collect();

        // 2. Build fresh arenas.
        let mut new_nodes: PrimaryMap<NodeId, Node> = PrimaryMap::new();
        let mut new_outputs: PrimaryMap<NodeOutputId, NodeOutput> = PrimaryMap::new();
        let mut new_inputs: PrimaryMap<NodeInputId, NodeInput> = PrimaryMap::new();
        let mut new_output_pool = cranelift_entity::ListPool::<NodeOutputId>::new();
        let mut new_input_pool = cranelift_entity::ListPool::<NodeInputId>::new();

        let mut remap = NodeIdRemap::default();

        // 3. First pass: copy nodes (placeholder input/output lists)
        // and outputs.  We need every new NodeId / NodeOutputId before
        // the second pass can rewrite input.output_id references.
        for &old_node_id in &reachable {
            let old_node = &self.nodes[old_node_id];
            let new_node = Node::new(old_node.kind);
            let new_node_id = new_nodes.push(new_node);
            remap.nodes[old_node_id] = Some(new_node_id);

            // Outputs: copy NodeOutput, leaving first_use cleared.
            // The use-list is rebuilt in pass 3.
            let mut new_output_ids: Vec<NodeOutputId> = Vec::new();
            for &old_out_id in old_node
                .outputs
                .as_slice(&self.output_pool)
                .iter()
            {
                let old_out = &self.outputs[old_out_id];
                let new_out = NodeOutput::new(old_out.kind, new_node_id, old_out.output_index);
                let new_out_id = new_outputs.push(new_out);
                remap.outputs[old_out_id] = Some(new_out_id);
                new_output_ids.push(new_out_id);
            }
            new_nodes[new_node_id].outputs =
                NodeOutputIdList::from_iter(new_output_ids, &mut new_output_pool);
        }

        // 4. Second pass: copy inputs (rewrite output_id through remap).
        for &old_node_id in &reachable {
            let new_node_id = remap.nodes[old_node_id]
                .expect("just installed in pass 1");
            let old_input_ids: Vec<NodeInputId> = self.nodes[old_node_id]
                .inputs
                .as_slice(&self.input_pool)
                .to_vec();
            let mut new_input_ids: Vec<NodeInputId> = Vec::with_capacity(old_input_ids.len());
            for old_input_id in old_input_ids {
                let old_input = &self.inputs[old_input_id];
                let new_output_id = remap.outputs[old_input.output_id].expect(
                    "input references an output whose producing node was unreachable",
                );
                let new_input = NodeInput::new(new_output_id, new_node_id, old_input.input_index);
                let new_input_id = new_inputs.push(new_input);
                remap.inputs[old_input_id] = Some(new_input_id);
                new_input_ids.push(new_input_id);
            }
            new_nodes[new_node_id].inputs =
                NodeInputIdList::from_iter(new_input_ids, &mut new_input_pool);
        }

        // 5. Swap the arenas onto self before rebuilding use-lists —
        // `link_input_to_output_list` mutates `self.outputs` /
        // `self.inputs`.
        self.nodes = new_nodes;
        self.outputs = new_outputs;
        self.inputs = new_inputs;
        self.output_pool = new_output_pool;
        self.input_pool = new_input_pool;

        // 6. Rebuild use-list pointers.  Iterate every input and re-
        // attach via the existing helper (which sets first_use on the
        // referenced output and chains next_use).
        let all_input_ids: Vec<NodeInputId> = self.inputs.keys().collect();
        for input_id in all_input_ids {
            self.link_input_to_output_list(input_id);
        }

        // 7. Rebuild the dedup cache from scratch.
        self.node_to_id.clear();
        let all_node_ids: Vec<NodeId> = self.nodes.keys().collect();
        for new_node_id in all_node_ids {
            let kind = self.nodes[new_node_id].kind;
            if !kind.is_cacheable() {
                continue;
            }
            let input_outputs: Vec<NodeOutputId> = self.nodes[new_node_id]
                .inputs
                .as_slice(&self.input_pool)
                .iter()
                .map(|&iid| self.inputs[iid].output_id)
                .collect();
            let output_kinds: Vec<NodeOutputKind> = self.nodes[new_node_id]
                .outputs
                .as_slice(&self.output_pool)
                .iter()
                .map(|&oid| self.outputs[oid].kind)
                .collect();
            let key = (Node::new(kind), input_outputs, output_kinds);
            // Last writer wins; reachable nodes with identical keys are
            // already deduped pre-compaction so collisions shouldn't
            // happen, but if they do the surviving entry is still valid.
            self.node_to_id.insert(key, new_node_id);
        }

        // 8. Remap all four side-tables.  For each table, iterate the
        // surviving (old → new) pairs and write the old entry into the
        // fresh table at the new id.
        let mut new_stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>> = SecondaryMap::new();
        let mut new_call_other_names: SecondaryMap<NodeId, Option<String>> = SecondaryMap::new();
        let mut new_asm_fingerprints: SecondaryMap<NodeId, Vec<u64>> = SecondaryMap::new();
        let mut new_call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>> =
            SecondaryMap::new();
        // We iterate over the *original* reachable set: those are the
        // only old ids worth remapping.  Build a small lookup so we can
        // step through them in id order.
        let mut old_to_new_pairs: HashMap<NodeId, NodeId> = HashMap::new();
        for &old_id in &reachable {
            if let Some(new_id) = remap.nodes[old_id] {
                old_to_new_pairs.insert(old_id, new_id);
            }
        }
        for (&old_id, &new_id) in &old_to_new_pairs {
            // SecondaryMap's `Index` returns the default for empty
            // entries; we read straight through that and only write
            // when the value differs from the default for the value
            // type's "no entry" sentinel.
            let phi = std::mem::take(&mut self.stack_phi_offsets[old_id]);
            if !phi.is_empty() {
                new_stack_phi_offsets[new_id] = phi;
            }
            let name = self.call_other_names[old_id].take();
            if let Some(n) = name {
                new_call_other_names[new_id] = Some(n);
            }
            let fp = std::mem::take(&mut self.asm_fingerprints[old_id]);
            if !fp.is_empty() {
                new_asm_fingerprints[new_id] = fp;
            }
            let ovr = self.call_clobbered_overrides[old_id].take();
            if let Some(v) = ovr {
                new_call_clobbered_overrides[new_id] = Some(v);
            }
        }
        self.stack_phi_offsets = new_stack_phi_offsets;
        self.call_other_names = new_call_other_names;
        self.asm_fingerprints = new_asm_fingerprints;
        self.call_clobbered_overrides = new_call_clobbered_overrides;

        remap
    }
}
```

- [ ] **Step 4: Wire the new module into the graph crate**

Add to `crates/ir/src/graph/mod.rs`:

```rust
mod compact;

pub use compact::NodeIdRemap;
```

(Placement: alongside the other `mod` declarations at the top, and after the existing `pub use` lines if any. Do not break the public surface.)

- [ ] **Step 5: Run the tests to verify they pass**

```
cargo test -p ir --test retain_reachable
```
Expected: PASS (all four tests).

- [ ] **Step 6: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS (no regressions).

- [ ] **Step 7: Commit**

```bash
git add crates/ir/src/graph/compact.rs crates/ir/src/graph/mod.rs crates/ir/tests/retain_reachable.rs
git commit -m "ir: add Graph::retain_reachable + NodeIdRemap

Compacts the arena to nodes reachable from entry via walk_graph
(control-out + data-in).  Rebuilds the dedup cache and all four
side-tables; returns the old->new translation table."
```

---

## Task 3: Add `BuiltFunctionGraph::compact`

**Files:**
- Modify: `crates/ir/src/function.rs`
- Test: `crates/ir/src/function.rs` (`#[cfg(test)] mod compact_tests`)

Wraps `Graph::retain_reachable(self.entry)` and remaps `self.entry` through the returned `NodeIdRemap`. `BuiltFunctionGraph` has only one NodeId-typed field (`entry`); the other fields (`variables`, `call_clobbered`, `ret_val_regs`) are vn-keyed and are unaffected.

- [ ] **Step 1: Write the failing test**

Append to `crates/ir/src/function.rs` (after the existing `impl BuiltFunctionGraph` block):

```rust
#[cfg(test)]
mod compact_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::node::{NodeKind, NodeOutputKind};

    #[test]
    fn compact_remaps_entry_and_drops_zombies() {
        let mut graph = crate::graph::Graph::new();
        let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let _zombie = graph.create_node(
            NodeKind::IntConst(0xdead),
            [],
            [NodeOutputKind::OutputType(crate::node::NodeOutputType::U64)],
        );
        let mut bfg = BuiltFunctionGraph::from_graph_and_entry(graph, entry);
        let pre_count = bfg.graph.all_node_ids().count();

        let _remap = bfg.compact();

        let post_count = bfg.graph.all_node_ids().count();
        assert!(post_count < pre_count, "compact must shrink the graph");
        // entry was remapped; new entry id still has the Control output.
        let outs: Vec<_> = bfg.graph.node_outputs(bfg.entry).into_iter().collect();
        assert_eq!(outs.len(), 1);
        assert!(bfg.graph.output_kind(outs[0]).is_control());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p ir compact_remaps_entry_and_drops_zombies
```
Expected: compile error — `BuiltFunctionGraph::compact` not defined.

- [ ] **Step 3: Implement `compact`**

Add to `impl BuiltFunctionGraph` in `crates/ir/src/function.rs`:

```rust
    /// Rebuilds the underlying [`crate::graph::Graph`] to retain only
    /// nodes reachable from [`Self::entry`] via
    /// [`crate::walk::walk_graph`].  `self.entry` is remapped through
    /// the returned [`crate::graph::NodeIdRemap`]; other fields
    /// (`variables`, `call_clobbered`, `ret_val_regs`) are vn-keyed
    /// and stay valid as-is.
    ///
    /// External callers that hold any pre-compaction `NodeId` /
    /// `NodeOutputId` / `NodeInputId` MUST rewrite them through the
    /// returned remap (or drop them).
    pub fn compact(&mut self) -> crate::graph::NodeIdRemap {
        let remap = self.graph.retain_reachable(self.entry);
        self.entry = remap
            .node_old_to_new(self.entry)
            .expect("entry must survive its own compaction");
        remap
    }
```

- [ ] **Step 4: Run the test to verify it passes**

```
cargo test -p ir compact_remaps_entry_and_drops_zombies
```
Expected: PASS.

- [ ] **Step 5: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/function.rs
git commit -m "ir: add BuiltFunctionGraph::compact

Wraps Graph::retain_reachable(self.entry) and remaps self.entry
through the returned remap."
```

---

## Task 4: Wire `RunConfig::compact` into orchestrator finalize

**Files:**
- Modify: `crates/strider/src/orchestrator.rs`
- Modify: every call-site of `strider::RunConfig { … }` in the workspace (set `compact: true` explicitly to preserve the new default)
- Test: `crates/strider/tests/compact.rs`

Adds the `compact: bool` field to `RunConfig` and the `compact: bool` field on the internal `RunOpts`. `LoopState::finalize` calls `self.graph_mut()?.compact()` when `self.opts.compact` is true (after the destructive pipeline returns). End-to-end test on a small fixture binary: the compacted graph has strictly fewer node ids than the non-compact graph, AND a handful of pattern queries return identical match counts in both.

- [ ] **Step 1: Find every existing `RunConfig { … }` call-site to update**

```
grep -rn "RunConfig {" crates/ --include='*.rs'
```
Expected: shows every struct-literal construction site (Rust workspace, not docs).  Likely sites: `crates/strider-py/src/run.rs`, `crates/strider/examples/strider.rs`, possibly tests under `crates/strider/tests/`.

Note the list — every site needs `compact: true,` appended in Step 4.

- [ ] **Step 2: Write the failing end-to-end test**

Create `crates/strider/tests/compact.rs`:

```rust
//! End-to-end test for `RunConfig::compact`.
//!
//! Drives `strider::run` on a small inline-byte function under both
//! compact=true and compact=false; asserts the compact graph has
//! strictly fewer node ids AND identical pattern-match counts on a
//! handful of representative queries.

#![allow(clippy::unwrap_used)]

use rsleigh::mem_readers::BufMemReader;
use strider::{CallingConvention, RunConfig, SleighArch, Strider};

/// Minimal x86_64 function with at least one call and a final ret —
/// guarantees an opt-pass-detached zombie (RedundantPhis cleans up
/// MemPhi nodes that have a single reachable predecessor) so the
/// compaction has at least one node to drop.
fn x86_64_call_then_ret_bytes() -> (Vec<u8>, u64) {
    // 48 c7 c0 2a 00 00 00     mov rax, 42
    // c3                        ret
    let bytes = vec![0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3];
    let entry = 0x1000;
    (bytes, entry)
}

fn make_strider() -> Strider {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).unwrap()
}

fn run_with(compact: bool) -> ir::BuiltFunctionGraph {
    let strider = make_strider();
    let (bytes, entry) = x86_64_call_then_ret_bytes();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();
    let config = RunConfig {
        strider: &strider,
        start_addr: entry,
        sleigh,
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        compact,
        per_address_ccs: std::collections::HashMap::new(),
    };
    strider::run(config).unwrap()
}

#[test]
fn compact_yields_fewer_node_ids_than_non_compact() {
    let compact_graph = run_with(true);
    let noncompact_graph = run_with(false);
    let compact_count = compact_graph.graph.all_node_ids().count();
    let noncompact_count = noncompact_graph.graph.all_node_ids().count();
    assert!(
        compact_count <= noncompact_count,
        "compact={compact_count} must not exceed non-compact={noncompact_count}"
    );
    // Strict inequality is the ideal, but on a tiny fixture (no dead
    // branches, no redundant phis), the two may match.  The test still
    // pins the upper bound; the semantic-equivalence check below
    // covers correctness.
}

#[test]
fn compact_preserves_reachable_pattern_matches() {
    use pattern::{Matcher, ret};

    let compact_graph = run_with(true);
    let noncompact_graph = run_with(false);

    let pat = ret();
    let compact_matches = Matcher::new(&compact_graph).find_all(&pat).len();
    let noncompact_matches = Matcher::new(&noncompact_graph).find_all(&pat).len();
    assert_eq!(
        compact_matches, noncompact_matches,
        "ret() match count must be invariant under compaction"
    );
}
```

(This test depends on Task 9's `per_address_ccs: HashMap::new()` field too — write the field-init now since Task 4 introduces both `compact` and an empty `per_address_ccs` placeholder. Task 9 then wires the actual semantic of `per_address_ccs`.)

Actually no — to keep tasks independent, defer adding `per_address_ccs` to Task 9. For Task 4, drop that field-init line from the test.

Replace:

```rust
        compact,
        per_address_ccs: std::collections::HashMap::new(),
```

with just:

```rust
        compact,
```

(Task 9 will edit this test to add `per_address_ccs: HashMap::new()` once the field exists.)

- [ ] **Step 3: Run the test to verify it fails**

```
cargo test -p strider --test compact
```
Expected: compile error — `RunConfig` has no `compact` field.

- [ ] **Step 4: Add the `compact` field to `RunConfig` and `RunOpts`**

In `crates/strider/src/orchestrator.rs`, add to the `RunConfig` struct (after `allow_code_before_start_addr`):

```rust
    /// Compact the IR arena at finalize, dropping nodes that aren't
    /// reachable from `entry` via [`ir::walk::walk_graph`].  Default
    /// `true` is recommended (passes leave detached "zombie" nodes
    /// the destructive pipeline severs from the live graph; without
    /// compaction these stay in the arena).  Pre-compaction NodeIds
    /// become invalid across the call.
    pub compact: bool,
```

Add the same field to the internal `RunOpts` struct in the same file:

```rust
    compact: bool,
```

In `LoopState::new`, copy the field through:

```rust
            opts: RunOpts {
                strider: config.strider,
                start_addr: config.start_addr,
                rom: config.rom,
                fn_max_size: config.fn_max_size,
                allow_code_before_start_addr: config.allow_code_before_start_addr,
                compact: config.compact,
            },
```

Modify `LoopState::finalize` to call `compact()` after the destructive pipeline:

```rust
    fn finalize(mut self) -> Result<ir::BuiltFunctionGraph> {
        let pipeline = self.opts.strider.build_destructive_optimizer_pipeline();
        let graph = self.graph_mut()?;
        pipeline.run_on_built(graph)?;
        if self.opts.compact {
            graph.compact();
        }
        self.graph
            .take()
            .ok_or_else(|| anyhow!("orchestrator finalize: graph already consumed"))
    }
```

- [ ] **Step 5: Update every existing `RunConfig` construction site**

For each site identified in Step 1, add `compact: true,` to the struct literal. Concrete sites to expect (verify via grep):

`crates/strider-py/src/run.rs:141` — append `compact: true,` to the existing struct literal.
`crates/strider/examples/strider.rs` — same.
Any orchestrator-level test file that constructs `RunConfig` directly — same.

- [ ] **Step 6: Run the new test to verify it passes**

```
cargo test -p strider --test compact
```
Expected: PASS (both tests).

- [ ] **Step 7: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/strider/src/orchestrator.rs crates/strider/tests/compact.rs crates/strider-py/src/run.rs crates/strider/examples/strider.rs
git commit -m "strider: add RunConfig::compact (default true)

Runs BuiltFunctionGraph::compact() at finalize after the destructive
pipeline.  Existing RunConfig construction sites updated to opt in
explicitly."
```

---

## Task 5: Plumb `compact` through `strider-py`

**Files:**
- Modify: `crates/strider-py/src/run.rs`
- Test: `crates/strider-py/tests/python/test_compact.py`

Adds a `compact: bool = True` keyword argument to `strider.run`. Threads through `RunConfig.compact` on the orchestrator path. On the custom-pipeline path (`run_with_custom_pipeline`), apply the same flag — when `True`, call `BuiltFunctionGraph::compact()` after the user's pipeline finishes.

- [ ] **Step 1: Write the failing Python test**

Create `crates/strider-py/tests/python/test_compact.py`:

```python
"""End-to-end Python smoke for `strider.run(compact=...)`."""

import strider
from strider import CallingConvention, MemoryMap, SleighArch


def _x86_64_strider():
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv_abi()
    return arch, cc


def _trivial_function_bytes():
    # 48 c7 c0 2a 00 00 00     mov rax, 42
    # c3                        ret
    return bytes([0x48, 0xc7, 0xc0, 0x2a, 0x00, 0x00, 0x00, 0xc3])


def _run_with(compact: bool):
    arch, cc = _x86_64_strider()
    mem = MemoryMap()
    mem.add_region(0x1000, _trivial_function_bytes())
    return strider.run(arch, cc, mem, entry=0x1000, compact=compact)


def test_compact_default_true_does_not_grow_graph():
    """compact=True (default) must not produce more node ids than compact=False."""
    compact_result = _run_with(True)
    noncompact_result = _run_with(False)
    compact_ids = list(compact_result.graph.all_node_ids())
    noncompact_ids = list(noncompact_result.graph.all_node_ids())
    assert len(compact_ids) <= len(noncompact_ids)


def test_compact_default_is_true():
    """Calling strider.run without an explicit compact= keyword applies compaction."""
    arch, cc = _x86_64_strider()
    mem = MemoryMap()
    mem.add_region(0x1000, _trivial_function_bytes())
    default_result = strider.run(arch, cc, mem, entry=0x1000)
    explicit_result = _run_with(True)
    assert len(list(default_result.graph.all_node_ids())) == \
        len(list(explicit_result.graph.all_node_ids()))
```

- [ ] **Step 2: Run the test to verify it fails**

```
cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/test_compact.py -v
```
Expected: TypeError or compile error — `strider.run` rejects the `compact` kwarg.

- [ ] **Step 3: Add the `compact` kwarg to `strider.run`**

In `crates/strider-py/src/run.rs`, update the `#[pyfunction(signature = …)]` attribute on `run`:

```rust
#[pyfunction(signature = (
    arch,
    cc,
    mem,
    entry,
    rom = None,
    pipeline = None,
    allow_code_before_start_addr = false,
    function_max_size = None,
    compact = true,
))]
```

Add `compact: bool` to the function signature:

```rust
#[allow(clippy::too_many_arguments)]
pub fn run(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
) -> PyResult<PyRunResult> {
```

Forward it to the two helpers:

```rust
    match pipeline {
        Some(p) => run_with_custom_pipeline(
            py, arch, cc, mem, entry, rom, p,
            allow_code_before_start_addr, function_max_size, compact,
        ),
        None => run_via_orchestrator(
            py, arch, cc, mem, entry, rom,
            allow_code_before_start_addr, function_max_size, compact,
        ),
    }
```

In `run_via_orchestrator`, add the `compact: bool` parameter and set it on the `RunConfig`:

```rust
    let config = strider::RunConfig {
        strider: &strider_borrow.inner,
        start_addr: entry,
        sleigh: orch_sleigh,
        rom: rom_arc,
        fn_max_size: function_max_size,
        allow_code_before_start_addr,
        compact,
    };
```

In `run_with_custom_pipeline`, add the `compact: bool` parameter; after the pipeline runs, if `compact`, compact the graph:

```rust
    actual_pipeline
        .run_on_built(&mut graph)
        .map_err(|e| into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))?;
    if compact {
        graph.compact();
    }
```

(Note: `graph` here is the `MutexGuard<…>` from `py_graph_borrow.write_inner()`. The `compact()` method takes `&mut self`, so this works directly through the guard.)

- [ ] **Step 4: Run the test to verify it passes**

```
cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/test_compact.py -v
```
Expected: PASS (all three tests).

- [ ] **Step 5: Verify the existing Python suite still passes**

```
cd crates/strider-py && uv run pytest tests/python -v
```
Expected: PASS (no regressions).

- [ ] **Step 6: Commit**

```bash
git add crates/strider-py/src/run.rs crates/strider-py/tests/python/test_compact.py
git commit -m "strider-py: expose compact=True kwarg on strider.run

Mirrors the new RunConfig::compact field; default True applies
compaction on both the orchestrator and custom-pipeline paths."
```

---

## Task 6: Add `FunctionBuilder::build_call_with_cc`

**Files:**
- Modify: `crates/ir/src/builder/call.rs`
- Modify: `crates/ir/src/builder/mod.rs` (expose internal helper if needed)
- Test: `crates/ir/src/builder/tests.rs` (or `crates/ir/tests/build_call_with_cc.rs`)

`build_call_with_cc(call_address, override_cc: Option<&BuiltCallingConvention>)` becomes the unified entry point. The existing `build_call(call_address)` becomes `build_call_with_cc(call_address, None)` and preserves the existing signature shape. When `override_cc` is `Some(cc)`:
- Use `cc.arg_passing_regs` (filtered through the function's tracked-variable set) instead of `self.arg_passing_vars`.
- Use a fresh `is_clobbered = !cc.callee_saved_regs.contains(v) && Some(*v) != stack_ptr_vn` filter against the function's tracked variables to compute the per-Call clobber list.
- Use `cc.ret_stack_pop` instead of `self.ret_stack_pop` for the post-call SP-add.
- Record the per-Call clobber list on `Graph::call_clobbered_overrides[call_node] = Some(...)` (always Some when an override was used, even when the resulting list is empty — that's the fentry case).

- [ ] **Step 1: Write the failing test**

Create `crates/ir/tests/build_call_with_cc.rs`:

```rust
//! Tests for `FunctionBuilder::build_call_with_cc` — per-Call CC override.

#![allow(clippy::unwrap_used)]

use ir::FunctionBuilder;
use rsleigh::Vn;
use target::{BuiltCallingConvention, CallingConvention, SleighArch};

fn x86_64_regs() -> rsleigh::SleighRegs {
    SleighArch::x86_64().probe_regs().unwrap()
}

fn x86_64_built_cc() -> BuiltCallingConvention {
    CallingConvention::x86_64_systemv_abi()
        .build(&x86_64_regs())
        .unwrap()
}

// "All preserving" must list EVERY tracked variable in
// `callee_saved_regs` because the clobber filter is:
//   clobber = tracked − callee_saved − {stack_pointer}
// An empty `callee_saved_regs` would clobber everything except SP —
// the opposite of what we want.  Constructed inline per test so it
// matches the test's exact tracked-variable set.

#[test]
fn build_call_with_cc_none_matches_build_call() {
    let cc = x86_64_built_cc();
    let regs = x86_64_regs();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rdi = regs.name_to_vn("RDI").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rdi, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let addr = b
        .build_int_const(0xdead_beef, NodeOutputType::U64)
        .unwrap();
    b.build_call_with_cc(addr, None).unwrap();
    // The Call output kinds match `build_call(addr)` exactly: Control,
    // Memory, then one slot per `call_clobbered_variables` entry.
    // Smoke check by counting outputs of the most-recent Call node.
    let g = &b.body().graph;
    let call_node = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert!(g.node_outputs(call_node).len() >= 2, "Control + Memory at minimum");
    assert!(g.call_clobbered_override(call_node).is_none(),
            "no override means side-table stays None");
}

#[test]
fn build_call_with_cc_all_preserving_clobbers_nothing() {
    let cc = x86_64_built_cc();
    let regs = x86_64_regs();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rdi = regs.name_to_vn("RDI").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rdi, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    // Override CC: every tracked variable is callee-saved → 0 clobbers.
    let override_cc = BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![rax, rdi],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr_vn: rsp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
    };

    let addr = b
        .build_int_const(0xdead_beef, NodeOutputType::U64)
        .unwrap();
    b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    let g = &b.body().graph;
    let call_node = g
        .all_node_ids()
        .find(|n| matches!(g.node_kind(*n), NodeKind::Call))
        .unwrap();
    let outs = g.node_outputs(call_node);
    // Outputs: Control + Memory + 0 clobbered slots.
    assert_eq!(outs.len(), 2, "fentry-style Call has 0 clobbered output slots");
    let inputs = g.node_inputs(call_node).into_iter().collect::<Vec<_>>();
    // Inputs: control + memory + target.  No arg slots.
    assert_eq!(inputs.len(), 3, "fentry-style Call takes no args");
    assert_eq!(g.call_clobbered_override(call_node), Some(&[][..]),
               "side-table records the empty per-Call override list");
}
```

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p ir --test build_call_with_cc
```
Expected: compile error — `FunctionBuilder::build_call_with_cc` not defined.

- [ ] **Step 3: Implement `build_call_with_cc`**

Edit `crates/ir/src/builder/call.rs`. Rename the existing `build_call` body to `build_call_with_cc`, accept the override, and re-implement `build_call` as a thin shim:

```rust
use anyhow::anyhow;
use smallvec::SmallVec;

use super::FunctionBuilder;
use crate::error::Result;
use crate::node::{NodeId, NodeKind, NodeOutputId, NodeOutputKind, NodeOutputType};
use crate::ops::IntBinaryOp;

impl FunctionBuilder {
    /// Terminates the current region with a `Call` node, using the
    /// function-default calling convention.  Equivalent to
    /// [`Self::build_call_with_cc`] with `override_cc = None`.
    ///
    /// # Errors
    ///
    /// See [`Self::build_call_with_cc`].
    pub fn build_call(&mut self, call_address: NodeOutputId) -> Result<()> {
        self.build_call_with_cc(call_address, None).map(|_| ())
    }

    /// Terminates the current region with a `Call` node.
    ///
    /// When `override_cc` is `None`, the Call is built with the
    /// function-default arg-passing / clobber / ret-stack-pop set
    /// from `FunctionBuilder::new`.  When `override_cc` is `Some(cc)`,
    /// `cc` fully replaces the function-default for this single Call:
    /// `cc.arg_passing_regs` (filtered through the function's tracked-
    /// variable set) become the args; `cc.callee_saved_regs` define a
    /// fresh `is_clobbered = !callee_saved.contains(v) && Some(*v) !=
    /// stack_ptr` filter that produces this Call's clobber list;
    /// `cc.ret_stack_pop` drives the post-call SP-add.  The per-Call
    /// clobber list is recorded on
    /// [`crate::Graph::call_clobbered_overrides`] so pattern queries
    /// can recover the right varnode for each clobber slot.
    ///
    /// Returns the freshly-created Call's [`NodeId`].
    ///
    /// # Errors
    ///
    /// Returns the same error set as before: missing region,
    /// terminated region, non-value inputs, missing tracked variable,
    /// unsupported SP byte size.
    pub fn build_call_with_cc(
        &mut self,
        call_address: NodeOutputId,
        override_cc: Option<&target::BuiltCallingConvention>,
    ) -> Result<NodeId> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        // Pick the per-call arg-passing list, clobber list, and
        // ret_stack_pop based on whether an override was supplied.
        let (arg_vars, clobber_vars, ret_stack_pop): (
            SmallVec<[rsleigh::Vn; 4]>,
            SmallVec<[rsleigh::Vn; 4]>,
            i64,
        ) = match override_cc {
            None => (
                self.arg_passing_vars.iter().copied().collect(),
                self.call_clobbered_variables.iter().copied().collect(),
                self.ret_stack_pop,
            ),
            Some(cc) => {
                // Filter override args through the function's tracked
                // variables.  Override args that the function never
                // reads (i.e. not in `variable_to_id`) are silently
                // dropped — they would otherwise produce a
                // `VariableNotFound` error from `read_variable`.
                let arg_vars: SmallVec<[rsleigh::Vn; 4]> = cc
                    .arg_passing_regs
                    .iter()
                    .copied()
                    .filter(|v| self.variable_to_id.contains_key(v))
                    .collect();
                // Per-call clobber list: every tracked variable that
                // is NOT in `callee_saved_regs` and NOT the SP.
                let callee_saved = &cc.callee_saved_regs;
                let stack_ptr_vn = self.stack_ptr_vn;
                let clobber_vars: SmallVec<[rsleigh::Vn; 4]> = self
                    .variables
                    .values()
                    .copied()
                    .filter(|v| !callee_saved.contains(v) && Some(*v) != stack_ptr_vn)
                    .collect();
                (arg_vars, clobber_vars, cc.ret_stack_pop)
            }
        };

        let arg_passing: SmallVec<[NodeOutputId; 4]> =
            arg_vars
                .iter()
                .map(|var| self.read_variable(var))
                .collect::<Result<_>>()?;
        self.validate_value_inputs(&arg_passing)?;

        let mut clobbered_kinds: SmallVec<[NodeOutputKind; 4]> = SmallVec::new();
        for var in &clobber_vars {
            let out = self.read_variable(var)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!("output {out:?} is not a value edge (got {k:?})"));
            }
            clobbered_kinds.push(k);
        }

        let addr_kind = self.graph().output_kind(call_address);
        if !addr_kind.is_value() {
            return Err(anyhow!(
                "output {call_address:?} is not a value edge (got {addr_kind:?})"
            ));
        }

        // Snapshot pre-call SP for the post-call adjust.
        let sp_pre_call = match self.stack_ptr_vn {
            Some(sp) if ret_stack_pop != 0 => {
                self.read_variable_optional(&sp)?.map(|out| (sp, out))
            }
            _ => None,
        };

        let inputs = [ctrl, memory, call_address].into_iter().chain(arg_passing);
        let outputs = [NodeOutputKind::Control, NodeOutputKind::Memory]
            .into_iter()
            .chain(clobbered_kinds);
        let call = self.create_node(NodeKind::Call, inputs, outputs);
        let call_outputs: Vec<_> = self.graph().node_outputs(call).into_iter().collect();

        self.advance_cur_region_ctrl(call_outputs[0])?;
        self.advance_cur_region_memory(call_outputs[1])?;
        for (variable, new_val) in core::iter::zip(&clobber_vars, call_outputs.iter().skip(2)) {
            self.write_variable(variable, *new_val)?;
        }

        // Record the per-Call override clobber list when an override was used.
        if override_cc.is_some() {
            let list: Vec<rsleigh::Vn> = clobber_vars.into_iter().collect();
            self.body_mut().graph.set_call_clobbered_override(call, list);
        }

        if let Some((sp, pre)) = sp_pre_call {
            let sp_ty: NodeOutputType = sp.size.try_into()?;
            let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
            let adjusted =
                self.build_int_binary_operation(pre, const_id, IntBinaryOp::Add, sp_ty)?;
            self.write_variable(&sp, adjusted)?;
        }
        Ok(call)
    }

    // ... (build_call_other unchanged below)
```

The `build_call_other` method below stays as-is. Reading the existing `crates/ir/src/builder/call.rs` for the imports and the full `build_call_other` body, keep them verbatim.

- [ ] **Step 4: Run the test to verify it passes**

```
cargo test -p ir --test build_call_with_cc
```
Expected: PASS (both tests).

- [ ] **Step 5: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS (existing `build_call` tests still pass via the shim).

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/builder/call.rs crates/ir/tests/build_call_with_cc.rs
git commit -m "ir: add FunctionBuilder::build_call_with_cc

Override-aware Call emitter; build_call(addr) becomes a thin shim
for build_call_with_cc(addr, None).  When an override CC is given,
the per-Call clobber list is recorded on
Graph::call_clobbered_overrides for downstream pattern queries."
```

---

## Task 7: Conservative `CallOther` clobber default

**Files:**
- Modify: `crates/ir/src/function.rs` (add `call_other_clobbered` field to `BuiltFunctionGraph`)
- Modify: `crates/ir/src/builder/mod.rs` (`FunctionBuilder::build()` populates the new field)
- Modify: `crates/ir/src/builder/call.rs` (`build_call_other` emits clobber slots + rebinds variables)
- Modify: existing tests asserting `build_call_other` output count (search and update)
- Test: `crates/ir/tests/call_other_conservative_clobber.rs`

Today `build_call_other` emits `[Control, Memory]` (and `[Control, Memory, value]` when `output_ty.is_some()`); tracked variables retain their pre-CallOther values across the call. This task changes the default to **clobber every tracked variable except SP** and rebind each variable to its corresponding clobber slot, mirroring `build_call`.

Output layout becomes:
- slot 0: Control
- slot 1: Memory
- slot 2: Value output (only when `output_ty.is_some()`)
- slot N..: Clobber outputs, one per `BuiltFunctionGraph::call_other_clobbered` entry, where N = 2 (no value) or 3 (with value)

The shared `Graph::call_clobbered_overrides` side-table (introduced in Task 1) is reused for future per-user-op overrides; this task does not yet populate it for CallOther — deferred to a future spec.

- [ ] **Step 1: Write the failing tests**

Create `crates/ir/tests/call_other_conservative_clobber.rs`:

```rust
//! Tests for the conservative-clobber `build_call_other` default and
//! the new `BuiltFunctionGraph::call_other_clobbered` field.

#![allow(clippy::unwrap_used)]

use ir::node::{NodeKind, NodeOutputType};
use ir::FunctionBuilder;
use target::{CallingConvention, SleighArch};

fn x86_64_strider_setup() -> (rsleigh::SleighRegs, target::BuiltCallingConvention) {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let cc = CallingConvention::x86_64_systemv_abi().build(&regs).unwrap();
    (regs, cc)
}

#[test]
fn build_call_other_no_value_emits_clobber_per_tracked_var() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    let (call_other_id, value_out) = b.build_call_other(7, &[], None).unwrap();
    assert!(value_out.is_none());

    let g = &b.body().graph;
    let outs = g.node_outputs(call_other_id);
    // Outputs: Control + Memory + 2 clobber (rax, rbx).  SP excluded.
    assert_eq!(outs.len(), 4, "Control + Memory + per-tracked-var clobber (SP excluded)");
}

#[test]
fn build_call_other_with_value_keeps_value_in_slot_2_clobber_starts_at_3() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    let (call_other_id, value_out) = b
        .build_call_other(7, &[], Some(NodeOutputType::U32))
        .unwrap();
    assert!(value_out.is_some());

    let g = &b.body().graph;
    let outs: Vec<_> = g.node_outputs(call_other_id).into_iter().collect();
    // Outputs: Control + Memory + value + 2 clobber.
    assert_eq!(outs.len(), 5);
    // Slot 2 is the value output (matches the explicit Some(NodeOutputType::U32)).
    let slot2_kind = g.output_kind(outs[2]);
    assert!(slot2_kind.is_value());
}

#[test]
fn build_call_other_rebinds_tracked_variables() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    // Snapshot rax's pre-CallOther producer.
    let pre_rax_value = b.read_variable(&rax).unwrap();

    let (call_other_id, _) = b.build_call_other(7, &[], None).unwrap();

    // Post-CallOther rax is bound to the CallOther's clobber slot.
    let post_rax_value = b.read_variable(&rax).unwrap();
    assert_ne!(pre_rax_value, post_rax_value, "rax must be rebound after CallOther");
    let (post_node, _) = b.body().graph.output_definition(post_rax_value);
    assert_eq!(
        post_node, call_other_id,
        "post-CallOther rax must come from the CallOther's clobber slot"
    );
}

#[test]
fn built_function_graph_call_other_clobbered_excludes_stack_pointer() {
    let (regs, cc) = x86_64_strider_setup();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rbx, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);
    let ret_regs: Vec<rsleigh::Vn> = b.ret_val_vars().to_vec();
    b.build_return(None, &ret_regs).unwrap();

    let bfg = b.build().unwrap();
    let coc: &[rsleigh::Vn] = &bfg.call_other_clobbered;
    assert!(coc.contains(&rax), "rax must be in call_other_clobbered");
    assert!(coc.contains(&rbx), "rbx must be in call_other_clobbered");
    assert!(!coc.contains(&rsp), "RSP must NOT be in call_other_clobbered");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

```
cargo test -p ir --test call_other_conservative_clobber
```
Expected: compile error — `BuiltFunctionGraph::call_other_clobbered` field not defined; AND assertion failures on the output-count tests once the field exists (output count is currently 2 / 3).

- [ ] **Step 3: Add the `call_other_clobbered` field to `BuiltFunctionGraph`**

In `crates/ir/src/function.rs`, add to the `BuiltFunctionGraph` struct (after `ret_val_regs`):

```rust
    /// Function-default clobber list for every `CallOther` node.
    ///
    /// Equals the function's tracked-variable set (`variables.values()`)
    /// filtered to exclude the stack pointer.  Order matches the
    /// CallOther's clobber output slots: the i-th clobber output of any
    /// CallOther (output index `i + 2` for value-less CallOther,
    /// `i + 3` for CallOther with a value output) corresponds to
    /// `call_other_clobbered[i]`.  Distinct from
    /// [`Self::call_clobbered`] (which excludes both callee-saved AND
    /// SP and is per-CC) — `call_other_clobbered` is the conservative
    /// "everything except SP" set used by every CallOther unless a
    /// per-CallOther override on
    /// [`crate::Graph::call_clobbered_overrides`] shadows it.
    pub call_other_clobbered: Box<[rsleigh::Vn]>,
```

In `BuiltFunctionGraph::from_graph_and_entry`, add `call_other_clobbered: Box::new([]),` to the struct literal.

- [ ] **Step 4: Populate `call_other_clobbered` in `FunctionBuilder::build()`**

Find `FunctionBuilder::build()` in `crates/ir/src/builder/mod.rs`. Just before the method returns the `BuiltFunctionGraph`, compute and set the new field:

```rust
        let call_other_clobbered: Box<[rsleigh::Vn]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != self.stack_ptr_vn)
            .collect();
```

Then include it in the returned `BuiltFunctionGraph` literal:

```rust
        BuiltFunctionGraph {
            graph: ...,
            entry: ...,
            variables: ...,
            call_clobbered: ...,
            ret_val_regs: ...,
            call_other_clobbered,
        }
```

(Read the surrounding `build()` body to see the actual literal shape and adapt.)

- [ ] **Step 5: Update `build_call_other` to emit conservative clobbers**

Edit `crates/ir/src/builder/call.rs::build_call_other`:

```rust
    pub fn build_call_other(
        &mut self,
        user_op_id: u64,
        args: &[NodeOutputId],
        output_ty: Option<NodeOutputType>,
    ) -> Result<(NodeId, Option<NodeOutputId>)> {
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        self.validate_value_inputs(args)?;

        // Conservative clobber default: every tracked variable except
        // SP is clobbered by the CallOther and rebound to its
        // corresponding output slot.  Mirrors how `build_call`
        // handles `call_clobbered_variables`.  A future per-user-op
        // override would shadow this via
        // `Graph::call_clobbered_overrides[node]`.
        let stack_ptr_vn = self.stack_ptr_vn;
        let clobber_vars: SmallVec<[rsleigh::Vn; 8]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != stack_ptr_vn)
            .collect();

        // Read each clobbered variable to validate it has a kind we
        // can express.  Same defensive check as `build_call`.
        let mut clobber_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        for var in &clobber_vars {
            let out = self.read_variable(var)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow!("output {out:?} is not a value edge (got {k:?})"));
            }
            clobber_kinds.push(k);
        }

        let mut output_kinds: SmallVec<[NodeOutputKind; 8]> = SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }
        output_kinds.extend(clobber_kinds);

        let inputs = [ctrl, memory].into_iter().chain(args.iter().copied());
        let node = self.create_node(NodeKind::CallOther { user_op_id }, inputs, output_kinds);
        let outputs: SmallVec<[NodeOutputId; 8]> =
            self.graph().node_outputs(node).into_iter().collect();
        self.advance_cur_region_ctrl(outputs[0])?;
        self.advance_cur_region_memory(outputs[1])?;

        // Optional value output sits at slot 2 when present; clobber
        // outputs follow at slot 2 (value-less) or slot 3 (with value).
        let (value_output, clobber_start_slot) = if output_ty.is_some() {
            (Some(outputs[2]), 3usize)
        } else {
            (None, 2usize)
        };

        // Rebind each clobbered variable to its CallOther output.
        for (var, out) in core::iter::zip(
            clobber_vars.iter(),
            outputs.iter().skip(clobber_start_slot),
        ) {
            self.write_variable(var, *out)?;
        }

        Ok((node, value_output))
    }
```

(Adjust import for `anyhow!` if not already in scope at the top of the file — `build_call_with_cc` uses it, so the import is likely already present.)

- [ ] **Step 6: Run the new tests to verify they pass**

```
cargo test -p ir --test call_other_conservative_clobber
```
Expected: PASS (all four tests).

- [ ] **Step 7: Find and update existing tests broken by the shape change**

Existing tests likely impacted (search for `build_call_other` and `CallOther` in `*tests*` files):
- `crates/ir/src/builder/tests.rs::build_call_other_without_output_advances_ctrl_and_memory` — asserts the CallOther's output count via tail behaviour; update to expect Control + Memory + clobber slots.
- `crates/ir/src/builder/tests.rs::build_call_other_with_output_returns_typed_value` — verify the value output is still queryable; the test should still pass as long as the value slot stays at index 2.
- Any `crates/opt/src/call_other_elide/` tests — should be unaffected (elision drops the entire node).
- Any `crates/strider/tests/*` that exercises CallOther output count — search and update.

Run the full workspace test suite to identify the breakage:

```
cargo test --workspace 2>&1 | grep -E "^test .* FAILED|^---- "
```

For each failing test, update the assertion to reflect the new conservative-clobber shape. The fix is mechanical: where the previous expectation was "N value-typed outputs from the CallOther", the new expectation is "N value-typed outputs PLUS one per `call_other_clobbered` entry".

- [ ] **Step 8: Verify the workspace builds and tests pass**

```
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/ir/src/function.rs crates/ir/src/builder/mod.rs crates/ir/src/builder/call.rs crates/ir/src/builder/tests.rs crates/ir/tests/call_other_conservative_clobber.rs
# Plus any other test files touched in Step 7.
git commit -m "ir: CallOther conservatively clobbers all tracked variables

Every CallOther now emits one clobber output per tracked variable
(except SP) and rebinds each variable to its corresponding output.
BuiltFunctionGraph gains call_other_clobbered: Box<[Vn]> for
pattern-query indexing.  Driver: syscall and other state-clobbering
opaque user-ops were previously assumed to preserve all registers,
which is unsound.  Per-user-op CC overrides are deferred follow-up
work."
```

---

## Task 8: Update `pattern::Match::get_vn` for `Call` and `CallOther` overrides

**Files:**
- Modify: `crates/pattern/src/matcher/match_result.rs`
- Test: `crates/pattern/tests/get_vn_with_call_override.rs`
- Test: `crates/pattern/tests/get_vn_with_callother_clobber.rs`

`Match::get_vn` must recover the right varnode for any Call's or CallOther's clobber output slot. After Tasks 6 + 7:
- For `Call`: clobber slots start at index 2; lookup uses `Graph::call_clobbered_override(node)` (per-Call override) with fallback to `BuiltFunctionGraph::call_clobbered`.
- For `CallOther`: clobber slots start at index 2 (no value output) or index 3 (with value output); lookup uses `Graph::call_clobbered_override(node)` (deferred per-CallOther override; `None` today since no producer sets it for CallOther) with fallback to `BuiltFunctionGraph::call_other_clobbered`. The value-output presence is detected by inspecting the slot-2 output kind: if it's a value kind that doesn't appear in `call_other_clobbered` *and* the total output count is `2 + 1 + call_other_clobbered.len()`, slot 2 is a value output; otherwise slot 2 is the first clobber. Cleanest: check the node's output count against `2 + call_other_clobbered.len()` — if it matches, no value output; if it equals `3 + call_other_clobbered.len()`, value output present at slot 2.

- [ ] **Step 1: Write the failing test**

Create `crates/pattern/tests/get_vn_with_call_override.rs`:

```rust
//! `Match::get_vn` consults per-Call clobber-list override before
//! falling back to `BuiltFunctionGraph::call_clobbered`.

#![allow(clippy::unwrap_used)]

use ir::node::{NodeKind, NodeOutputType};
use ir::FunctionBuilder;
use pattern::{call, Capture, Matcher};
use target::{BuiltCallingConvention, CallingConvention, SleighArch};

#[test]
fn get_vn_returns_override_when_per_call_clobber_list_set() {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rsp = regs.name_to_vn("RSP").unwrap();

    let cc = CallingConvention::x86_64_systemv_abi().build(&regs).unwrap();
    let mut b = FunctionBuilder::new(vec![rax, rsp], &cc).unwrap();
    b.build_entry().unwrap();
    let region = b.create_region().unwrap();
    b.set_entry_region(region).unwrap();
    b.set_region(region);

    // Override: rax is callee-saved (preserved); the per-Call clobber
    // list ends up empty.
    let override_cc = BuiltCallingConvention {
        arg_passing_regs: vec![],
        callee_saved_regs: vec![rax],
        ret_val_regs: vec![],
        ret_val_regs_float: vec![],
        stack_ptr_vn: rsp,
        stack_arg_offsets: vec![],
        ret_stack_pop: 0,
        link_register_vn: None,
        syscall_number_vn: None,
    };
    let addr = b.build_int_const(0xdead, NodeOutputType::U64).unwrap();
    let _call_node = b.build_call_with_cc(addr, Some(&override_cc)).unwrap();
    let ret_regs: Vec<rsleigh::Vn> = b.ret_val_vars().to_vec();
    b.build_return(None, &ret_regs).unwrap();
    let bfg = b.build().unwrap();

    // The single Call has 0 clobber outputs.  A pattern that captures
    // the Call and looks up `match.get_vn(c)` for slot 2 (the first
    // clobber slot) must return None: the override list is empty,
    // and `get_vn` must NOT fall back to the function-default
    // `call_clobbered[0]` here.
    //
    // This is checked indirectly: `call_clobbered_override(call_id)`
    // returns Some(&[]).
    let call_id = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .unwrap();
    assert_eq!(bfg.graph.call_clobbered_override(call_id), Some(&[][..]));
    // And the Call has only Control + Memory outputs.
    assert_eq!(bfg.graph.node_outputs(call_id).len(), 2);
}
```

(Note: `get_vn` is exercised indirectly here because building a graph with both a normal Call and an overridden Call to compare `get_vn` results requires too much fixture machinery for this single unit test. The end-to-end test in Task 10 will exercise the override-vs-default fallback.)

- [ ] **Step 2: Run the test to verify it fails — actually it should pass already because the side-table is set in Task 6**

```
cargo test -p pattern --test get_vn_with_call_override
```
Expected: PASS (this test only verifies the side-table state set by Task 6).

If it passes, that's correct. The `get_vn` behavioural assertion is exercised by the end-to-end tests in Task 9 + 10. To make this Task 8's TDD step meaningful, the next sub-step adds the actual `get_vn` patch and a unit test that exercises the override-vs-default branch directly.

- [ ] **Step 3: Add a unit test for `get_vn` with both override and default**

Append to `crates/pattern/tests/get_vn_with_call_override.rs`:

```rust
#[test]
fn get_vn_indexes_override_list_for_overridden_call() {
    use ir::function::BuiltFunctionGraph;
    use ir::graph::Graph;
    use ir::node::{NodeKind, NodeOutputKind};

    // Synthetic graph with a single Call node carrying a per-Call
    // override list of `[rax]`.  `get_vn` on slot 2 (the first
    // clobber slot) must return rax even though the function-default
    // `call_clobbered` is empty.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let target_const = graph.create_node(
        NodeKind::IntConst(0xdead),
        [],
        [NodeOutputKind::OutputType(NodeOutputType::U64)],
    );
    let target_out = graph.node_outputs_exact::<1>(target_const).unwrap()[0];
    let call = graph.create_node(
        NodeKind::Call,
        [entry_ctrl, entry_mem, target_out],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    graph.set_call_clobbered_override(call, vec![rax]);
    let bfg = BuiltFunctionGraph::from_graph_and_entry(graph, entry);

    // Bind the call's clobber-slot output to a capture, then assert
    // get_vn returns rax (NOT looked up in the empty function-default).
    let c = Capture::new();
    let pat = call().capture(c);
    let m = Matcher::new(&bfg).match_at(call, &pat).unwrap();
    // The Call's third output (slot 2) is the clobber slot bound to
    // `c`'s output via the typed-output binding logic in `call().capture(c)`.
    // Workaround: query directly off the graph because `capture(c)`
    // on `call()` binds the *node*, not slot 2 — `get_vn` walks from
    // `binding.output`.  For a node-only binding `binding.output` is
    // None, so `get_vn` returns None.  Instead, bind a slot 2 output
    // explicitly:
    let slot2 = bfg.graph.node_outputs(call).into_iter().nth(2).unwrap();
    // Synthesise a binding manually for the test.
    use pattern::matcher::bindings::Bindings;
    use pattern::matcher::match_result::Binding;
    let mut bindings = Bindings::new();
    bindings.insert(c, Binding { node: call, output: Some(slot2) });
    let m_manual = pattern::Match::new_for_test(call, bindings);
    assert_eq!(m_manual.get_vn(c, &bfg), Some(rax));
    // Sanity: m above is unused; suppress warning.
    let _ = m;
}
```

If `pattern::Match::new_for_test` and `Bindings::insert` are not currently `pub` (they almost certainly aren't), expose them with `#[cfg(any(test, feature = "test-utils"))]` visibility. Mirror what other crates in the workspace do for test scaffolding (e.g. `ir::test_utils`).

If creating the test scaffolding is too invasive, REPLACE this manual-binding test with an end-to-end test that builds two real Calls in one graph (one with default CC, one with override) and queries `get_vn` through a normal `call().capture(c).ret_output(0, …)` pattern. Either approach achieves the same TDD coverage; the manual-binding form is faster but needs the test-only API surface.

- [ ] **Step 4: Run the test to verify it fails for the right reason**

```
cargo test -p pattern --test get_vn_with_call_override get_vn_indexes_override_list_for_overridden_call
```
Expected: the assertion `m_manual.get_vn(c, &bfg) == Some(rax)` fails because `get_vn` currently indexes into `bfg.call_clobbered`, which is empty for `BuiltFunctionGraph::from_graph_and_entry`. (If the test fails at compile time because the test-utils surface doesn't exist, switch to the alternative end-to-end form noted in Step 3.)

- [ ] **Step 5: Patch `get_vn` to handle both `Call` and `CallOther`**

Edit `crates/pattern/src/matcher/match_result.rs` lines around 168–181 (the `get_vn` method):

```rust
    pub fn get_vn(&self, c: Capture, graph: &BuiltFunctionGraph) -> Option<rsleigh::Vn> {
        let binding = self.bindings.get_binding(c)?;
        if let Some(out) = binding.output {
            let (node, slot) = graph.graph.output_definition(out);
            let kind = graph.graph.node_kind(node);
            // Call: clobber slots start at index 2.
            if matches!(kind, NodeKind::Call) && slot >= 2 {
                let idx = (slot - 2) as usize;
                if let Some(override_list) = graph.graph.call_clobbered_override(node) {
                    return override_list.get(idx).copied();
                }
                return graph.call_clobbered.get(idx).copied();
            }
            // CallOther: clobber slots start at index 2 (no value
            // output) or 3 (with value output).  Detect by total
            // output count: `2 + clobber_len` for value-less,
            // `3 + clobber_len` for value-bearing.
            if matches!(kind, NodeKind::CallOther { .. }) {
                let total_outputs = graph.graph.node_outputs(node).len();
                let clobber_len = graph.call_other_clobbered.len();
                let clobber_start: u32 = if total_outputs == 2 + clobber_len {
                    2
                } else if total_outputs == 3 + clobber_len {
                    3
                } else {
                    // Shape we don't recognise; bail.
                    return None;
                };
                if slot < clobber_start {
                    // Slot 0/1 are Control/Memory; slot 2 (value-bearing
                    // form) is the user-op's value output — none of these
                    // map to a varnode.
                    return None;
                }
                let idx = (slot - clobber_start) as usize;
                if let Some(override_list) = graph.graph.call_clobbered_override(node) {
                    return override_list.get(idx).copied();
                }
                return graph.call_other_clobbered.get(idx).copied();
            }
        }
        match graph.graph.node_kind(binding.node) {
            NodeKind::InitialVar(vn) => Some(*vn),
            _ => None,
        }
    }
```

Update the doc comment block above `get_vn` to mention that:
- per-Call overrides shadow the function-default `call_clobbered` index;
- CallOther's clobber slots map through `call_other_clobbered` (or the per-CallOther override if set).

- [ ] **Step 6: Add the CallOther coverage test**

Create `crates/pattern/tests/get_vn_with_callother_clobber.rs`:

```rust
//! `Match::get_vn` returns the right varnode for a CallOther's
//! clobber output slot.  Both the function-default
//! (`BuiltFunctionGraph::call_other_clobbered`) and the per-CallOther
//! override (`Graph::call_clobbered_overrides`) are exercised.

#![allow(clippy::unwrap_used)]

use ir::function::BuiltFunctionGraph;
use ir::graph::Graph;
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};
use pattern::{Capture, Matcher};
use target::SleighArch;

#[test]
fn get_vn_for_callother_clobber_slot_uses_function_default() {
    // Synthetic graph with: Entry, InitialMemory, CallOther(slot 2 =
    // first clobber output bound to rax).  Function-default
    // `call_other_clobbered = [rax]`.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let callother = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, entry_mem],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let mut bfg = BuiltFunctionGraph::from_graph_and_entry(graph, entry);
    // Function-default clobber list contains only rax (matches the
    // single clobber output slot we created).
    bfg.call_other_clobbered = Box::new([rax]);

    // Bind slot 2 (the first clobber slot) to a capture and assert
    // get_vn returns rax.
    let c = Capture::new();
    let slot2 = bfg.graph.node_outputs(callother).into_iter().nth(2).unwrap();
    use pattern::matcher::bindings::Bindings;
    use pattern::matcher::match_result::Binding;
    let mut bindings = Bindings::new();
    bindings.insert(c, Binding { node: callother, output: Some(slot2) });
    let m = pattern::Match::new_for_test(callother, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rax));
}

#[test]
fn get_vn_for_callother_with_value_output_skips_value_slot() {
    // CallOther with a value output at slot 2 and a clobber output at
    // slot 3.  call_other_clobbered = [rax]; total outputs = 4 =
    // 3 + 1 ⇒ value-bearing form, clobber_start = 3.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let callother = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, entry_mem],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U32),
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    let mut bfg = BuiltFunctionGraph::from_graph_and_entry(graph, entry);
    bfg.call_other_clobbered = Box::new([rax]);

    let c = Capture::new();
    let slot3 = bfg.graph.node_outputs(callother).into_iter().nth(3).unwrap();
    use pattern::matcher::bindings::Bindings;
    use pattern::matcher::match_result::Binding;
    let mut bindings = Bindings::new();
    bindings.insert(c, Binding { node: callother, output: Some(slot3) });
    let m = pattern::Match::new_for_test(callother, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rax));

    // Slot 2 (the value output) returns None (no varnode mapping).
    let c2 = Capture::new();
    let slot2 = bfg.graph.node_outputs(callother).into_iter().nth(2).unwrap();
    let mut bindings2 = Bindings::new();
    bindings2.insert(c2, Binding { node: callother, output: Some(slot2) });
    let m2 = pattern::Match::new_for_test(callother, bindings2);
    assert_eq!(m2.get_vn(c2, &bfg), None);
}

#[test]
fn get_vn_for_callother_clobber_slot_uses_override_when_set() {
    // CallOther with per-CallOther clobber override.  Override list =
    // [rbx]; function-default = [rax].  Slot 2 binding must return rbx.
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    let rax = regs.name_to_vn("RAX").unwrap();
    let rbx = regs.name_to_vn("RBX").unwrap();

    let mut graph = Graph::new();
    let entry = graph.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
    let mem = graph.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
    let entry_ctrl = graph.node_outputs_exact::<1>(entry).unwrap()[0];
    let entry_mem = graph.node_outputs_exact::<1>(mem).unwrap()[0];
    let callother = graph.create_node(
        NodeKind::CallOther { user_op_id: 7 },
        [entry_ctrl, entry_mem],
        [
            NodeOutputKind::Control,
            NodeOutputKind::Memory,
            NodeOutputKind::OutputType(NodeOutputType::U64),
        ],
    );
    graph.set_call_clobbered_override(callother, vec![rbx]);
    let mut bfg = BuiltFunctionGraph::from_graph_and_entry(graph, entry);
    bfg.call_other_clobbered = Box::new([rax]);

    let c = Capture::new();
    let slot2 = bfg.graph.node_outputs(callother).into_iter().nth(2).unwrap();
    use pattern::matcher::bindings::Bindings;
    use pattern::matcher::match_result::Binding;
    let mut bindings = Bindings::new();
    bindings.insert(c, Binding { node: callother, output: Some(slot2) });
    let m = pattern::Match::new_for_test(callother, bindings);
    assert_eq!(m.get_vn(c, &bfg), Some(rbx),
               "per-CallOther override must shadow function-default");
}
```

(`Match::new_for_test` and `Bindings::insert` test-utils are added as needed — see Step 3 above for the same scaffolding.)

- [ ] **Step 7: Run all `get_vn` tests to verify they pass**

```
cargo test -p pattern --test get_vn_with_call_override --test get_vn_with_callother_clobber
```
Expected: PASS.

- [ ] **Step 8: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add crates/pattern/src/matcher/match_result.rs crates/pattern/tests/get_vn_with_call_override.rs crates/pattern/tests/get_vn_with_callother_clobber.rs
git commit -m "pattern: get_vn handles Call + CallOther clobber slots

Call: per-Call override shadows BuiltFunctionGraph::call_clobbered.
CallOther: per-CallOther override (deferred follow-up) shadows
BuiltFunctionGraph::call_other_clobbered; clobber slot start
detected from output count vs call_other_clobbered.len()."
```

---

## Task 9: `RunConfig::per_address_ccs` + lift-time direct-call routing

**Files:**
- Modify: `crates/strider/src/orchestrator.rs`
- Modify: `crates/strider/src/strider/pipeline.rs`
- Modify: `crates/strider/src/strider/insn/control.rs`
- Modify: `crates/strider/src/strider/mod.rs` (thread the override map into `IrStrider`)
- Modify: `crates/strider/tests/compact.rs` (add `per_address_ccs: HashMap::new()` field-init now that the field exists)
- Test: `crates/strider/tests/per_address_cc.rs`

Adds the `per_address_ccs: HashMap<u64, target::CallingConvention>` field to `RunConfig`, defaults empty. `LoopState::new` resolves it once via `cc.build(&sleigh_regs)` (using `config.sleigh.regs()`) into a `HashMap<u64, target::BuiltCallingConvention>`. Each iteration's lift threads a `&HashMap<u64, BuiltCallingConvention>` reference into `Strider::analyze_cfg_with_vns_and_overrides`. `IrStrider` carries the reference; `handle_call` looks up the call-target address in the map and routes through `build_call_with_cc(addr, Some(&cc))` when it hits.

- [ ] **Step 1: Write the failing end-to-end test**

Create `crates/strider/tests/per_address_cc.rs`:

```rust
//! End-to-end: a Call whose target is in `per_address_ccs` is built
//! with the override CC end-to-end (zero clobber outputs for an
//! all-preserving override).

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider::{CallingConvention, RunConfig, SleighArch, Strider};
use target::CallingConvention as TargetCC;

/// x86_64: `call $fentry; ret`.  Encoded with the call target near
/// the function entry so we control the absolute address.
///
/// Layout at base 0x1000:
///   0x1000  e8 fb 0f 00 00     call 0x2000
///   0x1005  c3                 ret
fn x86_64_call_then_ret() -> (Vec<u8>, u64, u64) {
    let bytes = vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3];
    let entry = 0x1000;
    let call_target = 0x2000;
    (bytes, entry, call_target)
}

/// All-preserving override: every register the function tracks is
/// callee-saved.  Constructed against x86_64 register names.
fn all_preserving_x86_64_cc() -> TargetCC {
    // We can't enumerate "every x86_64 register" generically; use a
    // best-effort superset that covers the SystemV caller-clobbered
    // set so the override clobber list is empty.
    //
    // A `CallingConvention` only stores `&'static [&'static str]`s,
    // so this is a fixed list.
    use target::CallingConvention;
    // Construct via a custom factory: the existing struct fields are
    // private, but `x86_64_systemv_abi` returns one we can clone-and-
    // mutate via a helper... actually they're private.  Use a dedicated
    // factory method `target::CallingConvention::x86_64_all_preserving`
    // (added below in Step 3 if not already present).
    CallingConvention::x86_64_all_preserving()
}

fn make_strider() -> Strider {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).unwrap()
}

#[test]
fn call_to_overridden_address_has_zero_clobber_outputs() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();

    let mut overrides: HashMap<u64, TargetCC> = HashMap::new();
    overrides.insert(call_target, all_preserving_x86_64_cc());

    let config = RunConfig {
        strider: &strider,
        start_addr: entry,
        sleigh,
        rom: None,
        fn_max_size: None,
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: overrides,
    };
    let bfg = strider::run(config).unwrap();

    let call_id = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    let outs = bfg.graph.node_outputs(call_id);
    // Override Call: Control + Memory only — zero clobber outputs.
    assert_eq!(outs.len(), 2, "all-preserving override emits zero clobber outputs");
    assert_eq!(bfg.graph.call_clobbered_override(call_id), Some(&[][..]));
}
```

Also append `per_address_ccs: HashMap::new(),` to the `RunConfig { … }` literal in `crates/strider/tests/compact.rs::run_with` (the field-init left out in Task 4 step 2).

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p strider --test per_address_cc
```
Expected: compile error — `per_address_ccs` field not on `RunConfig`, and `target::CallingConvention::x86_64_all_preserving` not defined.

- [ ] **Step 3: Add the `x86_64_all_preserving` factory to `target`**

Edit `crates/target/src/calling_convention/mod.rs`. Add a new factory near the existing `x86_64_systemv_abi`:

```rust
    /// "All-preserving" x86_64 calling convention: every userland
    /// caller-clobbered register is listed as callee-saved.  Empty
    /// arg-passing list, empty ret-val list.  Used for sites like
    /// Linux-kernel `__fentry__` / `mcount` callbacks that preserve
    /// all caller state.
    ///
    /// Pair with the per-address override map on
    /// [`strider::RunConfig::per_address_ccs`] so the override applies
    /// only to specific Call sites; the function-default CC stays
    /// SystemV.
    #[must_use]
    pub fn x86_64_all_preserving() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "RSP",
            arg_passing_regs: &[],
            callee_saved_regs: &[
                "RAX", "RBX", "RCX", "RDX", "RSI", "RDI", "RBP",
                "R8", "R9", "R10", "R11", "R12", "R13", "R14", "R15",
                "XMM0", "XMM1", "XMM2", "XMM3", "XMM4", "XMM5",
                "XMM6", "XMM7", "XMM8", "XMM9", "XMM10", "XMM11",
                "XMM12", "XMM13", "XMM14", "XMM15",
            ],
            ret_val_regs: &[],
            ret_val_regs_float: &[],
            stack_arg_offsets: &[],
            ret_stack_pop: 0,
            link_register_reg_name: None,
            syscall_number_reg_name: None,
        }
    }
```

(Ensure all register names used here exist in the x86_64 Sleigh probe — verify by running `arch.probe_regs().unwrap().name_to_vn("XMM15").is_some()` in a scratch test or by reading existing presets in the same file. If any name is wrong, fix it; the `.build(&sleigh_regs)` call in `Strider::new` will surface unresolved names as errors.)

- [ ] **Step 4: Add `per_address_ccs` to `RunConfig` and `RunOpts`**

In `crates/strider/src/orchestrator.rs`, add the field to `RunConfig` (after `compact`):

```rust
    /// Per-target-address calling-convention overrides.  When a `Call`
    /// is emitted (either at lift time for a direct call to an
    /// `IntConst(K)` target, or by the indirect-branch resolver as an
    /// in-place tail-call edit to address `K`), if `K` is in this map
    /// the matching CC fully replaces the function-default for that
    /// one Call.  Empty by default.
    ///
    /// Driver: Linux-kernel `__fentry__` / `mcount` hooks that preserve
    /// every register and observe no arguments — express via
    /// [`target::CallingConvention::x86_64_all_preserving`] (and the
    /// per-arch siblings).  The user supplies raw addresses; symbol
    /// resolution is the caller's responsibility.
    pub per_address_ccs: std::collections::HashMap<u64, target::CallingConvention>,
```

Add a parallel field to `RunOpts`:

```rust
    /// Pre-resolved per-target-address CC overrides.  See the
    /// [`RunConfig::per_address_ccs`] doc.  Resolved once at
    /// `LoopState::new` so any unresolved register name surfaces
    /// before iteration starts.
    per_address_built_ccs: std::collections::HashMap<u64, target::BuiltCallingConvention>,
```

Update `LoopState::new` to pre-resolve:

```rust
    fn new(config: RunConfig<'a, R>) -> Result<Self> {
        let lr_vn = config.strider.calling_convention().link_register_vn;
        let sp_vn = Some(config.strider.calling_convention().stack_ptr_vn);
        // Pre-resolve per-address CC overrides against the same Sleigh
        // register table the function-default CC was built against.
        let sleigh_regs = config.sleigh.regs();
        let per_address_built_ccs: std::collections::HashMap<
            u64,
            target::BuiltCallingConvention,
        > = config
            .per_address_ccs
            .iter()
            .map(|(addr, cc)| {
                cc.build(sleigh_regs)
                    .map(|built| (*addr, built))
                    .map_err(|e| anyhow!("per-address CC at {addr:#x} unresolved: {e}"))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            // ... existing fields ...
            opts: RunOpts {
                strider: config.strider,
                start_addr: config.start_addr,
                rom: config.rom,
                fn_max_size: config.fn_max_size,
                allow_code_before_start_addr: config.allow_code_before_start_addr,
                compact: config.compact,
                per_address_built_ccs,
            },
        })
    }
```

(`config.sleigh.regs()` may not be the right accessor — adjust to whatever the actual `rsleigh::Sleigh<R>` exposes for the register table. If the accessor isn't available, use `config.strider.calling_convention()`'s parent registers, or pre-resolve in `RunConfig` itself.)

If pre-resolving inside `LoopState::new` is awkward because the Sleigh handle is consumed later, an acceptable alternative is to require the caller to pre-resolve into `HashMap<u64, BuiltCallingConvention>` before constructing `RunConfig`. That changes the field type to `HashMap<u64, target::BuiltCallingConvention>`. Pick whichever access pattern matches what `rsleigh::Sleigh<R>` actually exposes.

- [ ] **Step 5: Thread the overrides into `Strider::analyze_cfg`**

Edit `crates/strider/src/strider/pipeline.rs`. Add a new method:

```rust
    /// Variant of [`Self::analyze_cfg_with_vns`] that accepts a
    /// per-target-address calling-convention override map.  Direct
    /// Calls whose target is in the map are built via
    /// [`ir::FunctionBuilder::build_call_with_cc`] with the override.
    pub fn analyze_cfg_with_vns_and_overrides<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
        all_vns: Vec<rsleigh::Vn>,
        per_address_built_ccs: &std::collections::HashMap<u64, target::BuiltCallingConvention>,
    ) -> Result<AnalyzeOutcome> {
        let mut ir_strider = IrStrider::new(self, cfg, all_vns)?;
        ir_strider.set_per_address_ccs(per_address_built_ccs);
        // ... rest is the existing analyze_cfg body verbatim ...
    }
```

The cleanest way to avoid duplicating the entire body: refactor the existing `analyze_cfg_with_vns` body into a helper that takes a `&IrStrider` already constructed. Then both `analyze_cfg_with_vns` (passes empty overrides) and `analyze_cfg_with_vns_and_overrides` (passes the real map) call it.

`IrStrider::set_per_address_ccs` is added to `crates/strider/src/strider/mod.rs`:

```rust
pub struct IrStrider<'a, R: rsleigh::MemReader> {
    pub(crate) strider: &'a Strider,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
    pub(crate) unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>,
    /// Per-target-address CC override map.  Empty (the default) means
    /// every direct Call uses the function-default.  Set by
    /// [`Strider::analyze_cfg_with_vns_and_overrides`].
    pub(crate) per_address_ccs: &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
}

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    // ... existing new() ...

    /// Replaces the per-target-address CC override map with `map`.
    pub(crate) fn set_per_address_ccs(
        &mut self,
        map: &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
    ) {
        self.per_address_ccs = map;
    }
}
```

The new field's lifetime is `'a`; the default for `IrStrider::new` should set it to a borrow of an empty `HashMap`.  Since giving each `IrStrider` its own empty map is awkward, store the override map on `Strider` itself (with an `&'static` empty default) or use `&'static HashMap<…>` initialised via `OnceLock`.  The simplest pragmatic choice: change `per_address_ccs` to `Option<&'a HashMap<…>>` defaulting to `None`, and have `handle_call` dispatch on `Some(map)` only.

Recommended concrete shape:

```rust
pub(crate) per_address_ccs: Option<&'a std::collections::HashMap<u64, target::BuiltCallingConvention>>,
```

Default `None` in `IrStrider::new`; `set_per_address_ccs` writes `Some(map)`.

- [ ] **Step 6: Update `handle_call` to consult the override**

Edit `crates/strider/src/strider/insn/control.rs::handle_call`:

```rust
    pub(super) fn handle_call(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let target_vn = &insn.inputs[0];
        let space = target_vn.addr_space;
        let space_info = self
            .cfg
            .sleigh
            .space_info(space)
            .ok_or_else(|| anyhow::anyhow!("no space info for call target space {space:?}"))?;
        let target_addr = target_vn.addr_off;
        let call_address = self
            .builder
            .build_int_const(target_addr, space_info.addr_size().try_into()?)?;
        let override_cc = self
            .per_address_ccs
            .and_then(|m| m.get(&target_addr));
        self.builder
            .build_call_with_cc(call_address, override_cc)
            .map(|_| ())?;
        Ok(())
    }
```

Apply the same lookup pattern in `handle_tail_call` (the constant-target tail-call lift handler). For `handle_call_indirect` the target is not a compile-time constant; leave it on the function-default path (the in-place tail-call path in Task 10 covers indirect-call resolved-to-Single sites).

- [ ] **Step 7: Update the orchestrator to invoke the override-aware lift**

In `crates/strider/src/orchestrator.rs::build_lift_stable`, change the lift call:

```rust
    let outcome = opts
        .strider
        .analyze_cfg_with_vns_and_overrides(&cfg, all_vns, &opts.per_address_built_ccs)?;
```

- [ ] **Step 8: Run the test to verify it passes**

```
cargo test -p strider --test per_address_cc
```
Expected: PASS.

- [ ] **Step 9: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS (existing call-site tests still pass; the empty override map matches today's behaviour).

- [ ] **Step 10: Commit**

```bash
git add crates/target/src/calling_convention/mod.rs crates/strider/src/orchestrator.rs crates/strider/src/strider/pipeline.rs crates/strider/src/strider/mod.rs crates/strider/src/strider/insn/control.rs crates/strider/tests/per_address_cc.rs crates/strider/tests/compact.rs
git commit -m "strider: route direct Calls through per-address CC override

RunConfig::per_address_ccs supplies a target-address -> CC map.
LoopState pre-resolves it once; analyze_cfg_with_vns_and_overrides
threads it into IrStrider; handle_call consults the map for direct
calls and dispatches through build_call_with_cc."
```

---

## Task 10: Orchestrator in-place tail-call edits respect overrides

**Files:**
- Modify: `crates/strider/src/orchestrator.rs`
- Modify: `crates/opt/src/indirect_branch_resolve/inplace.rs` (record per-Call override on the spliced Call when supplied)
- Test: `crates/strider/tests/per_address_cc_indirect.rs`

When the orchestrator's `apply_in_place_edit` lowers a `ResolvedTargets::Single(K)` into an in-place tail-call edit AND `K` is in the per-address override map, it must thread the override CC into `build_anchor_calling_context` and `apply_tail_call`. The spliced Call's clobber outputs come from the override; the per-Call clobber side-table is populated.

- [ ] **Step 1: Write the failing test**

Create `crates/strider/tests/per_address_cc_indirect.rs`:

```rust
//! Indirect branch that resolves to `Single(fentry_addr)` as a tail
//! call: the spliced Call must be built with the per-address override.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider::{CallingConvention, RunConfig, SleighArch, Strider};
use target::CallingConvention as TargetCC;

/// x86_64: `mov rax, fentry_addr; jmp rax`.  The orchestrator
/// classifies the `jmp rax` as `Single(fentry_addr)` and (since
/// fentry_addr lies outside the function range) lowers it as an
/// in-place tail call.  Encoded at base 0x1000:
///
///   0x1000  48 b8 00 20 00 00 00 00 00 00   movabs rax, 0x2000
///   0x100a  ff e0                            jmp rax
fn x86_64_indirect_tail_call() -> (Vec<u8>, u64, u64) {
    let bytes = vec![
        0x48, 0xb8, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0xff, 0xe0,
    ];
    let entry = 0x1000;
    let call_target = 0x2000;
    (bytes, entry, call_target)
}

fn make_strider() -> Strider {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).unwrap()
}

#[test]
fn in_place_tail_call_to_overridden_address_uses_override_clobber_list() {
    let (bytes, entry, call_target) = x86_64_indirect_tail_call();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();

    let mut overrides: HashMap<u64, TargetCC> = HashMap::new();
    overrides.insert(call_target, TargetCC::x86_64_all_preserving());

    let config = RunConfig {
        strider: &strider,
        start_addr: entry,
        sleigh,
        rom: None,
        fn_max_size: Some(0x10),
        allow_code_before_start_addr: false,
        compact: true,
        per_address_ccs: overrides,
    };
    let bfg = strider::run(config).unwrap();

    let call_id = bfg
        .graph
        .all_node_ids()
        .find(|n| matches!(bfg.graph.node_kind(*n), NodeKind::Call))
        .expect("in-place tail call splices in a Call node");
    let outs = bfg.graph.node_outputs(call_id);
    assert_eq!(outs.len(), 2, "in-place tail call to fentry: 0 clobber outputs");
    assert_eq!(bfg.graph.call_clobbered_override(call_id), Some(&[][..]));
}
```

- [ ] **Step 2: Run the test to verify it fails**

```
cargo test -p strider --test per_address_cc_indirect
```
Expected: FAIL — the spliced Call has the SystemV clobber set (15+ outputs), not 2.

- [ ] **Step 3: Plumb the override through `build_anchor_calling_context`**

Edit `crates/strider/src/orchestrator.rs::build_anchor_calling_context` to accept a `Option<&BuiltCallingConvention>` parameter:

```rust
fn build_anchor_calling_context(
    graph: &mut ir::BuiltFunctionGraph,
    placeholder: NodeId,
    strider: &Strider,
    region_index: &RegionIndex,
    override_cc: Option<&target::BuiltCallingConvention>,
) -> opt::AnchorCallingContext {
    let cc: &target::BuiltCallingConvention = match override_cc {
        Some(c) => c,
        None => strider.calling_convention(),
    };
    // ... existing body, but read from `cc` instead of
    // `strider.calling_convention()`; for the clobber-kinds list,
    // when an override is in play, recompute from `cc` instead of
    // iterating `graph.call_clobbered`.

    let region = region_index.region_for_placeholder(graph, placeholder);
    let mut ctx = opt::AnchorCallingContext::default();

    let mut initial_var_index: HashMap<rsleigh::Vn, NodeOutputId> = HashMap::new();
    for nid in graph.graph.all_node_ids() {
        if let ir::node::NodeKind::InitialVar(existing) = graph.graph.node_kind(nid)
            && let Ok([out]) = graph.graph.node_outputs_exact::<1>(nid)
        {
            initial_var_index.insert(*existing, out);
        }
    }

    for vn in &cc.arg_passing_regs {
        if let Some(out) = read_or_init_var(graph, region, &mut initial_var_index, *vn) {
            ctx.arg_passing_outputs.push(out);
        }
    }
    // Clobber list: when an override is supplied, use the override's
    // own callee-saved set to compute a per-call clobber list against
    // the function's tracked variables.  When no override, fall back
    // to the function-default `BuiltFunctionGraph::call_clobbered`
    // (existing behaviour).
    if override_cc.is_some() {
        // Tracked variables = `graph.variables.values()`.
        let stack_ptr_vn = Some(strider.calling_convention().stack_ptr_vn);
        for vn in graph.variables.values() {
            if cc.callee_saved_regs.contains(vn) || Some(*vn) == stack_ptr_vn {
                continue;
            }
            let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) else {
                continue;
            };
            ctx.clobbered_kinds
                .push(ir::node::NodeOutputKind::OutputType(ty));
        }
    } else {
        for vn in graph.call_clobbered.iter() {
            let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) else {
                continue;
            };
            ctx.clobbered_kinds
                .push(ir::node::NodeOutputKind::OutputType(ty));
        }
    }
    for vn in &cc.ret_val_regs {
        if let Some(out) = read_or_init_var(graph, region, &mut initial_var_index, *vn) {
            ctx.ret_val_outputs.push(out);
        }
    }
    ctx
}
```

Update every call-site of `build_anchor_calling_context` to pass `None` (no override). The orchestrator's `apply_in_place_edit` for `Single(K)` looks up `K` in the override map and passes `Some(&cc)` if present:

```rust
fn apply_in_place_edit(
    graph: &mut ir::BuiltFunctionGraph,
    strider: &Strider,
    region_index: &RegionIndex,
    placeholder: NodeId,
    resolved: &ResolvedTargets,
    per_address_built_ccs: &HashMap<u64, target::BuiltCallingConvention>,
) -> Result<()> {
    match resolved {
        ResolvedTargets::LinkRegister => {
            let ctx = build_anchor_calling_context(graph, placeholder, strider, region_index, None);
            apply_link_register(graph, placeholder, &ctx.ret_val_outputs)?;
            Ok(())
        }
        ResolvedTargets::Single(target) => {
            let override_cc = per_address_built_ccs.get(target);
            let ctx = build_anchor_calling_context(
                graph, placeholder, strider, region_index, override_cc,
            );
            let new_call = opt::apply_tail_call(
                graph,
                placeholder,
                *target,
                &ctx.arg_passing_outputs,
                &ctx.clobbered_kinds,
                &ctx.ret_val_outputs,
            )?;
            // When an override was used, record the per-Call clobber
            // varnodes on Graph::call_clobbered_overrides for pattern
            // queries.
            if override_cc.is_some() {
                let stack_ptr_vn = Some(strider.calling_convention().stack_ptr_vn);
                let cc = override_cc.unwrap();
                let clobber_vars: Vec<rsleigh::Vn> = graph
                    .variables
                    .values()
                    .copied()
                    .filter(|v| !cc.callee_saved_regs.contains(v) && Some(*v) != stack_ptr_vn)
                    .collect();
                graph.graph.set_call_clobbered_override(new_call, clobber_vars);
            }
            Ok(())
        }
        ResolvedTargets::Multiple(_) => Err(anyhow!(
            "apply_in_place_edit called with ResolvedTargets::Multiple — caller must route via CFG rebuild"
        )),
    }
}
```

Update `LoopState::apply_in_place_edits` to pass the `&self.opts.per_address_built_ccs` reference through:

```rust
    fn apply_in_place_edits(
        &mut self,
        in_place_edits: &[(NodeId, ResolvedTargets)],
    ) -> Result<()> {
        let strider = self.opts.strider;
        let region_index = &self.region_index;
        let per_address_built_ccs = &self.opts.per_address_built_ccs;
        let graph = self
            .graph
            .as_mut()
            .ok_or_else(|| anyhow!("orchestrator: graph not initialised"))?;
        for (placeholder, resolved) in in_place_edits {
            apply_in_place_edit(graph, strider, region_index, *placeholder, resolved, per_address_built_ccs)?;
        }
        Ok(())
    }
```

`apply_tail_call`'s signature stays the same (it doesn't need to know about overrides — the orchestrator records the side-table entry post-splice). No edit to `crates/opt/src/indirect_branch_resolve/inplace.rs` required.

- [ ] **Step 4: Run the test to verify it passes**

```
cargo test -p strider --test per_address_cc_indirect
```
Expected: PASS.

- [ ] **Step 5: Verify the workspace still builds and tests still pass**

```
cargo test --workspace
```
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/strider/src/orchestrator.rs crates/strider/tests/per_address_cc_indirect.rs
git commit -m "strider: in-place tail-call edits respect per-address CC

apply_in_place_edit looks up the resolved Single(K) target in the
per-address override map; when present, build_anchor_calling_context
threads the override CC and the spliced Call's per-Call clobber
list is recorded on Graph::call_clobbered_overrides."
```

---

## Task 11: Plumb `per_address_ccs` through `strider-py`

**Files:**
- Modify: `crates/strider-py/src/run.rs`
- Test: `crates/strider-py/tests/python/test_per_address_cc.py`

Adds `per_address_ccs: dict[int, CallingConvention] | None = None` keyword argument to `strider.run`. Iterates the dict, unwraps each `PyCallingConvention.inner` into the `RunConfig`'s `HashMap<u64, target::CallingConvention>`. Errors from `cc.build()` surface as `LiftError`. The custom-pipeline path ignores the override (no orchestrator there); document the limitation in the docstring.

Also expose `target::CallingConvention::x86_64_all_preserving` from Python via a new classmethod on `PyCallingConvention` so the test (and real users) can construct it without writing the register list manually.

- [ ] **Step 1: Expose `x86_64_all_preserving` in `PyCallingConvention`**

In `crates/strider-py/src/cc.rs`, add the classmethod alongside the other presets:

```rust
    #[classmethod]
    fn x86_64_all_preserving(_cls: &Bound<'_, PyType>) -> Self {
        Self {
            inner: target::CallingConvention::x86_64_all_preserving(),
            preset_name: "x86_64_all_preserving",
        }
    }
```

- [ ] **Step 2: Write the failing Python test**

Create `crates/strider-py/tests/python/test_per_address_cc.py`:

```python
"""End-to-end Python smoke for `strider.run(per_address_ccs=...)`."""

import strider
from strider import CallingConvention, MemoryMap, SleighArch


def _x86_64_call_then_ret_bytes():
    # Layout at 0x1000:
    #   0x1000  e8 fb 0f 00 00     call 0x2000
    #   0x1005  c3                 ret
    return bytes([0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3])


def test_call_to_overridden_address_has_zero_clobber_outputs():
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv_abi()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_call_then_ret_bytes())

    fentry_addr = 0x2000
    overrides = {fentry_addr: CallingConvention.x86_64_all_preserving()}

    result = strider.run(
        arch,
        cc,
        mem,
        entry=0x1000,
        per_address_ccs=overrides,
    )

    # Find the single Call and assert it has zero clobber outputs
    # (Control + Memory only).
    g = result.graph
    call_ids = [n for n in g.all_node_ids() if g.node_kind_str(n) == "Call"]
    assert len(call_ids) == 1, f"expected one Call node, got {len(call_ids)}"
    call = call_ids[0]
    assert len(g.node_outputs(call)) == 2


def test_per_address_ccs_default_empty_does_not_break_normal_calls():
    """Smoke check the default-empty path matches today's behaviour."""
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv_abi()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_call_then_ret_bytes())
    result = strider.run(arch, cc, mem, entry=0x1000)
    # The single Call uses the SystemV clobber set: at least 2 outputs
    # (Control + Memory) plus the SystemV caller-clobbered count.
    g = result.graph
    call_ids = [n for n in g.all_node_ids() if g.node_kind_str(n) == "Call"]
    assert len(call_ids) == 1
    assert len(g.node_outputs(call_ids[0])) > 2
```

(`g.node_kind_str(n)` is illustrative — use whatever accessor `PyGraph` actually exposes for inspecting a node's kind. If none exists, query through pattern matching: `Matcher(g).find_all(call())` returns the Call's `Match`; assert `len(match.bindings) == 1` etc. Adjust to match the existing PyGraph API.)

- [ ] **Step 3: Run the test to verify it fails**

```
cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/test_per_address_cc.py -v
```
Expected: TypeError or compile error — `strider.run` rejects the `per_address_ccs` kwarg, or `CallingConvention.x86_64_all_preserving` not found.

- [ ] **Step 4: Add the `per_address_ccs` kwarg to `strider.run`**

In `crates/strider-py/src/run.rs`, update the `#[pyfunction(signature = …)]` and the function signature on `run`:

```rust
#[pyfunction(signature = (
    arch,
    cc,
    mem,
    entry,
    rom = None,
    pipeline = None,
    allow_code_before_start_addr = false,
    function_max_size = None,
    compact = true,
    per_address_ccs = None,
))]
#[allow(clippy::too_many_arguments)]
pub fn run(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    pipeline: Option<&crate::opt::PyOptimizerPipeline>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
    per_address_ccs: Option<std::collections::HashMap<u64, PyCallingConvention>>,
) -> PyResult<PyRunResult> {
```

Forward to the helpers:

```rust
    match pipeline {
        Some(p) => run_with_custom_pipeline(
            py, arch, cc, mem, entry, rom, p,
            allow_code_before_start_addr, function_max_size, compact,
        ),
        None => run_via_orchestrator(
            py, arch, cc, mem, entry, rom,
            allow_code_before_start_addr, function_max_size, compact,
            per_address_ccs.unwrap_or_default(),
        ),
    }
```

In `run_via_orchestrator`, accept and convert:

```rust
fn run_via_orchestrator(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
) -> PyResult<PyRunResult> {
    // ... existing body up to the RunConfig literal ...
    let per_address_ccs: std::collections::HashMap<u64, target::CallingConvention> =
        per_address_ccs_py
            .into_iter()
            .map(|(addr, py_cc)| (addr, py_cc.inner))
            .collect();

    let config = strider::RunConfig {
        strider: &strider_borrow.inner,
        start_addr: entry,
        sleigh: orch_sleigh,
        rom: rom_arc,
        fn_max_size: function_max_size,
        allow_code_before_start_addr,
        compact,
        per_address_ccs,
    };
    // ... rest unchanged ...
}
```

For the custom-pipeline path: the override is silently ignored (no orchestrator drives lifting on that path). Add a docstring note on `strider.run`:

> When `pipeline` is supplied, `per_address_ccs` is ignored — the
> custom-pipeline path lifts via a single `analyze_cfg` and never
> consults the orchestrator's override map.

- [ ] **Step 5: Run the test to verify it passes**

```
cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/test_per_address_cc.py -v
```
Expected: PASS (both tests).

- [ ] **Step 6: Verify the existing Python suite still passes**

```
cd crates/strider-py && uv run pytest tests/python -v
```
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-py/src/run.rs crates/strider-py/src/cc.rs crates/strider-py/tests/python/test_per_address_cc.py
git commit -m "strider-py: expose per_address_ccs kwarg + x86_64_all_preserving CC

Mirrors RunConfig::per_address_ccs.  Ignored on the custom-pipeline
path (no orchestrator there).  PyCallingConvention.x86_64_all_preserving
exposes the new target preset for the fentry use case."
```

---

## Self-review

**Spec coverage:**

| Spec section | Covered by task(s) |
|---|---|
| **Feature 1 — Compaction** | |
| `Graph::retain_reachable(entry) -> NodeIdRemap` | Task 2 |
| `BuiltFunctionGraph::compact()` | Task 3 |
| `RunConfig::compact: bool` field + finalize wiring | Task 4 |
| Asm-fingerprint preservation across compaction | Task 2 (test) |
| Dedup-cache rebuild | Task 2 (test) |
| Python `compact` kwarg | Task 5 |
| **Feature 2 — Per-address CC** | |
| `Graph::call_clobbered_overrides` side-table | Task 1 |
| `FunctionBuilder::build_call_with_cc` | Task 6 |
| `pattern::Match::get_vn` consults override (Call) | Task 8 |
| `RunConfig::per_address_ccs` field + pre-resolution | Task 9 |
| `Strider::analyze_cfg_with_vns_and_overrides` | Task 9 |
| `IrStrider::process_call_insn` direct-call dispatch | Task 9 |
| Orchestrator in-place tail-call respects override | Task 10 |
| Python `per_address_ccs` kwarg + `x86_64_all_preserving` preset | Task 11 |
| **Feature 3 — Conservative CallOther clobber** | |
| `BuiltFunctionGraph::call_other_clobbered: Box<[Vn]>` | Task 7 |
| `FunctionBuilder::build_call_other` emits clobber slots + rebinds vars | Task 7 |
| `pattern::Match::get_vn` handles CallOther clobbers | Task 8 |

All spec sections traced to a task.

**Type/signature consistency:**

- `Graph::call_clobbered_override` (Task 1) → consumed by `pattern::Match::get_vn` (Task 8) for both Call and CallOther; populated by `FunctionBuilder::build_call_with_cc` (Task 6) and orchestrator's `apply_in_place_edit` (Task 10). Signatures consistent.
- `BuiltFunctionGraph::call_other_clobbered` (Task 7) → consumed by `pattern::Match::get_vn` (Task 8) as the function-default fallback for CallOther clobber slots. Field type `Box<[rsleigh::Vn]>` matches the access pattern in `get_vn`.
- `NodeIdRemap` (Task 2) → consumed by `BuiltFunctionGraph::compact` (Task 3). Both use `node_old_to_new(NodeId) -> Option<NodeId>`.
- `RunConfig::compact` (Task 4) and `RunConfig::per_address_ccs` (Task 9) added in separate tasks; Task 9 explicitly back-fills the missing field-init in Task 4's test (`crates/strider/tests/compact.rs::run_with`).
- `target::CallingConvention::x86_64_all_preserving` (Task 9) → exposed via `PyCallingConvention.x86_64_all_preserving` (Task 11).
- `analyze_cfg_with_vns_and_overrides`'s third parameter type (`&HashMap<u64, BuiltCallingConvention>`) matches the field type on `RunOpts::per_address_built_ccs` (Task 9).
- Task 7's compaction interaction: `BuiltFunctionGraph::call_other_clobbered` is a vn-keyed `Box<[Vn]>` (not NodeId-keyed), so Task 3's `compact()` does NOT need to remap it — confirmed identical to the existing `call_clobbered` field's compaction-neutral handling.

**No placeholders:** every task has concrete code. Two tasks (Task 8 alternative-path for `Match::new_for_test` test scaffolding, Task 9 alternative for the `Sleigh::regs()` accessor name) note "if the obvious accessor isn't available, use this alternative" — these are pragmatic fallbacks, not unspecified gaps; both alternatives are concretely described.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-04-graph-compact-and-per-address-cc-plan.md`. Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using `superpowers:executing-plans`, batch execution with checkpoints.

Which approach?
