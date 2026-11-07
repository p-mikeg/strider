//! Abstract evaluation of a jump-table dispatch cone: the concrete target for
//! a concrete index.
//!
//! A value is a constant or SP-relative, folded through constant arithmetic,
//! ROM reads, SP-relative loads of the reaching store, and agreeing `Phi`s.
//! Anything else, or a cycle, yields `None`.

use rustc_hash::FxHashMap;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory, UBig};

use crate::mem_analysis::{MemQuery, Policy, reduce};

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
    /// Each folded value, and whether its fold read the pinned index.
    map: FxHashMap<ValueId, (Abs, bool)>,
    pinned: Option<ValueId>,
    /// Answers every reaching-store probe.
    mem_index: MemQuery,
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
            pinned: None,
            mem_index: MemQuery::default(),
        }
    }

    pub(crate) fn rom(&self) -> Option<&'a dyn ReadOnlyMemory> {
        self.rom
    }

    /// Pins `idx_value` to `idx`, dropping every fold that read the previous
    /// pin, or every fold when the pinned value changes. A pin-free fold is a
    /// function of the graph alone, so it is kept across indices and roots.
    pub(crate) fn begin_index(&mut self, idx_value: ValueId, idx: u128) {
        if self.pinned == Some(idx_value) {
            self.map.retain(|_, (_, reads_pin)| !*reads_pin);
        } else {
            self.map.clear();
            self.pinned = Some(idx_value);
        }
        self.map
            .insert(idx_value, (Abs::Const(UBig::from(idx)), true));
    }

    /// Evaluates `dispatch` over `order` against the open index. Bails on the
    /// first node that does not fold; the cone order puts every operand first.
    pub(crate) fn eval_root(&mut self, order: &[ValueId], dispatch: ValueId) -> Option<u64> {
        for &val in order {
            if self.map.contains_key(&val) {
                continue;
            }
            let a = self.eval_node(val)?;
            let f = self.function;
            let reads_pin = f
                .node_inputs(f.producer(val))
                .into_iter()
                .any(|i| self.map.get(&i).is_some_and(|&(_, reads)| reads));
            self.map.insert(val, (a, reads_pin));
        }
        u64::try_from(self.get(dispatch)?.as_const()?).ok()
    }

    fn get(&self, value: ValueId) -> Option<&Abs> {
        self.map.get(&value).map(|(abs, _)| abs)
    }

    /// The SP layer: the entry SP, an `Add` of a constant to an SP-relative
    /// value, an alignment mask of one, and a RAM load at one. A value reading
    /// no SP-relative input folds as a constant.
    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        let f = self.function;
        if f.is_entry_sp(value) {
            return Some(Abs::SpRel {
                base: value,
                offset: 0,
            });
        }
        let node = f.producer(value);
        let kind = *f.node_kind(node);
        if kind == NodeKind::Phi {
            return self.eval_phi(node);
        }
        let Some((sp, base, offset)) = f.value_inputs(node).find_map(|i| match self.get(i)? {
            &Abs::SpRel { base, offset } => Some((i, base, offset)),
            Abs::Const(_) => None,
        }) else {
            return self.eval_const_node(value);
        };
        match kind {
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add) => {
                let [l, r] = f.producer_inputs_exact::<2>(value).ok()?;
                let c = self.get(if l == sp { r } else { l })?.as_const()?;
                // Signed, so a negative frame offset subtracts correctly;
                // reduced, so the probe names the slot `decompose` does.
                let ty = f.value_type_opt(value)?;
                let delta = ty.get_signed_int(u128::try_from(c).ok()?)?;
                Some(Abs::SpRel {
                    base,
                    offset: reduce(Some(ty), offset.wrapping_add(delta)),
                })
            }
            // A fresh opaque SP base at offset 0, matching `decompose`'s And arm.
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And)
                if crate::mem_analysis::alignment_masked_operand(f, node)
                    .is_some_and(|(operand, _)| operand == sp) =>
            {
                Some(Abs::SpRel {
                    base: value,
                    offset: 0,
                })
            }
            NodeKind::Load(space @ rsleigh::VnSpace::RAM) if f.load_addr(node) == sp => {
                let load_ty = f.value_type_opt(value)?;
                let size = load_ty.byte_size() as i128;
                let clobber = self.mem_index.nearest_sp_clobber(
                    f,
                    Policy::strict(f),
                    f.load_mem(node),
                    base,
                    offset,
                    size,
                    space,
                );
                let hit = crate::mem_analysis::anchored_store(f, clobber, base)?;
                // The store must sit exactly at the probed offset.
                let data = (hit.store_offset == offset).then(|| hit.data(f))?;
                let raw = f.int_const(data)?.clone();
                Some(Abs::Const(self.reshape(
                    raw,
                    f.value_type_opt(data)?,
                    load_ty,
                    space,
                )?))
            }
            _ => None,
        }
    }

    /// The const-domain fold, for every kind with no `SpRel` layering.
    fn eval_const_node(&self, value: ValueId) -> Option<Abs> {
        let resolve = |v| self.get(v)?.as_const().cloned();
        crate::const_eval::eval_node_const(self.function, value, &resolve, self.rom).map(Abs::Const)
    }

    /// `v`, a store's data, as the `load_ty` load in `space` at the store's
    /// address reads it. Equal widths pass through. `None` for a load wider
    /// than the store, a same-size type mismatch, and any non-integer pair.
    fn reshape(
        &self,
        v: UBig,
        data_ty: ValueType,
        load_ty: ValueType,
        space: rsleigh::VnSpace,
    ) -> Option<UBig> {
        use crate::const_eval::LoadSlice;
        match crate::const_eval::load_slice(self.function.byte_order(), space, data_ty, load_ty)? {
            LoadSlice::Same => Some(v),
            LoadSlice::Narrow(shift) => {
                let shift = usize::try_from(shift).ok()?;
                Some(strider_ir::eval::mask(&(v >> shift), load_ty))
            }
        }
    }

    /// Every value arm must resolve to the same `Abs`.
    fn eval_phi(&self, node: NodeId) -> Option<Abs> {
        let mut arms = self.function.value_inputs(node).map(|arm| self.get(arm));
        let first = arms.next()??;
        arms.all(|arm| arm == Some(first)).then(|| first.clone())
    }
}

