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

use crate::sp_expr::SpAliasCfg;

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

    /// Evaluate `dispatch` over `order` (producers-before-consumers) with
    /// `idx_value` bound to `idx`. Returns the target as `u64`, or `None` if
    /// anything fails to collapse to a concrete number.
    ///
    /// **Fail-fast:** every node in `order` is a value-ancestor of `dispatch`
    /// (the cone is `dispatch`'s backward slice), so the first node that does
    /// not fold to an [`Abs`] means `dispatch` cannot be constant either —
    /// return `None` at once rather than evaluating the rest of the cone.  A
    /// candidate that *does* resolve never hits a `None` node, so this is exact
    /// (same result, and it skips the bulk of a large not-a-table cone whose
    /// index-independent leaves fail early).
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
        // The SP spine is identified STRUCTURALLY from the already-evaluated
        // inputs, not by re-running `SpAliasCfg.decompose` (a full cone walk)
        // on every node of every fold.  On the converged graph the only SP
        // shapes are the terminal `InitialVar(sp)`, `Add(sp-rooted, const)`
        // (handled by `eval_add` from the operands' `Abs`), and the alignment
        // base `And(sp-rooted, alignmask)`.  `reaching_store` still owns the
        // memory-SSA store lookup for SP-relative loads (rare); this just drops
        // the redundant per-node classification.
        if matches!(kind, NodeKind::InitialVar(id) if f.initial_vn(id) == f.default_cc().stack_vn) {
            return Some(Abs::SpRel {
                base: value,
                offset: 0,
            });
        }
        let out_ty = f.value_type_opt(value);
        let ins: SmallVec<[ValueId; 2]> = value_input_producers(f, node).collect();
        match kind {
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::Add) => self.eval_add(
                value,
                self.get(*ins.first()?)?,
                self.get(*ins.get(1)?)?,
                out_ty?,
            ),
            // Alignment base `(sp-rooted & mask)`: a fresh opaque SP base
            // (offset 0), matching `SpAnalyzer::classify_sp_node`'s And arm.
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And) => {
                let [l, r] = [*ins.first()?, *ins.get(1)?];
                let sp_operand = if f.int_const_u128(r).is_some_and(crate::sp_expr::is_alignment_mask)
                {
                    l
                } else if f.int_const_u128(l).is_some_and(crate::sp_expr::is_alignment_mask) {
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

    /// Route a pure-constant node through the shared
    /// [`crate::const_eval::eval_node_const`] SSoT, resolving each input from
    /// the abstract map's already-computed `Const` facts.  This is the single
    /// const-domain fold for every kind the abstract evaluator does not layer
    /// the `SpRel` domain on top of (and the `Const` arms of `eval_add` /
    /// `eval_load`).
    fn eval_const_node(&self, value: ValueId) -> Option<Abs> {
        let resolve = |v| self.get(v).and_then(Abs::as_const);
        crate::const_eval::eval_node_const(self.function, value, &resolve, self.rom).map(Abs::Const)
    }

    /// `Add` in the abstract domain: `Const+Const` (routed through the shared
    /// `const_eval` resolver via [`Self::eval_const_node`]), or `SpRel ± Const`.
    fn eval_add(&self, value: ValueId, l: Abs, r: Abs, ty: ValueType) -> Option<Abs> {
        match (l, r) {
            // Const+Const is exactly what `eval_node_const`'s Add arm computes
            // from the resolved inputs — route it through the SSoT rather than
            // re-deriving `eval_int_binary` here.
            (Abs::Const(_), Abs::Const(_)) => self.eval_const_node(value),
            (Abs::SpRel { base, offset }, Abs::Const(c))
            | (Abs::Const(c), Abs::SpRel { base, offset }) => {
                // Signed interpretation so a negative frame offset (stored as
                // 0xFFFF..) subtracts correctly.
                let delta = ty.get_signed_int(c)?;
                Some(Abs::SpRel {
                    base,
                    offset: offset.wrapping_add(delta),
                })
            }
            (Abs::SpRel { .. }, Abs::SpRel { .. }) => None,
        }
    }

    /// `LoadReadOnly` (const address) then `LoadForward` (SP-relative).
    fn eval_load(&mut self, node: NodeId, value: ValueId) -> Option<Abs> {
        let f = self.function;
        let load_ty = f.value_type_opt(value)?;
        match self.get(f.load_addr(node))? {
            // Const-address ROM read — exactly the `Load(RAM)` arm of
            // `eval_node_const`, so route it through that SSoT (the resolver
            // re-reads the already-`Const` address from the map).
            Abs::Const(_) => self.eval_const_node(value),
            Abs::SpRel { base, offset } => {
                let [mem, _addr] = f.node_inputs_exact::<2>(node).ok()?;
                let load_size = load_ty.byte_size() as i128;
                let reaching = {
                    let cfg = SpAliasCfg::call_blocking(self.alias_mode);
                    cfg.reaching_store(f, mem, base, offset, load_size)
                }?;
                // Exact anchor: the store must sit at the probed offset.
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

    /// Reshape a stored constant to a narrower load width (mirrors
    /// `LoadForward::narrow`). Equal widths pass through; load wider than store
    /// → `None`.
    fn reshape(&self, v: u128, data_ty: ValueType, load_ty: ValueType) -> Option<u128> {
        if data_ty == load_ty {
            return Some(v);
        }
        if data_ty.is_integer() && load_ty.is_integer() && load_ty.byte_size() < data_ty.byte_size()
        {
            // SSoT for the endianness-aware byte-slice shift (shared with
            // `LoadForward::narrow`); LE → 0, so the low bytes pass through.
            let shift_bits =
                crate::sp_expr::high_low_shift_bits(data_ty, load_ty, self.function.endianness());
            return load_ty.get_unsigned_int(v >> shift_bits);
        }
        None
    }

    /// All-arms-agree: every value arm must resolve to the same `Abs`.
    fn eval_phi(&mut self, node: NodeId) -> Option<Abs> {
        let arms: SmallVec<[ValueId; 4]> = value_input_producers(self.function, node).collect();
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

/// Successor relation for the *unpruned* dispatch-cone walk (test-only — the
/// classifier now decomposes the addressing structurally and only ever folds
/// over the index-pruned cone): a value's successors are its own value-input
/// producers.  Feeding this backward relation to a post-order walk yields
/// producers before consumers.
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
        value_input_producers(self.function, self.function.producer(value)).try_for_each(f)
    }
}

/// The full dispatch cone in producers-before-consumers order (test-only; see
/// [`ValueInputSuccs`]).
#[cfg(test)]
pub(crate) fn cone_order(function: &strider_ir::Function, root: ValueId) -> Vec<ValueId> {
    graph_algorithms::walk::entity_postorder(ValueInputSuccs { function }, [root]).collect()
}

/// Like [`ValueInputSuccs`] but treats `stop` as a leaf: its value-input
/// producers are NOT followed.  Used to prune a dispatch cone at the index
/// value, which is pinned to a concrete constant during evaluation and so has
/// no need of its own upstream producers.
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
        // The pinned index is a leaf: stop the backward walk here so its
        // (irrelevant, once-constant) producer cone is never visited.
        if value == self.stop {
            return std::ops::ControlFlow::Continue(());
        }
        value_input_producers(self.function, self.function.producer(value)).try_for_each(f)
    }
}

/// The dispatch cone in producers-before-consumers order, **pruned at `stop`**:
/// backward reachability from `root` over value edges, but never descending
/// through `stop`'s own inputs.  `stop` (the index value) is pinned to a
/// concrete constant at eval time, so its entire upstream computation — which
/// on a real dispatch is the instruction-decode chain that produces the index,
/// often thousands of nodes — is dead weight and must not be walked or
/// re-evaluated per fold.  Evaluating over this pruned cone is O(nodes on the
/// index→dispatch path) instead of O(whole backward slice from `root`).
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

    // `dispatch = (idx_raw + 5) + 100`.  Pin the INNER `idx = idx_raw + 5` as
    // the dispatch index.  Once `idx` is a constant, its ancestor `idx_raw`
    // (a symbolic register read) is irrelevant — the pruned cone must stop at
    // `idx` and never walk into `idx_raw`, yet still evaluate to the right
    // target.  This is the exact shape that made the real cone 7,101 nodes:
    // the index's whole upstream decode chain hanging off it, dead weight once
    // the index is pinned.
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

        // The full backward cone reaches the index's ancestor; the pruned one
        // stops at `idx` and must NOT contain it.
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

        // And it still evaluates correctly for several pinned indices.
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        assert_eq!(ev.eval_target(&pruned, dispatch, idx, 7), Some(107));
        assert_eq!(ev.eval_target(&pruned, dispatch, idx, 8), Some(108));
    }

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
        // The evaluator's contract (fail-fast) is that `order` is `dispatch`'s
        // cone pruned at the index — exactly what the classifier passes.
        let order = cone_order_pruned(&function, sum, idx);
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        assert_eq!(ev.eval_target(&order, sum, idx, 5), Some(105));
        assert_eq!(ev.eval_target(&order, sum, idx, 7), Some(107)); // re-seed, fresh map
    }

    #[test]
    fn unseeded_index_is_none() {
        let (function, _idx, sum) = build_add_idx_100();
        let mut ev = Evaluator::new(&function, None, crate::AliasMode::default());
        // Seeding dispatch=sum directly: its pruned-at-sum cone is just `[sum]`.
        let order_sum = cone_order_pruned(&function, sum, sum);
        assert_eq!(ev.eval_target(&order_sum, sum, sum, 5), Some(5));
        // Seeding an unrelated leaf leaves the real index unresolved: the
        // register read fails to fold, and fail-fast reports the branch `None`.
        let const_100 = sum_unrelated_leaf(&function, sum);
        let order = cone_order_pruned(&function, sum, const_100);
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
