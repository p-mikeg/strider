//! Structural verification of the bipartite graph's core invariant:
//! **input ↔ use-list consistency**.
//!
//! Every node input edge is mirrored by an entry in the target value's
//! use-list, and vice-versa.  This invariant is maintained *by construction* —
//! every mutating verb (`create_node`, `add_node_input`, `update_input`,
//! `replace_all_uses`) updates both sides atomically — so through the safe API
//! it can never be violated.  The only way to break it is the `corrupt_*`
//! injectors (test-only), which exist precisely so this checker's detection can
//! be tested.  It therefore lives here, at the graph layer that owns the
//! invariant, rather than in any payload-specific validator downstream.

use rustc_hash::FxHashSet;

use crate::cache::NodeCacheable;
use crate::graph::Graph;
use crate::ids::{NodeId, UseId, ValueId};

/// A violation of the input ↔ use-list consistency invariant.  Empty results
/// from [`Graph::use_list_inconsistencies`] mean the graph is consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UseListInconsistency {
    /// A node input whose target value's use-list does not list it — the
    /// forward link (producer → consumer) is missing.
    InputMissingFromUseList {
        node: NodeId,
        input_idx: usize,
        value: ValueId,
    },
    /// A use-list entry that no longer points back to the value that owns the
    /// list — a stale backward link.
    StaleInputInUseList { value: ValueId, listed_use: UseId },
}

impl<N, V, C: NodeCacheable<N, V>> Graph<N, V, C> {
    /// Checks the bidirectional input ↔ use-list consistency invariant over the
    /// **whole arena** (reachable or not), returning every violation found; an
    /// empty vector means the graph is consistent.
    ///
    /// This should always be empty for a graph built through the safe mutation
    /// API — the invariant is structural.  It is exposed so that guarantee can
    /// be asserted directly (fed a deliberately-corrupted graph via the
    /// `corrupt_*` injectors) and as a debug oracle.
    ///
    /// Cost is O(E): a single sweep over every value's use-list records the set
    /// of listed `UseId`s (and runs the backward check), after which the
    /// forward check is a per-input O(1) membership test.
    pub fn use_list_inconsistencies(&self) -> Vec<UseListInconsistency> {
        let mut errs = Vec::new();

        // Backward sweep: every use-list entry must reference the value whose
        // list it appears in.  Simultaneously collect the set of `UseId`s that
        // appear in *some* use-list, for the forward check below.
        let mut listed: FxHashSet<UseId> = FxHashSet::default();
        for value in self.all_value_ids() {
            let mut cur = self.value_first_use_id(value);
            while let Some(iid) = cur {
                listed.insert(iid);
                if self.value_of_use(iid) != value {
                    errs.push(UseListInconsistency::StaleInputInUseList {
                        value,
                        listed_use: iid,
                    });
                }
                cur = self.next_use(iid);
            }
        }

        // Forward check: every node input's `UseId` must appear in some
        // use-list — otherwise the input edge exists but the producer never
        // admitted it as a consumer.
        for node in self.all_node_ids() {
            let input_count = self.node_inputs(node).len();
            for idx in 0..input_count {
                // The index range is valid by construction (just measured); a
                // failure here would be an internal bug, so skip it rather than
                // mis-report.
                let Ok(use_id) = self.node_input_id_at(node, idx) else {
                    continue;
                };
                if !listed.contains(&use_id) {
                    errs.push(UseListInconsistency::InputMissingFromUseList {
                        node,
                        input_idx: idx,
                        value: self.value_of_use(use_id),
                    });
                }
            }
        }

        errs
    }
}

#[cfg(test)]
mod tests {
    use crate::cache::NodeCacheable;
    use crate::graph::Graph;
    use crate::ids::ValueId;
    use crate::storage::RawStore;

    use super::UseListInconsistency;

    // Minimal concrete payload: a `Const`/`Add` int graph (never cached, so
    // every `create_node` allocates fresh and the structural shape is exactly
    // what the test builds).
    #[derive(Clone, PartialEq, Eq, Debug)]
    enum K {
        Const(i64),
        Add,
    }
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    struct Int;
    struct Cacher;
    impl NodeCacheable<K, Int> for Cacher {
        fn should_cache(_: &K) -> bool {
            false
        }
        fn hash(_: &K, _: &[ValueId], _: &[Int]) -> u64 {
            0
        }
        fn eq(_: &RawStore<K, Int>, _: crate::ids::NodeId, _: &K, _: &[ValueId], _: &[Int]) -> bool {
            false
        }
    }
    type G = Graph<K, Int, Cacher>;

    fn konst(g: &mut G, v: i64) -> ValueId {
        let n = g.create_node(K::Const(v), [], [Int]);
        g.node_outputs(n)[0]
    }

    #[test]
    fn consistent_graph_reports_no_inconsistencies() {
        let mut g = G::default();
        let a = konst(&mut g, 1);
        let b = konst(&mut g, 2);
        g.create_node(K::Add, [a, b], [Int]);
        assert!(
            g.use_list_inconsistencies().is_empty(),
            "a graph built through the safe API must be use-list-consistent"
        );
    }

    #[test]
    fn corrupt_clear_first_use_flags_input_missing_from_use_list() {
        let mut g = G::default();
        let a = konst(&mut g, 1);
        let b = konst(&mut g, 2);
        g.create_node(K::Add, [a, b], [Int]);

        // Sever the forward link from `b` to its consumers: `b`'s use-list head
        // is cleared, but the Add's input edge at slot 1 still points at `b`.
        g.corrupt_clear_first_use(b);

        let errs = g.use_list_inconsistencies();
        assert!(
            errs.iter().any(|e| matches!(
                e,
                UseListInconsistency::InputMissingFromUseList { input_idx: 1, .. }
            )),
            "clearing b's use-list head must flag the slot-1 input as missing; got {errs:?}"
        );
    }

    #[test]
    fn corrupt_retarget_input_flags_stale_use_list_entry() {
        let mut g = G::default();
        let a = konst(&mut g, 1);
        let b = konst(&mut g, 2);
        let add = g.create_node(K::Add, [a, b], [Int]);

        // Retarget the Add's slot-0 input from `a` to `b` WITHOUT touching any
        // use-list: `a`'s use-list still references this use, but the use now
        // points at `b` — a stale backward link.
        let use_id = g.node_input_id_at(add, 0).unwrap();
        g.corrupt_retarget_input(use_id, b);

        let errs = g.use_list_inconsistencies();
        assert!(
            errs.iter()
                .any(|e| matches!(e, UseListInconsistency::StaleInputInUseList { value, .. } if *value == a)),
            "retargeting a's input must flag a's use-list entry as stale; got {errs:?}"
        );
    }
}
