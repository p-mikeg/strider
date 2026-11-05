use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Graph, IRViewer};

use crate::capture::Capture;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Binding {
    /// A capture on a node with no anchor output (`If`, `Return`, `Switch`, ...).
    /// `Call` / `CallOther` anchor on their memory token, so bind a `Value`.
    Node(NodeId),
    /// A capture on an output value.
    Value(ValueId),
}

/// The captures bound by one match attempt. Rebinding a `Capture` to something
/// else fails the match; backtracking truncates back to a `mark`.
#[derive(Clone, Default)]
pub struct Bindings {
    /// In bind order, one entry per capture.
    entries: Vec<(Capture, Binding)>,
    /// Every node the match covered; may hold duplicates.
    matched: Vec<NodeId>,
}

/// A [`Bindings`] length to roll back to.
#[derive(Clone, Copy)]
pub(crate) struct BindingsMark {
    entries: usize,
    matched: usize,
}

// `$variant` names both the `NodeKind` variant and the returned op type.
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

    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        self.entries.truncate(mark.entries);
        self.matched.truncate(mark.matched);
    }

    /// Call only once a pattern node has fully matched `node`.
    pub(crate) fn record_matched(&mut self, node: NodeId) {
        self.matched.push(node);
    }

    pub(crate) fn matched_nodes(&self) -> &[NodeId] {
        &self.matched
    }

    /// `false` on a conflicting rebind, which keeps the existing binding.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        if let Some(bound) = self.get_binding(c) {
            return bound == binding;
        }
        self.entries.push((c, binding));
        true
    }

    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        self.entries.iter().find(|(e, _)| *e == c).map(|&(_, b)| b)
    }

    /// The bound value; `None` if unbound or bound to a node.
    pub fn get_value(&self, c: Capture) -> Option<ValueId> {
        match self.get_binding(c)? {
            Binding::Value(out) => Some(out),
            Binding::Node(_) => None,
        }
    }

    /// The visible bindings sorted by capture id: the per-root dedup key of
    /// [`crate::Matcher::find_all`].
    pub(crate) fn binding_signature(&self) -> Vec<(u32, Binding)> {
        let mut sig: Vec<(u32, Binding)> = self.iter().map(|(c, b)| (c.id(), b)).collect();
        sig.sort_unstable_by_key(|&(id, _)| id);
        sig
    }

    pub fn is_bound(&self, c: Capture) -> bool {
        self.get_binding(c).is_some()
    }

    /// The bound node, or the bound value's producer.
    pub fn get_node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        match self.get_binding(c)? {
            Binding::Node(node) => Some(node),
            Binding::Value(out) => Some(graph.producer(out)),
        }
    }

    /// In bind order, skipping internal captures.
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Capture, Binding)> + '_ {
        self.entries
            .iter()
            .filter(|(c, _)| !c.is_internal())
            .map(|(c, b)| (*c, *b))
    }

    // The extractors below return `None` when unbound or the wrong kind; the value ones also
    // when node-bound.

    /// The bound `IntConst`'s value, at any width.
    pub fn get_uint(
        &self,
        c: Capture,
        function: &strider_ir::Function,
    ) -> Option<strider_ir::UBig> {
        function.int_const(self.get_value(c)?).cloned()
    }

    /// The bound value's type; `None` for control, memory and phi-token outputs.
    pub fn get_type(
        &self,
        c: Capture,
        function: &strider_ir::Function,
    ) -> Option<strider_ir::node::ValueType> {
        function.value_kind(self.get_value(c)?).as_value()
    }

    /// The bound `FloatConst`'s raw IEEE 754 bits.
    pub fn get_float_bits(&self, c: Capture, graph: &Graph) -> Option<u64> {
        let value = self.get_value(c)?;
        match graph.kind_of_value(value) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
    use strider_ir_test_utils::make_empty_fn;

    #[test]
    fn capture_bind_and_get_with_real_output_ids() {
        // `return(IntConst(1) + IntConst(2))`, for two distinct `ValueId`s.
        let mut a_value = None;
        let mut b_value = None;
        let _function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            let bv = b.build_int_const(2u64, ValueType::int(64)).unwrap();
            a_value = Some(av);
            b_value = Some(bv);
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::int(64))
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
            let av = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            let bv = b.build_int_const(2u64, ValueType::int(64)).unwrap();
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::int(64))
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
            let c = b.build_int_const(7u64, ValueType::int(64)).unwrap();
            c_value = Some(c);
            Ok(c)
        })
        .expect("build graph");
        let c = c_value.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Value(c)));
        assert_eq!(
            bindings.get_uint(v, &function),
            Some(strider_ir::UBig::from(7u8))
        );
    }

    /// A 4-limb `I256` constant comes back whole.
    #[test]
    fn get_uint_reads_a_constant_past_the_carrier() {
        use strider_ir::UBig;
        let wide = UBig::from_words(&[1, 2, 3, 4]);
        let mut c_value = None;
        let function = make_empty_fn(|b| {
            let c = b
                .build_int_const(wide.clone(), ValueType::int(256))
                .unwrap();
            c_value = Some(c);
            Ok(c)
        })
        .expect("build graph");

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Value(c_value.unwrap())));
        assert_eq!(bindings.get_uint(v, &function), Some(wide));
    }

    #[test]
    fn get_uint_returns_none_when_not_an_int_const() {
        let mut s_value = None;
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            let bv = b.build_int_const(2u64, ValueType::int(64)).unwrap();
            let s = b
                .build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::int(64))
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
    fn get_node_reads_a_node_binding() {
        let mut s_value = None;
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            let bv = b.build_int_const(2u64, ValueType::int(64)).unwrap();
            let s = b
                .build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::int(64))
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
        assert_eq!(bindings.get_node(v, function.graph()), Some(add_node));
        assert_eq!(bindings.get_value(v), None);
    }

    #[test]
    fn unbound_capture_yields_none_for_every_typed_extractor() {
        let function =
            make_empty_fn(|b| b.build_int_const(0u64, ValueType::int(64))).expect("build graph");
        let bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get_value(v), None);
        assert_eq!(bindings.get_node(v, function.graph()), None);
        assert_eq!(bindings.get_uint(v, &function), None);
        assert_eq!(bindings.get_float_bits(v, function.graph()), None);
        assert_eq!(bindings.get_float_binary_op(v, function.graph()), None);
        assert_eq!(bindings.get_float_unary_op(v, function.graph()), None);
        assert_eq!(bindings.get_float_cmp_op(v, function.graph()), None);
    }

    /// A rolled-back capture must rebind as brand-new rather than bouncing
    /// off a stale entry.
    #[test]
    fn restore_drops_tail_and_allows_rebind() {
        let function =
            make_empty_fn(|b| b.build_int_const(1u64, ValueType::int(64))).expect("build graph");
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

        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_some());
        assert!(bindings.get_binding(dropped_b).is_some());

        bindings.restore(mark);

        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_none());
        assert!(bindings.get_binding(dropped_b).is_none());

        // No stale entry may survive the truncate.
        assert!(bindings.bind_capture(dropped_a, Binding::Node(n)));
        assert!(bindings.get_binding(dropped_a).is_some());
    }

    /// A partial restore keeps earlier bindings and drops later ones.
    #[test]
    fn partial_restore_keeps_survivors_and_reindexes_cleanly() {
        // Two distinct nodes so kept and dropped bindings differ.
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, ValueType::int(64)).unwrap();
            let bv = b.build_int_const(2u64, ValueType::int(64)).unwrap();
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, ValueType::int(64))
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
            make_empty_fn(|b| b.build_int_const(1u64, ValueType::int(64))).expect("build graph");
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
            let c = b.build_int_const(7u64, ValueType::int(64)).unwrap();
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
}
