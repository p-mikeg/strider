//! Property + edge-case + stress suite proving the generic graph data
//! structure with concrete TEST payloads (no strider-ir dependency).
//!
//! The payloads mirror the IR's caching policy in miniature: `Const`/`Add`
//! dedup, `Region` never does — exactly the IR's Region/Phi exclusion.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;
use smallvec::SmallVec;

use strider_graph::{Graph, NodeCacheable, NodeId, RawStore, ValueId};

// ── test payloads ───────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum TestKind {
    Const(i64),
    Add,
    Region,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum TestVal {
    Int,
    Ctrl,
}

/// A cacher that dedups `Const`/`Add` by `(kind, inputs, outputs)` but never
/// `Region`. The `Hash + Eq` bound lives HERE, on the caching impl — never on
/// `Graph` or [`NodeCacheable`].
#[derive(Default)]
struct TestCacher {
    cache: HashMap<(TestKind, Vec<ValueId>, Vec<TestVal>), NodeId>,
}

/// Whether a payload is one the cacher dedups.
fn is_cacheable(kind: &TestKind) -> bool {
    matches!(kind, TestKind::Const(_) | TestKind::Add)
}

/// Recomputes a cacheable node's structural key by reading its current shape
/// from the store. Returns `None` for non-cacheable nodes.
fn key_from_store(
    store: &RawStore<TestKind, TestVal>,
    node: NodeId,
) -> Option<(TestKind, Vec<ValueId>, Vec<TestVal>)> {
    let kind = store.kind_of(node).clone();
    if !is_cacheable(&kind) {
        return None;
    }
    Some((
        kind,
        store.input_values(node).to_vec(),
        store.output_kinds(node).to_vec(),
    ))
}

impl NodeCacheable<TestKind, TestVal> for TestCacher {
    fn create(
        &mut self,
        store: &mut RawStore<TestKind, TestVal>,
        kind: TestKind,
        inputs: SmallVec<[ValueId; 4]>,
        outputs: SmallVec<[TestVal; 4]>,
    ) -> NodeId {
        if is_cacheable(&kind) {
            let key = (kind.clone(), inputs.to_vec(), outputs.to_vec());
            if let Some(&existing) = self.cache.get(&key) {
                return existing;
            }
            let id = store.alloc_node(kind, inputs, outputs);
            self.cache.insert(key, id);
            return id;
        }
        store.alloc_node(kind, inputs, outputs)
    }

    /// Drop the dedup entry keyed on `node`'s CURRENT (pre-mutation) shape, so a
    /// later `create` of that same shape allocates fresh instead of returning
    /// the now-mutated node.
    fn invalidate(&mut self, node: NodeId, store: &RawStore<TestKind, TestVal>) {
        if let Some(key) = key_from_store(store, node)
            && self.cache.get(&key) == Some(&node)
        {
            self.cache.remove(&key);
        }
    }

    /// Rebuild the whole cache over the renumbered survivors after compaction:
    /// clear, then re-insert every surviving cacheable node by its current key.
    fn rebuild(&mut self, store: &RawStore<TestKind, TestVal>) {
        self.cache.clear();
        for node in store.node_ids() {
            if let Some(key) = key_from_store(store, node) {
                self.cache.insert(key, node);
            }
        }
    }
}

type TestGraph = Graph<TestKind, TestVal, TestCacher>;

// ── helpers ─────────────────────────────────────────────────────────────────

fn const_node(g: &mut TestGraph, v: i64) -> ValueId {
    let n = g.create_node(TestKind::Const(v), [], [TestVal::Int]);
    g.node_outputs(n)[0]
}

fn add_node(g: &mut TestGraph, a: ValueId, b: ValueId) -> ValueId {
    let n = g.create_node(TestKind::Add, [a, b], [TestVal::Int]);
    g.node_outputs(n)[0]
}

fn region_node(g: &mut TestGraph) -> NodeId {
    g.create_node(TestKind::Region, [], [TestVal::Ctrl])
}

