//! Read-only abstract evaluation of a jump-table dispatch cone.
//!
//! Computes the concrete branch target for a concrete index by evaluating the
//! dispatch value's cone in producers-before-consumers order. The abstract
//! value is a concrete number or stack-pointer-relative (`Abs`), because a
//! stack address can't be a pure number (the SP is symbolic). Three node
//! families do the work — ConstFold arithmetic, `LoadReadOnly` (constant-address
//! ROM read), and `LoadForward` (index folded into an `SpRel` offset, then the
//! existing `reaching_store` finds the store at that concrete offset). No graph
//! mutation, no clone, no pipeline. Any unresolved value, a non-`Const` dispatch
//! result, or a cycle yields `None`, so the caller rejects the candidate.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory};
use strider_target::Endianness;

use crate::opt::constant_fold::eval_int::eval_int_binary;
use crate::sp_expr::{SpAliasCfg, SpDecomposer, SpExpr, SpExprMemo};

/// Returns an iterator over the value-typed inputs of `node` — i.e., inputs
/// whose `value_type_opt` is `Some`.  Skips control, memory, and phi-token
/// slots.  Used in both the evaluator and the cone-order traversal to avoid
/// repeating the `value_type_opt(i).is_some()` filter inline.
pub(crate) fn value_input_producers(
    f: &strider_ir::Function,
    node: NodeId,
) -> impl Iterator<Item = ValueId> + '_ {
    f.node_inputs(node)
        .into_iter()
        .filter(move |&i| f.value_type_opt(i).is_some())
}

