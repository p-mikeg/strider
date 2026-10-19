use anyhow::anyhow;

use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

/// Each generated method reads a fixed input slot, panicking on the arity the
/// validator already guarantees.
macro_rules! semantic_slot_accessors {
    ($($(#[$m:meta])* $name:ident => $arity:literal [$slot:expr] $msg:literal;)+) => { $(
        $(#[$m])*
        fn $name(&self, node: NodeId) -> ValueId {
            self.node_inputs_exact::<$arity>(node).expect($msg)[$slot]
        }
    )+ };
}

/// Point reads over a [`Function`].
///
/// The checks a builder runs on its operands are its own, not reads:
///
/// ```compile_fail,E0599
/// use strider_ir::node::ValueId;
/// use strider_ir::{Function, IRViewer};
///
/// fn check(f: &Function, v: ValueId) -> strider_ir::Result<()> {
///     f.require_value_kind(v)
/// }
/// ```
pub trait IRViewer {
    fn function(&self) -> &Function;

    fn node_kind(&self, node: NodeId) -> &NodeKind {
        self.function().graph().node_kind(node)
    }

    fn node_inputs(&self, node: NodeId) -> crate::Inputs<'_> {
        self.function().graph().node_inputs(node)
    }

    /// Every input of a `Phi` / `MemPhi` except the structural `PhiToken`,
    /// leaving one data input per predecessor.
    fn phi_data_inputs(&self, phi: NodeId) -> impl Iterator<Item = ValueId> + '_ {
        let g = self.function().graph();
        g.node_inputs(phi)
            .into_iter()
            .filter(move |&v| g.value_kind(v) != crate::node::ValueKind::PhiToken)
    }

    fn node_outputs(&self, node: NodeId) -> &[ValueId] {
        self.function().graph().node_outputs(node)
    }

    fn node_inputs_exact<const N: usize>(&self, node: NodeId) -> crate::Result<[ValueId; N]> {
        self.function().graph().node_inputs_exact(node)
    }

    /// Value-keyed [`Self::node_inputs_exact`].
    fn producer_inputs_exact<const N: usize>(&self, value: ValueId) -> crate::Result<[ValueId; N]> {
        self.node_inputs_exact::<N>(self.producer(value))
    }

    /// Data operands only: the `Control` / `Memory` / `PhiToken` edges are
    /// dropped.
    fn value_inputs(&self, node: NodeId) -> impl Iterator<Item = ValueId> + '_ {
        self.node_inputs(node)
            .into_iter()
            .filter(move |&i| self.value_type_opt(i).is_some())
    }

    /// Value-keyed, integer-only [`Self::value_inputs`].
    fn int_inputs(&self, value: ValueId) -> impl Iterator<Item = ValueId> + '_ {
        self.node_inputs(self.producer(value))
            .into_iter()
            .filter(move |&i| self.value_type_opt(i).is_some_and(|t| t.is_integer()))
    }

    fn node_outputs_exact<const N: usize>(&self, node: NodeId) -> crate::Result<[ValueId; N]> {
        self.function().graph().node_outputs_exact(node)
    }

    fn single_value_output(&self, node: NodeId) -> crate::Result<(ValueId, ValueType)> {
        let [value] = self.node_outputs_exact::<1>(node)?;
        let ty = self.value_type(value)?;
        Ok((value, ty))
    }

    fn node_input_id_at(&self, node: NodeId, idx: usize) -> crate::Result<crate::node::UseId> {
        self.function().graph().node_input_id_at(node, idx)
    }

    /// The value on input slot `idx`, or `None` when the node is shorter.
    fn nth_input(&self, node: NodeId, idx: usize) -> Option<ValueId> {
        self.function().graph().nth_input(node, idx)
    }

    fn value_uses(&self, value_id: ValueId) -> impl Iterator<Item = (NodeId, u32)> + '_ {
        self.function().graph().value_uses(value_id)
    }

    /// O(1): stops after two steps rather than counting the whole use-list.
    fn value_has_one_use(&self, value_id: ValueId) -> bool {
        self.function().graph().value_has_one_use(value_id)
    }

    fn value_kind(&self, value_id: ValueId) -> crate::node::ValueKind {
        self.function().graph().value_kind(value_id)
    }

    /// `Option`-returning [`Self::value_type`].
    fn value_type_opt(&self, value_id: ValueId) -> Option<ValueType> {
        self.value_kind(value_id).as_value()
    }

    fn producer(&self, value_id: ValueId) -> NodeId {
        self.function().graph().producer(value_id)
    }

    fn value_definition(&self, value_id: ValueId) -> (NodeId, u32) {
        self.function().graph().value_definition(value_id)
    }

    fn kind_of_value(&self, value_id: ValueId) -> &NodeKind {
        let g = self.function().graph();
        g.node_kind(g.producer(value_id))
    }

    /// The constant `value` holds, masked to its declared width. `None` for a
    /// non-constant, or for an `I256`/`I512` value exceeding `u128`.
    fn int_const_u128(&self, value: ValueId) -> Option<u128> {
        let ty = self.value_kind(value).as_value()?;
        if !ty.is_integer() {
            return None;
        }
        let NodeKind::IntConst(id) = *self.kind_of_value(value) else {
            return None;
        };
        let v = self.function().const_value(id).fits_u128()?;
        Some(ty.wrap_u128(v))
    }

    /// [`Self::int_const_u128`] sign-extended from its declared width.
    ///
    /// `None` above 128 bits, where there is no signed carrier: an `I256` or
    /// `I512` constant that [`Self::int_const_u128`] reads back still answers
    /// `None` here.
    fn int_const_i128(&self, value: ValueId) -> Option<i128> {
        let v = self.int_const_u128(value)?;
        self.value_kind(value).as_value()?.get_signed_int(v)
    }

    /// Little-endian bytes of a constant whose declared type is wider than
    /// `u64`, zero-extended to that type's byte size. `None` for a narrower
    /// declared type, whatever the value.
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

    /// [`Self::int_const_u128`] under an `I1` guard.
    fn bool_const_val(&self, value: ValueId) -> Option<bool> {
        if !self.value_kind(value).is_bool() {
            return None;
        }
        self.int_const_u128(value).map(|v| v != 0)
    }

    fn first_value_output_of(&self, node_id: NodeId) -> Option<ValueId> {
        let g = self.function().graph();
        g.node_outputs(node_id)
            .iter()
            .copied()
            .find(|&value| g.value_kind(value).as_value().is_some())
    }

    /// Errors when `node_id` has no `Memory` output, or more than one.
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

    /// The incoming memory token of a memory-chain node: `None` for every kind
    /// but `Store` / `Load` / `Call` / `CallOther`. The exit sinks (`Return`,
    /// `IndirectBranch`, `Unreachable`) take a memory token at
    /// [`NodeKind::CTRL_MEM_SLOT`] but produce none, so no backward walk off a
    /// memory value reaches them.
    fn memory_input_of(&self, node: NodeId) -> Option<ValueId> {
        let inputs = self.node_inputs(node);
        match *self.node_kind(node) {
            NodeKind::Store(_) | NodeKind::Load(_) => {
                inputs.get(NodeKind::ACCESS_MEM_SLOT).copied()
            }
            NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                inputs.get(NodeKind::CTRL_MEM_SLOT).copied()
            }
            _ => None,
        }
    }

    // Each panics on an arity the validator guarantees for a well-formed node
    // of that kind.
    semantic_slot_accessors! {
        /// `If` input [`NodeKind::IF_COND_SLOT`] of `[control, cond]`.
        if_cond => 2[NodeKind::IF_COND_SLOT] "If node has [control, cond] inputs";

        /// `Store` input [`NodeKind::ACCESS_ADDR_SLOT`] of `[memory, addr, data]`.
        store_addr => 3[NodeKind::ACCESS_ADDR_SLOT] "Store node has [memory, addr, data] inputs";

        /// `Store` input [`NodeKind::STORE_DATA_SLOT`] of `[memory, addr, data]`.
        store_data => 3[NodeKind::STORE_DATA_SLOT] "Store node has [memory, addr, data] inputs";

        /// `Load` input [`NodeKind::ACCESS_ADDR_SLOT`] of `[memory, addr]`.
        load_addr => 2[NodeKind::ACCESS_ADDR_SLOT] "Load node has [memory, addr] inputs";
    }

    /// The address of a `Load` / `Store`; `None` for any other kind, and for a
    /// short input list, since whole-graph sweeps reach malformed and dead
    /// nodes.
    fn access_address(&self, node: NodeId) -> Option<ValueId> {
        if !matches!(self.node_kind(node), NodeKind::Load(_) | NodeKind::Store(_)) {
            return None;
        }
        self.node_inputs(node)
            .get(NodeKind::ACCESS_ADDR_SLOT)
            .copied()
    }

    /// `IndirectBranch` dispatch value, input
    /// [`NodeKind::INDIRECT_BRANCH_TARGET_SLOT`] of `[control, memory, target,
    /// isa_mode?]` (arity 3 or 4).
    fn indirect_branch_target(&self, node: NodeId) -> ValueId {
        assert!(
            matches!(self.node_kind(node), NodeKind::IndirectBranch),
            "indirect_branch_target on a non-IndirectBranch node"
        );
        self.node_inputs(node)
            .get(NodeKind::INDIRECT_BRANCH_TARGET_SLOT)
            .copied()
            .expect("IndirectBranch has a [control, memory, target] head")
    }

    /// The interworking ISA-mode value the `IndirectBranch`'s instruction
    /// commits for its target(s) (the optional input past the head), or `None`
    /// for a non-switching branch. Carried as a live input so the optimizer
    /// maintains its cone; the resolver mode-tags each resolved target off it,
    /// arch-agnostically.
    fn indirect_branch_isa_mode(&self, node: NodeId) -> Option<ValueId> {
        assert!(
            matches!(self.node_kind(node), NodeKind::IndirectBranch),
            "indirect_branch_isa_mode on a non-IndirectBranch node"
        );
        self.node_inputs(node)
            .get(NodeKind::IndirectBranch.input_head_len())
            .copied()
    }

    /// The ISA-mode value a `Switch`'s instruction commits for its arms (the
    /// optional input past the head), as [`Self::indirect_branch_isa_mode`] is
    /// for the placeholder it replaced.
    fn switch_isa_mode(&self, node: NodeId) -> Option<ValueId> {
        assert!(
            matches!(self.node_kind(node), NodeKind::Switch(_)),
            "switch_isa_mode on a non-Switch node"
        );
        self.node_inputs(node)
            .get(self.node_kind(node).input_head_len())
            .copied()
    }

    /// `Switch` dispatch value, input [`NodeKind::SWITCH_SELECTOR_SLOT`] of
    /// `[control, selector, isa_mode?]`.
    fn switch_selector(&self, node: NodeId) -> ValueId {
        assert!(
            matches!(self.node_kind(node), NodeKind::Switch(_)),
            "switch_selector on a non-Switch node"
        );
        self.node_inputs(node)
            .get(NodeKind::SWITCH_SELECTOR_SLOT)
            .copied()
            .expect("Switch has a [control, selector] head")
    }

    /// Ascending-`NodeId` order.
    fn reachable_kind_iter<'a>(
        &'a self,
        reachable: &'a crate::walk::NodeIdSet,
    ) -> impl Iterator<Item = (NodeId, &'a NodeKind)> + 'a {
        let g = self.function().graph();
        reachable.iter().map(move |n| (n, g.node_kind(n)))
    }

    /// Errors on a control, memory, or phi-token edge.
    fn value_type(&self, value_id: ValueId) -> crate::Result<ValueType> {
        let kind = self.function().graph().value_kind(value_id);
        kind.as_value()
            .ok_or_else(|| anyhow!("output {value_id:?} is not a value edge (got {kind:?})"))
    }

    /// Float values keep their type; integers map to the float of the same
    /// byte size, so the result is always bitcastable from `value`. Errors on a
    /// byte size with no float counterpart.
    fn infer_float_type(&self, value: ValueId) -> crate::Result<ValueType> {
        let ty = self.value_type(value)?;
        if ty.is_float() {
            return Ok(ty);
        }
        // `byte_size()` is at most 64.
        #[allow(clippy::cast_possible_truncation)]
        ValueType::float_from_bytes(ty.byte_size() as u32)
            .map_err(|e| anyhow!("infer_float_type of {ty}: {e}"))
    }
}

