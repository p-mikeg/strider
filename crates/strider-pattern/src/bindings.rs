// `dead_code` allow: the matcher + rewrite modules consume
// `Binding`, `Bindings::{mark,restore,bind_capture,get_binding,iter}`,
// and `BindingsMark`.  These items are pub(crate) — there are no
// external callers, and clippy --release runs `-D warnings` so we
// silence the dead-code complaints at the module level.
#![allow(dead_code)]

//! Capture-to-binding journal that backs every pattern match.
//!
//! [`Bindings`] is the per-match list of capture-to-binding entries.
//! Rebind-conflict detection is a linear scan over the (typically tiny)
//! entry list, and rollback (`mark` / `restore`) truncates that list.
//! Typed extraction of constant values and op-variant discriminants
//! happens through `Match` / [`Bindings`] helpers (`get_uint`,
//! `get_int_binary_op`, …) which look up the bound `NodeId` and inspect
//! the underlying `NodeKind`.

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Graph, IntBinaryOp, IntCmpOp, IntUnaryOp};

use crate::capture::Capture;

/// One [`Capture`] binding: either a value-producing binding (a specific
/// `NodeOutputId`) or a control-flow / node-only binding (a `NodeId`).
///
/// Value-producing patterns (`add`, `int_const`, the variant-agnostic
/// `*_any` constructors, …) bind [`Binding::Output`] — the bound
/// `NodeOutputId` uniquely identifies the producing node via
/// [`strider_ir::Graph::node_for_output`].  Control-flow patterns
/// (`Call`, `If`, `Return`, `CallOther`) and zero-output captures bind
/// [`Binding::Node`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Binding {
    /// Control-flow / zero-output capture: only the owning `NodeId` is
    /// meaningful.  Produced by `Call` / `If` / `Return` / `CallOther`
    /// captures.
    Node(NodeId),
    /// Value-producing capture: a specific `NodeOutputId` whose owning
    /// `NodeId` is recoverable via [`strider_ir::Graph::node_for_output`].
    Output(NodeOutputId),
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
/// Storage shape: a single append-only `Vec<(Capture, Binding)>`.
/// `restore` is a pure `Vec::truncate`; `bind_capture` / `get_binding`
/// linearly scan the (typically tiny) entry list.
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
}

/// Opaque marker returned by [`Bindings::mark`] and consumed by
/// [`Bindings::restore`].  Represents "the binding state at the moment of
/// marking"; rolling back discards entries appended after the mark.
#[derive(Clone, Copy)]
pub(crate) struct BindingsMark(usize);

impl Bindings {
    /// Snapshot the current state in O(1) with no allocations.
    /// Use with [`Self::restore`] to roll back failed match attempts.
    pub(crate) fn mark(&self) -> BindingsMark {
        BindingsMark(self.entries.len())
    }

