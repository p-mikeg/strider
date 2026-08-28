//! Read-only abstract evaluation of a jump-table dispatch cone: the concrete
//! branch target for a concrete index, computed in producers-before-consumers
//! order.
//!
//! An abstract value is either a concrete number or SP-relative, because the SP
//! is symbolic and a stack address cannot be a pure number.  Three foldings do
//! the work: ConstFold arithmetic, a constant-address ROM read, and an
//! SP-relative load resolved against the [`SlotMap`] of the stores above it.
//! Any unresolved value, a non-const dispatch result, or a cycle yields `None`.

use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use strider_ir::node::{NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{IRViewer, ReadOnlyMemory};

use crate::mem_analysis::{MemAnalyzer, MemExpr, MemKind, MemOptions, store_value_byte_size};

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
    map: FxHashMap<ValueId, Abs>,
    /// One [`SlotMap`] per probe point, keyed by `(memory token, stack base)`.
    /// Survives [`Self::begin_index`]: the memory segment above a load does not
    /// depend on the index.
    slot_maps: FxHashMap<(ValueId, ValueId), SlotMap>,
    /// Held for the same reason `slot_maps` is: it carries an arg-window memo,
    /// and rebuilding it per probe discards that on every table index.
    off_segment: MemAnalyzer,
}

impl<'a> Evaluator<'a> {
    pub(crate) fn new(
        function: &'a strider_ir::Function,
        rom: Option<&'a dyn ReadOnlyMemory>,
        stack_global_disjoint: bool,
    ) -> Self {
        Self {
            function,
            rom,
            map: FxHashMap::default(),
            slot_maps: FxHashMap::default(),
            off_segment: MemAnalyzer::new(MemOptions::call_blocking(stack_global_disjoint)),
        }
    }

    /// Opens an index: drops the previous index's values and pins `idx_value`.
    ///
    /// The boundary is the INDEX, not the root.  A branch's target and ISA-mode
    /// cones overlap almost entirely, so a per-root reset re-folds the shared
    /// dispatch load and everything under it.
    pub(crate) fn begin_index(&mut self, idx_value: ValueId, idx: u128) {
        self.map.clear();
        self.map.insert(idx_value, Abs::Const(idx));
    }

