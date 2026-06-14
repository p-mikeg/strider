//! The shared IR **read** vocabulary: [`IRViewer`] (point reads) and
//! [`IRWalker`] (control-aware walks), both layered over a single accessor.
//!
//! [`IRViewer`] has one required method — [`IRViewer::function`] — and
//! provides every pure point read (structural edge queries, value/const
//! inspection, the strider-specific graph selectors) as a default method
//! reading through `self.function()`.  [`Function`] implements it directly
//! (`function(&self) -> &Function { self }`), and
//! [`crate::FunctionBuilder`] / [`crate::EditFunction`] each implement it by
//! returning their wrapped field directly, so `Function`,
//! [`crate::FunctionBuilder`], and [`crate::EditFunction`] share ONE read
//! vocabulary with no duplication.
//!
//! [`IRWalker`] layers the control-aware walks (`walk`, `walk_kind`,
//! `reverse_postorder_filter`, `postorder` / `reverse_postorder`, …) on top of [`IRViewer`],
//! delegating to the crate's `walk` primitives over
//! `self.function().graph()`.  It is the single source of truth for
//! traversing a function's IR graph; [`crate::EditFunction`] shadows the
//! order-producing methods with inherent versions that reuse its cached
//! live/roots bookkeeping.

use anyhow::anyhow;

use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

/// Unified return shape for [`IRViewer::const_value`].
///
/// `Int { val, ty }` carries the raw `u128` payload of an `IntConst`
/// node alongside its declared `ValueType` so callers can decide
/// whether to view it unsigned / signed / mask / etc.  `Float` carries
/// the raw bit pattern of a `FloatConst` — the analyzer never needs
/// the float type for constant folding (`f32` vs `f64` is inferred
/// from the surrounding op), so the type isn't carried here.
#[derive(Debug, Clone, Copy)]
pub enum ConstValue {
    Int { val: u128, ty: ValueType },
    Float { bits: u64 },
}

/// The shared IR **point-read** vocabulary, available on every value that
/// can hand out a `&Function` — [`Function`] itself and every [`IRBuilder`](crate::IRBuilder)
/// (the lift builder, the editing context).
///
/// One required method, [`Self::function`]; everything else is a
/// provided default that reads through it.  All methods are pure reads — no
/// node creation, no mutation — so the build-only constructors live on
/// [`crate::IRBuilderExt`] instead.
pub trait IRViewer {
    /// Read access to the [`Function`] under view.
    fn function(&self) -> &Function;

    // ── structural reads ─────────────────────────────────────────────────
    //
    // Forwarded onto `self.function().graph()`, so every viewer
    // (`Function` / `FunctionBuilder` / `EditFunction`) shares one vocabulary
    // for querying the graph's nodes / edges.

    /// Returns the [`NodeKind`] of `node`.
    fn node_kind(&self, node: NodeId) -> &NodeKind {
        self.function().graph().node_kind(node)
    }