impl IRViewer for Function {
    #[inline]
    fn function(&self) -> &Function {
        self
    }
}

impl IRViewer for crate::FunctionBuilder {
    #[inline]
    fn function(&self) -> &Function {
        &self.function
    }
}

impl IRViewer for crate::EditFunction<'_> {
    #[inline]
    fn function(&self) -> &Function {
        &*self.function
    }
}

/// Control-aware traversals of a function's IR graph.
pub trait IRWalker: IRViewer {
    /// `None` seeds from the function entry. The result carries the reachable
    /// set and input-less roots the post-order family consumes.
    fn walk_info(&self, seed: Option<NodeId>) -> crate::walk::GraphWalkInfo {
        let f = self.function();
        let s = seed.unwrap_or_else(|| f.entry());
        crate::walk::GraphWalkInfo::compute_full(f.graph(), s)
    }

    /// Pre-order over everything reachable from entry, following control-out
    /// forward and data-in backward.
    fn walk(&self) -> crate::walk::GraphWalk<'_> {
        let f = self.function();
        crate::walk::walk_graph(f.graph(), f.entry())
    }

    /// [`Self::walk`] seeded from `seed` instead of entry.
    fn walk_from(&self, seed: NodeId) -> crate::walk::GraphWalk<'_> {
        crate::walk::walk_graph(self.function().graph(), seed)
    }

    fn walk_kind<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        self.walk().filter(move |&n| pred(self.node_kind(n)))
    }

    /// Every producer before its consumers unless the use closes a
    /// def-use cycle, a root first, over the reachable set `info` captured.
    fn reverse_postorder(&self, info: &crate::walk::GraphWalkInfo) -> Vec<NodeId> {
        info.reverse_postorder(self.function().graph())
    }

    /// [`Self::reverse_postorder`] from entry, filtered by `pred`. Empty only
    /// when `pred` matches nothing reachable from entry.
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