    /// Evaluates `dispatch` over `order` against the open index.
    ///
    /// Bailing on the first non-folding node is exact, not just an
    /// optimization: every node in `order` is a value-ancestor of `dispatch`,
    /// so one that fails to fold means `dispatch` cannot be constant either.
    pub(crate) fn eval_root(&mut self, order: &[ValueId], dispatch: ValueId) -> Option<u64> {
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
                let data = self.reaching_store_data(mem, base, offset, load_size)?;
                // Jump targets are constants on the converged graph.
                let data_ty = f.value_type_opt(data)?;
                let raw = f.int_const_u128(data)?;
                Some(Abs::Const(self.reshape(raw, data_ty, load_ty)?))
            }
        }
    }

    /// The data of the store anchored exactly at the probed slot, or `None`
    /// when the reaching store is elsewhere or is not a store at all.
    ///
    /// Answered from the probe point's [`SlotMap`] where the segment covers it,
    /// which is what keeps an n-entry stack table off an O(n) walk per entry;
    /// past the segment it is the plain [`MemAnalyzer::reaching_store`] query.
    fn reaching_store_data(
        &mut self,
        mem: ValueId,
        base: ValueId,
        offset: i128,
        size: i128,
    ) -> Option<ValueId> {
        let f = self.function;
        let map = self
            .slot_maps
            .entry((mem, base))
            .or_insert_with(|| SlotMap::build(f, mem, base));
        let reaching = map.reaching(offset, size);
        match reaching {
            Reaching::Store(store) => Some(f.store_data(store)),
            Reaching::Absent => None,
            Reaching::OffSegment => {
                let hit = self
                    .off_segment
                    .reaching_store(f, mem, base, offset, size)?;
                // The store must be anchored exactly at the probed offset.
                (hit.store_offset == offset).then(|| hit.data(f))
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
            let shift_bits = crate::mem_analysis::high_low_shift_bits(
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

#[cfg(test)]
thread_local! {
    /// Memory-chain nodes visited building [`SlotMap`]s, the unit the
    /// stack-table cost test counts alongside `mem_analysis::WALK_STEPS`.
    pub(crate) static SLOT_MAP_STEPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One store on the segment, covering some byte.
#[derive(Clone, Copy)]
struct Claim {
    /// Position along the chain, nearest first.
    rank: u32,
    /// The store's own anchor offset, which a probe must match exactly.
    offset: i128,
    node: NodeId,
}

/// Chain steps one [`SlotMap`] build may spend across its whole arm tree, and
/// how deep the arms may nest: a join under a join multiplies the arms, and a
/// loop's back-edge arm re-enters the token it came from.  Exhausting either
/// leaves that arm incomplete, which falls its probes back to a real walk.
const SLOT_MAP_BUDGET: u32 = 4096;
const MAX_JOIN_DEPTH: u32 = 8;

/// Which store an SP-relative probe at one memory token reaches, for every
/// offset, without walking the chain per probe.
///
/// Covers the straight-line RAM segment above the probe point whose stores all
/// root at one stack base.  It ends at the first def whose effect is
/// probe-dependent (a `Call`, a store rooted at another stack base, a global,
/// or an opaque pointer), because there the reaching store cannot be read off
/// an offset; probes landing past the end fall back to a real walk.  A
/// heap-rooted store is stepped over.
///
/// A `MemPhi` is path-dependent, not probe-dependent: the segment ends there
/// too, but each arm gets its own map and a probe past the segment is answered
/// by agreement across them.  Without that, a probe below a join falls back to
/// a per-probe walk of the whole chain, which is what the map exists to avoid.
struct SlotMap {
    /// Byte offset from the base -> the nearest store covering that byte.
    claims: FxHashMap<i128, Claim>,
    /// The segment bottomed out at `InitialMemory`, so an unclaimed byte has no
    /// reaching store rather than one beyond the mapped part.
    complete: bool,
    /// Per-arm maps of the `MemPhi` the segment ended at, empty otherwise.
    arms: Vec<SlotMap>,
}

enum Reaching {
    /// A store anchored exactly at the probed offset.
    Store(NodeId),
    /// No such store, so the probe cannot fold.
    Absent,
    /// Past the mapped segment; only a walk can answer.
    OffSegment,
}

impl SlotMap {
    fn build(function: &strider_ir::Function, mem: ValueId, base: ValueId) -> Self {
        let mut budget = SLOT_MAP_BUDGET;
        Self::build_within(function, mem, base, &mut budget, 0)
    }

    fn build_within(
        function: &strider_ir::Function,
        mem: ValueId,
        base: ValueId,
        budget: &mut u32,
        depth: u32,
    ) -> Self {
        let mut claims: FxHashMap<i128, Claim> = FxHashMap::default();
        let mut cur = Some(mem);
        let mut rank = 0u32;
        let mut complete = false;
        let mut arms: Vec<SlotMap> = Vec::new();
        while let Some(v) = cur {
            let Some(left) = budget.checked_sub(1) else {
                break;
            };
            *budget = left;
            #[cfg(test)]
            SLOT_MAP_STEPS.with(|c| c.set(c.get() + 1));
            let node = function.producer(v);
            match *function.node_kind(node) {
                NodeKind::InitialMemory => {
                    complete = true;
                    break;
                }
                NodeKind::MemPhi if depth < MAX_JOIN_DEPTH => {
                    arms = function
                        .phi_data_inputs(node)
                        .collect::<SmallVec<[ValueId; 4]>>()
                        .into_iter()
                        .map(|arm| Self::build_within(function, arm, base, budget, depth + 1))
                        .collect();
                    break;
                }
                NodeKind::Store(space) if space == rsleigh::VnSpace::RAM => {
                    match crate::mem_analysis::decompose(function, function.store_addr(node)) {
                        Some(MemExpr {
                            base: store_base,
                            offset,
                            kind: MemKind::Stack,
                        }) if store_base == base => {
                            let size = store_value_byte_size(function, function.store_data(node));
                            for byte in 0..size {
                                let Some(at) = offset.checked_add(byte) else {
                                    break;
                                };
                                claims.entry(at).or_insert(Claim { rank, offset, node });
                            }
                            rank += 1;
                        }
                        // No allocation overlaps the stack.
                        Some(MemExpr {
                            kind: MemKind::Heap | MemKind::HeapOpaque,
                            ..
                        }) => {}
                        // Another stack base, a global, or an opaque pointer:
                        // whether it clobbers is the probe's business.
                        _ => break,
                    }
                }
                // The probed location is RAM (see `MemAnalyzer::reaching_store`),
                // so another space cannot alias it.
                NodeKind::Store(_) => {}
                _ => break,
            }
            cur = function.memory_input_of(node);
        }
        Self {
            claims,
            complete,
            arms,
        }
    }

    /// A store not anchored at the probe but overlapping it hides everything
    /// behind it, which is [`Reaching::Absent`] just as a non-matching
    /// `reaching_store` is `None`.
    fn reaching(&self, offset: i128, size: i128) -> Reaching {
        // The nearest store covering any probed byte is the nearest store
        // overlapping the probe at all.
        let nearest = (0..size)
            .filter_map(|byte| self.claims.get(&offset.checked_add(byte)?))
            .min_by_key(|claim| claim.rank);
        match nearest {
            Some(claim) if claim.offset == offset => Reaching::Store(claim.node),
            Some(_) => Reaching::Absent,
            None if self.complete => Reaching::Absent,
            None if self.arms.is_empty() => Reaching::OffSegment,
            None => self.reaching_through_arms(offset, size),
        }
    }

    /// The probe folds only if every incoming path reaches the SAME store, so
    /// the value the load reads does not depend on the path.  An arm that
    /// cannot answer leaves the whole probe on the walk.
    fn reaching_through_arms(&self, offset: i128, size: i128) -> Reaching {
        let mut agreed: Option<NodeId> = None;
        for arm in &self.arms {
            match arm.reaching(offset, size) {
                Reaching::Store(store) if agreed.is_none_or(|prev| prev == store) => {
                    agreed = Some(store);
                }
                Reaching::Store(_) | Reaching::Absent => return Reaching::Absent,
                Reaching::OffSegment => return Reaching::OffSegment,
            }
        }
        agreed.map_or(Reaching::Absent, Reaching::Store)
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

        let mut ev = Evaluator::new(&function, None, true);
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
        let mut ev = Evaluator::new(&function, None, true);
        ev.begin_index(idx, 5);
        assert_eq!(ev.eval_root(&order, sum), Some(105));
        ev.begin_index(idx, 7); // fresh map
        assert_eq!(ev.eval_root(&order, sum), Some(107));
    }

    #[test]
    fn unseeded_index_is_none() {
        let (function, _idx, sum) = build_add_idx_100();
        let mut ev = Evaluator::new(&function, None, true);
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
