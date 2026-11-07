use anyhow::anyhow;

use crate::function::Function;
use crate::node::{NodeId, NodeKind, ValueId, ValueType};

/// Fixed-slot readers that panic on a malformed arity.
macro_rules! semantic_slot_accessors {
    ($($(#[$m:meta])* $name:ident => $arity:literal [$slot:expr] $msg:literal;)+) => { $(
        $(#[$m])*
        fn $name(&self, node: NodeId) -> ValueId {
            self.node_inputs_exact::<$arity>(node).expect($msg)[$slot]
        }
    )+ };
}

/// Point reads over a [`Function`].
pub trait IRViewer {
    fn function(&self) -> &Function;

    fn node_kind(&self, node: NodeId) -> &NodeKind {
        self.function().graph().node_kind(node)
    }

    fn node_inputs(&self, node: NodeId) -> crate::Inputs<'_> {
        self.function().graph().node_inputs(node)
    }

    /// A `Phi` / `MemPhi`'s inputs less the `PhiToken`: one per predecessor.
    fn phi_data_inputs(&self, phi: NodeId) -> impl Iterator<Item = ValueId> + '_ {
        let g = self.function().graph();
        g.node_inputs(phi)
            .into_iter()
            .filter(move |&v| g.value_kind(v) != crate::node::ValueKind::PhiToken)
    }

    /// The `Region` a `Phi` / `MemPhi` joins at.
    fn phi_region(&self, phi: NodeId) -> NodeId {
        self.producer(self.node_inputs(phi)[NodeKind::PHI_TOKEN_SLOT])
    }

    /// `(control edge, arm value)` per input of [`Self::phi_region`].
    fn phi_arms(&self, phi: NodeId) -> impl Iterator<Item = (ValueId, ValueId)> + '_ {
        let edges = self.node_inputs(self.phi_region(phi));
        let inputs = self.node_inputs(phi);
        assert_eq!(
            edges.len() + 1,
            inputs.len(),
            "a phi takes one arm per input of its region"
        );
        edges.into_iter().zip(inputs.into_iter().skip(1))
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

    /// Typed inputs only.
    fn value_inputs(&self, node: NodeId) -> impl Iterator<Item = ValueId> + '_ {
        self.node_inputs(node)
            .into_iter()
            .filter(move |&i| self.value_type_opt(i).is_some())
    }

    /// Value-keyed, integer-only [`Self::value_inputs`].
    fn int_inputs(&self, value: ValueId) -> impl Iterator<Item = ValueId> + '_ {
        self.node_inputs(self.producer(value))
            .into_iter()
            .filter(move |&i| self.value_type_opt(i).is_some_and(ValueType::is_integer))
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

    /// `None` when the node is shorter.
    fn nth_input(&self, node: NodeId, idx: usize) -> Option<ValueId> {
        self.function().graph().nth_input(node, idx)
    }

    fn value_uses(&self, value_id: ValueId) -> impl Iterator<Item = (NodeId, u32)> + '_ {
        self.function().graph().value_uses(value_id)
    }

    /// O(1).
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

    /// The constant an integer `value` holds; `None` unless an `IntConst`
    /// produces it. See also [`Self::int_const_u128`] and
    /// [`Self::int_const_i128`].
    fn int_const(&self, value: ValueId) -> Option<&dashu_int::UBig> {
        if !self.value_kind(value).as_value()?.is_integer() {
            return None;
        }
        let NodeKind::IntConst(id) = *self.kind_of_value(value) else {
            return None;
        };
        Some(self.function().const_value(id))
    }

    /// `None` for a wide value past `u128`.
    fn int_const_u128(&self, value: ValueId) -> Option<u128> {
        let ty = self.value_kind(value).as_value()?;
        let v = u128::try_from(self.int_const(value)?).ok()?;
        // Interning masks to the declared width.
        debug_assert!(
            ty.int_mask_u128().is_none_or(|m| v <= m),
            "unmasked stored constant"
        );
        Some(v)
    }

    /// [`Self::int_const`] read as two's complement at its width.
    fn int_const_ibig(&self, value: ValueId) -> Option<dashu_int::IBig> {
        let ty = self.value_kind(value).as_value()?;
        crate::eval::signed(self.int_const(value)?, ty)
    }

    /// [`Self::int_const_ibig`] as `i128`; `None` for types wider than 128
    /// bits, even when [`Self::int_const_u128`] answers.
    fn int_const_i128(&self, value: ValueId) -> Option<i128> {
        let v = self.int_const_u128(value)?;
        self.value_kind(value).as_value()?.get_signed_int(v)
    }

    /// `None` unless `value` is an `I1` constant.
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

    /// The memory input of a `Store` / `Load` / `Call` / `CallOther`; `None`
    /// for every other kind, exit sinks included (they produce no memory, so
    /// no backward memory walk reaches them).
    fn memory_input_of(&self, node: NodeId) -> Option<ValueId> {
        let inputs = self.node_inputs(node);
        match *self.node_kind(node) {
            NodeKind::Store(_) | NodeKind::Load(_) => {
                inputs.get(NodeKind::ACCESS_MEM_SLOT).copied()
            }
            NodeKind::Call { .. } | NodeKind::CallOther { .. } => {
                inputs.get(NodeKind::CTRL_MEM_SLOT).copied()
            }
            NodeKind::Entry
            | NodeKind::InitialMemory
            | NodeKind::InitialVar(_)
            | NodeKind::Region
            | NodeKind::MemPhi
            | NodeKind::Phi
            | NodeKind::If
            | NodeKind::Switch(_)
            | NodeKind::Return
            | NodeKind::IndirectBranch
            | NodeKind::Unreachable
            | NodeKind::IntConst(_)
            | NodeKind::IntUnaryOp(_)
            | NodeKind::IntBinaryOp(_)
            | NodeKind::IntCmpOp(_)
            | NodeKind::Truncate
            | NodeKind::Popcount
            | NodeKind::Lzcount
            | NodeKind::Extend(_)
            | NodeKind::FloatConst(_)
            | NodeKind::FloatBinaryOp(_)
            | NodeKind::FloatUnaryOp(_)
            | NodeKind::FloatCmpOp(_)
            | NodeKind::IntToFloat
            | NodeKind::FloatToInt
            | NodeKind::FloatToFloat
            | NodeKind::IntBitsToFloat
            | NodeKind::FloatBitsToInt
            | NodeKind::CPoolRef
            | NodeKind::New => None,
        }
    }

    semantic_slot_accessors! {
        if_cond => 2[NodeKind::IF_COND_SLOT] "If node has [control, cond] inputs";

        store_addr => 3[NodeKind::ACCESS_ADDR_SLOT] "Store node has [memory, addr, data] inputs";

        store_data => 3[NodeKind::STORE_DATA_SLOT] "Store node has [memory, addr, data] inputs";

        load_mem => 2[NodeKind::ACCESS_MEM_SLOT] "Load node has [memory, addr] inputs";

        load_addr => 2[NodeKind::ACCESS_ADDR_SLOT] "Load node has [memory, addr] inputs";
    }

    /// The address of a `Load` / `Store`; `None` for any other kind or a short
    /// input list (sweeps reach malformed dead nodes).
    fn access_address(&self, node: NodeId) -> Option<ValueId> {
        if !matches!(self.node_kind(node), NodeKind::Load(_) | NodeKind::Store(_)) {
            return None;
        }
        self.node_inputs(node)
            .get(NodeKind::ACCESS_ADDR_SLOT)
            .copied()
    }

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

    /// The ISA-mode value the branch commits for its targets; `None` for a
    /// non-switching branch.
    fn indirect_branch_isa_mode(&self, node: NodeId) -> Option<ValueId> {
        assert!(
            matches!(self.node_kind(node), NodeKind::IndirectBranch),
            "indirect_branch_isa_mode on a non-IndirectBranch node"
        );
        self.node_inputs(node)
            .get(NodeKind::IndirectBranch.input_head_len())
            .copied()
    }

    /// As [`Self::indirect_branch_isa_mode`], for a `Switch`.
    fn switch_isa_mode(&self, node: NodeId) -> Option<ValueId> {
        assert!(
            matches!(self.node_kind(node), NodeKind::Switch(_)),
            "switch_isa_mode on a non-Switch node"
        );
        self.node_inputs(node)
            .get(self.node_kind(node).input_head_len())
            .copied()
    }

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

    /// Errors on a control, memory, or phi-token edge.
    fn value_type(&self, value_id: ValueId) -> crate::Result<ValueType> {
        let kind = self.function().graph().value_kind(value_id);
        kind.as_value()
            .ok_or_else(|| anyhow!("output {value_id:?} is not a value edge (got {kind:?})"))
    }

    /// The float type of `value`'s byte size (bitcastable from it). Errors on
    /// a size with no float type.
    fn infer_float_type(&self, value: ValueId) -> crate::Result<ValueType> {
        let ty = self.value_type(value)?;
        if ty.is_float() {
            return Ok(ty);
        }
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

/// Control-aware walks.
pub trait IRWalker: IRViewer {
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

    /// Nodes the def->use walk from the input-less roots reaches, matching
    /// `pred`; producers before consumers except across a def-use cycle.
    fn reverse_postorder_filter<'a>(
        &'a self,
        pred: impl Fn(&NodeKind) -> bool + 'a,
    ) -> impl Iterator<Item = NodeId> + 'a {
        let f = self.function();
        let rpo = crate::walk::GraphWalkInfo::compute_full(f.graph(), f.entry())
            .reverse_postorder(f.graph());
        rpo.into_iter().filter(move |&n| pred(self.node_kind(n)))
    }
}