/// Value-input edges, whose post-order is producers before consumers; `stop`
/// is a leaf.
struct ValueInputSuccs<'a> {
    function: &'a strider_ir::Function,
    stop: ValueId,
}

impl graph_algorithms::walk::GraphRef for ValueInputSuccs<'_> {
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

/// The cone of `root` in producers-before-consumers order, not descending
/// through `stop`, the pinned index.
pub(crate) fn cone_order_pruned(
    function: &strider_ir::Function,
    root: ValueId,
    stop: ValueId,
) -> Vec<ValueId> {
    graph_algorithms::walk::PostOrder::new(ValueInputSuccs { function, stop }, [root]).collect()
}

#[cfg(test)]
mod tests {
    use super::{Evaluator, cone_order_pruned};
    use crate::test_support::{COLLAPSE, run_passes};
    use strider_ir::node::{ValueId, ValueType};
    use strider_ir::{FunctionBuilder, IRBuilderExt, IntBinaryOp, UBig};
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    /// A single-region function whose one argument is a `bytes`-wide
    /// register, and its entry value.
    fn arg_fn(bytes: u32) -> (FunctionBuilder, ValueId) {
        let vn = reg_vn(0x1000, bytes);
        let b = RegisterSet::new()
            .tracked(vn)
            .arg(vn)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let arg = b.read_variable(&vn).expect("arg");
        (b, arg)
    }

    /// `b` returning `value`, built.
    fn returning(mut b: FunctionBuilder, value: ValueId) -> strider_ir::Function {
        b.build_return(Some(value), &[]).expect("build_return");
        b.set_lift_addr(None);
        b.build().expect("build")
    }

