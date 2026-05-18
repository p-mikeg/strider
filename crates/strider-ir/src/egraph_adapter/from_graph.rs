//! `EGraphAdapter::from_graph` — converts a strider [`crate::Graph`] into an
//! `egg::EGraph<StriderLang, ()>` walking only the value subgraph.
//!
//! Phase 1 Task 1.5 spike — step 3 implementation.
//!
//! # Algorithm
//!
//! 1. Walk the reachable graph from `entry` via [`crate::walk::walk_graph`].
//! 2. For every reachable node, for every value output it produces, call
//!    [`Self::add_value_output`].
//! 3. `add_value_output` is memoised:
//!    - If the output is opaque (phi / `InitialVar` / `FunctionArg` / `Load`
//!      value / `Call`/`CallOther` value / `InitialMemory` accidentally
//!      consumed): add an [`StriderLang::Opaque`] leaf with a stable u64
//!      payload derived from the [`crate::node::NodeOutputId`] arena index.
//!    - Else: recursively add value inputs, then build the matching
//!      [`StriderLang`] internal-op variant.
//!
//! # Out of scope (preserved structurally)
//!
//! `Control`, `Memory`, `PhiToken` edges. Multi-output node bookkeeping
//! (each value output of a `Call` becomes its own opaque leaf, but the
//! `Call` node itself never appears in the egraph). All this is recovered
//! by [`super::extract::EGraphAdapter::extract_into_graph`] which threads
//! through the original `Graph` directly.

use std::collections::HashMap;

use egg::{Analysis, EGraph, Id};

use super::language::StriderLang;
use crate::graph::Graph;
use crate::node::{NodeId, NodeKind, NodeOutputId};

/// Adapter holding the egraph plus the mapping tables needed to round-trip
/// a strider graph through egg with zero rewrites applied.
///
/// Parameterised over the egg [`Analysis`] attached to the e-graph.
/// Defaults to `()` (no per-eclass metadata).  Passes that need
/// per-eclass data (e.g. [`crate::egraph_adapter::from_graph::EGraphAdapter`]
/// users such as `KnownBitsEgg`) parameterise on their own
/// `Analysis<StriderLang>` impl and construct the adapter via
/// [`EGraphAdapter::from_graph_with_analysis`].
pub struct EGraphAdapter<A: Analysis<StriderLang> = ()> {
    /// The egg egraph built from the value-slice subgraph of the source
    /// [`Graph`].
    pub egraph: EGraph<StriderLang, A>,
    /// Maps every value [`NodeOutputId`] added to the egraph to its
    /// e-class id.  Multi-value nodes (`Call`, modeled `CallOther`) get
    /// one entry per value output — each is a distinct opaque leaf.
    pub output_to_eclass: HashMap<NodeOutputId, Id>,
    /// Reverse map for opaque leaves: payload `u64` → source
    /// [`NodeOutputId`].  Used by `extract_into_graph` to recover the
    /// strider-side origin when an opaque-leaf e-class is materialised
    /// in the rebuilt graph.
    pub leaf_to_output: HashMap<u64, NodeOutputId>,
}

impl EGraphAdapter<()> {
    /// Builds an egraph from the value-slice subgraph of `g` reachable
    /// from `entry`.  Zero rewrites are applied — the resulting egraph
    /// has exactly one e-node per value output, so [`Self::extract`]
    /// trivially recovers the original structure.
    ///
    /// This is the default constructor producing an adapter with the
    /// unit (`()`) analysis.  Passes that need per-eclass metadata
    /// should use [`EGraphAdapter::from_graph_with_analysis`] instead.
    #[must_use]
    pub fn from_graph(g: &Graph, entry: NodeId) -> Self {
        Self::from_graph_with_analysis(g, entry, ())
    }
}

impl<A: Analysis<StriderLang>> EGraphAdapter<A> {
    /// Builds an egraph (parameterised on the supplied [`Analysis`]) from
    /// the value-slice subgraph of `g` reachable from `entry`.  Zero
    /// rewrites are applied; the resulting egraph has exactly one
    /// e-node per value output.
    ///
    /// The analysis's [`Analysis::make`] transfer function runs as each
    /// e-node is added, so per-eclass `Data` lattices are populated
    /// bottom-up automatically.  Opaque leaves get `Analysis::make`'d
    /// with `StriderLang::Opaque(_)` — the analysis should return its
    /// "no information" `Data` value for that variant.
    #[must_use]
    pub fn from_graph_with_analysis(g: &Graph, entry: NodeId, analysis: A) -> Self {
        Self::from_graph_with_analysis_and_visit(g, entry, analysis, |_, _, _, _| {})
    }