/// Counts the input edges in the whole arena that reference `v`, by scanning
/// every node's inputs (independent of the use-list, so it cross-checks it).
fn input_edges_referencing(g: &TestGraph, v: ValueId) -> usize {
    g.all_node_ids()
        .flat_map(|n| {
            let inputs: Vec<ValueId> = g.node_inputs(n).into_iter().collect();
            inputs.into_iter()
        })
        .filter(|&val| val == v)
        .count()
}

/// Asserts bidirectional use-list consistency over the whole graph:
/// (a) every input edge appears in its consumed value's use-list; and
/// (b) every use-list entry corresponds to a real input edge.
fn assert_use_list_consistent(g: &TestGraph) {
    // (a) For each node input slot, that (node, slot) must appear in the
    //     value's use-list.
    for node in g.all_node_ids() {
        let inputs: Vec<ValueId> = g.node_inputs(node).into_iter().collect();
        for (slot, value) in inputs.into_iter().enumerate() {
            let found = g
                .value_uses(value)
                .any(|(c, i)| c == node && i as usize == slot);
            assert!(
                found,
                "input edge ({node:?} slot {slot} -> {value:?}) missing from use-list",
            );
        }
    }

    // (b) For every value, each use-list entry must name a real input edge.
    for node in g.all_node_ids() {
        for &value in g.node_outputs(node) {
            for (consumer, slot) in g.value_uses(value) {
                let actual = g.nth_input(consumer, slot as usize);
                assert_eq!(
                    actual,
                    Some(value),
                    "use-list entry ({consumer:?} slot {slot}) does not point back to {value:?}",
                );
            }
        }
    }
}

// ── proptest: random valid-DAG construction ─────────────────────────────────