/// Abstract value: a concrete number, or `sp_base + offset`.
#[derive(Clone, Copy, PartialEq)]
enum Abs {
    Const(u128),
    SpRel { base: ValueId, offset: i64 },
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
    endianness: Endianness,
    sp_memo: SpExprMemo,
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
            endianness: function.endianness(),
            sp_memo: SpExprMemo::default(),
            map: FxHashMap::default(),
        }
    }

    /// Evaluate `dispatch` over `order` (from [`cone_order`]) with `idx_value`
    /// bound to `idx`. Returns the target as `u64`, or `None` if anything fails
    /// to collapse to a concrete number.
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
            if let Some(a) = self.eval_node(val) {
                self.map.insert(val, a);
            }
        }
        u64::try_from(self.map.get(&dispatch).copied()?.as_const()?).ok()
    }

    fn get(&self, value: ValueId) -> Option<Abs> {
        self.map.get(&value).copied()
    }

    fn eval_node(&mut self, value: ValueId) -> Option<Abs> {
        let f = self.function;
        // An sp-rooted constant expression — InitialVar(sp), an alignment-masked
        // `(sp & mask)`, or either plus a constant `Add` chain — decomposes to
        // its SP terminal + offset via the same decomposer the stores /
        // `reaching_store` use, so the aligned base is recognized and matches
        // the stores' base. Memoized in `sp_memo`, so the load's index-
        // independent sp-spine is computed once and reused across indices.
        if let Some(SpExpr { base, offset }) =
            SpDecomposer::new(f, &mut self.sp_memo).decompose(value)
        {
            return Some(Abs::SpRel { base, offset });
        }
        let node = f.producer(value);
        let kind = *f.node_kind(node);
        let out_ty = f.value_type_opt(value);
        let ins: SmallVec<[ValueId; 2]> = value_input_producers(f, node).collect();
        match kind {
            NodeKind::IntConst(_) => Some(Abs::Const(f.int_const_u128(value)?)),
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add) => {
                self.eval_add(self.get(*ins.first()?)?, self.get(*ins.get(1)?)?, out_ty?)
            }
            NodeKind::Load(_) => self.eval_load(node, value),
            NodeKind::Phi => self.eval_phi(node),
            _ => {
                let resolve = |v| self.get(v).and_then(Abs::as_const);
                crate::const_eval::eval_node_const(
                    self.function,
                    value,
                    &resolve,
                    self.rom,
                    self.endianness,
                )
                .map(Abs::Const)
            }
        }
    }

    /// `Add` in the abstract domain: `Const+Const`, or `SpRel ± Const`.
    fn eval_add(&self, l: Abs, r: Abs, ty: ValueType) -> Option<Abs> {
        match (l, r) {
            (Abs::Const(a), Abs::Const(b)) => Some(Abs::Const(eval_int_binary(
                strider_ir::IntBinaryOp::Add,
                a,
                b,
                ty,
            )?)),
            (Abs::SpRel { base, offset }, Abs::Const(c))
            | (Abs::Const(c), Abs::SpRel { base, offset }) => {
                // Signed interpretation so a negative frame offset (stored as
                // 0xFFFF..) subtracts correctly.
                let delta = i64::try_from(ty.get_signed_int(c)?).ok()?;
                Some(Abs::SpRel { base, offset: offset.wrapping_add(delta) })
            }
            (Abs::SpRel { .. }, Abs::SpRel { .. }) => None,
        }
    }

    /// `LoadReadOnly` (const address) then `LoadForward` (SP-relative).
    fn eval_load(&mut self, node: NodeId, value: ValueId) -> Option<Abs> {
        let f = self.function;
        let load_ty = f.value_type_opt(value)?;
        match self.get(f.load_addr(node))? {
            Abs::Const(c) => {
                let rom = self.rom?;
                let addr = u64::try_from(c).ok()?;
                crate::const_eval::read_rom_const(rom, addr, load_ty, self.endianness)
                    .map(Abs::Const)
            }
            Abs::SpRel { base, offset } => {
                let [mem, _addr] = f.node_inputs_exact::<2>(node).ok()?;
                let load_size = load_ty.byte_size() as i64;
                let reaching = {
                    let mut cfg = SpAliasCfg::call_blocking(&mut self.sp_memo, self.alias_mode);
                    cfg.reaching_store(f, mem, base, offset, load_size)
                }?;
                // Exact anchor: the store must sit at the probed offset.
                if reaching.store_offset != offset {
                    return None;
                }
                // Jump targets are constants on the converged graph.
                let data_ty = f.value_type_opt(reaching.data)?;
                let raw = f.int_const_u128(reaching.data)?;
                Some(Abs::Const(self.reshape(raw, data_ty, load_ty)?))
            }
        }
    }

    /// Reshape a stored constant to a narrower load width (mirrors
    /// `LoadForward::narrow`). Equal widths pass through; load wider than store
    /// → `None`.
    fn reshape(&self, v: u128, data_ty: ValueType, load_ty: ValueType) -> Option<u128> {
        if data_ty == load_ty {
            return Some(v);
        }
        if data_ty.is_integer()
            && load_ty.is_integer()
            && load_ty.byte_size() < data_ty.byte_size()
        {
            let shifted = match self.endianness {
                Endianness::Little => v,
                Endianness::Big => {
                    let shift_bits = ((data_ty.byte_size() - load_ty.byte_size()) as u32) * 8;
                    v >> shift_bits
                }
            };
            return load_ty.get_unsigned_int(shifted);
        }
        None
    }

    /// All-arms-agree: every value arm must resolve to the same `Abs`.
    fn eval_phi(&mut self, node: NodeId) -> Option<Abs> {
        let arms: SmallVec<[ValueId; 4]> =
            value_input_producers(self.function, node).collect();
        let mut agreed: Option<Abs> = None;
        for arm in arms {
            let v = self.get(arm)?;
            match agreed {
                None => agreed = Some(v),
                Some(prev) if prev == v => {}
                Some(_) => return None,
            }
        }
        // Zero-arm phi violates a validator invariant; fail closed (None).
        agreed
    }
}