impl<T: IRViewer + ?Sized> IRWalker for T {}

#[cfg(test)]
mod tests {
    //! Fixtures are built inline: `strider_ir_test_utils` links a separate
    //! compilation of strider-ir, so its `Function` is a different type here.

    use dashu_int::UBig;

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

    /// Little-endian, zero-extended to the declared byte size, whichever
    /// path built the stored value.
    #[test]
    fn le_bytes_serialises_little_endian_at_the_declared_width() {
        let bytes = |limbs: &[u64], ty: ValueType| {
            let cc = strider_target::BuiltCallingConvention::default();
            let mut b = FunctionBuilder::new(Vec::new(), cc, strider_target::Endianness::Little)
                .expect("FunctionBuilder::new");
            let v = b
                .build_int_const(UBig::from_words(limbs), ty)
                .expect("const");
            crate::eval::le_bytes(b.int_const(v).expect("an int const"), ty)
        };
        // Bytes 1..=8 little-endian.
        let low: u64 = 0x0807_0605_0403_0201;
        let mut i256 = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        i256.resize(32, 0);
        let mut past_carrier = vec![0u8; 32];
        past_carrier[16] = 1;
        assert_eq!(
            bytes(&[low], ValueType::int(80)),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0]
        );
        assert_eq!(
            bytes(&[low], ValueType::int(128)),
            vec![1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(bytes(&[low, 0, 0, 0], ValueType::int(256)), i256);
        // Bit 128 is the first bit no `u128` carries.
        assert_eq!(bytes(&[0, 0, 1, 0], ValueType::int(256)), past_carrier);
    }
}
