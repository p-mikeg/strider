//! Capture-to-binding journal backing every pattern match.
//!
//! Rebind-conflict detection is an O(1) hashed lookup against a capture index
//! kept in lockstep with the entry list; rollback (`mark` / `restore`) drains
//! that list's tail. The typed extractors (`get_uint`, `get_int_binary_op`,
//! ...) look up the bound `NodeId` and read its `NodeKind`.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Graph, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::capture::Capture;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Binding {
    /// Control-flow or zero-output capture (`Call`, `If`, `Return`,
    /// `CallOther`): only the owning node is meaningful.
    Node(NodeId),
    /// Value-producing capture (`add`, `int_const`, the `*_any`
    /// constructors). The owning node comes back via
    /// [`strider_ir::Graph::producer`].
    Value(ValueId),
}

/// Bindings accumulated during one match attempt. Append-only: rebinding a
/// `Capture` to a different value fails the containing match.
///
/// Backtracking is journal-based rather than a state copy. A site attempting
/// a speculative sub-match calls `mark` first and `restore` on failure; the
/// marker is a cursor into the entry `Vec`, so rollback allocates nothing.
/// `restore` also de-indexes the rolled-back captures, work bounded by the
/// number of binds being undone, keeping each bind amortized O(1) including
/// its eventual rollback.
///
/// A capture binds at most once, so the index is a bijection and
/// `bind_capture` / `get_binding` / `is_bound` are O(1). The `Vec` stays the
/// source of truth for `iter()` order and `restore`.
///
/// Read-only to external callers: construction is `Default::default()` and
/// both the mutation path and the `mark` / `restore` journal are
/// `pub(crate)`, since only the matcher's speculative paths need them.
#[derive(Clone, Default)]
pub struct Bindings {
    /// In production order, which is what `iter()` reports.
    entries: Vec<(Capture, Binding)>,
    /// Each bound capture to its sole `entries` index, avoiding a linear
    /// scan.
    index: FxHashMap<Capture, usize>,
    /// The match's structural footprint: root, interior and captured leaves.
    /// Shares the `entries` journal's mark / restore lifecycle, so a node
    /// recorded during a failed commutative ordering rolls back like a
    /// capture. A separate un-journaled accumulator would leak
    /// failed-ordering nodes into the footprint.
    ///
    /// May hold duplicates when a DAG sub-pattern matched twice; consumers
    /// that union fingerprints do not care.
    matched: Vec<NodeId>,
}

/// The binding state at the moment of marking. Rolling back to it discards
/// both the capture entries and the matched nodes appended since.
#[derive(Clone, Copy)]
pub(crate) struct BindingsMark {
    entries: usize,
    matched: usize,
}

