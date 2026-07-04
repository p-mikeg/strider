//! Capture-to-binding journal that backs every pattern match.
//!
//! [`Bindings`] is the per-match list of capture-to-binding entries.
//! Rebind-conflict detection is an O(1) hashed lookup against a capture
//! index kept in lockstep with the entry list, and rollback
//! (`mark` / `restore`) drains the tail of that list.
//! Typed extraction of constant values and op-variant discriminants
//! happens through `Match` / [`Bindings`] helpers (`get_uint`,
//! `get_int_binary_op`, …) which look up the bound `NodeId` and inspect
//! the underlying `NodeKind`.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{
    FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Graph, IRViewer, IntBinaryOp, IntCmpOp, IntUnaryOp,
};

use crate::capture::Capture;

/// One [`Capture`] binding: either a value-producing binding (a specific
/// `ValueId`) or a control-flow / node-only binding (a `NodeId`).
///
/// Value-producing patterns (`add`, `int_const`, the variant-agnostic
/// `*_any` constructors, …) bind [`Binding::Value`] — the bound
/// `ValueId` uniquely identifies the producing node via
/// [`strider_ir::Graph::producer`].  Control-flow patterns
/// (`Call`, `If`, `Return`, `CallOther`) and zero-output captures bind
/// [`Binding::Node`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Binding {
    /// Control-flow / zero-output capture: only the owning `NodeId` is
    /// meaningful.  Produced by `Call` / `If` / `Return` / `CallOther`
    /// captures.
    Node(NodeId),
    /// Value-producing capture: a specific `ValueId` whose owning
    /// `NodeId` is recoverable via [`strider_ir::Graph::producer`].
    Value(ValueId),
}

/// A set of capture-variable bindings accumulated during a single
/// match attempt.
///
/// Bindings are append-only: once a `Capture` is bound it cannot be
/// rebound to a different value.  A mismatch (trying to bind an
/// already-bound variable to a different value) makes the containing
/// match fail.
///
/// Backtracking uses a journal-based scheme: every match site that
/// wants to speculatively attempt sub-matches calls `Self::mark`
/// before the attempt and `Self::restore` on failure — the marker is
/// a `usize` cursor into the append-only entry `Vec`, and restoring is
/// an O(1) `Vec::truncate`.  No allocations, no deep copy of the full
/// state.
///
/// Storage shape: an append-only `Vec<(Capture, Binding)>` paired with an
/// `FxHashMap<Capture, usize>` index into it.  A capture binds at most once
/// (`bind_capture` refuses to append a second entry for an already-present
/// capture), so the index is a bijection — `bind_capture` / `get_binding` /
/// `is_bound` are O(1).  `restore` drains the tail entries appended after
/// the mark and removes their captures from the index; that work is bounded
/// by the number of binds being rolled back, so each bind stays amortized
/// O(1) including its eventual rollback.  The `Vec` remains the source of
/// truth for `iter()` ordering and `restore`.
///
/// External callers see `Bindings` as read-only: construction is via
/// `Default::default()`, and the production mutation path
/// (`Self::bind_capture`) is `pub(crate)`.  The `mark` / `restore`
/// journal API is `pub(crate)` because only the matcher's
/// commutative-retry / speculative-attempt paths legitimately need it.
#[derive(Clone, Default)]
pub struct Bindings {
    /// Append-only journal of `(Capture, Binding)` insertions in the
    /// order they were produced — preserves `iter()` ordering and is
    /// the source of truth for `restore`.
    entries: Vec<(Capture, Binding)>,
    /// `Capture` → index into `entries`, kept in lockstep with the journal
    /// so capture lookups avoid a linear scan.  A capture appears at most
    /// once in `entries`, so this maps each bound capture to its sole entry.
    index: FxHashMap<Capture, usize>,
    /// Append-only journal of every IR `NodeId` that fully matched a pat
    /// node during this attempt — the match's structural *footprint*
    /// (root + interior + captured leaves), recorded by the matcher at each
    /// successful [`crate::matcher`] node match.  Shares the `entries`
    /// journal's `mark` / `restore` lifecycle so a node recorded during a
    /// failed commutative ordering (or any speculative sub-attempt) is
    /// rolled back exactly like a capture — a separate, un-journaled
    /// accumulator would leak failed-ordering nodes into the footprint.
    /// May contain duplicates (a DAG sub-pattern matched twice); consumers
    /// that union fingerprints are duplicate-insensitive.
    matched: Vec<NodeId>,
}