#[cfg(test)]
mod tests {
    //! Fixtures are built inline: `strider_ir_test_utils` links a separate
    //! compilation of strider-ir, so its `Function` is a different type here.

    use crate::builder::IRBuilderExt;
    use crate::node::ValueType;
    use crate::{FunctionBuilder, IRViewer, IRWalker, IntBinaryOp};
    use strider_ir_test_utils::SENTINEL_LIFT_ADDR;

    /// `Entry -> Region -> Return(Add(1, 2))`: the Add and its two constants
    /// form a data cone off the control spine.
    #[test]
    fn walk_from_seed_visits_only_the_seed_cone() {
        let cc = strider_target::BuiltCallingConvention::default();
        let mut b = FunctionBuilder::new(Vec::new(), cc, strider_target::Endianness::Little)
            .expect("FunctionBuilder::new");
        let region = b.create_region_all().expect("create_region");
        b.set_entry_region_all(region).expect("set_entry_region");
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let one = b.build_int_const(1u64, ValueType::int(64)).unwrap();
        let two = b.build_int_const(2u64, ValueType::int(64)).unwrap();
        let add = b
            .build_int_binary_operation(one, two, IntBinaryOp::Add, ValueType::int(64))
            .unwrap();
        let add_node = b.producer(add);
        b.build_return(Some(add), &[]).unwrap();
        b.set_lift_addr(None);
        let f = b.build().unwrap();

        let entry = f.entry();
        let all: Vec<_> = f.walk().collect();
        let mid = *all.iter().find(|&&n| n != entry).unwrap();
        let from_mid: std::collections::HashSet<_> = f.walk_from(mid).collect();
        assert!(from_mid.contains(&mid));
        let all_set: std::collections::HashSet<_> = all.into_iter().collect();
        assert!(from_mid.is_subset(&all_set));

        // Seeding at the Add reaches its data cone and nothing else: not the
        // Return consumer, not the spine.
        let from_add: std::collections::HashSet<_> = f.walk_from(add_node).collect();
        assert!(from_add.contains(&add_node));
        assert!(from_add.len() < all_set.len());
        assert!(from_add.is_subset(&all_set));
    }

    /// A `Load` short of its address is no access, not a panic: whole-graph
    /// sweeps reach malformed nodes.
    #[test]
    fn access_address_of_a_short_load_is_none() {
        use crate::node::{NodeKind, ValueKind};
        let mut f = crate::function::test_function();
        let mem = crate::function::test_initial_memory(&f);
        let [mem_value] = f.node_outputs_exact::<1>(mem).unwrap();
        let load = f.graph_mut().create_node(
            NodeKind::Load(rsleigh::VnSpace::RAM),
            [mem_value],
            [ValueKind::Typed(ValueType::int(64))],
        );
        assert_eq!(f.access_address(load), None);
    }
}
