use ir::Graph;
use ir::node::{NodeId, NodeKind, NodeOutputId};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::var::Capture;

// ── Bindings ──────────────────────────────────────────────────────────────────

/// One [`Capture`] binding: the matched node id, plus the value
/// `NodeOutputId` when the pattern that produced the binding is
/// value-producing.  Control-flow patterns (`Call`, `If`, `Return`,
/// `CallOther`) bind only the `NodeId` and leave `output = None`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub node: NodeId,
    pub output: Option<NodeOutputId>,
}

impl Binding {
    #[must_use]
    pub fn new(node: NodeId, output: Option<NodeOutputId>) -> Self {
        Self { node, output }
    }
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
/// an O(1) `Vec::truncate`.  No allocations, no per-kind HashMap
/// clones, no deep copy of the full state.
///
/// Lookups (`get_*`) are linear scans over the entry `Vec`.  In the
/// patterns we currently exercise (constant-fold rules, indirect-
/// branch resolvers) bindings stay in the single-digit range; if
/// profiling shows the scan as hot we can layer a hash overlay on top
/// of the journaled `Vec` without changing the public API.
///
/// External callers see `Bindings` as read-only: construction is via
/// `Default::default()`, the production mutation path
/// ([`Self::bind_capture`]) is `pub(crate)`, and test scaffolds reach
/// for [`Self::bind_capture_for_test`].  The `mark` / `restore`
/// journal API is `pub(crate)` because only the matcher's
/// commutative-retry / speculative-attempt paths legitimately need it.
#[derive(Clone, Default)]
pub struct Bindings {
    entries: Vec<(Capture, Binding)>,
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
    pub(crate) fn restore(&mut self, mark: BindingsMark) {
        self.entries.truncate(mark.0);
    }

    /// Bind `c` to `binding`.  Returns `true` on new or idempotent
    /// (full-binding-equal) bind, `false` on conflict (no mutation).
    ///
    /// Tightened to `pub(crate)`: callers outside `pattern` (test
    /// scaffolds in particular) construct bindings via
    /// [`Self::bind_capture_for_test`] which has the same shape but
    /// a name signal that the caller is bypassing the matcher's
    /// normal accumulation path.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        for (k, existing) in &self.entries {
            if *k == c {
                return *existing == binding;
            }
        }
        self.entries.push((c, binding));
        true
    }

    /// Test-only setter: directly install a `(Capture, Binding)`
    /// pair on this `Bindings` value, bypassing the matcher.  Same
    /// semantics as [`Self::bind_capture`] (returns `true` on new or
    /// idempotent bind, `false` on conflict).  The `_for_test` suffix
    /// signals that the caller is hand-building a `Bindings` for use
    /// with [`crate::Match::new_for_test`] rather than going through
    /// [`crate::Matcher::find_all`].
    pub fn bind_capture_for_test(&mut self, c: Capture, binding: Binding) -> bool {
        self.bind_capture(c, binding)
    }

    /// Returns the [`Binding`] (node + optional value output) bound to
    /// `c`, or `None` if `c` was not captured in this match.
    #[must_use]
    pub fn get_binding(&self, c: Capture) -> Option<Binding> {
        self.entries
            .iter()
            .find(|(k, _)| *k == c)
            .map(|(_, b)| *b)
    }

    /// Convenience: returns the value `NodeOutputId` bound to `c`, or
    /// `None` if `c` was not captured or the binding was control-flow.
    #[must_use]
    pub fn get_output(&self, c: Capture) -> Option<NodeOutputId> {
        self.get_binding(c).and_then(|b| b.output)
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
        self.get_binding(c).map(|b| b.node)
    }

    /// Iterates over every `(Capture, Binding)` recorded by this match.
    /// Used by [`crate::Matcher::find_all_requirements`] to compute cross-pattern
    /// shared-capture agreement.  Order is the order bindings were
    /// appended during matching (preorder of the pattern tree).
    pub fn iter(&self) -> impl Iterator<Item = (Capture, Binding)> + '_ {
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