    /// Like [`Self::from_graph_with_analysis`] but invokes a per-add
    /// callback `visit(egraph, oid, kind, eclass_id)` after each value
    /// output is added.  The callback runs *before* any parent e-node
    /// is built, so consumers can patch an opaque leaf's analysis data
    /// (e.g. inject the strider-side output type that the
    /// `StriderLang::Opaque(u64)` payload doesn't carry) and have
    /// the patched value visible to subsequent `Analysis::make` calls
    /// on parent enodes.
    ///
    /// Use case: `KnownBitsEgg` patches each opaque leaf's
    /// `BitLattice::type_mask` so Popcount / Lzcount / Extend transfer
    /// functions on parent enodes see the input's width.
    pub fn from_graph_with_analysis_and_visit<F>(
        g: &Graph,
        entry: NodeId,
        analysis: A,
        mut visit: F,
    ) -> Self
    where
        F: FnMut(&mut EGraph<StriderLang, A>, NodeOutputId, &NodeKind, Id),
    {
        let mut adapter = Self {
            egraph: EGraph::new(analysis),
            output_to_eclass: HashMap::new(),
            leaf_to_output: HashMap::new(),
        };
        // Walk all reachable nodes; for each, attempt to add every value
        // output it produces.  The recursive `add_value_output` helper
        // memoises on `output_to_eclass`, so multiple consumers of the
        // same producer hit the same e-class.
        for node_id in crate::walk::walk_graph(g, entry) {
            for oid in g.node_outputs(node_id) {
                if g.output_kind(oid).is_value() {
                    adapter.add_value_output(g, oid, &mut visit);
                }
            }
        }
        adapter.egraph.rebuild();
        adapter
    }

    /// Adds `oid` (a value output) to the egraph and returns its e-class id.
    /// Memoised; safe to call recursively for child slots.
    fn add_value_output<F>(&mut self, g: &Graph, oid: NodeOutputId, visit: &mut F) -> Id
    where
        F: FnMut(&mut EGraph<StriderLang, A>, NodeOutputId, &NodeKind, Id),
    {
        if let Some(&id) = self.output_to_eclass.get(&oid) {
            return id;
        }
        let (node_id, _output_index) = g.output_definition(oid);
        let kind = *g.node_kind(node_id);
        let out_kind = g.output_kind(oid);

        let id = if is_opaque_value_kind(&kind) {
            // Opaque leaf: payload encodes the source NodeOutputId arena
            // index so distinct outputs (including multiple value outputs
            // of the same Call node) never collide.
            let payload = oid.as_u32() as u64;
            self.leaf_to_output.insert(payload, oid);
            self.egraph.add(StriderLang::Opaque(payload))
        } else {
            // Internal e-node: build the matching StriderLang variant.
            // Every internal-op's inputs are value outputs, so we can
            // recurse on them.
            let ty = out_kind.as_value().expect(
                "internal-e-node output must carry a NodeOutputType — \
                 opaque classification protects against memory / control / \
                 PhiToken outputs reaching this branch",
            );
            let lang = self.build_internal_enode(g, node_id, &kind, ty, visit);
            self.egraph.add(lang)
        };

        self.output_to_eclass.insert(oid, id);
        // Per-add callback: lets callers patch the analysis data of an
        // opaque leaf (e.g. inject the strider-side output type) before
        // any parent e-node's `Analysis::make` reads the leaf's data.
        visit(&mut self.egraph, oid, &kind, id);
        id
    }