    /// Discard every entry appended after `mark` was taken.  Idempotent:
    /// restoring to a mark that's already current is a no-op.
    ///
    /// A pure `Vec::truncate` — the entry list is the sole source of
    /// truth, so dropping the tail fully restores the pre-mark view.
    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        self.entries.truncate(mark.0);
    }

    /// Bind `c` to `binding`.  Returns `true` on new or idempotent
    /// (full-binding-equal) bind, `false` on conflict (no mutation).
    ///
    /// A hit returns the existing binding's equality; a miss appends to
    /// `entries`.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        if let Some((_, existing)) = self.entries.iter().find(|(k, _)| *k == c) {
            return *existing == binding;
        }
        self.entries.push((c, binding));
        true
    }

    /// Returns the [`Binding`] (an `Output` value binding or a
    /// `Node`-only binding) bound to `c`, or `None` if `c` was not
    /// captured in this match.
    ///
    /// Scans newest-first so a re-bound capture resolves to its most
    /// recent value.
    #[must_use]
    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        self.entries.iter().rev().find(|(k, _)| *k == c).map(|(_, b)| *b)
    }

    /// Convenience: returns the value `NodeOutputId` bound to `c`, or
    /// `None` if `c` was not captured or the binding was control-flow
    /// (a `Binding::Node`).
    #[must_use]
    pub fn get_output(&self, c: Capture) -> Option<NodeOutputId> {
        match self.get_binding(c)? {
            Binding::Output(out) => Some(out),
            Binding::Node(_) => None,
        }
    }

    /// Alias for [`Self::get_output`] — kept short because it is the
    /// most-used accessor inside `*_const_with!` macro bodies and
    /// post-match `when_match` closures.
    #[must_use]
    pub fn get(&self, c: Capture) -> Option<NodeOutputId> {
        self.get_output(c)
    }

    /// Whether `c` was bound in this match (either variant of
    /// `Binding`).  Graph-free — useful when the only question is
    /// "did this capture fire?" and a `&Graph` isn't already in scope.
    #[must_use]
    pub fn is_bound(&self, c: Capture) -> bool {
        self.entries.iter().any(|(k, _)| *k == c)
    }

    /// Convenience: returns the `NodeId` bound to `c`, or `None` if `c`
    /// was not captured.
    ///
    /// For a `Binding::Output` the owning node is recovered via
    /// [`strider_ir::Graph::node_for_output`]; for a `Binding::Node`
    /// the stored id is returned directly.
    #[must_use]
    pub fn get_node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        match self.get_binding(c)? {
            Binding::Node(node) => Some(node),
            Binding::Output(out) => Some(graph.node_for_output(out)),
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
    #[must_use]
    pub fn get_uint(&self, c: Capture, graph: &Graph) -> Option<u128> {
        let out = self.get_output(c)?;
        let NodeKind::IntConst(val) = graph.kind_of_output(out) else {
            return None;
        };
        let ty = graph.output_kind(out).as_value()?;
        ty.get_unsigned_int(*val)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant sign-extended from the output type's bit width to
    /// `i128`.
    #[must_use]
    pub fn get_int(&self, c: Capture, graph: &Graph) -> Option<i128> {
        let out = self.get_output(c)?;
        let NodeKind::IntConst(val) = graph.kind_of_output(out) else {
            return None;
        };
        let ty = graph.output_kind(out).as_value()?;
        ty.get_signed_int(*val)
    }

    /// If the node bound to `c` is a boolean constant (an `IntConst`
    /// typed `I1`), returns the stored boolean value (`!= 0`).
    #[must_use]
    pub fn get_bool(&self, c: Capture, graph: &Graph) -> Option<bool> {
        let out = self.get_output(c)?;
        let NodeKind::IntConst(val) = graph.kind_of_output(out) else {
            return None;
        };
        let ty = graph.output_kind(out).as_value()?;
        if !ty.is_bool() {
            return None;
        }
        Some(*val != 0)
    }

    /// If the node bound to `c` is a `FloatConst`, returns the raw
    /// IEEE 754 bit pattern as `u64`.
    #[must_use]
    pub fn get_float_bits(&self, c: Capture, graph: &Graph) -> Option<u64> {
        let out = self.get_output(c)?;
        match graph.kind_of_output(out) {
            NodeKind::FloatConst(bits) => Some(*bits),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_unary_op(&self, c: Capture, graph: &Graph) -> Option<IntUnaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_int_cmp_op(&self, c: Capture, graph: &Graph) -> Option<IntCmpOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntCmpOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a boolean binary op (an `IntBinaryOp`
    /// whose output is `I1`), returns the op variant.
    #[must_use]
    pub fn get_bool_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
        let node = self.get_node(c, graph)?;
        let NodeKind::IntBinaryOp(op) = graph.node_kind(node) else {
            return None;
        };
        let out = self.get_output(c)?;
        if !graph.output_kind(out).as_value()?.is_bool() {
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
    #[must_use]
    pub fn get_float_binary_op(&self, c: Capture, graph: &Graph) -> Option<FloatBinaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::FloatBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_unary_op(&self, c: Capture, graph: &Graph) -> Option<FloatUnaryOp> {
        let node = self.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::FloatUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatCmpOp`, returns the op variant.
    #[must_use]
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
    use strider_ir::node::NodeOutputType;
    use strider_ir_test_utils::make_empty_fn;

    // ── Capture (unified node + output) ──────────────────────────────────

    #[test]
    fn capture_bind_and_get_with_real_output_ids() {
        // Build `return(IntConst(1) + IntConst(2))` to harvest two
        // distinct `NodeOutputId`s from the graph.
        let mut a_out = None;
        let mut b_out = None;
        let _function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
            a_out = Some(av);
            b_out = Some(bv);
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, NodeOutputType::I64)
        })
        .expect("build graph");
        let a = a_out.unwrap();
        let b = b_out.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get(v), None);
        let ba = Binding::Output(a);
        let bb = Binding::Output(b);
        assert!(bindings.bind_capture(v, ba));
        assert_eq!(bindings.get(v), Some(a));

        // Idempotent with same output.
        assert!(bindings.bind_capture(v, ba));
        assert_eq!(bindings.get(v), Some(a));

        // Conflict preserves original.
        assert!(!bindings.bind_capture(v, bb));
        assert_eq!(bindings.get(v), Some(a));
    }

    #[test]
    fn capture_bind_and_get_with_real_node_ids() {
        // Thread distinct values through an Add so both constants
        // stay reachable.
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
            b.build_int_binary_operation(av, bv, IntBinaryOp::Add, NodeOutputType::I64)
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
        assert_eq!(bindings.get_node(v, &function), None);
        let b1 = Binding::Node(n1);
        let b2 = Binding::Node(n2);
        assert!(bindings.bind_capture(v, b1));
        assert_eq!(bindings.get_node(v, &function), Some(n1));
        assert!(bindings.bind_capture(v, b1));
        assert!(!bindings.bind_capture(v, b2));
        assert_eq!(bindings.get_node(v, &function), Some(n1));
    }

    // ── Typed extractors read through the graph ──────────────────────────

    #[test]
    fn get_uint_reads_int_const_through_bound_capture() {
        let mut c_out = None;
        let function = make_empty_fn(|b| {
            let c = b.build_int_const(7u64, NodeOutputType::I64).unwrap();
            c_out = Some(c);
            Ok(c)
        })
        .expect("build graph");
        let c = c_out.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Output(c)));
        assert_eq!(bindings.get_uint(v, &function), Some(7));
    }

    #[test]
    fn get_uint_returns_none_when_not_an_int_const() {
        let mut s_out = None;
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
            let s = b
                .build_int_binary_operation(av, bv, IntBinaryOp::Add, NodeOutputType::I64)
                .unwrap();
            s_out = Some(s);
            Ok(s)
        })
        .expect("build graph");
        let s = s_out.unwrap();

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Output(s)));
        assert_eq!(bindings.get_uint(v, &function), None);
    }

    #[test]
    fn get_int_binary_op_reads_op_variant_through_bound_capture() {
        let mut s_out = None;
        let function = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::I64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::I64).unwrap();
            let s = b
                .build_int_binary_operation(av, bv, IntBinaryOp::Add, NodeOutputType::I64)
                .unwrap();
            s_out = Some(s);
            Ok(s)
        })
        .expect("build graph");
        let s = s_out.unwrap();
        let add_node = function.node_for_output(s);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding::Node(add_node)));
        assert_eq!(
            bindings.get_int_binary_op(v, &function),
            Some(IntBinaryOp::Add)
        );
    }

    #[test]
    fn unbound_capture_yields_none_for_every_typed_extractor() {
        let function = make_empty_fn(|b| b.build_int_const(0u64, NodeOutputType::I64))
            .expect("build graph");
        let bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get(v), None);
        assert_eq!(bindings.get_node(v, &function), None);
        assert_eq!(bindings.get_uint(v, &function), None);
        assert_eq!(bindings.get_int(v, &function), None);
        assert_eq!(bindings.get_bool(v, &function), None);
        assert_eq!(bindings.get_float_bits(v, &function), None);
        assert_eq!(bindings.get_int_binary_op(v, &function), None);
        assert_eq!(bindings.get_int_unary_op(v, &function), None);
        assert_eq!(bindings.get_int_cmp_op(v, &function), None);
        assert_eq!(bindings.get_bool_binary_op(v, &function), None);
        // No `get_bool_unary_op` accessor — bool NOT is `Xor(_, 1):I1`,
        // recovered via `get_bool_binary_op`.
        assert_eq!(bindings.get_float_binary_op(v, &function), None);
        assert_eq!(bindings.get_float_unary_op(v, &function), None);
        assert_eq!(bindings.get_float_cmp_op(v, &function), None);
    }

    // ── mark / restore rollback ──────────────────────────────────────────

    /// `restore` after a speculative `bind_capture` must drop the tail
    /// entries so the post-rollback view is indistinguishable from the
    /// pre-mark view — and a subsequent `bind_capture(c, _)` for the
    /// rolled-back capture must succeed as brand-new (not bounce off a
    /// stale entry).
    #[test]
    fn restore_drops_tail_and_allows_rebind() {
        let function = make_empty_fn(|b| b.build_int_const(1u64, NodeOutputType::I64))
            .expect("build graph");
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

    /// Restoring to a mark that's already the current cursor must be a
    /// no-op — truncating to the current length leaves the list intact.
    #[test]
    fn restore_to_current_mark_is_noop() {
        let function = make_empty_fn(|b| b.build_int_const(1u64, NodeOutputType::I64))
            .expect("build graph");
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

    /// `restore` is a pure truncate: entries appended after the mark
    /// vanish, the kept entry survives, and a rolled-back capture
    /// rebinds cleanly afterwards.
    #[test]
    fn restore_is_pure_truncate_and_rebind_succeeds() {
        let function = make_empty_fn(|b| b.build_int_const(1u64, NodeOutputType::I64)).unwrap();
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