#[derive(Clone, Debug)]
enum Op {
    Const(i64),
    Add(usize, usize),
    Replace(usize, usize),
    RegionAddInput(usize, usize),
    RegionRemoveInput(usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (-5i64..5).prop_map(Op::Const),
        (0usize..32, 0usize..32).prop_map(|(a, b)| Op::Add(a, b)),
        (0usize..32, 0usize..32).prop_map(|(a, b)| Op::Replace(a, b)),
        (0usize..8, 0usize..32).prop_map(|(r, v)| Op::RegionAddInput(r, v)),
        (0usize..8).prop_map(Op::RegionRemoveInput),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    /// Build a random valid DAG via a sequence of ops, then assert the
    /// structural invariants hold.
    #[test]
    fn random_dag_preserves_invariants(ops in prop::collection::vec(op_strategy(), 1..80)) {
        let mut g = TestGraph::new();
        let mut values: Vec<ValueId> = Vec::new();
        let mut regions: Vec<NodeId> = Vec::new();

        // Seed with one const so Add/Replace have operands.
        values.push(const_node(&mut g, 0));

        for op in ops {
            match op {
                Op::Const(v) => values.push(const_node(&mut g, v)),
                Op::Add(a, b) => {
                    if values.is_empty() { continue; }
                    let a = values[a % values.len()];
                    let b = values[b % values.len()];
                    values.push(add_node(&mut g, a, b));
                }
                Op::Replace(a, b) => {
                    if values.is_empty() { continue; }
                    let old = values[a % values.len()];
                    let new = values[b % values.len()];
                    // (c) replace_all_uses invariant, checked in detail below.
                    let old_uses: Vec<(NodeId, u32)> = g.value_uses(old).collect();
                    let new_uses_before: HashSet<(NodeId, u32)> =
                        g.value_uses(new).collect();
                    if old != new {
                        g.replace_all_uses(old, new);
                        // old now has no uses.
                        prop_assert_eq!(g.value_uses(old).count(), 0);
                        // new gained exactly old's former uses (set union).
                        let new_uses_after: HashSet<(NodeId, u32)> =
                            g.value_uses(new).collect();
                        for u in &old_uses {
                            prop_assert!(new_uses_after.contains(u),
                                "new value must have gained old's use {u:?}");
                        }
                        for u in &new_uses_before {
                            prop_assert!(new_uses_after.contains(u),
                                "new value kept its own prior uses");
                        }
                    }
                }
                Op::RegionAddInput(r, v) => {
                    // Lazily create regions on demand up to index r.
                    while regions.len() <= (r % 8) {
                        regions.push(region_node(&mut g));
                    }
                    if values.is_empty() { continue; }
                    let region = regions[r % regions.len().max(1)];
                    let value = values[v % values.len()];
                    g.add_node_input(region, value);
                }
                Op::RegionRemoveInput(r) => {
                    if regions.is_empty() { continue; }
                    let region = regions[r % regions.len()];
                    let arity = g.node_inputs(region).len() as u32;
                    if arity > 0 {
                        g.remove_node_input(region, arity - 1);
                    }
                }
            }
        }

        // (a)+(b) use-list bidirectional consistency.
        assert_use_list_consistent(&g);

        // value_uses(v).count() == #input edges referencing v.
        for node in g.all_node_ids() {
            for &value in g.node_outputs(node) {
                let via_use_list = g.value_uses(value).count();
                let via_scan = input_edges_referencing(&g, value);
                prop_assert_eq!(via_use_list, via_scan,
                    "use-list count must equal input-edge count for {:?}", value);
            }
        }

        // (e) toposort yields each producer before its consumers.
        if let Ok(order) = petgraph::algo::toposort(&g, None) {
            let pos: HashMap<strider_graph::Vertex, usize> =
                order.iter().enumerate().map(|(i, &v)| (v, i)).collect();
            for node in g.all_node_ids() {
                let node_v = strider_graph::Vertex::Node(node);
                for &out in g.node_outputs(node) {
                    let val_v = strider_graph::Vertex::Value(out);
                    // producer node precedes its value vertex.
                    prop_assert!(pos[&node_v] < pos[&val_v]);
                    // value vertex precedes each consumer node.
                    for (consumer, _) in g.value_uses(out) {
                        let cons_v = strider_graph::Vertex::Node(consumer);
                        prop_assert!(pos[&val_v] < pos[&cons_v],
                            "value {:?} must precede consumer {:?} in toposort", out, consumer);
                    }
                }
            }
        }
    }

    /// (d) retain_reachable keeps exactly the reachable node set and the remap
    /// is injective on survivors.
    #[test]
    fn retain_reachable_keeps_reachable_set(
        consts in prop::collection::vec(-5i64..5, 1..10),
    ) {
        let mut g = TestGraph::new();
        let vals: Vec<ValueId> = consts.iter().map(|&v| const_node(&mut g, v)).collect();
        // Build a small chain of adds rooted at the last const.
        let mut root_val = vals[0];
        for &v in &vals[1..] {
            root_val = add_node(&mut g, root_val, v);
        }
        let root = g.producer(root_val);

        // A zombie const not connected to the root.
        let _zombie = const_node(&mut g, 1234);

        // Expected reachable set (by inputs) from `root`.
        let expected: HashSet<NodeId> = g.preorder_seeds([root]).into_iter().collect();

        let remap = g.retain_reachable_roots([root]);

        // Survivors: exactly the expected set remapped to Some.
        let mut survivor_news: Vec<NodeId> = Vec::new();
        for &old in &expected {
            let new = remap.node_old_to_new(old);
            prop_assert!(new.is_some(), "reachable {old:?} must survive");
            survivor_news.push(new.unwrap());
        }
        // Injective on survivors.
        let unique: HashSet<NodeId> = survivor_news.iter().copied().collect();
        prop_assert_eq!(unique.len(), survivor_news.len(), "remap not injective");

        // The compacted graph has exactly |expected| nodes.
        prop_assert_eq!(g.all_node_ids().count(), expected.len());
    }
}

// ── edge-case units ─────────────────────────────────────────────────────────

#[test]
fn add_with_repeated_operand_counts_two_uses() {
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 7);
    let _sum = add_node(&mut g, x, x);
    assert_eq!(g.value_uses(x).count(), 2, "x is consumed twice by Add(x,x)");
    assert_use_list_consistent(&g);
}

#[test]
fn multi_output_node() {
    let mut g = TestGraph::new();
    // Region with two outputs (non-cacheable so we can shape it freely).
    let region = g.create_node(TestKind::Region, [], [TestVal::Ctrl, TestVal::Int]);
    let outs = g.node_outputs(region);
    assert_eq!(outs.len(), 2);
    assert_eq!(g.value_kind(outs[0]), TestVal::Ctrl);
    assert_eq!(g.value_kind(outs[1]), TestVal::Int);
    for &o in g.node_outputs(region) {
        assert_eq!(g.producer(o), region);
    }
}