// The `NodeKind` variant name doubles as the returned op type, so one
// `$variant` token drives both. `get_bool_binary_op` needs an extra I1 check
// and stays hand-written below.
macro_rules! op_extractor {
    ($(#[$attr:meta])* $fn:ident => $variant:ident) => {
        $(#[$attr])*
        pub fn $fn(&self, c: Capture, graph: &Graph) -> Option<$variant> {
            let node = self.get_node(c, graph)?;
            match graph.node_kind(node) {
                NodeKind::$variant(op) => Some(*op),
                _ => None,
            }
        }
    };
}

impl Bindings {
    pub(crate) fn mark(&self) -> BindingsMark {
        BindingsMark {
            entries: self.entries.len(),
            matched: self.matched.len(),
        }
    }

    /// Idempotent: restoring to an already-current mark is a no-op. The two
    /// journals are the sole source of truth, so dropping their tails fully
    /// restores the pre-mark view.
    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        for (c, _) in self.entries.drain(mark.entries..) {
            self.index.remove(&c);
        }
        self.matched.truncate(mark.matched);
    }

    /// Called only once a pat node has *fully* matched `node`, meaning kind,
    /// outputs, predicates, inputs and capture, so the footprint holds only
    /// committed matches.
    pub(crate) fn record_matched(&mut self, node: NodeId) {
        self.matched.push(node);
    }

    pub(crate) fn matched_nodes(&self) -> &[NodeId] {
        &self.matched
    }

    /// `true` on a new or idempotent bind, `false` on conflict, which leaves
    /// the existing binding untouched.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        if let Some(&i) = self.index.get(&c) {
            return self.entries[i].1 == binding;
        }
        self.index.insert(c, self.entries.len());
        self.entries.push((c, binding));
        true
    }

    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        self.index.get(&c).map(|&i| self.entries[i].1)
    }

    /// `None` for an unbound capture or a control-flow binding.
    pub fn get_value(&self, c: Capture) -> Option<ValueId> {
        match self.get_binding(c)? {
            Binding::Value(out) => Some(out),
            Binding::Node(_) => None,
        }
    }

    /// A match's identity for [`crate::Matcher::find_all`]'s dedup.
    ///
    /// The matcher enumerates every operand ordering of every commutative
    /// node, so one root is reachable through several configurations. Two
    /// that bind the same captures to the same things are the SAME match:
    /// `add(x, x)` swapped is one match, not two. Two that bind a capture to
    /// different operands are genuinely distinct and both get reported.
    /// Keying on the bindings alone, not the root, the `matched` footprint or
    /// `entries` order, is what draws that line; a capture-free pattern
    /// collapses to the empty key and so can never duplicate.
    ///
    /// Sorted by capture id, making the key independent of bind order.
    pub(crate) fn binding_signature(&self) -> Vec<(u32, Binding)> {
        let mut sig: Vec<(u32, Binding)> = self.entries.iter().map(|&(c, b)| (c.id(), b)).collect();
        sig.sort_unstable_by_key(|&(id, _)| id);
        sig
    }

    /// Graph-free, for when no `&Graph` is in scope.
    pub fn is_bound(&self, c: Capture) -> bool {
        self.index.contains_key(&c)
    }

    /// A `Binding::Value` resolves its owning node via
    /// [`strider_ir::Graph::producer`].
    pub fn get_node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        match self.get_binding(c)? {
            Binding::Node(node) => Some(node),
            Binding::Value(out) => Some(graph.producer(out)),
        }
    }

    /// In bind order, which is preorder of the pattern tree. Drives
    /// `Matcher::find_joined`'s cross-pattern shared-capture agreement.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Capture, Binding)> + '_ {
        self.entries.iter().map(|(c, b)| (*c, *b))
    }

    // Every typed extractor below returns `None` for an unbound capture, a
    // control-flow binding, or a producer whose `NodeKind` is the wrong
    // shape.

    /// Masked to the output type's bit width.
    pub fn get_uint(&self, c: Capture, function: &strider_ir::Function) -> Option<u128> {
        function.int_const_u128(self.get_value(c)?)
    }

    /// Sign-extended from the output type's bit width.
    pub fn get_int(&self, c: Capture, function: &strider_ir::Function) -> Option<i128> {
        function.int_const_i128(self.get_value(c)?)
    }

    /// A boolean constant is an `IntConst` typed `I1`.
    pub fn get_bool(&self, c: Capture, function: &strider_ir::Function) -> Option<bool> {
        function.bool_const_val(self.get_value(c)?)
    }

    /// `None` for a non-value output kind (`Control` / `Memory` /
    /// `PhiToken`) as well as for the usual unbound / control-flow cases.
    pub fn get_type(
        &self,
        c: Capture,
        function: &strider_ir::Function,
    ) -> Option<strider_ir::node::ValueType> {
        function.value_kind(self.get_value(c)?).as_value()
    }

    /// The raw IEEE 754 bit pattern.
    pub fn get_float_bits(&self, c: Capture, graph: &Graph) -> Option<u64> {
        let value = self.get_value(c)?;
        match graph.kind_of_value(value) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }

    op_extractor! {
        get_int_binary_op => IntBinaryOp
    }

    op_extractor! {
        get_int_unary_op => IntUnaryOp
    }

    op_extractor! {
        get_int_cmp_op => IntCmpOp
    }

    /// A boolean binary op is an `IntBinaryOp` whose output is `I1`.
    pub fn get_bool_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
        let node = self.get_node(c, graph)?;
        let NodeKind::IntBinaryOp(op) = graph.node_kind(node) else {
            return None;
        };
        let value = self.get_value(c)?;
        if !graph.value_kind(value).as_value()?.is_bool() {
            return None;
        }
        Some(*op)
    }

    // No `get_bool_unary_op`: boolean NOT is `Xor(x, IntConst(1)):I1`, so a
    // bool unary op comes back from `get_bool_binary_op` as
    // `IntBinaryOp::Xor`.

    op_extractor! {
        get_float_binary_op => FloatBinaryOp
    }

    op_extractor! {
        get_float_unary_op => FloatUnaryOp
    }

    op_extractor! {
        get_float_cmp_op => FloatCmpOp
    }
}

