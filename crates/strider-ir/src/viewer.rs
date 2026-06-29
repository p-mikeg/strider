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
//! `reverse_postorder_filter` / `reverse_postorder`, …) on top of [`IRViewer`],
//! delegating to the crate's `walk` primitives over
//! `self.function().graph()`.  It is the single source of truth for
//! traversing a function's IR graph; [`crate::EditFunction`] shadows the
//! order-producing methods with inherent versions that reuse its cached
//! live/roots bookkeeping.

use anyhow::anyhow;

use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

/// Generates the `require_*` edge-kind guards on [`IRViewer`]: each errors
/// unless `value_id`'s [`ValueKind`] satisfies the named predicate.  Doc
/// attributes are forwarded, so every generated method keeps its own docs.
macro_rules! value_kind_requirements {
    ($($(#[$m:meta])* $name:ident => $pred:ident, $noun:literal;)+) => { $(
        $(#[$m])*
        fn $name(&self, value_id: ValueId) -> crate::Result<()> {
            let kind = self.function().graph().value_kind(value_id);
            if kind.$pred() {
                Ok(())
            } else {
                Err(anyhow!("output {value_id:?} is not {} (got {kind:?})", $noun))
            }
        }
    )+ };
}

/// Generates the named operand reads on [`IRViewer`]: each returns the input
/// `value` at a fixed slot of a node, panicking on the arity the validator
/// already guarantees.  Doc attributes are forwarded per method.
macro_rules! semantic_slot_accessors {
    ($($(#[$m:meta])* $name:ident => $arity:literal [$slot:literal] $msg:literal;)+) => { $(
        $(#[$m])*
        fn $name(&self, node: NodeId) -> ValueId {
            self.node_inputs_exact::<$arity>(node).expect($msg)[$slot]
        }
    )+ };
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

    /// Returns the exactly-`N` input value edges of the node that *produces*
    /// `value` — the value-keyed form of [`Self::node_inputs_exact`], saving
    /// the `node_inputs_exact(self.producer(value))` two-step at call sites.
    ///
    /// # Errors
    /// Returns an error if the producing node does not have exactly `N` inputs.
    fn producer_inputs_exact<const N: usize>(&self, value: ValueId) -> crate::Result<[ValueId; N]> {
        self.node_inputs_exact::<N>(self.producer(value))
    }

    /// Returns the exactly-`N` output value edges of `node`.
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly `N` outputs.
    fn node_outputs_exact<const N: usize>(&self, node: NodeId) -> crate::Result<[ValueId; N]> {
        self.function().graph().node_outputs_exact(node)
    }

    /// Returns the single value output of `node` together with its
    /// [`ValueType`] — the common "this node produces exactly one typed value"
    /// shape, saving the `node_outputs_exact::<1>` + `value_type` two-step at
    /// call sites.
    ///
    /// # Errors
    /// Returns an error if the node does not have exactly one output, or that
    /// output is not a typed value edge.
    fn single_value_output(&self, node: NodeId) -> crate::Result<(ValueId, ValueType)> {
        let [value] = self.node_outputs_exact::<1>(node)?;
        let ty = self.value_type(value)?;
        Ok((value, ty))
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

    /// The [`ValueType`] of `value_id` if it carries a typed value, or `None`
    /// for a `Control` / `Memory` / `PhiToken` edge.  The `Option`-returning
    /// counterpart to [`Self::value_type`] (which errors on a non-value edge);
    /// use this when "no type" is an expected, non-error outcome.
    fn value_type_opt(&self, value_id: ValueId) -> Option<ValueType> {
        self.value_kind(value_id).as_value()
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
    /// constants through this (or its signed `i128` projection
    /// [`Self::int_const_i128`]) so the storage representation stays
    /// encapsulated.
    ///
    /// Returns `None` for `IntConst` nodes whose interned value exceeds 128
    /// bits (`I256`/`I512` values that don't fit `u128`); returns `Some` for
    /// every value that fits `u128`, masked to the declared width.
    fn int_const_u128(&self, value: ValueId) -> Option<u128> {
        let ty = self.value_kind(value).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        let NodeKind::IntConst(id) = *self.kind_of_value(value) else {
            return None;
        };
        let v = self.function().const_value(id).fits_u128()?;
        Some(v & ty.bit_mask_u128())
    }

    /// Signed projection of [`Self::int_const_u128`]: the value sign-extended
    /// from its declared width to `i128`, or `None`.  Delegates the decode to
    /// [`Self::int_const_u128`] so the two stay in lock-step.
    fn int_const_i128(&self, value: ValueId) -> Option<i128> {
        let v = self.int_const_u128(value)?;
        self.value_kind(value).as_value()?.get_signed_int(v)
    }

    /// Little-endian bytes of a WIDE-typed (`I80`/`I128`/`I256`/`I512`)
    /// integer-constant node — 10 / 16 / 32 / 64 bytes respectively.  The byte
    /// width is taken from the node's output type, so a small-valued wide
    /// constant (e.g. `IntConst(5):I128`) still yields its full 16-byte
    /// representation.
    ///
    /// Returns `None` for a narrow (≤ `I64`) constant — use
    /// [`Self::int_const_u128`] there — or for a
    /// non-`IntConst` node / a node without a single value output.
    fn int_const_wide_le_bytes(&self, node: crate::node::NodeId) -> Option<Vec<u8>> {
        let [out] = self.node_outputs_exact::<1>(node).ok()?;
        let ty = self.value_kind(out).as_value()?;
        if !ty.is_wide_int() {
            return None;
        }
        let NodeKind::IntConst(id) = *self.node_kind(node) else {
            return None;
        };
        Some(self.function().const_value(id).to_le_bytes(ty.byte_size()))
    }

    /// Returns the boolean constant value of `value`, or `None` if it is not an
    /// `I1`-typed `IntConst`. Booleans are 1-bit integers, so this derives from
    /// [`Self::int_const_u128`] (the read SSoT) under an `I1` guard.
    fn bool_const_val(&self, value: ValueId) -> Option<bool> {
        if !self.value_kind(value).is_bool() {
            return None;
        }
        self.int_const_u128(value).map(|v| v != 0)
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

    // ── semantic-slot accessors ──────────────────────────────────────────
    //
    // Named operand reads keyed off the node already in hand, deriving the
    // fixed input slot (per `node_signature`) so consumers never re-encode the
    // positional index `node_inputs_exact::<N>(n)[k]` at each call site. Each
    // panics on the arity invariant the validator already guarantees for a
    // well-formed node of that kind.

    semantic_slot_accessors! {
        /// The condition value of an `If` node — input slot 1 (`[control, cond]`).
        ///
        /// # Panics
        /// Panics if `node` does not have the `If` input arity (2 inputs); a
        /// validator-guaranteed invariant for a well-formed `If`.
        if_cond => 2[1] "If node has [control, cond] inputs";

        /// The dispatch value of an `IndirectBranch` node — input slot 2
        /// (`[control, memory, target]`).
        ///
        /// # Panics
        /// Panics if `node` does not have the `IndirectBranch` input arity (3
        /// inputs); a validator-guaranteed invariant for a well-formed node.
        indirect_branch_target => 3[2] "IndirectBranch node has [control, memory, target] inputs";

        /// The address operand of a `Store` node — input slot 1
        /// (`[memory, addr, data]`).
        ///
        /// # Panics
        /// Panics if `node` does not have the `Store` input arity (3 inputs); a
        /// validator-guaranteed invariant for a well-formed `Store`.
        store_addr => 3[1] "Store node has [memory, addr, data] inputs";

        /// The data operand of a `Store` node — input slot 2
        /// (`[memory, addr, data]`).
        ///
        /// # Panics
        /// Panics if `node` does not have the `Store` input arity (3 inputs); a
        /// validator-guaranteed invariant for a well-formed `Store`.
        store_data => 3[2] "Store node has [memory, addr, data] inputs";

        /// The address operand of a `Load` node — input slot 1 (`[memory, addr]`).
        ///
        /// # Panics
        /// Panics if `node` does not have the `Load` input arity (2 inputs); a
        /// validator-guaranteed invariant for a well-formed `Load`.
        load_addr => 2[1] "Load node has [memory, addr] inputs";
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

    value_kind_requirements! {
        /// Errors unless `value_id` is a value edge.
        ///
        /// # Errors
        /// Returns an error when `value_id` is not a value edge.
        require_value_kind => is_value, "a value edge";

        /// Errors unless `value_id` carries a bool value.
        ///
        /// # Errors
        /// Returns an error when `value_id` is not a bool value.
        require_bool_value => is_bool, "a bool value";

        /// Errors unless `value_id` is a phi-token edge.
        ///
        /// # Errors
        /// Returns an error when `value_id` is not a phi-token edge.
        require_phi_token_kind => is_phi_token, "a phi-token edge";

        /// Errors unless `value_id` is a control edge.
        ///
        /// # Errors
        /// Returns an error when `value_id` is not a control edge.
        require_control_kind => is_control, "a control edge";

        /// Errors unless `value_id` is a memory edge.
        ///
        /// # Errors
        /// Returns an error when `value_id` is not a memory edge.
        require_memory_kind => is_memory, "a memory edge";
    }

    /// Errors unless `value_id` carries an integer value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not an integer value.
    fn require_integer_value(&self, value_id: ValueId) -> crate::Result<()> {
        ensure_value_type(
            value_id,
            self.value_type(value_id)?.is_integer(),
            "an integer value",
        )
    }

    /// Errors unless `value_id` carries a float value.
    ///
    /// # Errors
    /// Returns an error when `value_id` is not a float value.
    fn require_float_value(&self, value_id: ValueId) -> crate::Result<()> {
        ensure_value_type(
            value_id,
            self.value_type(value_id)?.is_float(),
            "a float value",
        )
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
/// The "global" order [`Self::reverse_postorder`] takes a
/// [`crate::walk::GraphWalkInfo`] — compute it once via
/// [`Self::walk_info`] and hand it to whichever orders you need without
/// re-walking, e.g. `let info = walker.walk_info(None); walker.reverse_postorder(&info)`.
pub trait IRWalker: IRViewer {
    /// Resolves the seed (`None` ⇒ the function's entry, [`Function::entry`];
    /// `Some(n)` ⇒ `n`) and computes the [`crate::walk::GraphWalkInfo`] — the
    /// reachable set + input-less roots the post-order family consumes.
    fn walk_info(&self, seed: Option<NodeId>) -> crate::walk::GraphWalkInfo {
        let f = self.function();
        let s = seed.unwrap_or_else(|| f.entry());
        crate::walk::GraphWalkInfo::compute_full(f.graph(), s)
    }

    /// Returns a pre-order walk over every node reachable from the function's
    /// entry (control-out forward + data-in backward).
    fn walk(&self) -> crate::walk::GraphWalk<'_> {
        let f = self.function();
        crate::walk::walk_graph(f.graph(), f.entry())
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
        let info = self.walk_info(None);
        let rpo = self.reverse_postorder(&info);
        rpo.into_iter().filter(move |&n| pred(self.node_kind(n)))
    }
}

impl<T: IRViewer + ?Sized> IRWalker for T {}

/// Shared body for the `require_*_value` value-type checks (no kind to report).
fn ensure_value_type(value_id: ValueId, ok: bool, noun: &str) -> crate::Result<()> {
    if ok {
        Ok(())
    } else {
        Err(anyhow!("output {value_id:?} is not {noun}"))
    }
}