#[test]
fn value_with_zero_uses() {
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 3);
    assert_eq!(g.value_uses(x).count(), 0);
    assert!(!g.value_has_one_use(x));
    assert_eq!(g.value_first_use_id(x), None);
}

#[test]
fn empty_graph_retain_reachable_no_op() {
    let mut g = TestGraph::new();
    let remap = g.retain_reachable_roots([]);
    assert_eq!(g.all_node_ids().count(), 0);
    // No survivors to query; just ensure it doesn't panic and bumps gen.
    assert_eq!(g.generation(), 1);
    // Remap of any (nonexistent) id is None by construction.
    let _ = remap;
}

#[test]
fn cacher_dedups_const_distinct_regions() {
    let mut g = TestGraph::new();
    let a = g.create_node(TestKind::Const(5), [], [TestVal::Int]);
    let b = g.create_node(TestKind::Const(5), [], [TestVal::Int]);
    assert_eq!(a, b, "two identical Const(5) must dedup to one NodeId");

    let r1 = region_node(&mut g);
    let r2 = region_node(&mut g);
    assert_ne!(r1, r2, "two Regions must be distinct NodeIds");
}

#[test]
fn cacher_dedups_identical_add() {
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    let n1 = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    let n2 = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    assert_eq!(n1, n2, "identical Add(x,y) must dedup");
}

#[test]
fn mutating_cached_node_evicts_it() {
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    let z = const_node(&mut g, 3);

    // Cache an Add(x, y).
    let add = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    // Re-creating the same shape must dedup to it (the cache entry is live).
    assert_eq!(
        g.create_node(TestKind::Add, [x, y], [TestVal::Int]),
        add,
        "identical Add must dedup before mutation",
    );

    // Mutate the cached node's first input x -> z via update_input. This must
    // invalidate the stale (Add, [x, y], _) cache entry BEFORE the change.
    let slot0 = g.node_input_id_at_opt(add, 0).unwrap();
    g.update_input(slot0, z);
    assert_eq!(g.nth_input(add, 0), Some(z), "input was rewritten");

    // Re-creating an Add over the ORIGINAL inputs must NOT dedup to the now
    // mutated node — a fresh node proves the stale entry was evicted.
    let fresh = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    assert_ne!(fresh, add, "stale cache entry must have been evicted");
    assert_eq!(g.nth_input(fresh, 0), Some(x), "fresh node keeps the original inputs");
}

#[test]
fn compaction_rebuilds_cache() {
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    // Build a deduped Add(x, y), kept reachable through a Return-like sink.
    let add = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    let add_val = g.node_outputs(add)[0];

    // A region root that keeps the Add reachable by inputs.
    let root = region_node(&mut g);
    g.add_node_input(root, add_val);

    // A zombie const, unreachable from `root`.
    let _zombie = const_node(&mut g, 999);

    // Compact: ids are renumbered, so the pre-compaction cache is stale; the
    // rebuild hook must re-key the cache over the survivors.
    let remap = g.retain_reachable_roots([root]);
    let add_new = remap.node_old_to_new(add).expect("Add survives");
    let x_new = remap.value_old_to_new(x).expect("x survives");
    let y_new = remap.value_old_to_new(y).expect("y survives");

    // Creating a structurally-equal Add over the surviving inputs must dedup to
    // the surviving node — proving the cache was rebuilt over the new ids.
    let dedup = g.create_node(TestKind::Add, [x_new, y_new], [TestVal::Int]);
    assert_eq!(dedup, add_new, "post-compaction create must dedup to the survivor");
}

