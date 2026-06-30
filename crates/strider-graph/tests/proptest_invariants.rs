//! Property + edge-case + stress suite proving the generic graph data
//! structure with concrete TEST payloads (no strider-ir dependency).
//!
//! The payloads mirror the IR's caching policy in miniature: `Const`/`Add`
//! dedup, `Region` never does — exactly the IR's Region/Phi exclusion.

use std::collections::{HashMap, HashSet};

use proptest::prelude::*;

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

/// A stateless ZST policy that dedups `Const`/`Add` by
/// `(kind, inputs, outputs)` but never `Region`. The `Hash`/`PartialEq` bounds
/// live HERE, in the `hash`/`eq` method bodies — never on `Graph` or
/// [`NodeCacheable`]. All the cache state lives in the generic `NodeCache`
/// inside `Graph`.
struct TestCacher;

/// Hashes a `(kind, inputs, outputs)` structural key. Returns a raw `u64` with
/// NO sentinel knowledge — sentinel avoidance is the generic cache's concern.
fn hash_key(kind: &TestKind, inputs: &[ValueId], outputs: &[TestVal]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    kind.hash(&mut h);
    inputs.hash(&mut h);
    outputs.hash(&mut h);
    h.finish()
}

impl NodeCacheable<TestKind, TestVal> for TestCacher {
    fn should_cache(kind: &TestKind) -> bool {
        matches!(kind, TestKind::Const(_) | TestKind::Add)
    }

    fn hash(kind: &TestKind, inputs: &[ValueId], outputs: &[TestVal]) -> u64 {
        hash_key(kind, inputs, outputs)
    }