/// Opaque marker returned by [`Bindings::mark`] and consumed by
/// [`Bindings::restore`].  Represents "the binding state at the moment of
/// marking"; rolling back discards both the capture entries and the matched
/// nodes appended after the mark.
#[derive(Clone, Copy)]
pub(crate) struct BindingsMark {
    entries: usize,
    matched: usize,
}

impl Bindings {
    /// Snapshot the current state in O(1) with no allocations.
    /// Use with [`Self::restore`] to roll back failed match attempts.
    pub(crate) fn mark(&self) -> BindingsMark {
        BindingsMark {
            entries: self.entries.len(),
            matched: self.matched.len(),
        }
    }

    /// Discard every capture entry AND matched node appended after `mark`
    /// was taken.  Idempotent: restoring to a mark that's already current is
    /// a no-op.
    ///
    /// Drains the entry tail (de-indexing each rolled-back capture) and
    /// truncates the matched journal — the two journals are the sole source
    /// of truth, so dropping their tails fully restores the pre-mark view.
    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        for (c, _) in self.entries.drain(mark.entries..) {
            self.index.remove(&c);
        }
        self.matched.truncate(mark.matched);
    }

    /// Record `node` into the match footprint.  Called by the matcher once a
    /// pat node has *fully* matched `node` (kind + outputs + predicates +
    /// inputs + capture), so the footprint reflects only committed matches.
    pub(crate) fn record_matched(&mut self, node: NodeId) {
        self.matched.push(node);
    }

    /// The IR nodes that fully matched during this attempt — the match's
    /// structural footprint (root + interior + captured leaves).  May
    /// contain duplicates for a DAG sub-pattern matched along two paths.
    pub(crate) fn matched_nodes(&self) -> &[NodeId] {
        &self.matched
    }

    /// Bind `c` to `binding`.  Returns `true` on new or idempotent
    /// (full-binding-equal) bind, `false` on conflict (no mutation).
    ///
    /// A hit returns the existing binding's equality; a miss appends to
    /// `entries` and records its index.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        if let Some(&i) = self.index.get(&c) {
            return self.entries[i].1 == binding;
        }
        self.index.insert(c, self.entries.len());
        self.entries.push((c, binding));
        true
    }

    /// Returns the [`Binding`] (a `Value` binding or a
    /// `Node`-only binding) bound to `c`, or `None` if `c` was not
    /// captured in this match.
    ///
    /// O(1) via the capture index; a capture binds at most once, so the
    /// indexed entry is the binding.
    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        self.index.get(&c).map(|&i| self.entries[i].1)
    }

    /// Convenience: returns the value `ValueId` bound to `c`, or
    /// `None` if `c` was not captured or the binding was control-flow
    /// (a `Binding::Node`).
    pub fn get_value(&self, c: Capture) -> Option<ValueId> {
        match self.get_binding(c)? {
            Binding::Value(out) => Some(out),
            Binding::Node(_) => None,
        }
    }

    /// Whether `c` was bound in this match (either variant of
    /// `Binding`).  Graph-free — useful when the only question is
    /// "did this capture fire?" and a `&Graph` isn't already in scope.
    pub fn is_bound(&self, c: Capture) -> bool {
        self.index.contains_key(&c)
    }

    /// Convenience: returns the `NodeId` bound to `c`, or `None` if `c`
    /// was not captured.
    ///
    /// For a `Binding::Value` the owning node is recovered via
    /// [`strider_ir::Graph::producer`]; for a `Binding::Node`
    /// the stored id is returned directly.
    pub fn get_node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        match self.get_binding(c)? {
            Binding::Node(node) => Some(node),
            Binding::Value(out) => Some(graph.producer(out)),
        }
    }

    /// Iterates over every `(Capture, Binding)` recorded by this match.
    /// Used by `Matcher::find_joined` to compute cross-pattern
    /// shared-capture agreement.  Order is the order bindings were
    /// appended during matching (preorder of the pattern tree).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (Capture, Binding)> + '_ {
        self.entries.iter().map(|(c, b)| (*c, *b))
    }

    // ── Typed extractors ──────────────────────────────────────────────
    //
    // These read the constant value or op variant that the bound node
    // carries.  All return `None` for unbound captures, control-flow
    // bindings, or producers whose `NodeKind` doesn't match the requested
    // shape — the same "wrong shape ⇒ None" contract the old typed-Var
    // getters had.

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant value masked to the output type's bit width.
    pub fn get_uint(&self, c: Capture, function: &strider_ir::Function) -> Option<u128> {
        function.int_const_u128(self.get_value(c)?)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant sign-extended from the output type's bit width to
    /// `i128`.
    pub fn get_int(&self, c: Capture, function: &strider_ir::Function) -> Option<i128> {
        function.int_const_i128(self.get_value(c)?)
    }

    /// If the node bound to `c` is a boolean constant (an `IntConst`
    /// typed `I1`), returns the stored boolean value (`!= 0`).
    pub fn get_bool(&self, c: Capture, function: &strider_ir::Function) -> Option<bool> {
        function.bool_const_val(self.get_value(c)?)
    }

    /// Returns the output `ValueType` of the value bound to `c`, or
    /// `None` for an unbound capture, a control-flow (`Binding::Node`)
    /// binding, or a non-value output kind (`Control` / `Memory` /
    /// `PhiToken`).
    pub fn get_type(
        &self,
        c: Capture,
        function: &strider_ir::Function,
    ) -> Option<strider_ir::node::ValueType> {
        function.value_kind(self.get_value(c)?).as_value()
    }

    /// If the node bound to `c` is a `FloatConst`, returns the raw
    /// IEEE 754 bit pattern as `u64`.
    pub fn get_float_bits(&self, c: Capture, graph: &Graph) -> Option<u64> {
        let value = self.get_value(c)?;
        match graph.kind_of_value(value) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntBinaryOp`, returns the op variant.
    pub fn get_int_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntUnaryOp`, returns the op variant.
    pub fn get_int_unary_op(&self, c: Capture, graph: &Graph) -> Option<IntUnaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntCmpOp`, returns the op variant.
    pub fn get_int_cmp_op(&self, c: Capture, graph: &Graph) -> Option<IntCmpOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntCmpOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a boolean binary op (an `IntBinaryOp`
    /// whose output is `I1`), returns the op variant.
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

    // Note: there is no `get_bool_unary_op` accessor.  A boolean
    // logical NOT is `Xor(x, IntConst(1)):I1` since the former BitNot
    // unary-op was removed in favour of `Xor(_, all_ones)`, so a
    // "bool unary" op is recovered via [`Self::get_bool_binary_op`]
    // (returning `IntBinaryOp::Xor`).

    /// If the node bound to `c` is a `FloatBinaryOp`, returns the op variant.
    pub fn get_float_binary_op(&self, c: Capture, graph: &Graph) -> Option<FloatBinaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::FloatBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatUnaryOp`, returns the op variant.
    pub fn get_float_unary_op(&self, c: Capture, graph: &Graph) -> Option<FloatUnaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::FloatUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatCmpOp`, returns the op variant.
    pub fn get_float_cmp_op(&self, c: Capture, graph: &Graph) -> Option<FloatCmpOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::FloatCmpOp(op) => Some(*op),
            _ => None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────
//
// These tests live inline (not under `crates/strider-pattern/tests/...`)
// because `Binding` and `Bindings::bind_capture` are `pub(crate)` —
// integration tests cannot reach them.  The unified-binding contract
// (idempotent rebind, conflict-preserves-original, typed extractors)
// is internal-API surface; we lock it down here.
#[cfg(test)]
mod tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer, IRWalker};
    use strider_ir_test_utils::make_empty_fn;

    // ── Capture (unified node + output) ──────────────────────────────────

    #[test]
    fn capture_bind_and_get_with_real_output_ids() {
        // Build `return(IntConst(1) + IntConst(2))` to harvest two
        // distinct `ValueId`s from the graph.
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

        // Idempotent with same output.
        assert!(bindings.bind_capture(v, ba));
        assert_eq!(bindings.get_value(v), Some(a));

        // Conflict preserves original.
        assert!(!bindings.bind_capture(v, bb));
        assert_eq!(bindings.get_value(v), Some(a));
    }

    #[test]
    fn capture_bind_and_get_with_real_node_ids() {
        // Thread distinct values through an Add so both constants
        // stay reachable.
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

    // ── Typed extractors read through the graph ──────────────────────────

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
        // No `get_bool_unary_op` accessor — bool NOT is `Xor(_, 1):I1`,
        // recovered via `get_bool_binary_op`.
        assert_eq!(bindings.get_float_binary_op(v, function.graph()), None);
        assert_eq!(bindings.get_float_unary_op(v, function.graph()), None);
        assert_eq!(bindings.get_float_cmp_op(v, function.graph()), None);
    }

    // ── mark / restore rollback ──────────────────────────────────────────

    /// `restore` after a speculative `bind_capture` must drop the tail
    /// entries so the post-rollback view is indistinguishable from the
    /// pre-mark view — and a subsequent `bind_capture(c, _)` for the
    /// rolled-back capture must succeed as brand-new (not bounce off a
    /// stale entry).
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

        // Pre-restore: all three visible in the entry list.
        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_some());
        assert!(bindings.get_binding(dropped_b).is_some());

        bindings.restore(mark);

        // Post-restore: only `kept` remains.
        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_none());
        assert!(bindings.get_binding(dropped_b).is_none());

        // Rebinding a rolled-back capture to a fresh binding must
        // succeed as brand-new — no stale entry may survive the
        // truncate.
        assert!(bindings.bind_capture(dropped_a, Binding::Node(n)));
        assert!(bindings.get_binding(dropped_a).is_some());
    }

    /// After a partial rollback the surviving captures must still resolve
    /// to their *original* bindings, and a fresh bind that reuses the freed
    /// entry slot must not collide with them.  This pins the capture index
    /// against the entry journal: a stale or mis-keyed index would either
    /// resurface a dropped binding or mis-resolve a kept one.
    #[test]
    fn partial_restore_keeps_survivors_and_reindexes_cleanly() {
        // Two distinct nodes so kept/dropped bindings are distinguishable.
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

        // Survivors keep their original, distinct bindings.
        assert_eq!(bindings.get_binding(a), Some(Binding::Node(n1)));
        assert_eq!(bindings.get_binding(b), Some(Binding::Node(n2)));
        assert!(bindings.get_binding(dropped).is_none());

        // A fresh capture reuses the slot freed by `dropped` without
        // disturbing the survivors.
        let fresh = Capture::new();
        assert!(bindings.bind_capture(fresh, Binding::Node(n2)));
        assert_eq!(bindings.get_binding(fresh), Some(Binding::Node(n2)));
        assert_eq!(bindings.get_binding(a), Some(Binding::Node(n1)));
        assert_eq!(bindings.get_binding(b), Some(Binding::Node(n2)));
    }

    /// Restoring to a mark that's already the current cursor must be a
    /// no-op — truncating to the current length leaves the list intact.
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

    /// A `Binding::Node` and a `Binding::Value` NEVER compare equal —
    /// even when the value's producer IS that node.  Binding the same
    /// capture as one kind and then the other is a conflict in both
    /// directions: the rebind is rejected and the original binding (and
    /// its kind-specific accessor view) is preserved.
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

        // Node first, then the node's own output value: conflict.
        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Node(node)));
        assert!(
            !bindings.bind_capture(v, Binding::Value(value)),
            "Value rebind must conflict with an existing Node binding",
        );
        // Original Node binding preserved; the value view stays empty.
        assert_eq!(bindings.get_node(v, function.graph()), Some(node));
        assert_eq!(bindings.get_value(v), None);

        // Value first, then the producing node: conflict the other way.
        let mut bindings = Bindings::default();
        let w = Capture::new();
        assert!(bindings.bind_capture(w, Binding::Value(value)));
        assert!(
            !bindings.bind_capture(w, Binding::Node(node)),
            "Node rebind must conflict with an existing Value binding",
        );
        // Original Value binding preserved — and `get_node` still
        // resolves the producer THROUGH the value binding.
        assert_eq!(bindings.get_value(w), Some(value));
        assert_eq!(bindings.get_node(w, function.graph()), Some(node));
    }

    /// `restore` is a pure truncate: entries appended after the mark
    /// vanish, the kept entry survives, and a rolled-back capture
    /// rebinds cleanly afterwards.
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
        assert!(b.bind_capture(dropped, Binding::Node(n))); // rebinds clean
    }
}
