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
#[derive(Clone, Default)]
pub struct Bindings {
    vars: HashMap<Var, NodeOutputId>,
    node_vars: HashMap<NodeVar, NodeId>,
    /// Values captured by [`IntVar`] bindings (integer constant bit patterns).
    int_vals: HashMap<IntVar, u64>,
    /// Values captured by [`BoolVar`] bindings (boolean constant values).
    bool_vals: HashMap<BoolVar, bool>,
    /// Values captured by [`FloatVar`] bindings (float constant IEEE 754 bit patterns).
    float_bits: HashMap<FloatVar, u64>,
    /// Operator variants captured by [`IntBinaryOpVar`] bindings.
    int_binary_ops: HashMap<IntBinaryOpVar, IntBinaryOp>,
    /// Operator variants captured by [`IntUnaryOpVar`] bindings.
    int_unary_ops: HashMap<IntUnaryOpVar, IntUnaryOp>,
    /// Operator variants captured by [`IntCmpOpVar`] bindings.
    int_cmp_ops: HashMap<IntCmpOpVar, IntCmpOp>,
    /// Operator variants captured by [`BoolBinaryOpVar`] bindings.
    bool_binary_ops: HashMap<BoolBinaryOpVar, BoolBinaryOp>,
    /// Operator variants captured by [`BoolUnaryOpVar`] bindings.
    bool_unary_ops: HashMap<BoolUnaryOpVar, BoolUnaryOp>,
    /// Operator variants captured by [`FloatBinaryOpVar`] bindings.
    float_binary_ops: HashMap<FloatBinaryOpVar, FloatBinaryOp>,
    /// Operator variants captured by [`FloatUnaryOpVar`] bindings.
    float_unary_ops: HashMap<FloatUnaryOpVar, FloatUnaryOp>,
    /// Operator variants captured by [`FloatCmpOpVar`] bindings.
    float_cmp_ops: HashMap<FloatCmpOpVar, FloatCmpOp>,
}

impl Bindings {
    pub(super) fn bind_var(&mut self, v: Var, out: NodeOutputId) -> bool {
        if let Some(&existing) = self.vars.get(&v) {
            existing == out
        } else {
            self.vars.insert(v, out);
            true
        }
    }

    pub(super) fn bind_node_var(&mut self, nv: NodeVar, node: NodeId) -> bool {
        if let Some(&existing) = self.node_vars.get(&nv) {
            existing == node
        } else {
            self.node_vars.insert(nv, node);
            true
        }
    }

    /// Bind `iv` to the integer constant `val`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same value.  Returns `false` if `iv` was already bound to a
    /// **different** value (the match should fail).
    pub fn bind_int(&mut self, iv: IntVar, val: u64) -> bool {
        if let Some(&existing) = self.int_vals.get(&iv) {
            existing == val
        } else {
            self.int_vals.insert(iv, val);
            true
        }
    }

    /// Bind `bv` to the boolean constant `val`.
    ///
    /// Returns `true` if the binding succeeded (new or idempotent), `false` on
    /// conflict.
    pub fn bind_bool(&mut self, bv: BoolVar, val: bool) -> bool {
        if let Some(&existing) = self.bool_vals.get(&bv) {
            existing == val
        } else {
            self.bool_vals.insert(bv, val);
            true
        }
    }

    /// Bind `fv` to the float constant IEEE 754 bit pattern `bits`.
    ///
    /// Returns `true` if the binding succeeded (new or idempotent), `false` on
    /// conflict.
    pub fn bind_float(&mut self, fv: FloatVar, bits: u64) -> bool {
        if let Some(&existing) = self.float_bits.get(&fv) {
            existing == bits
        } else {
            self.float_bits.insert(fv, bits);
            true
        }
    }

    /// Returns the `NodeOutputId` bound to `v`, or `None` if unbound.
    pub fn get(&self, v: Var) -> Option<NodeOutputId> {
        self.vars.get(&v).copied()
    }

    /// Returns the `NodeId` bound to `nv`, or `None` if unbound.
    pub fn get_node(&self, nv: NodeVar) -> Option<NodeId> {
        self.node_vars.get(&nv).copied()
    }

    /// Returns the integer constant value bound to `iv`, or `None` if unbound.
    pub fn get_int(&self, iv: IntVar) -> Option<u64> {
        self.int_vals.get(&iv).copied()
    }

