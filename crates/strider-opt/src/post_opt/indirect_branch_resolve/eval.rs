//! Read-only abstract evaluation of a jump-table dispatch cone: the concrete
//! branch target for a concrete index, computed in producers-before-consumers
//! order.
//!
//! An abstract value is either a concrete number or SP-relative, because the SP
//! is symbolic and a stack address cannot be a pure number.  Four foldings do
//! the work: ConstFold arithmetic, a constant-address ROM read, an SP-relative
//! load resolved against the store reaching it, and a `Phi` whose arms agree.
//! Any unresolved value, a non-const dispatch result, or a cycle yields
//! `None`.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory, UBig};

use crate::mem_analysis::{MemAnalyzer, MemOptions, MemPurpose, reduce};

#[derive(Clone, PartialEq)]
enum Abs {
    Const(UBig),
    SpRel { base: ValueId, offset: i128 },
}

impl Abs {
    fn as_const(&self) -> Option<&UBig> {
        match self {
            Abs::Const(c) => Some(c),
            Abs::SpRel { .. } => None,
        }
    }
}

pub(crate) struct Evaluator<'a> {
    function: &'a strider_ir::Function,
    rom: Option<&'a dyn ReadOnlyMemory>,
    map: FxHashMap<ValueId, Abs>,
    /// Answers every reaching-store probe, held so they share one memory index
    /// and one set of options.
    mem_index: MemAnalyzer,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(
        function: &'a strider_ir::Function,
        rom: Option<&'a dyn ReadOnlyMemory>,
    ) -> Self {
        Self {
            function,
            rom,
            map: FxHashMap::default(),
            // The probe climbs the memory dominator tree, which answers at or
            // nearer than the path walk it replaced.
            mem_index: MemAnalyzer::new(MemOptions::for_purpose(MemPurpose::TableWalk, function)),
        }
    }

    /// Opens an index: drops the previous index's values and pins `idx_value`.
    ///
    /// The boundary is the INDEX, not the root.  A branch's target and ISA-mode
    /// cones overlap almost entirely, so a per-root reset re-folds the shared
    /// dispatch load and everything under it.
    pub(crate) fn begin_index(&mut self, idx_value: ValueId, idx: u128) {
        self.map.clear();
        self.map.insert(idx_value, Abs::Const(UBig::from(idx)));
    }

    /// Evaluates `dispatch` over `order` against the open index.
    ///
    /// Bailing on the first non-folding node is exact, not just an
    /// optimization: every fold here needs every operand constant, so the
    /// first failure already forces `None` at `dispatch`.
    pub(crate) fn eval_root(&mut self, order: &[ValueId], dispatch: ValueId) -> Option<u64> {
        for &val in order {
            if self.map.contains_key(&val) {
                continue;
            }
            let a = self.eval_node(val)?;
            self.map.insert(val, a);
        }
        u64::try_from(self.map.get(&dispatch)?.as_const()?).ok()
    }

    fn get(&self, value: ValueId) -> Option<&Abs> {
        self.map.get(&value)
    }

    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        let f = self.function;
        let node = f.producer(value);
        let kind = *f.node_kind(node);
        // The SP spine is identified STRUCTURALLY from already-evaluated inputs,
        // not by re-running `decompose` per node per fold.  On the converged
        // graph the only SP shapes are the terminal `InitialVar(sp)`,
        // `Add(sp-rooted, const)`, and the alignment base `And(sp-rooted, mask)`.
        if f.is_entry_sp(value) {
            return Some(Abs::SpRel {
                base: value,
                offset: 0,
            });
        }
        let out_ty = f.value_type_opt(value);
        let ins: SmallVec<[ValueId; 2]> = f.value_inputs(node).collect();
        match kind {
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add) => {
                let (l, r) = (self.get(*ins.first()?)?, self.get(*ins.get(1)?)?);
                self.eval_add(value, l, r, out_ty?)
            }
            // A fresh opaque SP base at offset 0, matching `decompose`'s And arm.
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And) => {
                let Some(sp_operand) = crate::mem_analysis::alignment_masked_operand(f, node)
                else {
                    return self.eval_const_node(value);
                };
                match self.get(sp_operand) {
                    Some(Abs::SpRel { .. }) => Some(Abs::SpRel {
                        base: value,
                        offset: 0,
                    }),
                    _ => self.eval_const_node(value),
                }
            }
            // The memory index answers over RAM stores.  The space travels with
            // the call: `reshape` slices in it, so reading it back off the node
            // could fold a wrong target.
            NodeKind::Load(space @ rsleigh::VnSpace::RAM) => self.eval_load(node, value, space),
            NodeKind::Phi => self.eval_phi(node),
            _ => self.eval_const_node(value),
        }
    }

    /// The const-domain fold, for every kind with no `SpRel` layering.
    fn eval_const_node(&self, value: ValueId) -> Option<Abs> {
        let resolve = |v| self.get(v)?.as_const().cloned();
        crate::const_eval::eval_node_const(self.function, value, &resolve, self.rom).map(Abs::Const)
    }

    fn eval_add(&self, value: ValueId, l: &Abs, r: &Abs, ty: ValueType) -> Option<Abs> {
        match (l, r) {
            (Abs::Const(_), Abs::Const(_)) => self.eval_const_node(value),
            (Abs::SpRel { base, offset }, Abs::Const(c))
            | (Abs::Const(c), Abs::SpRel { base, offset }) => {
                // Signed, so a negative frame offset subtracts correctly;
                // reduced, so the probe names the slot `decompose` does.
                let delta = ty.get_signed_int(u128::try_from(c).ok()?)?;
                Some(Abs::SpRel {
                    base: *base,
                    offset: reduce(Some(ty), offset.wrapping_add(delta)),
                })
            }
            (Abs::SpRel { .. }, Abs::SpRel { .. }) => None,
        }
    }

    fn eval_load(&mut self, node: NodeId, value: ValueId, space: rsleigh::VnSpace) -> Option<Abs> {
        let f = self.function;
        let load_ty = f.value_type_opt(value)?;
        match self.get(f.load_addr(node))?.clone() {
            Abs::Const(_) => self.eval_const_node(value),
            Abs::SpRel { base, offset } => {
                let [mem, _addr] = f
                    .node_inputs_exact::<2>(node)
                    .expect("a Load carries its memory and its address");
                let load_size = load_ty.byte_size() as i128;
                let data = self.reaching_store_data(mem, base, offset, load_size)?;
                // Jump targets are constants on the converged graph.
                let data_ty = f.value_type_opt(data)?;
                let raw = f.int_const(data)?.clone();
                Some(Abs::Const(self.reshape(raw, data_ty, load_ty, space)?))
            }
        }
    }

    /// The data of the store anchored exactly at the probed slot, or `None`
    /// when the reaching store is elsewhere or is not a store at all.
    ///
    /// The index is built once per memory token and shared across indices, so
    /// an n-entry stack table costs no walk per entry.
    fn reaching_store_data(
        &mut self,
        mem: ValueId,
        base: ValueId,
        offset: i128,
        size: i128,
    ) -> Option<ValueId> {
        let f = self.function;
        let hit = self.mem_index.reaching_store(f, mem, base, offset, size)?;
        // The store must be anchored exactly at the probed offset.
        (hit.store_offset == offset).then(|| hit.data(f))
    }

    /// `v`, a store's data, as the `load_ty` load in `space` at the store's
    /// address reads it. Equal widths pass through.  `None` for a load wider
    /// than the store, a same-size type mismatch, and any non-integer pair.
    fn reshape(
        &self,
        v: UBig,
        data_ty: ValueType,
        load_ty: ValueType,
        space: rsleigh::VnSpace,
    ) -> Option<UBig> {
        if data_ty == load_ty {
            return Some(v);
        }
        if data_ty.is_integer() && load_ty.is_integer() && load_ty.byte_size() < data_ty.byte_size()
        {
            let order = self.function.byte_order();
            let shift_bits = crate::const_eval::slice_shift_bits(order, space, data_ty, load_ty)?;
            let shift = usize::try_from(shift_bits).ok()?;
            return Some(strider_ir::eval::mask(&(v >> shift), load_ty));
        }
        None
    }

    /// Every value arm must resolve to the same `Abs`.
    fn eval_phi(&mut self, node: NodeId) -> Option<Abs> {
        let arms: SmallVec<[ValueId; 4]> = self.function.value_inputs(node).collect();
        let mut agreed: Option<Abs> = None;
        for arm in arms {
            let v = self.get(arm)?.clone();
            match &agreed {
                None => agreed = Some(v),
                Some(prev) if *prev == v => {}
                Some(_) => return None,
            }
        }
        agreed
    }
}

