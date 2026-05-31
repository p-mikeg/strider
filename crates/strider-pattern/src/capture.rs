// `dead_code` allow: the upcoming matcher + rewrite modules consume
// `Binding`, `Bindings::{mark,restore,bind_capture,get_binding,iter}`,
// `BindingsMark`, and `Match::from_root`.  These items are pub(crate)
// — there are no external callers yet, and clippy --release runs
// `-D warnings` so we silence the dead-code complaints at the module
// level until the matcher lands.
#![allow(dead_code)]

//! Capture variables, the binding journal that backs every pattern
//! match, and the public `Match` result.
//!
//! [`Capture`] is the unified data/control capture handle: every
//! pattern position that wants to bind a matched node uses the same
//! type.  After a successful match, [`Match::node`] returns the
//! `NodeId` and [`Match::output`] returns the value `NodeOutputId`
//! (or `None` for control-flow nodes that have no single value output).
//!
//! [`CaptureRef`] is a sealed handle used internally by the pattern
//! graph to record "this pattern node binds capture `c`".  It exists
//! to keep `Capture` (the user-facing identifier) and its in-graph
//! reference textually distinct in API signatures — the underlying id
//! is identical.
//!
//! [`Bindings`] is the per-match journal of capture-to-binding entries
//! with O(1) rebind-conflict detection and journal-based rollback
//! (`mark` / `restore`).  Typed extraction of constant values and
//! op-variant discriminants happens through [`Match`] / [`Bindings`]
//! helpers (`get_uint`, `get_int_binary_op`, …) which look up the
//! bound `NodeId` and inspect the underlying `NodeKind`.

use std::sync::atomic::{AtomicU32, Ordering};

use strider_ir::node::{NodeId, NodeKind, NodeOutputId};
use strider_ir::{FloatBinaryOp, FloatCmpOp, FloatUnaryOp, Graph, IntBinaryOp, IntCmpOp, IntUnaryOp};

// ── Capture ──────────────────────────────────────────────────────────────────

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Unified capture variable.  Binds to a single matched node — every
/// successful match records both the node's `NodeId` and (when the
/// pattern is value-producing) the value `NodeOutputId`.
///
/// Each `Capture::new()` call produces a globally unique id via a
/// process-wide atomic counter; uniqueness lets the matcher's
/// [`Bindings`] storage (an append-only `Vec`) identify entries
/// unambiguously without per-pattern bookkeeping.
///
/// The same `Capture` can appear in multiple positions of a pattern;
/// the matcher requires all occurrences to bind to the **same** node
/// (and the same value output, if applicable).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Capture(u32);

impl Capture {
    #[must_use]
    pub fn new() -> Self {
        Self(next_id())
    }

    /// Returns the globally-unique numeric id of this capture.
    ///
    /// Exposed for downstream consumers (e.g. PyO3 bindings) that need
    /// a stable hash key.  The raw id is meant only as an *opaque
    /// identifier*; callers must not rely on the value space being
    /// dense or sequential.
    #[must_use]
    pub fn id(self) -> u32 {
        self.0
    }

    /// Returns the in-graph reference handle for this capture.  Used
    /// by pattern builders that need to store a capture into
    /// `NodeData::capture` without surrendering the user-facing
    /// `Capture` value.
    #[must_use]
    pub fn as_ref(self) -> CaptureRef {
        CaptureRef(self)
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

/// Sealed handle used by `PatGraph::NodeData` to record "this pattern
/// node binds capture `c`".  Carries the same opaque id as the
/// underlying [`Capture`]; the distinct type is purely a clarity aid
/// at API boundaries (a `Capture` is a user-facing handle one constructs
/// with `Capture::new()`; a `CaptureRef` is the slot inside a pattern
/// node that points back at one).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct CaptureRef(Capture);

impl CaptureRef {
    /// Returns the [`Capture`] this reference points at.
    #[must_use]
    pub fn capture(self) -> Capture {
        self.0
    }

    /// Returns the underlying capture id — convenience accessor matching
    /// [`Capture::id`].
    #[must_use]
    pub fn id(self) -> u32 {
        self.0.id()
    }
}

impl From<Capture> for CaptureRef {
    fn from(c: Capture) -> Self {
        CaptureRef(c)
    }
}

impl From<CaptureRef> for Capture {
    fn from(r: CaptureRef) -> Self {
        r.0
    }
}

// ── Bindings ──────────────────────────────────────────────────────────────────

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
        if mark.0 < self.entries.len() {
            for (c, _) in &self.entries[mark.0..] {
                self.index.remove(c);
            }
            self.entries.truncate(mark.0);
        }
    }

