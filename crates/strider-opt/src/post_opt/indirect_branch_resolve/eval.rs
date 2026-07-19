//! Read-only abstract evaluation of a jump-table dispatch cone: the concrete
//! branch target for a concrete index, computed in producers-before-consumers
//! order.
//!
//! An abstract value is either a concrete number or SP-relative, because the SP
//! is symbolic and a stack address cannot be a pure number.  Three foldings do
//! the work: ConstFold arithmetic, a constant-address ROM read, and an
//! SP-relative load resolved via `reaching_store`.  Any unresolved value, a
//! non-const dispatch result, or a cycle yields `None`.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory};

use crate::sp_analysis::{SpAnalyzer, SpOptions};

#[derive(Clone, Copy, PartialEq)]
enum Abs {
    Const(u128),
    SpRel { base: ValueId, offset: i128 },
}

impl Abs {
    fn as_const(self) -> Option<u128> {
        match self {
            Abs::Const(c) => Some(c),
            Abs::SpRel { .. } => None,
        }
    }
}

pub(crate) struct Evaluator<'a> {
    function: &'a strider_ir::Function,
    rom: Option<&'a dyn ReadOnlyMemory>,
    alias_mode: crate::AliasMode,
    map: FxHashMap<ValueId, Abs>,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(
        function: &'a strider_ir::Function,
        rom: Option<&'a dyn ReadOnlyMemory>,
        alias_mode: crate::AliasMode,
    ) -> Self {
        Self {
            function,
            rom,
            alias_mode,
            map: FxHashMap::default(),
        }
    }

    /// Evaluates `dispatch` over `order` with `idx_value` bound to `idx`.
    ///
    /// Bailing on the first non-folding node is exact, not just an
    /// optimization: every node in `order` is a value-ancestor of `dispatch`,
    /// so one that fails to fold means `dispatch` cannot be constant either.
    pub(crate) fn eval_target(
        &mut self,
        order: &[ValueId],
        dispatch: ValueId,
        idx_value: ValueId,
        idx: u128,
    ) -> Option<u64> {
        self.map.clear();
        self.map.insert(idx_value, Abs::Const(idx));
        for &val in order {
            if self.map.contains_key(&val) {
                continue;
            }
            let a = self.eval_node(val)?;
            self.map.insert(val, a);
        }
        u64::try_from(self.map.get(&dispatch).copied()?.as_const()?).ok()
    }

    fn get(&self, value: ValueId) -> Option<Abs> {
        self.map.get(&value).copied()
    }

    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        let f = self.function;
        let node = f.producer(value);
        let kind = *f.node_kind(node);
        // The SP spine is identified STRUCTURALLY from already-evaluated inputs,
        // not by re-running `decompose` per node per fold.  On the converged
        // graph the only SP shapes are the terminal `InitialVar(sp)`,
        // `Add(sp-rooted, const)`, and the alignment base `And(sp-rooted, mask)`.
        if matches!(kind, NodeKind::InitialVar(id) if f.initial_vn(id) == f.default_cc().stack_vn) {
            return Some(Abs::SpRel {
                base: value,
                offset: 0,
            });
        }
        let out_ty = f.value_type_opt(value);
        let ins: SmallVec<[ValueId; 2]> = f.value_inputs(node).collect();
        match kind {
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add) => self.eval_add(
                value,
                self.get(*ins.first()?)?,
                self.get(*ins.get(1)?)?,
                out_ty?,
            ),
            // A fresh opaque SP base at offset 0, matching `decompose`'s And arm.
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And) => {
                let [l, r] = [*ins.first()?, *ins.get(1)?];
                let sp_operand = if f
                    .int_const_u128(r)
                    .is_some_and(crate::sp_analysis::is_alignment_mask)
                {
                    l
                } else if f
                    .int_const_u128(l)
                    .is_some_and(crate::sp_analysis::is_alignment_mask)
                {
                    r
                } else {
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
            NodeKind::Load(_) => self.eval_load(node, value),
            NodeKind::Phi => self.eval_phi(node),
            _ => self.eval_const_node(value),
        }
    }

    /// The const-domain fold, for every kind with no `SpRel` layering.
    fn eval_const_node(&self, value: ValueId) -> Option<Abs> {
        let resolve = |v| self.get(v).and_then(Abs::as_const);
        crate::const_eval::eval_node_const(self.function, value, &resolve, self.rom).map(Abs::Const)
    }

    fn eval_add(&self, value: ValueId, l: Abs, r: Abs, ty: ValueType) -> Option<Abs> {
        match (l, r) {
            (Abs::Const(_), Abs::Const(_)) => self.eval_const_node(value),
            (Abs::SpRel { base, offset }, Abs::Const(c))
            | (Abs::Const(c), Abs::SpRel { base, offset }) => {
                // Signed, so a negative frame offset subtracts correctly.
                let delta = ty.get_signed_int(c)?;
                Some(Abs::SpRel {
                    base,
                    offset: offset.wrapping_add(delta),
                })
            }
            (Abs::SpRel { .. }, Abs::SpRel { .. }) => None,
        }
    }

    fn eval_load(&mut self, node: NodeId, value: ValueId) -> Option<Abs> {
        let f = self.function;
        let load_ty = f.value_type_opt(value)?;
        match self.get(f.load_addr(node))? {
            Abs::Const(_) => self.eval_const_node(value),
            Abs::SpRel { base, offset } => {
                let [mem, _addr] = f.node_inputs_exact::<2>(node).ok()?;
                let load_size = load_ty.byte_size() as i128;
                let reaching = {
                    let cfg = SpAnalyzer::new(SpOptions::call_blocking(self.alias_mode));
                    cfg.reaching_store(f, mem, base, offset, load_size)
                }?;
                // The store must be anchored exactly at the probed offset.
                if reaching.store_offset != offset {
                    return None;
                }
                // Jump targets are constants on the converged graph.
                let data = reaching.data(f);
                let data_ty = f.value_type_opt(data)?;
                let raw = f.int_const_u128(data)?;
                Some(Abs::Const(self.reshape(raw, data_ty, load_ty)?))
            }
        }
    }

    /// Equal widths pass through; a load wider than the store gives `None`.
    fn reshape(&self, v: u128, data_ty: ValueType, load_ty: ValueType) -> Option<u128> {
        if data_ty == load_ty {
            return Some(v);
        }
        if data_ty.is_integer() && load_ty.is_integer() && load_ty.byte_size() < data_ty.byte_size()
        {
            // Little-endian gives 0, so the low bytes pass through.
            let shift_bits = crate::sp_analysis::high_low_shift_bits(
                data_ty,
                load_ty,
                self.function.endianness(),
            );
            return load_ty.get_unsigned_int(v >> shift_bits);
        }
        None
    }

    /// Every value arm must resolve to the same `Abs`.
    fn eval_phi(&mut self, node: NodeId) -> Option<Abs> {
        let arms: SmallVec<[ValueId; 4]> = self.function.value_inputs(node).collect();
        let mut agreed: Option<Abs> = None;
        for arm in arms {
            let v = self.get(arm)?;
            match agreed {
                None => agreed = Some(v),
                Some(prev) if prev == v => {}
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

    fn try_successors(
        &self,
        value: ValueId,
        f: impl FnMut(ValueId) -> std::ops::ControlFlow<()>,
    ) -> std::ops::ControlFlow<()> {
        self.function
            .value_inputs(self.function.producer(value))
            .try_for_each(f)
    }
}

#[cfg(test)]
pub(crate) fn cone_order(function: &strider_ir::Function, root: ValueId) -> Vec<ValueId> {
    graph_algorithms::walk::entity_postorder(ValueInputSuccs { function }, [root]).collect()
}

/// Treats `stop` as a leaf, so its own producers are never followed.
struct ValueInputSuccsPruned<'a> {
    function: &'a strider_ir::Function,
    stop: ValueId,
}

impl graph_algorithms::walk::GraphRef for ValueInputSuccsPruned<'_> {
    type NodeId = ValueId;

    fn try_successors(
        &self,
        value: ValueId,
        f: impl FnMut(ValueId) -> std::ops::ControlFlow<()>,
    ) -> std::ops::ControlFlow<()> {
        if value == self.stop {
            return std::ops::ControlFlow::Continue(());
        }
        self.function
            .value_inputs(self.function.producer(value))
            .try_for_each(f)
    }
}

