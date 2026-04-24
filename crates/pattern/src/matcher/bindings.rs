use std::collections::HashMap;

use ir::node::{NodeId, NodeOutputId};
use ir::{
    BoolBinaryOp, BoolUnaryOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp,
    IntUnaryOp,
};

use crate::var::{
    BoolBinaryOpVar, BoolUnaryOpVar, BoolVar, FloatBinaryOpVar, FloatCmpOpVar, FloatUnaryOpVar,
    FloatVar, IntBinaryOpVar, IntCmpOpVar, IntUnaryOpVar, IntVar, NodeVar, Var,
};

// ── Bindings ──────────────────────────────────────────────────────────────────

/// A set of capture-variable bindings accumulated during a single match attempt.
///
/// Bindings are append-only: once a variable is bound it cannot be rebound to a
/// different value.  A mismatch (trying to bind an already-bound variable to a
/// different value) makes the containing match fail.  The matcher snapshots and
/// restores `Bindings` to implement backtracking.
///
/// # Follow-up: journal-based backtracking
///
/// Today `Bindings::clone()` copies 13 `HashMap` headers on every backtrack
/// snapshot (in `NodePat::try_match_common`, `CapturePat`, `WhenPat`,
/// `WhenMatchPat`, the default `try_match_node`).  Even on empty hashbrown
/// maps the constructor-per-field cost adds up across a preorder walk of a
/// large graph.  A sketch of the win-large refactor:
///
/// - Replace the 13 `HashMap`s with one append-only `Vec<Entry>` (Entry =
///   enum over all kinds, ~16 bytes) plus an optional overlay index keyed
///   by `(Kind, u32)` built lazily when the entry count exceeds a
///   threshold.
/// - `snap = b.clone()` becomes `marker = b.entries.len()` (a `usize`,
///   stack-only).
/// - Backtrack: `b.entries.truncate(marker)` (and roll back overlay
///   entries added since `marker`).
///
/// Every current caller takes a full snapshot on entry and fully restores
/// on failure, so it slots in mechanically.  Paired with the `KindMatch`
/// enum refactor (see `node_pat.rs::kind_match`) this would remove most of
/// the dynamic-dispatch cost of the hot path.
#[derive(Clone, Default)]
pub struct Bindings {
    vars: HashMap<Var, NodeOutputId>,
    node_vars: HashMap<NodeVar, NodeId>,
    int_vals: HashMap<IntVar, u64>,
    bool_vals: HashMap<BoolVar, bool>,
    float_bits: HashMap<FloatVar, u64>,
    int_binary_ops: HashMap<IntBinaryOpVar, IntBinaryOp>,
    int_unary_ops: HashMap<IntUnaryOpVar, IntUnaryOp>,
    int_cmp_ops: HashMap<IntCmpOpVar, IntCmpOp>,
    bool_binary_ops: HashMap<BoolBinaryOpVar, BoolBinaryOp>,
    bool_unary_ops: HashMap<BoolUnaryOpVar, BoolUnaryOp>,
    float_binary_ops: HashMap<FloatBinaryOpVar, FloatBinaryOp>,
    float_unary_ops: HashMap<FloatUnaryOpVar, FloatUnaryOp>,
    float_cmp_ops: HashMap<FloatCmpOpVar, FloatCmpOp>,
}

/// Append-only insert: returns `true` if the binding was newly established or
/// idempotently re-established; `false` on conflict (and in that case the map
/// is NOT mutated).
fn bind<K: Eq + std::hash::Hash + Copy, V: Eq + Copy>(
    map: &mut HashMap<K, V>,
    k: K,
    v: V,
) -> bool {
    if let Some(&existing) = map.get(&k) {
        existing == v
    } else {
        map.insert(k, v);
        true
    }
}

impl Bindings {
    pub(crate) fn bind_var(&mut self, v: Var, out: NodeOutputId) -> bool {
        bind(&mut self.vars, v, out)
    }

    pub(crate) fn bind_node_var(&mut self, nv: NodeVar, node: NodeId) -> bool {
        bind(&mut self.node_vars, nv, node)
    }

    /// Returns the `NodeOutputId` bound to `v`, or `None` if unbound.
    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        self.vars.get(&v).copied()
    }

    /// Returns the `NodeId` bound to `nv`, or `None` if unbound.
    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        self.node_vars.get(&nv).copied()
    }
}

/// Emit a `bind_$name` / `get_$name` pair forwarding to the named field.
macro_rules! decl_bind_get {
    ($field:ident, $bind_name:ident, $get_name:ident, $var:ty, $val:ty, $doc_stem:literal) => {
        impl Bindings {
            #[doc = concat!("Bind `v` to ", $doc_stem, ".\n\nReturns `true` on new or idempotent binding, `false` on conflict (no mutation).")]
            pub fn $bind_name(&mut self, v: $var, val: $val) -> bool {
                bind(&mut self.$field, v, val)
            }

            #[doc = concat!("Returns the ", $doc_stem, " bound to `v`, or `None` if unbound.")]
            pub fn $get_name(&self, v: $var) -> Option<$val> {
                self.$field.get(&v).copied()
            }
        }
    };
}

decl_bind_get!(int_vals,        bind_int,             get_int,             IntVar,          u64,          "the integer constant value");
decl_bind_get!(bool_vals,       bind_bool,            get_bool,            BoolVar,         bool,         "the boolean constant value");
decl_bind_get!(float_bits,      bind_float,           get_float_bits,      FloatVar,        u64,          "the float constant IEEE 754 bit pattern");
decl_bind_get!(int_binary_ops,  bind_int_binary_op,   get_int_binary_op,   IntBinaryOpVar,  IntBinaryOp,  "the [`IntBinaryOp`] variant");
decl_bind_get!(int_unary_ops,   bind_int_unary_op,    get_int_unary_op,    IntUnaryOpVar,   IntUnaryOp,   "the [`IntUnaryOp`] variant");
decl_bind_get!(int_cmp_ops,     bind_int_cmp_op,      get_int_cmp_op,      IntCmpOpVar,     IntCmpOp,     "the [`IntCmpOp`] variant");
decl_bind_get!(bool_binary_ops, bind_bool_binary_op,  get_bool_binary_op,  BoolBinaryOpVar, BoolBinaryOp, "the [`BoolBinaryOp`] variant");
decl_bind_get!(bool_unary_ops,  bind_bool_unary_op,   get_bool_unary_op,   BoolUnaryOpVar,  BoolUnaryOp,  "the [`BoolUnaryOp`] variant");
decl_bind_get!(float_binary_ops,bind_float_binary_op, get_float_binary_op, FloatBinaryOpVar,FloatBinaryOp,"the [`FloatBinaryOp`] variant");
decl_bind_get!(float_unary_ops, bind_float_unary_op,  get_float_unary_op,  FloatUnaryOpVar, FloatUnaryOp, "the [`FloatUnaryOp`] variant");
decl_bind_get!(float_cmp_ops,   bind_float_cmp_op,    get_float_cmp_op,    FloatCmpOpVar,   FloatCmpOp,   "the [`FloatCmpOp`] variant");