    /// Bind `c` to `binding`.  Returns `true` on new or idempotent
    /// (full-binding-equal) bind, `false` on conflict (no mutation).
    ///
    /// O(1) via the `index` overlay: a hit returns the existing
    /// binding's equality; a miss appends to `entries` and updates the
    /// overlay.
    pub(crate) fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        if let Some(&idx) = self.index.get(&c) {
            return self.entries[idx].1 == binding;
        }
        let idx = self.entries.len();
        self.entries.push((c, binding));
        self.index.insert(c, idx);
        true
    }

    /// Returns the [`Binding`] (an `Output` value binding or a
    /// `Node`-only binding) bound to `c`, or `None` if `c` was not
    /// captured in this match.
    ///
    /// O(1) via the `index` overlay.
    #[must_use]
    pub(crate) fn get_binding(&self, c: Capture) -> Option<Binding> {
        let idx = *self.index.get(&c)?;
        Some(self.entries[idx].1)
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
        self.index.contains_key(&c)
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

// ── Match ─────────────────────────────────────────────────────────────────────

/// The result of a successful pattern match against a single root node.
///
/// Provides access to the captured variable bindings and convenience helpers
/// for reading constant values and op-variant discriminants from each
/// captured node.
#[derive(Clone)]
pub struct Match {
    pub(crate) root: NodeId,
    pub(crate) bindings: Bindings,
}

impl Match {
    /// Construct a [`Match`] from a root [`NodeId`] and the
    /// accumulated bindings.  `pub(crate)` because [`Bindings`] is
    /// constructed only by the matcher.
    pub(crate) fn from_root(root: NodeId, bindings: Bindings) -> Self {
        Self { root, bindings }
    }

    /// The root node where the top-level pattern matched.
    #[must_use]
    pub fn root(&self) -> NodeId {
        self.root
    }

    /// Returns the `NodeId` bound to `c`, or `None` if `c` was not
    /// captured in this match.  Every successful capture binds at
    /// least the matched node id; for value-producing captures the
    /// owning node is recovered from the bound `NodeOutputId` via
    /// [`strider_ir::Graph::node_for_output`], hence the `&Graph` arg.
    #[must_use]
    pub fn node(&self, c: Capture, graph: &Graph) -> Option<NodeId> {
        self.bindings.get_node(c, graph)
    }

    /// Returns the value `NodeOutputId` bound to `c`, or `None` if
    /// `c` was not captured or the binding was control-flow.
    /// Multi-output nodes (e.g. `Load = [Memory, Value]`) bind the
    /// value slot.
    #[must_use]
    pub fn output(&self, c: Capture) -> Option<NodeOutputId> {
        self.bindings.get_output(c)
    }

    /// Whether `c` is bound in this match (either variant of the
    /// internal `Binding` — value or node-only).  Graph-free — useful
    /// for `c in m` containment checks where the only question is
    /// "did this capture fire?".
    #[must_use]
    pub fn is_bound(&self, c: Capture) -> bool {
        self.bindings.is_bound(c)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant value masked to the output type's bit width.  Returns
    /// `None` for unbound captures, control-flow bindings, or
    /// non-`IntConst` producers.
    #[must_use]
    pub fn get_uint(&self, c: Capture, graph: &Graph) -> Option<u128> {
        self.bindings.get_uint(c, graph)
    }

    /// If the node bound to `c` is an `IntConst`, returns the stored
    /// constant sign-extended from the output type's bit width to
    /// `i128`.  Returns `None` otherwise.
    #[must_use]
    pub fn get_int(&self, c: Capture, graph: &Graph) -> Option<i128> {
        self.bindings.get_int(c, graph)
    }

    /// If the node bound to `c` is a boolean constant (an `IntConst` typed
    /// `I1`), returns the stored boolean value.  Returns `None` otherwise.
    #[must_use]
    pub fn get_bool(&self, c: Capture, graph: &Graph) -> Option<bool> {
        self.bindings.get_bool(c, graph)
    }

    /// If the node bound to `c` is a `FloatConst`, returns the raw
    /// IEEE 754 bit pattern as `u64`.  Returns `None` otherwise.
    #[must_use]
    pub fn get_float_bits(&self, c: Capture, graph: &Graph) -> Option<u64> {
        self.bindings.get_float_bits(c, graph)
    }

    /// If the node bound to `c` is an `IntBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
        self.bindings.get_int_binary_op(c, graph)
    }

    /// If the node bound to `c` is an `IntUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_int_unary_op(&self, c: Capture, graph: &Graph) -> Option<IntUnaryOp> {
        self.bindings.get_int_unary_op(c, graph)
    }

    /// If the node bound to `c` is an `IntCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_int_cmp_op(&self, c: Capture, graph: &Graph) -> Option<IntCmpOp> {
        self.bindings.get_int_cmp_op(c, graph)
    }

    /// If the node bound to `c` is a boolean binary op (an `IntBinaryOp`
    /// typed `I1`), returns the op variant.
    #[must_use]
    pub fn get_bool_binary_op(&self, c: Capture, graph: &Graph) -> Option<IntBinaryOp> {
        self.bindings.get_bool_binary_op(c, graph)
    }

    // Note: there is no `get_bool_unary_op` accessor.  A boolean
    // logical NOT is `Xor(x, IntConst(1)):I1` since the former BitNot unary-op
    // was removed in favour of `Xor(_, all_ones)`, so the op variant is
    // recovered via [`Self::get_bool_binary_op`] (which returns
    // `IntBinaryOp::Xor`).

    /// If the node bound to `c` is a `FloatBinaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_binary_op(&self, c: Capture, graph: &Graph) -> Option<FloatBinaryOp> {
        self.bindings.get_float_binary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatUnaryOp`, returns the op variant.
    #[must_use]
    pub fn get_float_unary_op(&self, c: Capture, graph: &Graph) -> Option<FloatUnaryOp> {
        self.bindings.get_float_unary_op(c, graph)
    }

    /// If the node bound to `c` is a `FloatCmpOp`, returns the op variant.
    #[must_use]
    pub fn get_float_cmp_op(&self, c: Capture, graph: &Graph) -> Option<FloatCmpOp> {
        self.bindings.get_float_cmp_op(c, graph)
    }

    /// Returns the [`rsleigh::Vn`] associated with the binding, if one
    /// can be determined.  The output-to-varnode mapping is well-defined
    /// only for a handful of producer kinds:
    ///
    /// * `InitialVar(vn)` — the varnode whose function-entry value is
    ///   read.
    /// * `Call` outputs at slot `2 + i` — the varnode at the per-Call
    ///   override on [`strider_ir::Function::call_clobbered_override`] when one
    ///   was recorded (e.g. `__fentry__` callbacks built via
    ///   [`strider_ir::FunctionBuilder::build_call_with_cc`]), otherwise the
    ///   varnode at `Graph::call_clobbered[i]`.
    /// * `CallOther` outputs in their clobber slot range (slot 2.. for
    ///   value-less CallOther, slot 3.. for CallOther with a value
    ///   output) — the varnode at the per-CallOther override on
    ///   [`strider_ir::Function::call_clobbered_override`] when one was recorded,
    ///   otherwise the varnode at
    ///   `Graph::call_other_clobbered[i]`.
    ///
    /// Returns `None` for unbound captures or producers without a
    /// well-defined varnode mapping.
    #[must_use]
    pub fn get_vn(&self, c: Capture, function: &strider_ir::Function) -> Option<rsleigh::Vn> {
        let binding = self.bindings.get_binding(c)?;
        if let Binding::Output(out) = binding {
            let (node, slot) = function.output_definition(out);
            let kind = function.node_kind(node);
            // Call: clobber slots start at index 2.
            if matches!(kind, NodeKind::Call) && slot >= 2 {
                let idx = (slot - 2) as usize;
                if let Some(override_list) = function.call_clobbered_override(node) {
                    return override_list.get(idx).copied();
                }
                return function.call_clobbered_regs().get(idx).copied();
            }
            // CallOther: clobber slots start at index 2 (no value
            // output) or 3 (with value output).  Detect by total
            // output count: `2 + clobber_len` for value-less,
            // `3 + clobber_len` for value-bearing.
            //
            // The clobber length here is per-CallOther: a precise-ABI
            // CallOther carries its own `call_clobbered_override` list,
            // and that list's length may differ from the function-default
            // `call_other_clobbered` (e.g. `syscall` writes RAX/RCX/R11
            // = 3 slots, while a SWI emits only `[r0]` = 1 slot, while
            // the function-default may be empty).  Use the override
            // length when present so `clobber_start` matches the actual
            // node shape — a function-default-based check would produce
            // a "shape we don't recognise" miss for every per-CallOther
            // override whose length differs from the default.
            if matches!(kind, NodeKind::CallOther { .. }) {
                let total_outputs = function.node_outputs(node).len();
                let clobber_len = function
                    .call_clobbered_override(node)
                    .map_or(function.call_other_clobbered_regs().len(), |ov| ov.len());
                let clobber_start: u32 = if total_outputs == 2 + clobber_len {
                    2
                } else if total_outputs == 3 + clobber_len {
                    3
                } else {
                    // Shape we don't recognise; bail.
                    return None;
                };
                if slot < clobber_start {
                    // Slot 0/1 are Control/Memory; slot 2 (value-bearing
                    // form) is the user-op's value output — none of these
                    // map to a varnode.
                    return None;
                }
                let idx = (slot - clobber_start) as usize;
                if let Some(override_list) = function.call_clobbered_override(node) {
                    return override_list.get(idx).copied();
                }
                return function.call_other_clobbered_regs().get(idx).copied();
            }
        }
        // Fallback: an `InitialVar` carries its varnode tag on the
        // owning node — recover the node id (directly for a
        // [`Binding::Node`], via `node_for_output` for a
        // [`Binding::Output`]) and inspect the kind.
        let node = self.bindings.get_node(c, function)?;
        match function.node_kind(node) {
            NodeKind::InitialVar(vn) => Some(*vn),
            _ => None,
        }
    }

    /// Returns the asm-instruction-address fingerprint of the node bound
    /// to `c`, as a sorted-deduplicated slice.  Returns an empty slice
    /// when the capture is unbound or when the bound node has no
    /// recorded contributors (legitimately empty for region / phi /
    /// initial-state kinds — see
    /// [`strider_ir::Function::asm_fingerprint`] for the documented exempt set).
    ///
    /// This is the proof-of-correctness aid: when a pattern query
    /// captures a value node, this slice lists the machine
    /// instructions whose lifting (or subsequent rewrite) contributed
    /// to that node's value.  See
    /// `docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md`
    /// for the full contract.
    #[must_use]
    pub fn asm_fingerprint<'g>(&self, c: Capture, graph: &'g strider_ir::Function) -> &'g [u64] {
        match self.bindings.get_node(c, graph) {
            Some(node) => graph.asm_fingerprint(node),
            None => &[],
        }
    }

    /// If the node bound to `c` is an [`strider_ir::node::NodeKind::IntConstWide`],
    /// returns the raw little-endian bytes of its stored value (32 bytes
    /// for `I256`, 64 for `I512`).  Returns `None` for unbound captures
    /// or non-`IntConstWide` producers — narrow constants go through
    /// [`Self::get_uint`] / [`Self::get_int`] instead.
    #[must_use]
    pub fn get_wide_bytes(&self, c: Capture, graph: &Graph) -> Option<Vec<u8>> {
        let node = self.bindings.get_node(c, graph)?;
        match graph.node_kind(node) {
            NodeKind::IntConstWide(id) => Some(graph.wide_const(*id).to_le_bytes()),
            _ => None,
        }
    }

    /// Returns an owned copy of the full [`Bindings`] captured by this match.
    /// Used by the rewrite-rule interpreter (drops the `Matcher` borrow
    /// before mutating the graph) and by tests.
    #[must_use]
    pub fn bindings_clone(&self) -> Bindings {
        self.bindings.clone()
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

    /// `restore` after a speculative `bind_capture` must wipe both the
    /// journal and the index overlay so the post-rollback view is
    /// indistinguishable from the pre-mark view — and a subsequent
    /// `bind_capture(c, _)` for the rolled-back capture must succeed as
    /// brand-new (not bounce off a stale overlay entry).
    #[test]
    fn restore_evicts_overlay_entries_for_dropped_journal_tail() {
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
        assert!(bindings.bind_capture(dropped_a, Binding::Node(n)));
        assert!(bindings.get_binding(dropped_a).is_some());
    }

    /// Restoring to a mark that's already the current cursor must be a
    /// no-op — covers the early-return guard in `restore`.
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
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
    }

    // ── CaptureRef ────────────────────────────────────────────────────────

    /// `Capture::as_ref()` and `CaptureRef::capture()` round-trip
    /// without loss of identity, and the underlying ids agree.
    #[test]
    fn capture_ref_round_trip_preserves_id() {
        let c = Capture::new();
        let r = c.as_ref();
        assert_eq!(r.id(), c.id());
        assert_eq!(r.capture(), c);
    }

    /// `From<Capture>` and `From<CaptureRef>` round-trip both ways.
    #[test]
    fn capture_ref_from_conversions() {
        let c = Capture::new();
        let r: CaptureRef = c.into();
        let back: Capture = r.into();
        assert_eq!(c, back);
    }
}