/// Feeding this backward relation to a post-order walk yields producers before
/// consumers.
#[cfg(test)]
struct ValueInputSuccs<'a> {
    function: &'a strider_ir::Function,
}

#[cfg(test)]
impl graph_algorithms::walk::GraphRef for ValueInputSuccs<'_> {
    type NodeId = ValueId;

    fn successors(&self, value: ValueId, mut f: impl FnMut(ValueId)) {
        for succ in self.function.value_inputs(self.function.producer(value)) {
            f(succ);
        }
    }
}

#[cfg(test)]
pub(crate) fn cone_order(function: &strider_ir::Function, root: ValueId) -> Vec<ValueId> {
    graph_algorithms::walk::PostOrder::new(ValueInputSuccs { function }, [root]).collect()
}

/// Treats `stop` as a leaf, so its own producers are never followed.
struct ValueInputSuccsPruned<'a> {
    function: &'a strider_ir::Function,
    stop: ValueId,
}

impl graph_algorithms::walk::GraphRef for ValueInputSuccsPruned<'_> {
    type NodeId = ValueId;

    fn successors(&self, value: ValueId, mut f: impl FnMut(ValueId)) {
        if value == self.stop {
            return;
        }
        for succ in self.function.value_inputs(self.function.producer(value)) {
            f(succ);
        }
    }
}

