use ir::node::{NodeId, NodeOutputId};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, Capture, FloatBinaryOpVar, FloatCmpOpVar,
    FloatUnaryOpVar, FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar,
};

// ── Bindings ──────────────────────────────────────────────────────────────────

/// One unified [`Capture`] binding: the matched node id, plus the value
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
/// Bindings are append-only: once a variable is bound it cannot be
/// rebound to a different value.  A mismatch (trying to bind an
/// already-bound variable to a different value) makes the containing
/// match fail.
///
/// Backtracking uses a journal-based scheme: every match site that
/// wants to speculatively attempt sub-matches calls [`Self::mark`]
/// before the attempt and [`Self::restore`] on failure — the marker is
/// a `usize` cursor into the append-only entry `Vec`, and restoring is
/// an O(1) `Vec::truncate`.  No allocations, no per-kind HashMap
/// clones, no deep copy of the full state.
///
/// Lookups (`get_*`) are linear scans filtered by entry variant.  In
/// the patterns we currently exercise (constant-fold rules,
/// indirect-branch resolvers) bindings stay in the single-digit
/// range; if profiling shows the scan as hot we can layer a hash
/// overlay on top of the journaled `Vec` without changing the public
/// API.
///
/// External callers see `Bindings` as read-only: construction is via
/// `Default::default()`, mutation goes through the `bind_*` family,
/// and the `mark` / `restore` journal API is `pub(crate)` because only
/// the matcher's commutative-retry / speculative-attempt paths
/// legitimately need it.
#[derive(Clone, Default)]
pub struct Bindings {
    entries: Vec<BindingEntry>,
}

/// One appended binding.  Kind-tagged: since every capture-variable type has
/// its own `u32` id drawn from a shared counter, a given id can only occur
/// in one variant — but the tagging still gives us type-safe lookup.
#[derive(Clone, Copy)]
enum BindingEntry {
    /// Unified data/control capture.  `output` is `Some` for
    /// value-producing patterns, `None` for control-flow patterns.
    Capture(Capture, Binding),
    Int(IntVar, u128),
    Bool(BoolVar, bool),
    Float(FloatVar, u64),
    IntBinaryOp(IntBinaryOpVar, IntBinaryOp),
    IntUnaryOp(IntUnaryOpVar, IntUnaryOp),
    IntCmpOp(IntCmpOpVar, IntCmpOp),
    BoolBinaryOp(BoolBinaryOpVar, BoolBinaryOp),
    BoolUnaryOp(BoolUnaryOpVar, BoolUnaryOp),
    FloatBinaryOp(FloatBinaryOpVar, FloatBinaryOp),
    FloatUnaryOp(FloatUnaryOpVar, FloatUnaryOp),
    FloatCmpOp(FloatCmpOpVar, FloatCmpOp),
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
    pub fn bind_capture(&mut self, c: Capture, binding: Binding) -> bool {
        for entry in &self.entries {
            if let BindingEntry::Capture(k, existing) = entry
                && *k == c
            {
                return *existing == binding;
            }
        }
        self.entries.push(BindingEntry::Capture(c, binding));
        true
    }

    /// Returns the [`Binding`] (node + optional value output) bound to
    /// `c`, or `None` if `c` was not captured in this match.
    #[must_use]
    pub fn get_binding(&self, c: Capture) -> Option<Binding> {
        for entry in &self.entries {
            if let BindingEntry::Capture(k, b) = entry
                && *k == c
            {
                return Some(*b);
            }
        }
        None
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
}

/// Emit a `bind_$name` / `get_$name` pair as a linear scan over `entries`
/// filtering by the given `BindingEntry` variant.
macro_rules! decl_bind_get {
    ($variant:ident, $bind_name:ident, $get_name:ident, $var:ty, $val:ty, $doc_stem:literal) => {
        impl Bindings {
            #[doc = concat!("Bind `v` to ", $doc_stem, ".\n\nReturns `true` on new or idempotent binding, `false` on conflict (no mutation).")]
            pub fn $bind_name(&mut self, v: $var, val: $val) -> bool {
                for entry in &self.entries {
                    if let BindingEntry::$variant(k, existing) = entry
                        && *k == v
                    {
                        return *existing == val;
                    }
                }
                self.entries.push(BindingEntry::$variant(v, val));
                true
            }

            #[doc = concat!("Returns the ", $doc_stem, " bound to `v`, or `None` if unbound.")]
            pub fn $get_name(&self, v: $var) -> Option<$val> {
                for entry in &self.entries {
                    if let BindingEntry::$variant(k, val) = entry
                        && *k == v
                    {
                        return Some(*val);
                    }
                }
                None
            }
        }
    };
}

decl_bind_get!(Int,             bind_int,             get_int,             IntVar,          u128,         "the integer constant value");
decl_bind_get!(Bool,            bind_bool,            get_bool,            BoolVar,         bool,         "the boolean constant value");
decl_bind_get!(Float,           bind_float,           get_float_bits,      FloatVar,        u64,          "the float constant IEEE 754 bit pattern");
decl_bind_get!(IntBinaryOp,     bind_int_binary_op,   get_int_binary_op,   IntBinaryOpVar,  IntBinaryOp,  "the [`IntBinaryOp`] variant");
decl_bind_get!(IntUnaryOp,      bind_int_unary_op,    get_int_unary_op,    IntUnaryOpVar,   IntUnaryOp,   "the [`IntUnaryOp`] variant");
decl_bind_get!(IntCmpOp,        bind_int_cmp_op,      get_int_cmp_op,      IntCmpOpVar,     IntCmpOp,     "the [`IntCmpOp`] variant");
decl_bind_get!(BoolBinaryOp,    bind_bool_binary_op,  get_bool_binary_op,  BoolBinaryOpVar, BoolBinaryOp, "the [`BoolBinaryOp`] variant");
decl_bind_get!(BoolUnaryOp,     bind_bool_unary_op,   get_bool_unary_op,   BoolUnaryOpVar,  BoolUnaryOp,  "the [`BoolUnaryOp`] variant");
decl_bind_get!(FloatBinaryOp,   bind_float_binary_op, get_float_binary_op, FloatBinaryOpVar,FloatBinaryOp,"the [`FloatBinaryOp`] variant");
decl_bind_get!(FloatUnaryOp,    bind_float_unary_op,  get_float_unary_op,  FloatUnaryOpVar, FloatUnaryOp, "the [`FloatUnaryOp`] variant");
decl_bind_get!(FloatCmpOp,      bind_float_cmp_op,    get_float_cmp_op,    FloatCmpOpVar,   FloatCmpOp,   "the [`FloatCmpOp`] variant");