    /// Builds the `StriderLang` variant for an internal (non-opaque) node.
    /// Recurses on every value input via [`Self::add_value_output`].
    ///
    /// # Panics
    ///
    /// Panics if the node's input shape doesn't match the kind's expected
    /// signature (validator-enforced invariant; the panic is structural,
    /// not a runtime concern for well-formed strider graphs).
    #[allow(clippy::too_many_lines)]
    fn build_internal_enode<F>(
        &mut self,
        g: &Graph,
        node_id: NodeId,
        kind: &NodeKind,
        ty: crate::node::NodeOutputType,
        visit: &mut F,
    ) -> StriderLang
    where
        F: FnMut(&mut EGraph<StriderLang, A>, NodeOutputId, &NodeKind, Id),
    {
        use crate::node::NodeKind as K;
        let inputs: Vec<NodeOutputId> = g.node_inputs(node_id).into_iter().collect();
        let mut child_ids: Vec<Id> = inputs
            .iter()
            .map(|&inp| self.add_value_output(g, inp, visit))
            .collect();

        match kind {
            K::IntConst(v) => StriderLang::IntConst(*v, ty),
            K::BoolConst(b) => StriderLang::BoolConst(*b),
            K::FloatConst(bits) => StriderLang::FloatConst(*bits, ty),
            K::IntBinaryOp(op) => {
                let [a, b] = take_two(&mut child_ids, "IntBinaryOp");
                StriderLang::IntBin(*op, ty, [a, b])
            }
            K::IntUnaryOp(op) => {
                let [a] = take_one(&mut child_ids, "IntUnaryOp");
                StriderLang::IntUn(*op, ty, [a])
            }
            K::IntCmpOp(op) => {
                let [a, b] = take_two(&mut child_ids, "IntCmpOp");
                StriderLang::IntCmp(*op, [a, b])
            }
            K::CastToInt => {
                let [a] = take_one(&mut child_ids, "CastToInt");
                StriderLang::CastToInt(ty, [a])
            }
            K::Truncate => {
                let [a] = take_one(&mut child_ids, "Truncate");
                StriderLang::Truncate(ty, [a])
            }
            K::Popcount => {
                let [a] = take_one(&mut child_ids, "Popcount");
                StriderLang::Popcount(ty, [a])
            }
            K::Lzcount => {
                let [a] = take_one(&mut child_ids, "Lzcount");
                StriderLang::Lzcount(ty, [a])
            }
            K::Extend(op) => {
                let [a] = take_one(&mut child_ids, "Extend");
                StriderLang::Extend(*op, ty, [a])
            }
            K::BoolUnaryOp(op) => {
                let [a] = take_one(&mut child_ids, "BoolUnaryOp");
                StriderLang::BoolUn(*op, [a])
            }
            K::BoolBinaryOp(op) => {
                let [a, b] = take_two(&mut child_ids, "BoolBinaryOp");
                StriderLang::BoolBin(*op, [a, b])
            }
            K::CastToBool => {
                let [a] = take_one(&mut child_ids, "CastToBool");
                StriderLang::CastToBool([a])
            }
            K::FloatBinaryOp(op) => {
                let [a, b] = take_two(&mut child_ids, "FloatBinaryOp");
                StriderLang::FloatBin(*op, ty, [a, b])
            }
            K::FloatUnaryOp(op) => {
                let [a] = take_one(&mut child_ids, "FloatUnaryOp");
                StriderLang::FloatUn(*op, ty, [a])
            }
            K::FloatCmpOp(op) => {
                let [a, b] = take_two(&mut child_ids, "FloatCmpOp");
                StriderLang::FloatCmp(*op, [a, b])
            }
            K::IntToFloat => {
                let [a] = take_one(&mut child_ids, "IntToFloat");
                StriderLang::IntToFloat(ty, [a])
            }
            K::FloatToInt => {
                let [a] = take_one(&mut child_ids, "FloatToInt");
                StriderLang::FloatToInt(ty, [a])
            }
            K::FloatToFloat => {
                let [a] = take_one(&mut child_ids, "FloatToFloat");
                StriderLang::FloatToFloat(ty, [a])
            }
            K::IntBitsToFloat => {
                let [a] = take_one(&mut child_ids, "IntBitsToFloat");
                StriderLang::IntBitsToFloat(ty, [a])
            }
            K::FloatBitsToInt => {
                let [a] = take_one(&mut child_ids, "FloatBitsToInt");
                StriderLang::FloatBitsToInt(ty, [a])
            }
            K::CastToFloat => {
                let [a] = take_one(&mut child_ids, "CastToFloat");
                StriderLang::CastToFloat(ty, [a])
            }
            other => panic!(
                "build_internal_enode: kind {other:?} should have been classified as opaque \
                 by is_opaque_value_kind"
            ),
        }
    }
}