// Inline rather than under `tests/` because `Binding` and `bind_capture` are
// `pub(crate)`, out of reach of an integration test.
#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer, IRWalker};
    use strider_ir_test_utils::make_empty_fn;

    #[test]
    fn capture_bind_and_get_with_real_output_ids() {
        // `return(IntConst(1) + IntConst(2))`, for two distinct `ValueId`s.
        let mut a_value = None;
        let mut b_value = None;
        let _function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::I64).unwrap();
            let bv = b.build_int_const(2u64, ValueType::I64).unwrap();
            a_value = Some(av);
            b_value = Some(bv);
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::I64)
        })
        .expect("build graph");
        let a = a_value.unwrap();
        let b = b_value.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get_value(v), None);
        let ba = Binding::Value(a);
        let bb = Binding::Value(b);
        assert!(bindings.bind_capture(v, ba));
        assert_eq!(bindings.get_value(v), Some(a));

        // Idempotent with the same output.
        assert!(bindings.bind_capture(v, ba));
        assert_eq!(bindings.get_value(v), Some(a));

        // Conflict preserves original.
        assert!(!bindings.bind_capture(v, bb));
        assert_eq!(bindings.get_value(v), Some(a));
    }

    #[test]
    fn capture_bind_and_get_with_real_node_ids() {
        // Thread through an Add so both constants stay reachable.
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::I64).unwrap();
            let bv = b.build_int_const(2u64, ValueType::I64).unwrap();
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::I64)
        })
        .expect("build graph");

        let mut ids = function
            .walk()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)));
        let n1 = ids.next().expect("first const node");
        let n2 = ids.next().expect("second const node");
        assert_ne!(n1, n2);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get_node(v, function.graph()), None);
        let b1 = Binding::Node(n1);
        let b2 = Binding::Node(n2);
        assert!(bindings.bind_capture(v, b1));
        assert_eq!(bindings.get_node(v, function.graph()), Some(n1));
        assert!(bindings.bind_capture(v, b1));
        assert!(!bindings.bind_capture(v, b2));
        assert_eq!(bindings.get_node(v, function.graph()), Some(n1));
    }

    #[test]
    fn get_uint_reads_int_const_through_bound_capture() {
        let mut c_value = None;
        let function = make_empty_fn(|b| {
            let c = b.build_int_const(7u64, ValueType::I64).unwrap();
            c_value = Some(c);
            Ok(c)
        })
        .expect("build graph");
        let c = c_value.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Value(c)));
        assert_eq!(bindings.get_uint(v, &function), Some(7));
    }

    #[test]
    fn get_uint_returns_none_when_not_an_int_const() {
        let mut s_value = None;
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::I64).unwrap();
            let bv = b.build_int_const(2u64, ValueType::I64).unwrap();
            let s = b
                .build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            s_value = Some(s);
            Ok(s)
        })
        .expect("build graph");
        let s = s_value.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Value(s)));
        assert_eq!(bindings.get_uint(v, &function), None);
    }

    #[test]
    fn get_int_binary_op_reads_op_variant_through_bound_capture() {
        let mut s_value = None;
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::I64).unwrap();
            let bv = b.build_int_const(2u64, ValueType::I64).unwrap();
            let s = b
                .build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::I64)
                .unwrap();
            s_value = Some(s);
            Ok(s)
        })
        .expect("build graph");
        let s = s_value.unwrap();
        let add_node = function.producer(s);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Node(add_node)));
        assert_eq!(
            bindings.get_int_binary_op(v, function.graph()),
            Some(IntBinaryOp::Add)
        );
    }

    #[test]
    fn unbound_capture_yields_none_for_every_typed_extractor() {
        let function =
            make_empty_fn(|b| b.build_int_const(0u64, ValueType::I64)).expect("build graph");
        let bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get_value(v), None);
        assert_eq!(bindings.get_node(v, function.graph()), None);
        assert_eq!(bindings.get_uint(v, &function), None);
        assert_eq!(bindings.get_int(v, &function), None);
        assert_eq!(bindings.get_bool(v, &function), None);
        assert_eq!(bindings.get_float_bits(v, function.graph()), None);
        assert_eq!(bindings.get_int_binary_op(v, function.graph()), None);
        assert_eq!(bindings.get_int_unary_op(v, function.graph()), None);
        assert_eq!(bindings.get_int_cmp_op(v, function.graph()), None);
        assert_eq!(bindings.get_bool_binary_op(v, function.graph()), None);
        // No `get_bool_unary_op`: bool NOT is `Xor(_, 1):I1`.
        assert_eq!(bindings.get_float_binary_op(v, function.graph()), None);
        assert_eq!(bindings.get_float_unary_op(v, function.graph()), None);
        assert_eq!(bindings.get_float_cmp_op(v, function.graph()), None);
    }

    /// A rolled-back capture must rebind as brand-new rather than bouncing
    /// off a stale entry.
    #[test]
    fn restore_drops_tail_and_allows_rebind() {
        let function =
            make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).expect("build graph");
        let n = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .expect("int const node");

        let mut bindings = Bindings::default();
        let kept = Capture::new();
        let dropped_a = Capture::new();
        let dropped_b = Capture::new();

        assert!(bindings.bind_capture(kept, Binding::Node(n)));
        let mark = bindings.mark();
        assert!(bindings.bind_capture(dropped_a, Binding::Node(n)));
        assert!(bindings.bind_capture(dropped_b, Binding::Node(n)));

        // Pre-restore: all three visible.
        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_some());
        assert!(bindings.get_binding(dropped_b).is_some());

        bindings.restore(mark);

        // Post-restore: only `kept` remains.
        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_none());
        assert!(bindings.get_binding(dropped_b).is_none());

        // No stale entry may survive the truncate.
        assert!(bindings.bind_capture(dropped_a, Binding::Node(n)));
        assert!(bindings.get_binding(dropped_a).is_some());
    }

    /// Pins the capture index against the entry journal: a stale or
    /// mis-keyed index would resurface a dropped binding or mis-resolve a
    /// kept one.
    #[test]
    fn partial_restore_keeps_survivors_and_reindexes_cleanly() {
        // Two distinct nodes so kept and dropped bindings differ.
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::I64).unwrap();
            let bv = b.build_int_const(2u64, ValueType::I64).unwrap();
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::I64)
        })
        .expect("build graph");
        let mut consts = function
            .walk()
            .filter(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)));
        let n1 = consts.next().expect("first const node");
        let n2 = consts.next().expect("second const node");

        let mut bindings = Bindings::default();
        let (a, b, dropped) = (Capture::new(), Capture::new(), Capture::new());
        assert!(bindings.bind_capture(a, Binding::Node(n1)));
        assert!(bindings.bind_capture(b, Binding::Node(n2)));
        let mark = bindings.mark();
        assert!(bindings.bind_capture(dropped, Binding::Node(n1)));

        bindings.restore(mark);

        // Survivors keep their original bindings.
        assert_eq!(bindings.get_binding(a), Some(Binding::Node(n1)));
        assert_eq!(bindings.get_binding(b), Some(Binding::Node(n2)));
        assert!(bindings.get_binding(dropped).is_none());

        // A fresh capture reuses `dropped`'s slot.
        let fresh = Capture::new();
        assert!(bindings.bind_capture(fresh, Binding::Node(n2)));
        assert_eq!(bindings.get_binding(fresh), Some(Binding::Node(n2)));
        assert_eq!(bindings.get_binding(a), Some(Binding::Node(n1)));
        assert_eq!(bindings.get_binding(b), Some(Binding::Node(n2)));
    }

    #[test]
    fn restore_to_current_mark_is_noop() {
        let function =
            make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).expect("build graph");
        let n = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .expect("int const node");

        let mut bindings = Bindings::default();
        let c = Capture::new();
        assert!(bindings.bind_capture(c, Binding::Node(n)));
        let mark = bindings.mark();
        bindings.restore(mark);
        assert!(bindings.get_binding(c).is_some());
    }

    /// A `Binding::Node` and a `Binding::Value` never compare equal, even
    /// when the value's producer IS that node. Binding one kind then the
    /// other conflicts in both directions.
    #[test]
    fn node_then_value_binding_for_same_capture_conflicts() {
        let mut c_value = None;
        let function = make_empty_fn(|b| {
            let c = b.build_int_const(7u64, ValueType::I64).unwrap();
            c_value = Some(c);
            Ok(c)
        })
        .expect("build graph");
        let value = c_value.unwrap();
        let node = function.producer(value);

        // Node first, then its own output value.
        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Node(node)));
        assert!(
            !bindings.bind_capture(v, Binding::Value(value)),
            "Value rebind must conflict with an existing Node binding",
        );
        // The Node binding survives; the value view stays empty.
        assert_eq!(bindings.get_node(v, function.graph()), Some(node));
        assert_eq!(bindings.get_value(v), None);

        // Value first, then the producing node.
        let mut bindings = Bindings::default();
        let w = Capture::new();
        assert!(bindings.bind_capture(w, Binding::Value(value)));
        assert!(
            !bindings.bind_capture(w, Binding::Node(node)),
            "Node rebind must conflict with an existing Value binding",
        );
        // `get_node` still resolves the producer THROUGH the value binding.
        assert_eq!(bindings.get_value(w), Some(value));
        assert_eq!(bindings.get_node(w, function.graph()), Some(node));
    }

    #[test]
    fn restore_is_pure_truncate_and_rebind_succeeds() {
        let function = make_empty_fn(|b| b.build_int_const(1u64, ValueType::I64)).unwrap();
        let n = function
            .walk()
            .find(|&n| matches!(function.node_kind(n), NodeKind::IntConst(_)))
            .unwrap();
        let mut b = Bindings::default();
        let (kept, dropped) = (Capture::new(), Capture::new());
        assert!(b.bind_capture(kept, Binding::Node(n)));
        let mark = b.mark();
        assert!(b.bind_capture(dropped, Binding::Node(n)));
        b.restore(mark);
        assert!(b.get_binding(kept).is_some());
        assert!(b.get_binding(dropped).is_none());
        assert!(b.bind_capture(dropped, Binding::Node(n)));
    }
}
