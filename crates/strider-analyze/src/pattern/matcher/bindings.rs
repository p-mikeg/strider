use strider_ir::Graph;
use strider_ir::node::{NodeId, NodeKind, NodeOutputId};
use strider_ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::pattern::var::Capture;

// ── Bindings ──────────────────────────────────────────────────────────────────

/// One [`Capture`] binding: the matched node id, plus the value
/// `NodeOutputId` when the pattern that produced the binding is
/// value-producing.  Control-flow patterns (`Call`, `If`, `Return`,
/// `CallOther`) bind only the `NodeId` and leave `output = None`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Binding(pub(crate) NodeId, pub(crate) Option<NodeOutputId>);

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
/// an O(1) `Vec::truncate`.  No allocations, no per-kind HashMap
/// clones, no deep copy of the full state.
///
/// Storage shape: an append-only journal `entries: Vec<(Capture,
/// Binding)>` (the rollback log) **plus** an `FxHashMap<Capture,
/// usize>` overlay whose value is the journal index.  `bind_capture` /
/// `get_binding` are O(1); `restore` truncates the journal and walks
/// the dropped tail to evict matching overlay entries (also O(k) in
/// the number of dropped entries, never O(N)).  `Capture` does not
/// impl `cranelift_entity::EntityRef` (ids come from a process-wide
/// atomic counter), so the overlay is `FxHashMap` rather than
/// `SecondaryMap`.
///
/// External callers see `Bindings` as read-only: construction is via
/// `Default::default()`, the production mutation path
/// (`Self::bind_capture`) is `pub(crate)`, and test scaffolds reach
/// for `Self::bind_capture_for_test`.  The `mark` / `restore`
/// journal API is `pub(crate)` because only the matcher's
/// commutative-retry / speculative-attempt paths legitimately need it.
#[derive(Clone, Default)]
pub struct Bindings {
    /// Append-only journal of `(Capture, Binding)` insertions in the
    /// order they were produced — preserves `iter()` ordering and is
    /// the source of truth for `restore`.
    entries: Vec<(Capture, Binding)>,
    /// O(1) `Capture → index-into-entries` overlay.  Kept in sync with
    /// `entries` on every push and on `restore`.
    index: rustc_hash::FxHashMap<Capture, usize>,
}

/// Opaque marker returned by [`Bindings::mark`] and consumed by
/// [`Bindings::restore`].  Represents "the binding state at the moment of
/// marking"; rolling back discards entries appended after the mark.
#[derive(Clone, Copy)]
pub struct BindingsMark(usize);

impl Bindings {
    /// Snapshot the current state in O(1) with no allocations.
    /// Use with [`Self::restore`] to roll back failed match attempts.
    pub(crate) fn mark(&self) -> BindingsMark {
        BindingsMark(self.entries.len())
    }