    /// Returns the input value edges of `node` as an iterator.
    fn node_inputs(&self, node: NodeId) -> crate::Inputs<'_> {
        self.function().graph().node_inputs(node)
    }

    /// Returns the data inputs of a `Phi` / `MemPhi` node — every input
    /// except the structural `PhiToken` (slot 0).  A `Phi`'s inputs are
    /// `[PhiToken, v0, v1, …]`, one data input per predecessor; this filters
    /// the leading token by kind so the layout assumption stays explicit.
    fn phi_data_inputs(&self, phi: NodeId) -> impl Iterator<Item = ValueId> + '_ {
        let g = self.function().graph();
        g.node_inputs(phi)
            .into_iter()
            .filter(move |&v| g.value_kind(v) != crate::node::ValueKind::PhiToken)
    }

    /// Returns the output value edges of `node`.
    fn node_outputs(&self, node: NodeId) -> &[ValueId] {
        self.function().graph().node_outputs(node)
    }

    /// Returns the exactly-`N` input value edges of `node`.
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` inputs.
    fn node_inputs_exact<const N: usize>(&self, node: NodeId) -> crate::Result<[ValueId; N]> {
        self.function().graph().node_inputs_exact(node)
    }

    /// Returns the exactly-`N` output value edges of `node`.
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` outputs.
    fn node_outputs_exact<const N: usize>(&self, node: NodeId) -> crate::Result<[ValueId; N]> {
        self.function().graph().node_outputs_exact(node)
    }

    /// Returns the [`crate::node::UseId`] of the input slot at position `idx`
    /// of `node`.
    ///
    /// # Errors
    ///
    /// Returns an error if `idx` is past the node's current input count.
    fn node_input_id_at(&self, node: NodeId, idx: usize) -> crate::Result<crate::node::UseId> {
        self.function().graph().node_input_id_at(node, idx)
    }

    /// Returns the [`ValueKind`](crate::node::ValueKind) of `value_id`.
    fn value_kind(&self, value_id: ValueId) -> crate::node::ValueKind {
        self.function().graph().value_kind(value_id)
    }

    /// Returns the [`NodeId`] that produces `value_id`.
    fn producer(&self, value_id: ValueId) -> NodeId {
        self.function().graph().producer(value_id)
    }

    /// Returns the `(NodeId, output_index)` pair that defines `value_id`.
    fn value_definition(&self, value_id: ValueId) -> (NodeId, u32) {
        self.function().graph().value_definition(value_id)
    }

    /// Returns the [`NodeKind`] of the node that produces `value_id`.
    fn kind_of_value(&self, value_id: ValueId) -> &NodeKind {
        let g = self.function().graph();
        g.node_kind(g.producer(value_id))
    }

    /// The integer-constant value carried by `value`, masked to its declared
    /// type and widened to `u128`, or `None` if `value` is not an integer
    /// constant. Single read SSoT for constant values — every consumer reads
    /// constants through this (or its `u64`/`i64` projections) so the storage
    /// representation stays encapsulated.
    ///
    /// Returns `None` for `IntConst(Wide(id))` nodes backed by `I256`/`I512`
    /// (their values don't fit in `u128`); returns `Some` for the
    /// `I80`/`I128`-backed `Wide` variants and all `Small` variants.
    fn int_const_u128(&self, value: ValueId) -> Option<u128> {
        use crate::node::IntPayload;
        let ty = self.value_kind(value).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        match *self.kind_of_value(value) {
            NodeKind::IntConst(IntPayload::Small(v)) => ty.get_unsigned_int(u128::from(v)),
            // Wide: read the interner.  I80/I128 fit in u128 (as_u128
            // returns Some); I256/I512 return None — too wide for this funnel.
            NodeKind::IntConst(IntPayload::Wide(id)) => self
                .function()
                .wide_const_opt(id)
                .and_then(|w| w.as_u128())
                .and_then(|v| ty.get_unsigned_int(v)),
            _ => None,
        }
    }

    /// Signed projection of [`Self::int_const_u128`]: the value sign-extended
    /// from its declared width to `i128`, or `None`.  Delegates the decode to
    /// [`Self::int_const_u128`] so the two stay in lock-step.
    fn int_const_i128(&self, value: ValueId) -> Option<i128> {
        let v = self.int_const_u128(value)?;
        self.value_kind(value).as_value()?.get_signed_int(v)
    }

    /// Signed projection narrowed to `i64`: the integer-constant value
    /// sign-extended from its declared width, or `None` if `value` is not an
    /// integer constant or the sign-extended value does not fit in `i64`. A
    /// narrowing projection of [`Self::int_const_i128`] (the signed read SSoT).
    ///
    /// Reads only a canonical `IntConst`: callers run after `ConstantFold`,
    /// which collapses `Neg(IntConst)` / `Truncate(IntConst)` /
    /// `Extend(IntConst)` to a single `IntConst`, so consumers never peel those
    /// wrappers themselves.
    fn int_const_i64(&self, value: ValueId) -> Option<i64> {
        i64::try_from(self.int_const_i128(value)?).ok()
    }

    /// Returns the integer constant value of `value` (masked to its declared
    /// type) narrowed to `u64`, or `None` if it is not an integer-constant
    /// value or its value does not fit in `u64`. A narrowing projection of
    /// [`Self::int_const_u128`] (the read SSoT) that discards values wider
    /// than `u64`.
    fn int_const_val(&self, value: ValueId) -> Option<u64> {
        self.int_const_u128(value)
            .and_then(|v| u64::try_from(v).ok())
    }

    /// Little-endian bytes of a WIDE-typed (`I80`/`I128`/`I256`/`I512`)
    /// integer-constant node — 10 / 16 / 32 / 64 bytes respectively —
    /// regardless of whether the payload is the inline `Small` form (a value
    /// that fits `u64`) or the interned `Wide` form.  The byte width is taken
    /// from the node's output type, so a small-valued wide constant
    /// (e.g. `IntConst(Small(5)):I128`) still yields its full 16-byte
    /// representation.
    ///
    /// Returns `None` for a narrow (≤ `I64`) constant — use
    /// [`Self::int_const_val`] / [`Self::int_const_u128`] there — or for a
    /// non-`IntConst` node / a node without a single value output.
    fn int_const_wide_le_bytes(&self, node: crate::node::NodeId) -> Option<Vec<u8>> {
        use crate::node::IntPayload;
        let [out] = self.node_outputs_exact::<1>(node).ok()?;
        let ty = self.value_kind(out).as_value()?;
        if !ty.is_wide_int() {
            return None;
        }
        let byte_size = ty.byte_size();
        match *self.node_kind(node) {
            NodeKind::IntConst(IntPayload::Wide(id)) => {
                Some(self.function().wide_const(id).to_le_bytes())
            }
            NodeKind::IntConst(IntPayload::Small(v)) => {
                let mut bytes = vec![0u8; byte_size];
                bytes[..8].copy_from_slice(&v.to_le_bytes());
                Some(bytes)
            }
            _ => None,
        }
    }

    /// Returns the boolean constant value of `value`, or `None` if it is not an
    /// `I1`-typed `IntConst`. Booleans are 1-bit integers, so this derives from
    /// [`Self::int_const_val`] (the read SSoT) under an `I1` guard.
    fn bool_const_val(&self, value: ValueId) -> Option<bool> {
        if !self.value_kind(value).is_bool() {
            return None;
        }
        self.int_const_val(value).map(|v| v != 0)
    }

    /// Returns the first [`ValueId`] of `node_id` whose kind is a value edge
    /// (`Typed(_)`), in output-slot order, or `None` if the node has no value
    /// output.
    fn first_value_output_of(&self, node_id: NodeId) -> Option<ValueId> {
        let g = self.function().graph();
        g.node_outputs(node_id)
            .iter()
            .copied()
            .find(|&value| g.value_kind(value).as_value().is_some())
    }

    /// Returns the single [`ValueId`] of `node_id` whose kind is
    /// [`crate::node::ValueKind::Memory`].
    ///
    /// # Errors
    ///
    /// Returns an error if `node_id` has no `Memory` output, or has more than
    /// one.
    fn memory_output_of(&self, node_id: NodeId) -> crate::Result<ValueId> {
        let g = self.function().graph();
        let mut found: Option<ValueId> = None;
        for &out in g.node_outputs(node_id) {
            if matches!(g.value_kind(out), crate::node::ValueKind::Memory) {
                if found.is_some() {
                    return Err(anyhow!("node {node_id:?} has more than one Memory output"));
                }
                found = Some(out);
            }
        }
        found.ok_or_else(|| anyhow!("node {node_id:?} has no Memory output"))
    }

    /// The incoming memory-token input of a memory-chain node, if any.  Slot 0
    /// for `Store` / `Load`; the call's memory input (slot 1) for `Call` /
    /// `CallOther`.  `None` for everything else — including `MemPhi` (whose
    /// slot 0 is the phi-token, not a memory input; its variadic memory
    /// predecessors are reached separately) and `InitialMemory` (the clean
    /// chain root, which has no incoming memory edge).
    fn memory_input_of(&self, node: NodeId) -> Option<ValueId> {
        let inputs = self.node_inputs(node);
        match *self.node_kind(node) {
            NodeKind::Store(_) | NodeKind::Load(_) => inputs.into_iter().next(),
            NodeKind::Call | NodeKind::CallOther { .. } => inputs.into_iter().nth(1),
            _ => None,
        }
    }

    /// Yields `(NodeId, &NodeKind)` for every node in the arena whose id is in
    /// `reachable`, in ascending-`NodeId` order.
    fn reachable_kind_iter<'a>(
        &'a self,
        reachable: &'a crate::walk::NodeIdSet,
    ) -> impl Iterator<Item = (NodeId, &'a NodeKind)> + 'a {
        // Iterate the reachable set directly (ascending NodeId order, sized to
        // the reachable set, not the zombie-bloated arena).
        let g = self.function().graph();
        reachable.iter().map(move |n| (n, g.node_kind(n)))
    }

    // ── read-only helpers ────────────────────────────────────────────────

    /// Retrieves the [`ValueType`] of `value_id`.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is a control, memory, or
    /// control-phi edge (i.e. not a value edge).
    fn value_type(&self, value_id: ValueId) -> crate::Result<ValueType> {
        let kind = self.function().graph().value_kind(value_id);
        kind.as_value()
            .ok_or_else(|| anyhow!("output {value_id:?} is not a value edge (got {kind:?})"))
    }

    /// Asserts that `value_id` already carries exactly `expected`, returning
    /// it unchanged on success.  The strict counterpart to the coercion
    /// helpers: the value-producing `build_*` constructors call it instead of
    /// silently truncating / extending / bit-casting an operand.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge, or when its
    /// type differs from `expected`.
    fn require_value_type(&self, value_id: ValueId, expected: ValueType) -> crate::Result<ValueId> {
        let actual = self.value_type(value_id)?;
        if actual != expected {
            return Err(anyhow!(
                "operand {value_id:?} has type {actual} but the operation \
                 requires {expected}; the caller must insert the truncate / \
                 extend / bitcast fix-up (builders no longer auto-coerce)"
            ));
        }
        Ok(value_id)
    }

    /// Errors unless `value_id` is a value edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a value edge.
    fn require_value_kind(&self, value_id: ValueId) -> crate::Result<()> {
        let kind = self.function().graph().value_kind(value_id);
        if !kind.is_value() {
            return Err(anyhow!(
                "output {value_id:?} is not a value edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Errors unless `value_id` carries a bool value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a bool value.
    fn require_bool_value(&self, value_id: ValueId) -> crate::Result<()> {
        if !self.function().graph().value_kind(value_id).is_bool() {
            return Err(anyhow!("output {value_id:?} is not a bool value"));
        }
        Ok(())
    }

    /// Errors unless `value_id` is a phi-token edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a phi-token edge.
    fn require_phi_token_kind(&self, value_id: ValueId) -> crate::Result<()> {
        if !self.function().graph().value_kind(value_id).is_phi_token() {
            return Err(anyhow!("output {value_id:?} is not a phi-token edge"));
        }
        Ok(())
    }

    /// Errors unless `value_id` is a control edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a control edge.
    fn require_control_kind(&self, value_id: ValueId) -> crate::Result<()> {
        let kind = self.function().graph().value_kind(value_id);
        if !kind.is_control() {
            return Err(anyhow!(
                "output {value_id:?} is not a control edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Errors unless `value_id` is a memory edge.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a memory edge.
    fn require_memory_kind(&self, value_id: ValueId) -> crate::Result<()> {
        let kind = self.function().graph().value_kind(value_id);
        if !kind.is_memory() {
            return Err(anyhow!(
                "output {value_id:?} is not a memory edge (got {kind:?})"
            ));
        }
        Ok(())
    }

    /// Errors unless `value_id` carries an integer value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not an integer value.
    fn require_integer_value(&self, value_id: ValueId) -> crate::Result<()> {
        if !self.value_type(value_id)?.is_integer() {
            return Err(anyhow!("output {value_id:?} is not an integer value"));
        }
        Ok(())
    }

    /// Errors unless `value_id` carries a float value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a float value.
    fn require_float_value(&self, value_id: ValueId) -> crate::Result<()> {
        if !self.value_type(value_id)?.is_float() {
            return Err(anyhow!("output {value_id:?} is not a float value"));
        }
        Ok(())
    }

    /// Errors unless `ty` is an integer type.
    ///
    /// # Errors
    /// Returns an error when `ty` is not an integer type.
    fn require_integer_type(ty: ValueType) -> crate::Result<()> {
        if !ty.is_integer() {
            return Err(anyhow!("type {ty:?} is not an integer type"));
        }
        Ok(())
    }

    /// Errors unless `ty` is a float type.
    ///
    /// # Errors
    /// Returns an error when `ty` is not a float type.
    fn require_float_type(ty: ValueType) -> crate::Result<()> {
        if !ty.is_float() {
            return Err(anyhow!("type {ty:?} is not a float type"));
        }
        Ok(())
    }

    /// Errors if any element of `inputs` is not a value edge.
    ///
    /// # Errors
    /// Returns an error when any input is not a value edge.
    fn validate_value_inputs(&self, inputs: &[ValueId]) -> crate::Result<()> {
        for &v in inputs {
            self.require_value_kind(v)?;
        }
        Ok(())
    }

    // ── constant inspection ──────────────────────────────────────────────

    /// Returns the constant value carried by `value_id` if its defining
    /// node is `IntConst` or `FloatConst`; `Ok(None)` otherwise.  The
    /// `get_as_*` helpers below are thin projections off this unified
    /// shape.  Booleans are `IntConst` values typed `I1`.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn const_value(&self, value_id: ValueId) -> crate::Result<Option<ConstValue>> {
        let ty = self.value_type(value_id)?;
        if ty.is_integer()
            && let Some(val) = self.int_const_u128(value_id)
        {
            return Ok(Some(ConstValue::Int { val, ty }));
        }
        Ok(match self.kind_of_value(value_id) {
            NodeKind::FloatConst(bits) if ty.is_float() => Some(ConstValue::Float { bits: *bits }),
            _ => None,
        })
    }

    /// If `value_id` is a constant node, returns its value truncated to the
    /// declared [`ValueType`] as an unsigned 64-bit integer.
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_unsigned_int(&self, value_id: ValueId) -> crate::Result<Option<u64>> {
        // Keep the value-edge type check (errors on a non-value edge), then
        // reuse the narrowing `u64` projection of `int_const_u128`.
        self.value_type(value_id)?;
        Ok(self.int_const_val(value_id))
    }

    /// If `value_id` is an integer constant, returns its value
    /// sign-extended to `i64` according to the declared [`ValueType`].
    /// An `I1` boolean folds as `0` / `1` per [`Self::get_as_unsigned_int`].
    ///
    /// Returns `Ok(None)` for non-constant nodes.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_signed_int(&self, value_id: ValueId) -> crate::Result<Option<i64>> {
        self.value_type(value_id)?;
        Ok(self
            .int_const_i128(value_id)
            .and_then(|v| i64::try_from(v).ok()))
    }

    /// Returns both the unsigned and signed interpretations of `value_id` if
    /// it is an integer constant, or `None` otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error when `value_id` is not a value edge.
    fn get_as_int(&self, value_id: ValueId) -> crate::Result<Option<(u64, i64)>> {
        Ok(self
            .get_as_unsigned_int(value_id)?
            .zip(self.get_as_signed_int(value_id)?))
    }

    /// Infers the float type to use for a value that may be int or float.
    /// If the value is already a float type, that type is used.
    /// For integers, maps byte size: ≤4 → F32, =8 → F64, =10 → F80.
    ///
    /// The 10-byte case targets x87 ST0/STn registers (which the analyzer
    /// represents as I80 on the int side); inferring F80 keeps the
    /// int→float bit-reinterpret round-trip width-preserving.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is not a value edge, or for an integer
    /// input whose byte size has no corresponding float type (5, 6, 7, 16,
    /// 32, 64).
    fn infer_float_type(&self, value: ValueId) -> crate::Result<ValueType> {
        let ty = self.value_type(value)?;
        if ty.is_float() {
            return Ok(ty);
        }
        match ty.byte_size() {
            0..=4 => Ok(ValueType::F32),
            8 => Ok(ValueType::F64),
            10 => Ok(ValueType::F80),
            other => Err(anyhow!(
                "infer_float_type: integer byte_size {other} has no corresponding \
                 float type (input type: {ty:?})"
            )),
        }
    }
}

/// [`Function`] is the canonical viewer: it IS the function under view.
impl IRViewer for Function {
    #[inline]
    fn function(&self) -> &Function {
        self
    }
}

/// The lift builder views the [`Function`] it owns.  Returns the wrapped
/// field directly (never `self.function()`, which would recurse into this
/// very method).
impl IRViewer for crate::FunctionBuilder {
    #[inline]
    fn function(&self) -> &Function {
        &self.function
    }
}

/// The editing context views the [`Function`] it borrows mutably.  Returns
/// the wrapped field directly (reborrowed as `&`); never `self.function()`,
/// which would recurse into this very method.
impl IRViewer for crate::EditFunction<'_> {
    #[inline]
    fn function(&self) -> &Function {
        &*self.function
    }
}

/// Control-aware graph walks layered over [`IRViewer`]: the single source of
/// truth for traversing a function's IR graph.
///
/// Every viewer gains the entry-rooted pre-order ([`Self::walk`]) plus the
/// kind-filtered / post-order family — all over `self.function().graph()`.
/// [`Function`] uses these defaults directly; [`crate::EditFunction`] shadows
/// the order-producing ones with inherent versions that reuse its cached
/// live/roots bookkeeping instead of re-walking from entry.
///
/// The "global" orders ([`Self::postorder`] / [`Self::reverse_postorder`])
/// take a [`crate::walk::GraphWalkInfo`] — compute it once via
/// [`Self::walk_info`] and hand it to whichever orders you need without
/// re-walking, e.g. `let info = walker.walk_info(None)?; walker.postorder(&info)`.
pub trait IRWalker: IRViewer {
    /// Resolves the seed (`None` ⇒ the function's entry, [`Function::entry`];
    /// `Some(n)` ⇒ `n`) and computes the [`crate::walk::GraphWalkInfo`] — the
    /// reachable set + input-less roots the post-order family consumes.
    /// Returns `None` when the seed resolves to no node (entry-less function,
    /// `None` seed).
    fn walk_info(&self, seed: Option<NodeId>) -> Option<crate::walk::GraphWalkInfo> {
        let f = self.function();
        seed.or_else(|| f.entry())
            .map(|s| crate::walk::GraphWalkInfo::compute_full(f.graph(), s))
    }

    /// Returns a pre-order walk over every node reachable from the function's
    /// entry (control-out forward + data-in backward).  Yields an empty walk
    /// when the entry has not been set.
    fn walk(&self) -> crate::walk::GraphWalk<'_> {
        let f = self.function();
        crate::walk::walk_graph_opt(f.graph(), f.entry())
    }

    /// Pre-order walk seeded at `seed` (control-out forward + data-in
    /// backward) — the explicit-seed counterpart to [`Self::walk`].
    fn walk_from(&self, seed: NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph_opt(self.function().graph(), Some(seed))
    }

    /// [`Self::walk`] restricted to nodes whose [`NodeKind`] satisfies `pred`.
    fn walk_kind<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.walk().filter(move |&n| pred(self.node_kind(n)))
    }

    /// Counts entry-reachable nodes whose [`NodeKind`] satisfies `pred`.
    fn count_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> usize {
        self.walk().filter(|&n| pred(self.node_kind(n))).count()
    }

    /// Returns `true` when at least one entry-reachable node satisfies `pred`.
    /// Short-circuits at the first match.
    fn has_kind(&self, pred: impl Fn(&NodeKind) -> bool) -> bool {
        self.walk().any(|n| pred(self.node_kind(n)))
    }

    /// Post-order (consumers before operands; roots last) of the reachable set
    /// captured by `info` — obtain `info` from [`Self::walk_info`].
    fn postorder(&self, info: &crate::walk::GraphWalkInfo) -> Vec<NodeId> {
        info.postorder(self.function().graph()).collect()
    }

    /// Real reverse-post-order (every producer before its consumers, roots
    /// first) of the reachable set captured by `info` — obtain `info` from
    /// [`Self::walk_info`].
    fn reverse_postorder(&self, info: &crate::walk::GraphWalkInfo) -> Vec<NodeId> {
        info.reverse_postorder(self.function().graph())
    }

    /// Entry-reachable nodes in **global reverse-post-order** (entry-first;
    /// every producer before its consumers), filtered by `pred`.  Yields an
    /// empty iterator when the entry has not been set.
    fn reverse_postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        let rpo = match self.walk_info(None) {
            Some(info) => self.reverse_postorder(&info),
            None => Vec::new(),
        };
        rpo.into_iter().filter(move |&n| pred(self.node_kind(n)))
    }

    /// Entry-reachable nodes in **global post-order** (consumers before
    /// operands; entry last), filtered by `pred` — the post-order counterpart
    /// of [`Self::reverse_postorder_filter`].  Yields an empty iterator when the entry has
    /// not been set.
    fn postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        let po = match self.walk_info(None) {
            Some(info) => self.postorder(&info),
            None => Vec::new(),
        };
        po.into_iter().filter(move |&n| pred(self.node_kind(n)))
    }
}

impl<T: IRViewer + ?Sized> IRWalker for T {}