/// Successor relation for the dispatch-cone walk: a value's successors are its
/// own value-input producers (the memory token is not followed — store data is
/// resolved at eval time via `reaching_store`).  Feeding this backward relation
/// to a post-order walk yields producers before consumers.
struct ValueInputSuccs<'a> {
    function: &'a strider_ir::Function,
}

impl graphwalk::GraphRef for ValueInputSuccs<'_> {
    type NodeId = ValueId;

    fn try_successors(
        &self,
        value: ValueId,
        f: impl FnMut(ValueId) -> std::ops::ControlFlow<()>,
    ) -> std::ops::ControlFlow<()> {
        value_input_producers(self.function, self.function.producer(value)).try_for_each(f)
    }
}

/// The dispatch cone in producers-before-consumers order: backward reachability
/// from `root` over value edges only (see [`ValueInputSuccs`]).  Reuses the
/// shared iterative post-order walk (`graphwalk::PostOrder`), so a deep cone
/// costs O(1) host stack and each value is yielded once; a cycle's back-edge
/// input is simply absent at eval time → `None`.
pub(crate) fn cone_order(function: &strider_ir::Function, root: ValueId) -> Vec<ValueId> {
    graphwalk::entity_postorder(ValueInputSuccs { function }, [root]).collect()
}

#[cfg(test)]
mod tests {
    use super::{Evaluator, cone_order};
    use strider_ir::IRBuilderExt;
    use strider_ir::IntBinaryOp;
    use strider_ir::node::ValueType;
    use strider_ir_test_utils::{RegisterSet, reg_vn};

    // Build `Add(idx, 100):I64` where `idx` is a tracked register read
    // (non-const so leaving it unseeded genuinely fails to resolve).
    // Returns (function, idx ValueId, sum ValueId).
    fn build_add_idx_100() -> (
        strider_ir::Function,
        strider_ir::node::ValueId,
        strider_ir::node::ValueId,
    ) {
        let vn = reg_vn(0x1000, 8); // 8-byte (I64) register
        let mut b = RegisterSet::new()
            .tracked(vn)
            .arg(vn)
            .build_fn_single_region()
            .expect("build_fn_single_region");
        let idx = b.read_variable(&vn).expect("read_variable");
        let c100 = b.build_int_const(100u64, ValueType::I64).expect("build_int_const");
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
        let order = cone_order(&function, sum);
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        assert_eq!(ev.eval_target(&order, sum, idx, 5), Some(105));
        assert_eq!(ev.eval_target(&order, sum, idx, 7), Some(107)); // re-seed, fresh map
    }

    #[test]
    fn unseeded_index_is_none() {
        let (function, _idx, sum) = build_add_idx_100();
        let order = cone_order(&function, sum);
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        // Seeding dispatch=sum directly returns the seed without evaluating the cone.
        assert_eq!(ev.eval_target(&order, sum, sum, 5), Some(5)); // sum seeded directly
        // A fresh eval where nothing relevant is seeded:
        let const_100 = sum_unrelated_leaf(&function, sum);
        assert_eq!(ev.eval_target(&order, sum, const_100, 5), None);
    }

    /// Returns the IntConst(100) value — it is in the cone of `sum` but is not
    /// the idx leaf, so seeding it instead of idx leaves idx symbolic and the
    /// sum cannot collapse to a concrete number.
    fn sum_unrelated_leaf(
        f: &strider_ir::Function,
        sum: strider_ir::node::ValueId,
    ) -> strider_ir::node::ValueId {
        use strider_ir::IRViewer;
        // The Add node's inputs are [idx, IntConst(100)].
        let add_node = f.producer(sum);
        let inputs = f.node_inputs(add_node);
        // Find the IntConst(100) input — that's the one that is NOT idx (the
        // non-const register read).  `int_const_u128` returns Some only for
        // IntConst nodes, so this reliably picks the constant operand.
        for input in inputs {
            if f.int_const_u128(input) == Some(100) {
                return input;
            }
        }
        panic!("could not find IntConst(100) among Add inputs");
    }
}