/// Backward reachability from `root` over value edges, never descending through
/// `stop`'s own inputs (it is pinned to a constant at eval time).  Makes
/// evaluation O(index-to-dispatch path) instead of O(backward slice from
/// `root`).
pub(crate) fn cone_order_pruned(
    function: &strider_ir::Function,
    root: ValueId,
    stop: ValueId,
) -> Vec<ValueId> {
    graph_algorithms::walk::entity_postorder(ValueInputSuccsPruned { function, stop }, [root])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Evaluator, cone_order, cone_order_pruned};
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
        let c5 = b.build_int_const(5u64, ValueType::I64).expect("c5");
        let idx = b
            .build_int_binary_operation(idx_raw, c5, IntBinaryOp::Add, ValueType::I64)
            .expect("idx");
        let c100 = b.build_int_const(100u64, ValueType::I64).expect("c100");
        let dispatch = b
            .build_int_binary_operation(idx, c100, IntBinaryOp::Add, ValueType::I64)
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

        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        assert_eq!(ev.eval_target(&pruned, dispatch, idx, 7), Some(107));
        assert_eq!(ev.eval_target(&pruned, dispatch, idx, 8), Some(108));
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
            .build_int_const(100u64, ValueType::I64)
            .expect("build_int_const");
        let sum = b
            .build_int_binary_operation(idx, c100, IntBinaryOp::Add, ValueType::I64)
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
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        assert_eq!(ev.eval_target(&order, sum, idx, 5), Some(105));
        assert_eq!(ev.eval_target(&order, sum, idx, 7), Some(107)); // fresh map
    }

    #[test]
    fn unseeded_index_is_none() {
        let (function, _idx, sum) = build_add_idx_100();
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        // Pruning at `sum` itself leaves a cone of just `[sum]`.
        let order_sum = cone_order_pruned(&function, sum, sum);
        assert_eq!(ev.eval_target(&order_sum, sum, sum, 5), Some(5));
        // Seeding an unrelated leaf leaves the real index unresolved.
        let const_100 = sum_unrelated_leaf(&function, sum);
        let order = cone_order_pruned(&function, sum, const_100);
        assert_eq!(ev.eval_target(&order, sum, const_100, 5), None);
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