    fn eq(
        store: &RawStore<TestKind, TestVal>,
        cand: NodeId,
        kind: &TestKind,
        inputs: &[ValueId],
        outputs: &[TestVal],
    ) -> bool {
        store.kind_of(cand) == kind
            && store.input_values(cand).as_slice() == inputs
            && store.output_kinds(cand).as_slice() == outputs
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

/// Backward-input closure of `roots`: every node reachable by following inputs
/// to their producers. A valid (backward-input-closed) argument to
/// `Graph::retain_reachable`. The graph no longer offers this — reachability is
/// the caller's concern — so the tests compute it themselves.
fn reachable_from<N, V, C: NodeCacheable<N, V>>(
    g: &Graph<N, V, C>,
    roots: impl IntoIterator<Item = NodeId>,
) -> HashSet<NodeId> {
    let mut seen: HashSet<NodeId> = HashSet::new();
    let mut stack: Vec<NodeId> = roots.into_iter().collect();
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        for input in g.node_inputs(n) {
            stack.push(g.producer(input));
        }
    }
    seen
}

fn region_node(g: &mut TestGraph) -> NodeId {
    g.create_node(TestKind::Region, [], [TestVal::Ctrl])
}

#[test]
fn canonicalize_merges_a_mutated_twin() {
    // A = Add(x, y) (cached). C = Add(x, z); rewire z->y so C becomes a
    // structural twin of A (and is invalidated by update_input).
    // canonicalize_node(C) must return A; a genuinely unique node returns None.
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    let z = const_node(&mut g, 3);
    let a = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    let c = g.create_node(TestKind::Add, [x, z], [TestVal::Int]);
    assert_ne!(a, c, "different inputs => not deduped at creation");

    // Rewire C's second input z -> y so C becomes structurally Add(x, y) == A.
    let c_use1 = g.node_input_id_at(c, 1).expect("c has input slot 1");
    g.update_input(c_use1, y);
    assert_eq!(
        g.canonicalize_node(c),
        Some(a),
        "a mutated structural twin canonicalizes to the existing node"
    );

    // A genuinely unique shape (operand order differs) has no twin.
    let d = g.create_node(TestKind::Add, [y, x], [TestVal::Int]);
    assert_eq!(g.canonicalize_node(d), None, "unique node has no twin");
}

#[test]
fn canonicalize_reinserts_unique_mutated_node_then_dedups() {
    // GR-2(1): a node whose inputs change (so it is evicted) but that has NO
    // twin must be RE-INSERTED by canonicalize as its own representative — and
    // a later identical create_node must then dedup to it.
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    let y = const_node(&mut g, 2);
    let z = const_node(&mut g, 3);

    // Cache Add(x, y), then rewire its second input y -> z so it becomes the
    // unique shape Add(x, z). update_input invalidates it (evicts from cache).
    let add = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    let slot1 = g.node_input_id_at(add, 1).expect("slot 1");
    g.update_input(slot1, z);
    assert_eq!(g.nth_input(add, 1), Some(z));

    // No twin exists for Add(x, z): canonicalize returns None AND re-inserts it.
    assert_eq!(
        g.canonicalize_node(add),
        None,
        "unique mutated node: no twin"
    );

    // The re-insert must be observable: an identical create now dedups to `add`.
    let dedup = g.create_node(TestKind::Add, [x, z], [TestVal::Int]);
    assert_eq!(
        dedup, add,
        "canonicalize must have re-established the cache entry for the mutated node"
    );
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
        let expected = reachable_from(&g, [root]);

        let remap = g.retain_reachable(expected.iter().copied());

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
    assert_eq!(
        g.value_uses(x).count(),
        2,
        "x is consumed twice by Add(x,x)"
    );
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
    let remap = g.retain_reachable(reachable_from(&g, []));
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
    let slot0 = g.node_input_id_at(add, 0).ok().unwrap();
    g.update_input(slot0, z);
    assert_eq!(g.nth_input(add, 0), Some(z), "input was rewritten");

    // Re-creating an Add over the ORIGINAL inputs must NOT dedup to the now
    // mutated node — a fresh node proves the stale entry was evicted.
    let fresh = g.create_node(TestKind::Add, [x, y], [TestVal::Int]);
    assert_ne!(fresh, add, "stale cache entry must have been evicted");
    assert_eq!(
        g.nth_input(fresh, 0),
        Some(x),
        "fresh node keeps the original inputs"
    );
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
    let remap = g.retain_reachable(reachable_from(&g, [root]));
    let add_new = remap.node_old_to_new(add).expect("Add survives");
    let x_new = remap.value_old_to_new(x).expect("x survives");
    let y_new = remap.value_old_to_new(y).expect("y survives");

    // Creating a structurally-equal Add over the surviving inputs must dedup to
    // the surviving node — proving the cache was rebuilt over the new ids.
    let dedup = g.create_node(TestKind::Add, [x_new, y_new], [TestVal::Int]);
    assert_eq!(
        dedup, add_new,
        "post-compaction create must dedup to the survivor"
    );
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
fn remove_node_inputs_batch_removes_many_in_one_pass() {
    let mut g = TestGraph::new();
    let vals: Vec<ValueId> = (0..6).map(|v| const_node(&mut g, v)).collect();
    let region = region_node(&mut g);
    for &v in &vals {
        g.add_node_input(region, v);
    }
    assert_eq!(g.node_inputs(region).len(), 6);

    // Remove slots 1, 3, 4 in a single batch (deliberately unsorted input).
    g.remove_node_inputs_batch(region, [3usize, 1, 4]);

    // Survivors are the inputs at original positions 0, 2, 5 in order.
    let remaining: Vec<ValueId> = g.node_inputs(region).into_iter().collect();
    assert_eq!(remaining, vec![vals[0], vals[2], vals[5]]);

    // Removed values lost their use; survivors kept theirs.
    assert_eq!(g.value_uses(vals[1]).count(), 0);
    assert_eq!(g.value_uses(vals[3]).count(), 0);
    assert_eq!(g.value_uses(vals[4]).count(), 0);
    assert_eq!(g.value_uses(vals[0]).count(), 1);
    assert_eq!(g.value_uses(vals[2]).count(), 1);
    assert_eq!(g.value_uses(vals[5]).count(), 1);

    // Surviving input_index values were compacted to 0..3 contiguously.
    assert_eq!(g.nth_input(region, 0), Some(vals[0]));
    assert_eq!(g.nth_input(region, 1), Some(vals[2]));
    assert_eq!(g.nth_input(region, 2), Some(vals[5]));
    assert_eq!(g.nth_input(region, 3), None);

    assert_use_list_consistent(&g);
}

#[test]
fn remove_node_inputs_batch_empty_and_out_of_bounds() {
    let mut g = TestGraph::new();
    let a = const_node(&mut g, 1);
    let b = const_node(&mut g, 2);
    let region = region_node(&mut g);
    g.add_node_input(region, a);
    g.add_node_input(region, b);

    // Empty batch is a no-op.
    g.remove_node_inputs_batch(region, []);
    assert_eq!(g.node_inputs(region).len(), 2);

    // Out-of-bounds indices are ignored; duplicates collapse harmlessly.
    g.remove_node_inputs_batch(region, [99usize, 0, 0]);
    let remaining: Vec<ValueId> = g.node_inputs(region).into_iter().collect();
    assert_eq!(remaining, vec![b]);
    assert_eq!(g.value_uses(a).count(), 0);
    assert_eq!(g.value_uses(b).count(), 1);
    assert_use_list_consistent(&g);
}

#[test]
fn remove_node_inputs_batch_matches_repeated_single_removes() {
    // The batch verb must produce the same result as removing the same slots
    // one at a time (in descending order to keep indices stable).
    let mut batch_g = TestGraph::new();
    let mut single_g = TestGraph::new();
    let batch_vals: Vec<ValueId> = (0..8).map(|v| const_node(&mut batch_g, v)).collect();
    let single_vals: Vec<ValueId> = (0..8).map(|v| const_node(&mut single_g, v)).collect();
    let br = region_node(&mut batch_g);
    let sr = region_node(&mut single_g);
    for i in 0..8 {
        batch_g.add_node_input(br, batch_vals[i]);
        single_g.add_node_input(sr, single_vals[i]);
    }

    let to_remove = [0usize, 2, 5, 7];
    batch_g.remove_node_inputs_batch(br, to_remove);
    // Descending so each single remove leaves lower indices intact.
    for &idx in to_remove.iter().rev() {
        assert!(single_g.remove_node_input(sr, idx as u32));
    }

    let batch_remaining: Vec<i64> = batch_g
        .node_inputs(br)
        .into_iter()
        .map(|v| match batch_g.kind_of_value(v) {
            TestKind::Const(n) => *n,
            _ => unreachable!(),
        })
        .collect();
    let single_remaining: Vec<i64> = single_g
        .node_inputs(sr)
        .into_iter()
        .map(|v| match single_g.kind_of_value(v) {
            TestKind::Const(n) => *n,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(batch_remaining, single_remaining);
    assert_eq!(batch_remaining, vec![1, 3, 4, 6]);
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
    assert_eq!(
        g.value_uses(a).count(),
        0,
        "a must have no uses after replace"
    );
    assert_eq!(g.value_uses(b).count(), 3, "b must have gained all 3 uses");

    // No-op when old has no uses.
    assert!(!g.replace_all_uses(a, b));
    assert_use_list_consistent(&g);
}

#[test]
fn replace_all_uses_self_is_noop_returns_false() {
    // GR-1: a self-redirect replaces nothing and must report `false`, even
    // though the value has live uses.
    let mut g = TestGraph::new();
    let a = const_node(&mut g, 1);
    let r = region_node(&mut g);
    g.add_node_input(r, a);
    g.add_node_input(r, a);
    assert_eq!(g.value_uses(a).count(), 2);

    assert!(
        !g.replace_all_uses(a, a),
        "self-replace redirects nothing, so it reports no-op"
    );
    // The uses are untouched.
    assert_eq!(g.value_uses(a).count(), 2);
    assert_use_list_consistent(&g);
}

#[test]
fn add_self_loop_input_then_canonicalize() {
    // GR-2(4): a node that consumes its own output via add_node_input, then
    // canonicalize. Must not panic, must not find a (spurious) twin, and the
    // node must remain its own canonical entry afterwards.
    let mut g = TestGraph::new();
    let x = const_node(&mut g, 1);
    // Add(x, x) cached.
    let add = g.create_node(TestKind::Add, [x, x], [TestVal::Int]);
    let add_val = g.node_outputs(add)[0];
    // Wire the node's own output back in as a third input → self-loop.
    g.add_node_input(add, add_val);
    assert_eq!(g.nth_input(add, 2), Some(add_val));

    // No other node shares this shape, so canonicalize finds no twin and
    // re-establishes `add` as its own representative.
    assert_eq!(g.canonicalize_node(add), None, "self-loop node has no twin");

    // Re-creating the same self-referential shape must now dedup to `add`,
    // proving the re-insert re-established the cache entry.
    let again = g.create_node(TestKind::Add, [x, x, add_val], [TestVal::Int]);
    assert_eq!(
        again, add,
        "re-created self-loop shape dedups to the survivor"
    );
    assert_use_list_consistent(&g);
}

#[test]
fn reachable_by_inputs_high_fanin_traversal_unchanged() {
    // GR-3: a single high-fan-in value consumed by many nodes. With
    // mark-on-push the producer is enqueued once, but the reachable set (and
    // hence the survivors of retain_reachable) must be identical.
    let mut g = TestGraph::new();
    let shared = const_node(&mut g, 7);
    // 200 Add nodes all consuming `shared` twice (Add(shared, shared) dedups,
    // so vary the other operand to keep them distinct consumers).
    let mut roots = Vec::new();
    for v in 0..200i64 {
        let other = const_node(&mut g, v);
        let sum = add_node(&mut g, shared, other);
        roots.push(g.producer(sum));
    }
    // A zombie unreachable from any root.
    let _zombie = const_node(&mut g, 9999);

    let before = g.all_node_ids().count();
    let remap = g.retain_reachable(reachable_from(&g, roots.clone()));
    // Every root + shared + each `other` const survives; the zombie does not.
    for r in &roots {
        assert!(remap.node_old_to_new(*r).is_some(), "root survives");
    }
    // One node (the zombie) was dropped.
    assert_eq!(g.all_node_ids().count(), before - 1);
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
    assert_eq!(
        *visited.last().unwrap(),
        root,
        "root last in post-order from root"
    );
}

// ── generic NodeCache mechanism: the three policy hooks ──────────────────────
//
// These exercise the generic dedup-cache mechanism directly through `Graph`,
// over minimal `N = u8` "kind" / `V = u8` "value-kind" policies, isolating each
// of the three `NodeCacheable` hooks (should_cache / hash / eq)
// plus the sentinel-avoidance and `NeverCacheable` paths.

mod node_cache_hooks {
    use super::*;

    fn raw_hash(kind: &u8, inputs: &[ValueId], outputs: &[u8]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        kind.hash(&mut h);
        inputs.hash(&mut h);
        outputs.hash(&mut h);
        h.finish()
    }

    fn raw_eq(
        store: &RawStore<u8, u8>,
        cand: NodeId,
        kind: &u8,
        inputs: &[ValueId],
        outputs: &[u8],
    ) -> bool {
        store.kind_of(cand) == kind
            && store.input_values(cand).as_slice() == inputs
            && store.output_kinds(cand).as_slice() == outputs
    }

    /// Caches every kind, hashes the whole tuple, eq by re-read. No
    /// canonicalization.
    struct CacheAll;
    impl NodeCacheable<u8, u8> for CacheAll {
        fn should_cache(_kind: &u8) -> bool {
            true
        }
        fn hash(kind: &u8, inputs: &[ValueId], outputs: &[u8]) -> u64 {
            raw_hash(kind, inputs, outputs)
        }
        fn eq(
            store: &RawStore<u8, u8>,
            cand: NodeId,
            kind: &u8,
            inputs: &[ValueId],
            outputs: &[u8],
        ) -> bool {
            raw_eq(store, cand, kind, inputs, outputs)
        }
    }

    /// Caches every kind but its `hash` ALWAYS returns `u64::MAX` — the lone
    /// sentinel value the generic cache must remap internally so eviction and
    /// membership stay correct. Equality still discriminates by re-read.
    struct SentinelHashPolicy;
    impl NodeCacheable<u8, u8> for SentinelHashPolicy {
        fn should_cache(_kind: &u8) -> bool {
            true
        }
        fn hash(_kind: &u8, _inputs: &[ValueId], _outputs: &[u8]) -> u64 {
            u64::MAX
        }
        fn eq(
            store: &RawStore<u8, u8>,
            cand: NodeId,
            kind: &u8,
            inputs: &[ValueId],
            outputs: &[u8],
        ) -> bool {
            raw_eq(store, cand, kind, inputs, outputs)
        }
    }

    use strider_graph::NeverCacheable;

    #[test]
    fn dedup_hit_returns_same_node() {
        let mut g: Graph<u8, u8, CacheAll> = Graph::new();
        let a = g.create_node(1u8, [], [9u8]);
        let b = g.create_node(1u8, [], [9u8]);
        assert_eq!(a, b, "identical (kind, inputs, outputs) must dedup");
        assert_eq!(g.all_node_ids().count(), 1, "only one node allocated");
    }

    #[test]
    fn distinct_keys_return_distinct_nodes() {
        let mut g: Graph<u8, u8, CacheAll> = Graph::new();
        // Differ by kind.
        let a = g.create_node(1u8, [], [9u8]);
        let b = g.create_node(2u8, [], [9u8]);
        assert_ne!(a, b, "different kind must not dedup");
        // Differ by output kind.
        let c = g.create_node(1u8, [], [8u8]);
        assert_ne!(a, c, "different output kind must not dedup");
        // Differ by inputs.
        let v = g.node_outputs(a)[0];
        let d = g.create_node(3u8, [v], [9u8]);
        let e = g.create_node(3u8, [], [9u8]);
        assert_ne!(d, e, "different inputs must not dedup");
    }

    #[test]
    fn invalidate_then_recreate_allocates_fresh() {
        let mut g: Graph<u8, u8, CacheAll> = Graph::new();
        let x = g.create_node(1u8, [], [9u8]);
        let xv = g.node_outputs(x)[0];
        // Cache a node, then mutate it (which evicts via invalidate).
        let n = g.create_node(2u8, [xv], [9u8]);
        assert_eq!(
            g.create_node(2u8, [xv], [9u8]),
            n,
            "identical create dedups before mutation",
        );
        // Detaching inputs invalidates the cache entry for `n`.
        g.detach_node_inputs(n);
        // Re-creating the ORIGINAL key must NOT revive `n` — a fresh node proves
        // the stale entry was evicted (no revival).
        let fresh = g.create_node(2u8, [xv], [9u8]);
        assert_ne!(fresh, n, "invalidated entry must not be revived");
    }

    #[test]
    fn rebuild_reestablishes_dedup_after_compaction() {
        let mut g: Graph<u8, u8, CacheAll> = Graph::new();
        let x = g.create_node(1u8, [], [9u8]);
        let xv = g.node_outputs(x)[0];
        let n = g.create_node(2u8, [xv], [9u8]);
        let nv = g.node_outputs(n)[0];

        // A non-caching-shaped root keeps `n` reachable. (Every kind caches here,
        // so we just keep `n` itself as the root.)
        let remap = g.retain_reachable(reachable_from(&g, [n]));
        let n_new = remap.node_old_to_new(n).expect("n survives");
        let x_new = remap.value_old_to_new(xv).expect("x survives");
        let _ = nv;

        // After compaction renumbers ids, a structurally-equal create must dedup
        // to the survivor — proving rebuild re-keyed the cache over new ids.
        let dedup = g.create_node(2u8, [x_new], [9u8]);
        assert_eq!(
            dedup, n_new,
            "post-compaction create must dedup to survivor"
        );
    }

    #[test]
    fn sentinel_hash_still_caches_and_evicts() {
        // A policy whose hash is ALWAYS u64::MAX. The generic cache's
        // avoid_sentinel remap must keep dedup, eviction, and membership correct.
        let mut g: Graph<u8, u8, SentinelHashPolicy> = Graph::new();
        let x = g.create_node(1u8, [], [9u8]);
        let xv = g.node_outputs(x)[0];

        // Two distinct keys both hash to u64::MAX → same bucket, discriminated by
        // re-read eq. They must remain distinct nodes.
        let a = g.create_node(2u8, [xv], [9u8]);
        let b = g.create_node(3u8, [xv], [9u8]);
        assert_ne!(a, b, "colliding-hash distinct keys stay distinct");
        // Identical key dedups despite the sentinel hash.
        assert_eq!(g.create_node(2u8, [xv], [9u8]), a, "identical key dedups");

        // Eviction must still find `a`'s bucket (membership tracked despite the
        // sentinel-remapped hash).
        g.detach_node_inputs(a);
        let fresh = g.create_node(2u8, [xv], [9u8]);
        assert_ne!(fresh, a, "sentinel-hashed entry still evicts correctly");
        // `b` was untouched and still dedups.
        assert_eq!(
            g.create_node(3u8, [xv], [9u8]),
            b,
            "untouched entry survives"
        );
    }

    #[test]
    fn canonicalize_ignores_hash_collision_non_twin() {
        // GR-2(2): under a policy where EVERY node hashes to the same bucket, a
        // mutated node whose new shape collides (same hash) with an existing,
        // structurally-DIFFERENT node must NOT be reported as a twin —
        // canonicalize must re-read structure via eq and re-insert the node as
        // its own representative.
        let mut g: Graph<u8, u8, SentinelHashPolicy> = Graph::new();
        let x = g.create_node(1u8, [], [9u8]);
        let xv = g.node_outputs(x)[0];
        let y = g.create_node(2u8, [], [9u8]);
        let yv = g.node_outputs(y)[0];

        // Two distinct cacheable nodes; both land in the same (sentinel) bucket.
        let keep = g.create_node(7u8, [xv], [9u8]);
        let mutate = g.create_node(8u8, [xv], [9u8]);
        assert_ne!(keep, mutate);

        // Mutate `mutate` so its shape is 8u8,[yv] — still collides on hash with
        // `keep` (8u8,[xv]) but is structurally different from BOTH. The kind
        // differs from `keep`, so there must be no twin.
        let slot0 = g.node_input_id_at(mutate, 0).ok().unwrap();
        g.update_input(slot0, yv);
        assert_eq!(
            g.canonicalize_node(mutate),
            None,
            "hash collision is not structural equality — no twin"
        );

        // Re-insert must be observable: recreating the mutated shape dedups to it.
        let dedup = g.create_node(8u8, [yv], [9u8]);
        assert_eq!(dedup, mutate, "canonicalize re-established the entry");
    }

    #[test]
    fn never_cacheable_always_allocates_fresh() {
        let mut g: Graph<u8, u8, NeverCacheable> = Graph::new();
        let a = g.create_node(1u8, [], [9u8]);
        let b = g.create_node(1u8, [], [9u8]);
        assert_ne!(a, b, "NeverCacheable never dedups");
        assert_eq!(g.all_node_ids().count(), 2, "two fresh nodes");
    }
}