/// Classifies a [`NodeKind`] for the egraph adapter.
///
/// Returns `true` if a value output of this kind should be represented as
/// an [`StriderLang::Opaque`] leaf.  Returns `false` for internal e-nodes
/// (arithmetic / cmp / cast / boolean / float ops and constants).
///
/// The spike's classification rules:
/// - All structural phi / initial-state kinds → opaque (`VarPhi`, `MemPhi`,
///   `ValuePhi`, `InitialVar`, `InitialMemory`, `FunctionArg`).
/// - `Load` value outputs → opaque (the memory chain stays pinned outside
///   the egraph).
/// - `Call` / `CallOther` value outputs → opaque (multi-output nodes whose
///   value slots come from external state).
/// - `Store`, `StackStore`, `StackStorePhi` → never have value outputs; if
///   they reach here it's a bug.
/// - Control-flow nodes (`Entry`, `ControlState`, `If`, `Return`,
///   `IndirectBranch`) → never have value outputs; if they reach here it's
///   a bug.
/// - `SegmentOp`, `CPoolRef`, `New` → opaque (opaque/user-defined; the
///   spike doesn't model them).
/// Public alias for [`is_opaque_value_kind`] used by
/// [`super::extract::EGraphAdapter::extract_into_graph`].
///
/// Kept as a single-source-of-truth predicate so the from/to classification
/// agrees by construction (no chance of from_graph treating a node as
/// internal but extract treating it as opaque).
#[inline]
pub(crate) fn is_opaque_value_kind_for_extract(kind: &NodeKind) -> bool {
    is_opaque_value_kind(kind)
}

fn is_opaque_value_kind(kind: &NodeKind) -> bool {
    use NodeKind as K;
    matches!(
        kind,
        K::VarPhi(..)
            | K::MemPhi
            | K::ValuePhi
            | K::InitialVar(..)
            | K::InitialMemory
            | K::FunctionArg { .. }
            | K::Load(..)
            | K::Call
            | K::CallOther { .. }
            | K::SegmentOp { .. }
            | K::CPoolRef
            | K::New
            // The IndirectBranch / Return / If / Entry / ControlState /
            // Store / StackStore* paths can never produce value outputs
            // that reach add_value_output (filtered by is_value() in the
            // caller), but include them defensively so a future shape
            // change surfaces as "opaque leaf" rather than "panic in
            // build_internal_enode".
            | K::Store(..)
            | K::StackStore { .. }
            | K::StackStorePhi { .. }
            | K::Entry
            | K::ControlState
            | K::If
            | K::Return
            | K::IndirectBranch
            // U256/U512 wide constants — model as opaque for the spike
            // since StriderLang::IntConst payload is u128.  Phase 3 will
            // extend with an IntConstWide variant if needed.
            | K::IntConstWide(..)
    )
}

#[inline]
#[track_caller]
fn take_one(v: &mut Vec<Id>, ctx: &str) -> [Id; 1] {
    assert_eq!(
        v.len(),
        1,
        "{ctx}: expected exactly 1 value input, found {}",
        v.len()
    );
    [v[0]]
}