    /// Returns the boolean constant value bound to `bv`, or `None` if unbound.
    pub fn get_bool(&self, bv: BoolVar) -> Option<bool> {
        self.bool_vals.get(&bv).copied()
    }

    /// Returns the float constant IEEE 754 bit pattern bound to `fv`, or `None`
    /// if unbound.
    pub fn get_float_bits(&self, fv: FloatVar) -> Option<u64> {
        self.float_bits.get(&fv).copied()
    }

    /// Bind `v` to the integer binary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_int_binary_op(&mut self, v: IntBinaryOpVar, op: IntBinaryOp) -> bool {
        if let Some(&existing) = self.int_binary_ops.get(&v) {
            existing == op
        } else {
            self.int_binary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`IntBinaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_int_binary_op(&self, v: IntBinaryOpVar) -> Option<IntBinaryOp> {
        self.int_binary_ops.get(&v).copied()
    }

    /// Bind `v` to the integer unary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_int_unary_op(&mut self, v: IntUnaryOpVar, op: IntUnaryOp) -> bool {
        if let Some(&existing) = self.int_unary_ops.get(&v) {
            existing == op
        } else {
            self.int_unary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`IntUnaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_int_unary_op(&self, v: IntUnaryOpVar) -> Option<IntUnaryOp> {
        self.int_unary_ops.get(&v).copied()
    }

    /// Bind `v` to the integer comparison operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_int_cmp_op(&mut self, v: IntCmpOpVar, op: IntCmpOp) -> bool {
        if let Some(&existing) = self.int_cmp_ops.get(&v) {
            existing == op
        } else {
            self.int_cmp_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`IntCmpOp`] bound to `v`, or `None` if unbound.
    pub fn get_int_cmp_op(&self, v: IntCmpOpVar) -> Option<IntCmpOp> {
        self.int_cmp_ops.get(&v).copied()
    }

    /// Bind `v` to the boolean binary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_bool_binary_op(&mut self, v: BoolBinaryOpVar, op: BoolBinaryOp) -> bool {
        if let Some(&existing) = self.bool_binary_ops.get(&v) {
            existing == op
        } else {
            self.bool_binary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`BoolBinaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_bool_binary_op(&self, v: BoolBinaryOpVar) -> Option<BoolBinaryOp> {
        self.bool_binary_ops.get(&v).copied()
    }

    /// Bind `v` to the boolean unary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_bool_unary_op(&mut self, v: BoolUnaryOpVar, op: BoolUnaryOp) -> bool {
        if let Some(&existing) = self.bool_unary_ops.get(&v) {
            existing == op
        } else {
            self.bool_unary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`BoolUnaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_bool_unary_op(&self, v: BoolUnaryOpVar) -> Option<BoolUnaryOp> {
        self.bool_unary_ops.get(&v).copied()
    }

    /// Bind `v` to the float binary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_float_binary_op(&mut self, v: FloatBinaryOpVar, op: FloatBinaryOp) -> bool {
        if let Some(&existing) = self.float_binary_ops.get(&v) {
            existing == op
        } else {
            self.float_binary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`FloatBinaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_float_binary_op(&self, v: FloatBinaryOpVar) -> Option<FloatBinaryOp> {
        self.float_binary_ops.get(&v).copied()
    }

    /// Bind `v` to the float unary operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_float_unary_op(&mut self, v: FloatUnaryOpVar, op: FloatUnaryOp) -> bool {
        if let Some(&existing) = self.float_unary_ops.get(&v) {
            existing == op
        } else {
            self.float_unary_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`FloatUnaryOp`] bound to `v`, or `None` if unbound.
    pub fn get_float_unary_op(&self, v: FloatUnaryOpVar) -> Option<FloatUnaryOp> {
        self.float_unary_ops.get(&v).copied()
    }

    /// Bind `v` to the float comparison operator `op`.
    ///
    /// Returns `true` if the binding was newly established or was already bound
    /// to the same variant.  Returns `false` on conflict.
    pub fn bind_float_cmp_op(&mut self, v: FloatCmpOpVar, op: FloatCmpOp) -> bool {
        if let Some(&existing) = self.float_cmp_ops.get(&v) {
            existing == op
        } else {
            self.float_cmp_ops.insert(v, op);
            true
        }
    }

    /// Returns the [`FloatCmpOp`] bound to `v`, or `None` if unbound.
    pub fn get_float_cmp_op(&self, v: FloatCmpOpVar) -> Option<FloatCmpOp> {
        self.float_cmp_ops.get(&v).copied()
    }
}