#[test]
fn detach_then_readd_inputs() {
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    let region = region_node(&mut g);
    g.add_node_input(region, x);
    g.add_node_input(region, y);
    assert_eq!(g.node_inputs(region).len(), 2);
    assert_eq!(g.value_uses(x).count(), 1);

    g.detach_node_inputs(region);
    assert_eq!(g.node_inputs(region).len(), 0);
    assert_eq!(g.value_uses(x).count(), 0);
    assert_eq!(g.value_uses(y).count(), 0);

    g.add_node_input(region, y);
    assert_eq!(g.node_inputs(region).len(), 1);
    assert_eq!(g.nth_input(region, 0), Some(y));
    assert_eq!(g.value_uses(y).count(), 1);
    assert_use_list_consistent(&g);
}

#[test]
fn remove_node_input_compacts_indices() {
    let mut g = TestGraph::new();
    let a = const_node(&mut g, 1);
    let b = const_node(&mut g, 2);
    let c = const_node(&mut g, 3);
    let region = region_node(&mut g);
    g.add_node_input(region, a);
    g.add_node_input(region, b);
    g.add_node_input(region, c);

    // Remove the middle input (b).
    assert!(g.remove_node_input(region, 1));
    assert_eq!(g.node_inputs(region).len(), 2);
    assert_eq!(g.nth_input(region, 0), Some(a));
    assert_eq!(g.nth_input(region, 1), Some(c));
    assert_eq!(g.value_uses(b).count(), 0);
    // Out-of-bounds removal returns false.
    assert!(!g.remove_node_input(region, 99));
    assert_use_list_consistent(&g);
}

#[test]
fn stress_10k_nodes_dedup_bounded() {
    let mut g = TestGraph::new();
    // Heavy Const reuse: only 4 distinct const values, each created 2500 times.
    for _ in 0..2500 {
        for v in 0..4 {
            let _ = const_node(&mut g, v);
        }
    }
    // 10_000 create calls but only 4 distinct deduped Const nodes.
    assert_eq!(
        g.all_node_ids().count(),
        4,
        "heavy Const reuse must collapse to the 4 distinct values",
    );

    // Now build a wide reduction tree over the 4 const values; each Add layer
    // also dedups when operands repeat.
    let base: Vec<ValueId> = (0..4)
        .map(|v| {
            let n = g.create_node(TestKind::Const(v), [], [TestVal::Int]);
            g.node_outputs(n)[0]
        })
        .collect();
    // Repeatedly add the same pair: must dedup to a single Add node.
    for _ in 0..1000 {
        let _ = add_node(&mut g, base[0], base[1]);
    }
    assert_eq!(
        g.all_node_ids().count(),
        5,
        "4 consts + 1 deduped Add despite 1000 create calls",
    );

    // toposort still works on the deduped graph.
    let order = petgraph::algo::toposort(&g, None).expect("acyclic");
    assert!(!order.is_empty());
}

#[test]
fn replace_all_uses_moves_uses() {
    let mut g = TestGraph::new();
    let a = const_node(&mut g, 1);
    let b = const_node(&mut g, 2);
    // Three consumers of `a` (via non-dedup Region inputs).
    let mut regions = Vec::new();
    for _ in 0..3 {
        let r = region_node(&mut g);
        g.add_node_input(r, a);
        regions.push(r);
    }
    assert_eq!(g.value_uses(a).count(), 3);
    assert_eq!(g.value_uses(b).count(), 0);

    assert!(g.replace_all_uses(a, b));
    assert_eq!(g.value_uses(a).count(), 0, "a must have no uses after replace");
    assert_eq!(g.value_uses(b).count(), 3, "b must have gained all 3 uses");

    // No-op when old has no uses.
    assert!(!g.replace_all_uses(a, b));
    assert_use_list_consistent(&g);
}

#[test]
fn dfs_post_order_runs_on_graph() {
    use petgraph::visit::{DfsPostOrder, Walker};
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    let sum = add_node(&mut g, x, y);
    let root = strider_graph::Vertex::Node(g.producer(sum));

    let visited: Vec<_> = DfsPostOrder::new(&g, root).iter(&g).collect();
    // The Add node's vertex must be present and come after its produced value
    // is NOT required here (DfsPostOrder over outgoing edges), but the root
    // (Add) must be the last in a post-order from itself.
    assert!(visited.contains(&root));
    assert_eq!(*visited.last().unwrap(), root, "root last in post-order from root");
}