#[inline]
#[track_caller]
fn take_two(v: &mut Vec<Id>, ctx: &str) -> [Id; 2] {
    assert_eq!(
        v.len(),
        2,
        "{ctx}: expected exactly 2 value inputs, found {}",
        v.len()
    );
    [v[0], v[1]]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntBinaryOp;
    use crate::node::NodeOutputKind;

    /// Building from an empty graph (just Entry + InitialMemory + Return)
    /// produces an empty egraph: no value-producing nodes to add.
    #[test]
    fn empty_function_produces_empty_egraph() {
        let mut g = Graph::new();
        let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = g.node_outputs(mem).into_iter().next().unwrap();
        let cs = g.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
        // Return with no value — just consumes control + memory.
        let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out], []);
        let adapter = EGraphAdapter::from_graph(&g, entry);
        // No value outputs in the reachable graph → no e-classes in the egraph.
        assert_eq!(adapter.output_to_eclass.len(), 0);
    }

    /// A single IntConst returned by the function produces one e-class
    /// (the IntConst leaf — internal e-node, not opaque).
    #[test]
    fn single_int_const_produces_one_eclass() {
        let mut g = Graph::new();
        let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = g.node_outputs(mem).into_iter().next().unwrap();
        let cs = g.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
        let c = g.create_node(
            NodeKind::IntConst(42),
            [],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U64,
            )],
        );
        let c_out = g.node_outputs(c).into_iter().next().unwrap();
        let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out, c_out], []);

        let adapter = EGraphAdapter::from_graph(&g, entry);
        assert_eq!(adapter.output_to_eclass.len(), 1);
        // No opaque leaves (IntConst is an internal e-node).
        assert_eq!(adapter.leaf_to_output.len(), 0);
    }

    /// A pair of IntConsts feeding an Add produces three e-classes (the
    /// two constants + the Add) — verifies child resolution.
    #[test]
    fn int_const_add_produces_three_eclasses() {
        let mut g = Graph::new();
        let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = g.node_outputs(mem).into_iter().next().unwrap();
        let cs = g.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
        let c1 = g.create_node(
            NodeKind::IntConst(5),
            [],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U64,
            )],
        );
        let c2 = g.create_node(
            NodeKind::IntConst(7),
            [],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U64,
            )],
        );
        let c1_out = g.node_outputs(c1).into_iter().next().unwrap();
        let c2_out = g.node_outputs(c2).into_iter().next().unwrap();
        let add = g.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c1_out, c2_out],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U64,
            )],
        );
        let add_out = g.node_outputs(add).into_iter().next().unwrap();
        let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out, add_out], []);

        let adapter = EGraphAdapter::from_graph(&g, entry);
        assert_eq!(
            adapter.output_to_eclass.len(),
            3,
            "two IntConsts + one Add = 3 value outputs"
        );
        assert_eq!(adapter.leaf_to_output.len(), 0);
    }

    /// A VarPhi reaching the egraph becomes an opaque leaf — the plan's
    /// "phi nodes are leaves" invariant.
    #[test]
    fn var_phi_becomes_opaque_leaf() {
        use rsleigh::{Vn, VnSpace};
        let mut g = Graph::new();
        let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = g.node_outputs(mem).into_iter().next().unwrap();
        let cs = g.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
        let cs_phi_token = g.node_outputs(cs).into_iter().nth(1).unwrap();
        let vn = Vn {
            size: 8,
            addr_off: 0x100,
            addr_space: VnSpace::REGISTER,
        };
        let phi = g.create_node(
            NodeKind::VarPhi(vn),
            [cs_phi_token],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U64,
            )],
        );
        let phi_out = g.node_outputs(phi).into_iter().next().unwrap();
        let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out, phi_out], []);

        let adapter = EGraphAdapter::from_graph(&g, entry);
        // 1 value output: the VarPhi.  It becomes an opaque leaf.
        assert_eq!(adapter.output_to_eclass.len(), 1);
        assert_eq!(adapter.leaf_to_output.len(), 1);
    }

    /// Two structurally identical IntConst nodes (same value, same type)
    /// share one e-class — egg's internal dedup mirrors strider's.
    /// Note: strider's `create_node` already dedupes cacheable kinds, so
    /// we can't easily test this from a single call — instead we use the
    /// same constant in two places to ensure both consumers hit the same
    /// e-class.
    #[test]
    fn shared_int_const_shares_eclass() {
        let mut g = Graph::new();
        let entry = g.create_node(NodeKind::Entry, [], [NodeOutputKind::Control]);
        let mem = g.create_node(NodeKind::InitialMemory, [], [NodeOutputKind::Memory]);
        let entry_out = g.node_outputs(entry).into_iter().next().unwrap();
        let mem_out = g.node_outputs(mem).into_iter().next().unwrap();
        let cs = g.create_node(
            NodeKind::ControlState,
            [entry_out],
            [NodeOutputKind::Control, NodeOutputKind::PhiToken],
        );
        let cs_ctrl = g.node_outputs(cs).into_iter().next().unwrap();
        let c = g.create_node(
            NodeKind::IntConst(13),
            [],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U32,
            )],
        );
        let c_out = g.node_outputs(c).into_iter().next().unwrap();
        // Use the same constant for both operands of an Add.
        let add = g.create_node(
            NodeKind::IntBinaryOp(IntBinaryOp::Add),
            [c_out, c_out],
            [NodeOutputKind::OutputType(
                crate::node::NodeOutputType::U32,
            )],
        );
        let add_out = g.node_outputs(add).into_iter().next().unwrap();
        let _ret = g.create_node(NodeKind::Return, [cs_ctrl, mem_out, add_out], []);

        let adapter = EGraphAdapter::from_graph(&g, entry);
        // Two distinct value outputs (the IntConst and the Add), both
        // entered exactly once into the egraph.
        assert_eq!(adapter.output_to_eclass.len(), 2);
        // The IntConst is shared by both inputs of the Add, so it
        // dedupes inside the egraph too — verifiable by computing the
        // egraph's total e-class count, which should equal 2.
        assert_eq!(adapter.egraph.number_of_classes(), 2);
    }
}