    /// Discard every entry appended after `mark` was taken.  Idempotent:
    /// restoring to a mark that's already current is a no-op.
    ///
    /// Iterates the dropped tail to evict the matching overlay entries
    /// — every overlay key is also in the journal at exactly one index,
    /// so dropping `entries[mark.0..]` and removing each `Capture` from
    /// `index` keeps the two views in sync.
    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        if mark.0 >= self.entries.len() {
            return;
        }
        for (c, _) in &self.entries[mark.0..] {
            self.index.remove(c);
        }
        self.entries.truncate(mark.0);
    }

    /// Bind `c` to `binding`.  Returns `true` on new or idempotent
    /// (full-binding-equal) bind, `false` on conflict (no mutation).
    ///
    /// O(1) via the `index` overlay: a hit returns the existing
    /// binding's equality; a miss appends to `entries` and updates the
    /// overlay.
    ///
    /// Tightened to `pub(crate)`: callers outside `pattern` (test
    /// scaffolds in particular) construct bindings via
    /// [`Self::bind_capture_for_test`] which has the same shape but
    /// a name signal that the caller is bypassing the matcher's
    /// normal accumulation path.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        if let Some(&idx) = self.index.get(&c) {
            return self.entries[idx].1 == binding;
        }
        let idx = self.entries.len();
        self.entries.push((c, binding));
        self.index.insert(c, idx);
        true
    }

    /// Test-only setter: directly install a `(Capture, Binding)`
    /// pair on this `Bindings` value, bypassing the matcher.  Same
    /// semantics as [`Self::bind_capture`] (returns `true` on new or
    /// idempotent bind, `false` on conflict).  The `_for_test` suffix
    /// signals that the caller is hand-building a `Bindings` rather
    /// than going through [`crate::pattern::Matcher::find_all`].
    #[allow(dead_code)]
    pub(crate) fn bind_capture_for_test(&mut self, c: Capture, binding: Binding) -> bool {
        self.bind_capture(c, binding)
    }

    /// Returns the [`Binding`] (node + optional value output) bound to
    /// `c`, or `None` if `c` was not captured in this match.
    ///
    /// O(1) via the `index` overlay.
    #[must_use]
    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        let idx = *self.index.get(&c)?;
        Some(self.entries[idx].1)
    }

    /// Convenience: returns the value `NodeOutputId` bound to `c`, or
    /// `None` if `c` was not captured or the binding was control-flow.
    #[must_use]
    pub fn get_output(&self, c: Capture) -> Option<NodeOutputId> {
        self.get_binding(c).and_then(|b| b.1)
    }

    /// Alias for [`Self::get_output`] — kept short because it is the
    /// most-used accessor inside `*_const_with!` macro bodies and
    /// post-match `when_match` closures.
    #[must_use]
    pub fn get(&self, c: Capture) -> Option<NodeOutputId> {
        self.get_output(c)
    }

    /// Convenience: returns the `NodeId` bound to `c`, or `None` if `c`
    /// was not captured.
    #[must_use]
    pub fn get_node(&self, c: Capture) -> Option<NodeId> {
        self.get_binding(c).map(|b| b.0)
    }

    /// Iterates over every `(Capture, Binding)` recorded by this match.
    /// Used by [`crate::pattern::Matcher::find_all_requirements`] to compute cross-pattern
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

    /// If the node bound to `c` is a `BoolConst`, returns the stored
    /// boolean value.
    #[must_use]
    pub fn get_bool(&self, c: Capture, graph: &Graph) -> Option<bool> {
        let out = self.get_output(c)?;
        match graph.kind_of_output(out) {
            NodeKind::BoolConst(val) => Some(*val),
            _ => None,
        }
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
    pub fn get_int_binary_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<IntBinaryOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::IntBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_unary_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<IntUnaryOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::IntUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is an `IntCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_int_cmp_op(&self, c: Capture, graph: &Graph) -> Option<IntCmpOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::IntCmpOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `BoolBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_bool_binary_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<BoolBinaryOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::BoolBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `BoolUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_bool_unary_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<BoolUnaryOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::BoolUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_binary_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<FloatBinaryOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::FloatBinaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_unary_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<FloatUnaryOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::FloatUnaryOp(op) => Some(*op),
            _ => None,
        }
    }

    /// If the node bound to `c` is a `FloatCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_float_cmp_op(
        &self,
        c: Capture,
        graph: &Graph,
    ) -> Option<FloatCmpOp> {
        let node = self.get_node(c)?;
        match graph.node_kind(node) {
            NodeKind::FloatCmpOp(op) => Some(*op),
            _ => None,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────
//
// These tests live inline (not under `crates/strider-analyze/tests/...`)
// because `Binding` and `Bindings::bind_capture` are `pub(crate)` —
// integration tests cannot reach them. The unified-binding contract
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
        let g = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
            a_out = Some(av);
            b_out = Some(bv);
            b.build_int_binary_operation(
                av,
                bv,
                IntBinaryOp::Add,
                NodeOutputType::U64,
            )
        })
        .expect("build graph");
        let a = a_out.unwrap();
        let b = b_out.unwrap();

        let na = g.get_node_from_output(a);
        let nb = g.get_node_from_output(b);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get(v), None);
        let ba = Binding(na, Some(a));
        let bb = Binding(nb, Some(b));
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
        let g = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
            b.build_int_binary_operation(
                av,
                bv,
                IntBinaryOp::Add,
                NodeOutputType::U64,
            )
        })
        .expect("build graph");

        let mut ids = g
            .preorder()
            .filter(|&n| matches!(g.node_kind(n), NodeKind::IntConst(_)));
        let n1 = ids.next().expect("first const node");
        let n2 = ids.next().expect("second const node");
        assert_ne!(n1, n2);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get_node(v), None);
        let b1 = Binding(n1, None);
        let b2 = Binding(n2, None);
        assert!(bindings.bind_capture(v, b1));
        assert_eq!(bindings.get_node(v), Some(n1));
        assert!(bindings.bind_capture(v, b1));
        assert!(!bindings.bind_capture(v, b2));
        assert_eq!(bindings.get_node(v), Some(n1));
    }

    // ── Typed extractors read through the graph ──────────────────────────

    #[test]
    fn get_uint_reads_int_const_through_bound_capture() {
        let mut c_out = None;
        let g = make_empty_fn(|b| {
            let c = b.build_int_const(7u64, NodeOutputType::U64).unwrap();
            c_out = Some(c);
            Ok(c)
        })
        .expect("build graph");
        let c = c_out.unwrap();
        let n = g.get_node_from_output(c);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding(n, Some(c))));
        assert_eq!(bindings.get_uint(v, &g), Some(7));
    }

    #[test]
    fn get_uint_returns_none_when_not_an_int_const() {
        let mut s_out = None;
        let g = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
            let s = b
                .build_int_binary_operation(
                    av,
                    bv,
                    IntBinaryOp::Add,
                    NodeOutputType::U64,
                )
                .unwrap();
            s_out = Some(s);
            Ok(s)
        })
        .expect("build graph");
        let s = s_out.unwrap();
        let add_node = g.get_node_from_output(s);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding(add_node, Some(s))));
        assert_eq!(bindings.get_uint(v, &g), None);
    }

    #[test]
    fn get_int_binary_op_reads_op_variant_through_bound_capture() {
        let mut s_out = None;
        let g = make_empty_fn(|b| {
            let av = b.build_int_const(1u64, NodeOutputType::U64).unwrap();
            let bv = b.build_int_const(2u64, NodeOutputType::U64).unwrap();
            let s = b
                .build_int_binary_operation(
                    av,
                    bv,
                    IntBinaryOp::Add,
                    NodeOutputType::U64,
                )
                .unwrap();
            s_out = Some(s);
            Ok(s)
        })
        .expect("build graph");
        let s = s_out.unwrap();
        let add_node = g.get_node_from_output(s);

        let mut bindings = Bindings::default();
        let v = Capture::new();
        assert!(bindings.bind_capture(v, Binding(add_node, None)));
        assert_eq!(
            bindings.get_int_binary_op(v, &g),
            Some(IntBinaryOp::Add)
        );
    }

    #[test]
    fn unbound_capture_yields_none_for_every_typed_extractor() {
        let g = make_empty_fn(|b| {
            b.build_int_const(0u64, NodeOutputType::U64)
        })
        .expect("build graph");
        let bindings = Bindings::default();
        let v = Capture::new();
        assert_eq!(bindings.get(v), None);
        assert_eq!(bindings.get_node(v), None);
        assert_eq!(bindings.get_uint(v, &g), None);
        assert_eq!(bindings.get_int(v, &g), None);
        assert_eq!(bindings.get_bool(v, &g), None);
        assert_eq!(bindings.get_float_bits(v, &g), None);
        assert_eq!(bindings.get_int_binary_op(v, &g), None);
        assert_eq!(bindings.get_int_unary_op(v, &g), None);
        assert_eq!(bindings.get_int_cmp_op(v, &g), None);
        assert_eq!(bindings.get_bool_binary_op(v, &g), None);
        assert_eq!(bindings.get_bool_unary_op(v, &g), None);
        assert_eq!(bindings.get_float_binary_op(v, &g), None);
        assert_eq!(bindings.get_float_unary_op(v, &g), None);
        assert_eq!(bindings.get_float_cmp_op(v, &g), None);
    }

    // ── mark / restore rollback ──────────────────────────────────────────

    /// `restore` after a speculative `bind_capture` must wipe both the
    /// journal and the index overlay so the post-rollback view is
    /// indistinguishable from the pre-mark view — and a subsequent
    /// `bind_capture(c, _)` for the rolled-back capture must succeed as
    /// brand-new (not bounce off a stale overlay entry).
    #[test]
    fn restore_evicts_overlay_entries_for_dropped_journal_tail() {
        let g = make_empty_fn(|b| b.build_int_const(1u64, NodeOutputType::U64))
            .expect("build graph");
        let n = g
            .preorder()
            .find(|&n| matches!(g.node_kind(n), NodeKind::IntConst(_)))
            .expect("int const node");

        let mut bindings = Bindings::default();
        let kept = Capture::new();
        let dropped_a = Capture::new();
        let dropped_b = Capture::new();

        assert!(bindings.bind_capture(kept, Binding(n, None)));
        let mark = bindings.mark();
        assert!(bindings.bind_capture(dropped_a, Binding(n, None)));
        assert!(bindings.bind_capture(dropped_b, Binding(n, None)));

        // Pre-restore: all three visible via O(1) overlay.
        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_some());
        assert!(bindings.get_binding(dropped_b).is_some());

        bindings.restore(mark);

        // Post-restore: only `kept` remains.
        assert!(bindings.get_binding(kept).is_some());
        assert!(bindings.get_binding(dropped_a).is_none());
        assert!(bindings.get_binding(dropped_b).is_none());

        // Rebinding a rolled-back capture to a fresh binding must
        // succeed as brand-new — the overlay must not retain a stale
        // entry pointing at a now-truncated journal index.
        assert!(bindings.bind_capture(dropped_a, Binding(n, None)));
        assert!(bindings.get_binding(dropped_a).is_some());
    }

    /// Restoring to a mark that's already the current cursor must be a
    /// no-op — covers the early-return guard in `restore`.
    #[test]
    fn restore_to_current_mark_is_noop() {
        let g = make_empty_fn(|b| b.build_int_const(1u64, NodeOutputType::U64))
            .expect("build graph");
        let n = g
            .preorder()
            .find(|&n| matches!(g.node_kind(n), NodeKind::IntConst(_)))
            .expect("int const node");

        let mut bindings = Bindings::default();
        let c = Capture::new();
        assert!(bindings.bind_capture(c, Binding(n, None)));
        let mark = bindings.mark();
        bindings.restore(mark);
        assert!(bindings.get_binding(c).is_some());
    }

    // ── Globally unique IDs ──────────────────────────────────────────────

    /// `Capture::new()` uses a process-wide atomic counter; allocating
    /// many must produce all-distinct IDs.  `Debug` output is the only
    /// public handle on the raw ID, so the test uses it as a set key.
    #[test]
    fn capture_ids_are_globally_unique_across_many_allocations() {
        const N: usize = 256;
        let mut ids: Vec<String> = Vec::with_capacity(N);
        for _ in 0..N {
            ids.push(format!("{:?}", Capture::new()));
        }
        let unique: std::collections::HashSet<&String> =
            ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }
}