    // Pinning the INNER `idx = idx_raw + 5` of `dispatch = (idx_raw + 5) + 100`
    // makes its ancestor `idx_raw` irrelevant, so the pruned cone must stop at
    // `idx` yet still evaluate correctly.
    #[test]
    fn pruned_cone_stops_at_index_and_evaluates_correctly() {
        let i64t = ValueType::int(64);
        let (mut b, idx_raw) = arg_fn(8);
        let idx = b
            .build_const_binop(5u64, idx_raw, IntBinaryOp::Add, i64t)
            .expect("idx");
        let dispatch = b
            .build_const_binop(100u64, idx, IntBinaryOp::Add, i64t)
            .expect("dispatch");
        let function = returning(b, dispatch);

        let full = cone_order_pruned(&function, dispatch, idx_raw);
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

    /// A value folded while another value was pinned is dropped once the
    /// value it reads is pinned instead.
    /// Mutation: keep every pin-free fold across a change of pinned value.
    #[test]
    fn pinning_a_new_value_drops_folds_that_read_it() {
        let i64t = ValueType::int(64);
        let (mut b, other) = arg_fn(8);
        let c5 = b.build_int_const(5u64, i64t).expect("c5");
        let q = b
            .build_const_binop(7u64, c5, IntBinaryOp::Add, i64t)
            .expect("q");
        let r = b
            .build_const_binop(100u64, q, IntBinaryOp::Add, i64t)
            .expect("r");
        let function = returning(b, r);

        let mut ev = Evaluator::new(&function, None);
        ev.begin_index(other, 0);
        assert_eq!(
            ev.eval_root(&cone_order_pruned(&function, r, other), r),
            Some(112)
        );
        ev.begin_index(q, 50);
        assert_eq!(
            ev.eval_root(&cone_order_pruned(&function, r, q), r),
            Some(150)
        );
    }

    /// A table base read out of ROM is the same for every index, so it is
    /// read once across them.
    /// Mutation: drop every fold at each index.
    #[test]
    fn an_index_free_rom_read_folds_once_across_indices() {
        struct Recording(strider_ir_test_utils::MockRom, std::cell::Cell<usize>);
        impl strider_ir::ReadOnlyMemory for Recording {
            fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
                if addr == 0x8000 {
                    self.1.set(self.1.get() + 1);
                }
                self.0.read(addr, buf)
            }
        }
        let i32t = ValueType::int(32);
        let (mut b, idx) = arg_fn(4);
        let got = b.build_int_const(0x8000u64, i32t).expect("got");
        let base = b
            .build_load(got, rsleigh::VnSpace::RAM, i32t)
            .expect("base");
        let scaled = b
            .build_const_binop(4u64, idx, IntBinaryOp::Mul, i32t)
            .expect("scaled");
        let addr = b
            .build_int_binary_operation(base, scaled, IntBinaryOp::Add, i32t)
            .expect("addr");
        let target = b
            .build_load(addr, rsleigh::VnSpace::RAM, i32t)
            .expect("target");
        let function = returning(b, target);
        let rom = Recording(
            strider_ir_test_utils::MockRom::fixed_table(&[
                (0x8000, 0x4000),
                (0x4000, 0x1000),
                (0x4004, 0x1004),
                (0x4008, 0x1008),
            ]),
            std::cell::Cell::new(0),
        );

        let order = cone_order_pruned(&function, target, idx);
        let mut ev = Evaluator::new(&function, Some(&rom));
        for (i, want) in [0x1000, 0x1004, 0x1008].into_iter().enumerate() {
            ev.begin_index(idx, i as u128);
            assert_eq!(ev.eval_root(&order, target), Some(want));
        }
        assert_eq!(rom.1.get(), 1);
    }

    // `idx` is a register read, so it is non-const and leaving it unseeded
    // genuinely fails to resolve.  Returns `(function, idx, sum)`.
    fn build_add_idx_100() -> (strider_ir::Function, ValueId, ValueId) {
        let (mut b, idx) = arg_fn(8);
        let sum = b
            .build_const_binop(100u64, idx, IntBinaryOp::Add, ValueType::int(64))
            .expect("sum");
        (returning(b, sum), idx, sum)
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
        let mut function = returning(b, loaded);
        run_passes(&mut function, COLLAPSE).expect("pipeline");
        let order = cone_order_pruned(&function, loaded, idx);
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
    fn sum_unrelated_leaf(f: &strider_ir::Function, sum: ValueId) -> ValueId {
        use strider_ir::IRViewer;
        f.value_inputs(f.producer(sum))
            .find(|&input| f.int_const_u128(input) == Some(100))
            .expect("IntConst(100) among the Add inputs")
    }
}