/// Backward reachability from `root` over value edges, never descending through
/// `stop`'s own inputs (it is pinned to a constant at eval time).  The order is
/// built once per root and replayed for every enumerated index, so leaving
/// `stop`'s cone out keeps the index computation off the per-index cost.
pub(crate) fn cone_order_pruned(
    function: &strider_ir::Function,
    root: ValueId,
    stop: ValueId,
) -> Vec<ValueId> {
    graph_algorithms::walk::PostOrder::new(ValueInputSuccsPruned { function, stop }, [root])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Evaluator, cone_order, cone_order_pruned};
    use strider_ir::UBig;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    // Pinning the INNER `idx = idx_raw + 5` of `dispatch = (idx_raw + 5) + 100`
    // makes its ancestor `idx_raw` irrelevant, so the pruned cone must stop at
    // `idx` yet still evaluate correctly.
    #[test]
    fn pruned_cone_stops_at_index_and_evaluates_correctly() {
        let vn = reg_vn(0x1000, 8);
        let mut b = RegisterSet::new()
            .tracked(vn)
            .arg(vn)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let idx_raw = b.read_variable(&vn).expect("idx_raw");
        let c5 = b.build_int_const(5u64, ValueType::int(64)).expect("c5");
        let idx = b
            .build_int_binary_operation(idx_raw, c5, IntBinaryOp::Add, ValueType::int(64))
            .expect("idx");
        let c100 = b.build_int_const(100u64, ValueType::int(64)).expect("c100");
        let dispatch = b
            .build_int_binary_operation(idx, c100, IntBinaryOp::Add, ValueType::int(64))
            .expect("dispatch");
        b.build_return(Some(dispatch), &[]).expect("build_return");
        b.set_lift_addr(None);
        let function = b.build().expect("build");

        let full = cone_order(&function, dispatch);
        let pruned = cone_order_pruned(&function, dispatch, idx);

        assert!(
            full.contains(&idx_raw),
            "full cone should include the index's ancestor"
        );
        assert!(
            !pruned.contains(&idx_raw),
            "pruned cone must stop at idx and exclude its ancestors"
        );
        assert!(pruned.contains(&idx), "pruned cone includes the stop node");
        assert!(pruned.contains(&dispatch), "pruned cone includes the root");

        let mut ev = Evaluator::new(&function, None);
        ev.begin_index(idx, 7);
        assert_eq!(ev.eval_root(&pruned, dispatch), Some(107));
        ev.begin_index(idx, 8);
        assert_eq!(ev.eval_root(&pruned, dispatch), Some(108));
    }

    // `idx` is a register read, so it is non-const and leaving it unseeded
    // genuinely fails to resolve.  Returns `(function, idx, sum)`.
    fn build_add_idx_100() -> (
        strider_ir::Function,
        strider_ir::node::ValueId,
        strider_ir::node::ValueId,
    ) {
        let vn = reg_vn(0x1000, 8); // 8-byte, so I64
        let mut b = RegisterSet::new()
            .tracked(vn)
            .arg(vn)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let idx = b.read_variable(&vn).expect("read_variable");
        let c100 = b
            .build_int_const(100u64, ValueType::int(64))
            .expect("build_int_const");
        let sum = b
            .build_int_binary_operation(idx, c100, IntBinaryOp::Add, ValueType::int(64))
            .expect("build_int_binary_operation");
        b.build_return(Some(sum), &[]).expect("build_return");
        b.set_lift_addr(None);
        let function = b.build().expect("build");
        (function, idx, sum)
    }

    #[test]
    fn evaluates_add_under_seed() {
        let (function, idx, sum) = build_add_idx_100();
        // The bail-on-first-failure contract requires `order` to be the cone
        // pruned at the index.
        let order = cone_order_pruned(&function, sum, idx);
        let mut ev = Evaluator::new(&function, None);
        ev.begin_index(idx, 5);
        assert_eq!(ev.eval_root(&order, sum), Some(105));
        ev.begin_index(idx, 7); // fresh map
        assert_eq!(ev.eval_root(&order, sum), Some(107));
    }

    #[test]
    fn unseeded_index_is_none() {
        let (function, _idx, sum) = build_add_idx_100();
        let mut ev = Evaluator::new(&function, None);
        // Pruning at `sum` itself leaves a cone of only `[sum]`.
        let order_sum = cone_order_pruned(&function, sum, sum);
        ev.begin_index(sum, 5);
        assert_eq!(ev.eval_root(&order_sum, sum), Some(5));
        // Seeding an unrelated leaf leaves the real index unresolved.
        let const_100 = sum_unrelated_leaf(&function, sum);
        let order = cone_order_pruned(&function, sum, const_100);
        ev.begin_index(const_100, 5);
        assert_eq!(ev.eval_root(&order, sum), None);
    }

    /// `Load(space)` at `sp` over a RAM store of `0x1234` at `sp`, evaluated
    /// with an unrelated index pinned.
    fn eval_load_over_a_ram_store_at_sp(space: rsleigh::VnSpace) -> Option<u64> {
        let sp = reg_vn(0x2000, 8);
        let idx_vn = reg_vn(0x1000, 8);
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .tracked(idx_vn)
            .arg(idx_vn)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let idx = b.read_variable(&idx_vn).expect("idx");
        let at = b.read_variable(&sp).expect("sp");
        let word = b
            .build_int_const(0x1234u64, ValueType::int(64))
            .expect("word");
        b.build_store(at, word, rsleigh::VnSpace::RAM)
            .expect("build_store");
        let loaded = b
            .build_load(at, space, ValueType::int(64))
            .expect("build_load");
        b.build_return(Some(loaded), &[]).expect("build_return");
        b.set_lift_addr(None);
        let mut function = b.build().expect("build");
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(&mut function, &crate::OptCtx::new(None))
            .expect("pipeline");
        let order = cone_order(&function, loaded);
        let mut ev = Evaluator::new(&function, None);
        ev.begin_index(idx, 0);
        ev.eval_root(&order, loaded)
    }

    /// A narrower load reads its slice of a store in its own space's byte
    /// order: on ARM BE8 the register block is little-endian under
    /// big-endian data.
    /// Mutation: slice in the data order whatever the space.
    #[test]
    fn a_reshaped_store_is_sliced_in_the_loads_space_order() {
        let sp = reg_vn(0x2000, 8);
        let mut b = strider_ir_test_utils::sp_frame(sp)
            .byte_order(strider_target::ByteOrder {
                data: strider_target::Endianness::Big,
                register: strider_target::Endianness::Little,
            })
            .build_fn_single_region()
            .expect("build_fn_single_region");
        b.build_return(None, &[]).expect("build_return");
        b.set_lift_addr(None);
        let function = b.build().expect("build");
        let ev = Evaluator::new(&function, None);
        let reshape = |space| {
            ev.reshape(
                UBig::from(0xDEAD_BEEFu32),
                ValueType::int(32),
                ValueType::int(8),
                space,
            )
        };
        assert_eq!(reshape(rsleigh::VnSpace::RAM), Some(UBig::from(0xDEu8)));
        assert_eq!(
            reshape(rsleigh::VnSpace::REGISTER),
            Some(UBig::from(0xEFu8))
        );
    }

    /// The memory index holds RAM stores, so only a RAM load reads it.
    #[test]
    fn a_non_ram_load_at_a_stack_address_does_not_read_the_index() {
        assert_eq!(
            eval_load_over_a_ram_store_at_sp(rsleigh::VnSpace::RAM),
            Some(0x1234)
        );
        assert_eq!(
            eval_load_over_a_ram_store_at_sp(rsleigh::VnSpace::REGISTER),
            None
        );
    }

    /// The IntConst(100) value: in the cone of `sum` but not the idx leaf, so
    /// seeding it instead leaves idx symbolic and the sum uncollapsible.
    fn sum_unrelated_leaf(
        f: &strider_ir::Function,
        sum: strider_ir::node::ValueId,
    ) -> strider_ir::node::ValueId {
        use strider_ir::IRViewer;
        let add_node = f.producer(sum);
        let inputs = f.node_inputs(add_node);
        for input in inputs {
            if f.int_const_u128(input) == Some(100) {
                return input;
            }
        }
        panic!("could not find IntConst(100) among Add inputs");
    }
}
