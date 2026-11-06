//! Table dispatch: a switch lowers to `branch f(index)` for one bounded index.
//! The index, or a phi of constants every input of the dispatch cone passes
//! through, is enumerated and the cone folded per value through
//! [`eval`](super::eval). Every value must fold, or the site defers: a
//! `Multiple` missing a runtime target drops an edge.
//!
//! Known limit: a read guard's range is trusted as the table's bound even
//! where an unread or lost test is tighter, and a test reading the cell
//! through a phi reads no bits of it. Each seats cells the code rules out.

use super::index_range::{Interval, MAX_WALK, ScaleOp, Step, scale_of, strip_to_cell, width_mask};
use super::{MAX_TABLE_ENTRIES, SiteClassifier};
use strider_cfg::{ResolvedTarget, ResolvedTargets};
use strider_ir::IRViewer;
use strider_ir::IntBinaryOp;
use strider_ir::eval;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};

/// A constant address term below this is a field offset, never a table base:
/// what an image maps in its first page is a PIE's headers or ARM's vectors.
const FIRST_PAGE_END: u64 = 0x1000;

/// The value the enumeration pins: `index` takes `x - rebase` for each `x` in
/// `range`. A non-zero `rebase` means the bound is on a guarded sibling
/// `Add(index, rebase)`, in whose coordinate `range` is.
#[derive(Clone, Copy)]
struct PinnedIndex {
    index: ValueId,
    range: Interval,
    rebase: u128,
    /// `index`'s type mask, the width `rebase` wraps at.
    mask: u128,
}

impl SiteClassifier<'_> {
    /// The table-dispatch arm over `target_value`, the dispatch value of `site`
    /// (an `IndirectBranch` or seated `Switch`), which scopes the range queries.
    pub(crate) fn classify_dispatch_value(
        &mut self,
        site: NodeId,
        target_value: ValueId,
        mode_value: Option<ValueId>,
    ) -> Option<ResolvedTargets> {
        let domain = self.dispatch_domain(target_value, site, false)?;
        if let Some(seated) = self.seat(target_value, mode_value, &domain) {
            return Some(seated);
        }
        // An index below a cell that seats nothing falls back to the cell's.
        let shallow = self.dispatch_domain(target_value, site, true)?;
        if shallow == domain {
            return None;
        }
        self.seat(target_value, mode_value, &shallow)
    }

    /// The table `target_value` and `mode_value` fold to per value of the
    /// pinned index; `None` when one does not fold or no table results.
    fn seat(
        &mut self,
        target_value: ValueId,
        mode_value: Option<ValueId>,
        &(pinned, ref values): &(ValueId, Vec<u128>),
    ) -> Option<ResolvedTargets> {
        let function = self.function;
        let ev = &mut self.eval;
        let pruned = super::eval::cone_order_pruned(function, target_value, pinned);
        // An interworking branch's committed mode, evaluated per arm.
        let mode = mode_value.map(|mv| (mv, super::eval::cone_order_pruned(function, mv, pinned)));
        let targets: Vec<ResolvedTarget> = values
            .iter()
            .map(|&x| {
                // The target and mode roots share this index's memo.
                ev.begin_index(pinned, x);
                let addr = ev.eval_root(&pruned, target_value)?;
                let isa_bit = match &mode {
                    // A committed mode that does not fold defers the branch.
                    Some((mv, order)) => Some(ev.eval_root(order, *mv)? != 0),
                    // The mode flowing into the branch.
                    None => None,
                };
                Some(ResolvedTarget::new(addr, isa_bit))
            })
            .collect::<Option<_>>()?;
        // `table` drops exact duplicates, so two arms left on one address
        // disagree on mode, which defers the site.
        let seated = ResolvedTargets::table(targets);
        let arms = seated.arms();
        // `table([])` is `Withdrawn`, which would overrule a caller's seed.
        (!arms.is_empty() && arms.windows(2).all(|w| w[0].addr != w[1].addr)).then_some(seated)
    }

    /// The value to pin for `target` and the values to pin it to: the constants
    /// of the phi every dispatch-cone input passes through, else a bounded
    /// index across its range. With both, constants outside the index range are
    /// dropped; if the index does not fold under some constant, the range wins.
    /// `shallow` keeps the bounded index nearest the target.
    fn dispatch_domain(
        &mut self,
        target: ValueId,
        site: NodeId,
        shallow: bool,
    ) -> Option<(ValueId, Vec<u128>)> {
        let function = self.function;
        let chain = dominator_chain(function, target)?;
        let source = chain.last().copied().unwrap_or(target);
        let constants = super::index_range::phi_constants(
            function,
            source,
            usize::try_from(MAX_TABLE_ENTRIES).ok()?,
        )
        .map(|mut constants| {
            let site_range = self.ranges.range_of(source, site);
            constants.retain(|&k| site_range.contains(k) && !self.ranges.excludes(source, k, site));
            constants
        });
        let Some(hit) = self.decompose_index(&chain, site, shallow) else {
            return constants.map(|values| (source, values));
        };
        if let Some(values) = constants.and_then(|k| self.constants_in_range(k, hit, source)) {
            return Some((source, values));
        }
        // `stride` divides the value spacing; one wider than `usize` defers.
        let step = usize::try_from(hit.range.stride()).ok()?;
        // Known bits and equality guards exclude values inside the interval.
        let scales = self.ranges.index_scales(hit.index);
        let values = (hit.range.lo()..=hit.range.hi())
            .step_by(step)
            .map(|x| x.wrapping_sub(hit.rebase) & hit.mask)
            .filter(|&x| {
                self.ranges.admits_known_bits(hit.index, x)
                    && !scales.iter().any(|&(v, factor)| {
                        x.is_multiple_of(factor) && self.ranges.excludes(v, x / factor, site)
                    })
            })
            .collect();
        Some((hit.index, values))
    }

    /// The `constants` `source` takes whose index `hit`'s range keeps, `None`
    /// when the index does not fold under one of them.
    fn constants_in_range(
        &mut self,
        constants: Vec<u128>,
        hit: PinnedIndex,
        source: ValueId,
    ) -> Option<Vec<u128>> {
        let order = super::eval::cone_order_pruned(self.function, hit.index, source);
        let mut kept = Vec::new();
        for k in constants {
            self.eval.begin_index(source, k);
            let index = self.eval.eval_root(&order, hit.index)?;
            if hit
                .range
                .contains(u128::from(index).wrapping_add(hit.rebase) & hit.mask)
            {
                kept.push(k);
            }
        }
        Some(kept)
    }

    /// The bounded index nearest the target in `chain`, the target's dominators
    /// in its variability cone. Dominance excludes a bypassed sub-branch: in a
    /// rotate `(x<<2) | (x>>30)`, `x>>30` is tightest but does not dominate.
    fn decompose_index(
        &mut self,
        chain: &[ValueId],
        site: NodeId,
        shallow: bool,
    ) -> Option<PinnedIndex> {
        let function = self.function;
        let first_hit = |this: &mut Self, from: usize| {
            chain
                .iter()
                .enumerate()
                .skip(from)
                .find_map(|(depth, &v)| this.bounded_index(site, v).map(|hit| (depth, hit)))
        };
        let shallowest = first_hit(self, 0)?;
        // A candidate stripping to a masked cell of a foldable table bounds the
        // table DATA; the index is a bounded value below that load, in its
        // address.
        let (depth, hit) = foldable_cell_load(function, shallowest.1.index)
            .filter(|_| !shallow)
            .and_then(|cell| chain.iter().position(|&v| v == cell))
            .and_then(|at| first_hit(self, at + 1))
            .unwrap_or(shallowest);
        offsets_keep_range(function, &chain[..depth], hit.index, hit.range).then_some(hit)
    }

    /// Can a table index be enumerated over `iv`, the range of `value` at
    /// `site`? `mask` is the index type's mask.
    fn enumerable(&mut self, site: NodeId, mask: u128, value: ValueId, iv: Interval) -> bool {
        if !index_bound_ok(mask, iv) {
            return false;
        }
        let narrowed = iv != self.ranges.kb_bound(value);
        !self.is_width_only(site, value, iv, narrowed)
            && !self
                .ranges
                .guard_left_unconnected(value, site, iv, narrowed)
    }

    /// `v` as an enumerable index, bounded by its own range or by the tightest
    /// guarded sibling `v + k` outside the dispatch cone (a `switch` over a
    /// negative case base, once `k` folds into the table base).
    fn bounded_index(&mut self, site: NodeId, v: ValueId) -> Option<PinnedIndex> {
        let function = self.function;
        debug_assert!(
            function.int_const_u128(v).is_none(),
            "the dominator chain skips constants"
        );
        let ty = function.value_type_opt(v).filter(|t| t.is_integer())?;
        let mask = ty.int_mask_u128()?;
        let iv = self.ranges.range_of(v, site);
        if self.enumerable(site, mask, v, iv) {
            return Some(PinnedIndex {
                index: v,
                range: iv,
                rebase: 0,
                mask,
            });
        }
        let ranges = &self.ranges;
        let sums = std::mem::take(self.sums.entry(v).or_insert_with(|| {
            function
                .value_uses(v)
                .filter_map(|(user, _)| {
                    let (operand, addend) = crate::const_eval::var_and_const_of(
                        function,
                        user,
                        NodeKind::IntBinaryOp(IntBinaryOp::Add),
                    )?;
                    let sum = function.first_value_output_of(user)?;
                    (operand == v
                        && function.value_type_opt(sum) == Some(ty)
                        && ranges.may_bound_sum(sum))
                    .then_some((sum, addend))
                })
                .collect()
        }));
        let best = sums
            .iter()
            .filter_map(|&(sum, addend)| {
                let iv = self.ranges.range_of(sum, site);
                self.enumerable(site, mask, sum, iv).then_some(PinnedIndex {
                    index: v,
                    range: iv,
                    rebase: addend,
                    mask,
                })
            })
            .min_by_key(|hit| hit.range.count());
        self.sums.insert(v, sums);
        best
    }

    /// Does `iv` cover every value `v`'s cell can produce (the [`strip_to_cell`]
    /// steps replayed, wrapping per [`wrap`]), making `v` table data rather than
    /// an index? An uncountable step or a strip past [`MAX_WALK`] answers `true`;
    /// a field read through a pointer asks `cell_test_left_unread` instead.
    fn is_width_only(&mut self, site: NodeId, v: ValueId, iv: Interval, narrowed: bool) -> bool {
        let function = self.function;
        let Some(strip) = strip_to_cell(function, v, |and| {
            self.ranges.identity_mask_operand(and).is_some()
        }) else {
            return true;
        };
        let base = strip.cell;
        if self.loads_a_field(site, base) {
            return self
                .ranges
                .cell_test_left_unread(v, &strip, site, iv, narrowed);
        }
        let Some(w) = function
            .value_type_opt(base)
            .map(|t| t.bit_width())
            .filter(|&w| w < 128)
        else {
            return true;
        };
        let mut count: u128 = eval::bit(w as u32);
        let mut spacing: u128 = 1;
        for &(value, step) in strip.steps.iter().rev() {
            if let Step::Truncate = step {
                match wrap(count, spacing, width_mask(function, value)) {
                    Some(wrapped) => (count, spacing) = wrapped,
                    None => return true,
                }
                continue;
            }
            match scale_of(step) {
                Some(ScaleOp::Mul(m)) => match spacing.checked_mul(m) {
                    Some(s) => spacing = s,
                    None => return true,
                },
                Some(ScaleOp::Udiv(c)) if spacing.is_multiple_of(c) => spacing /= c,
                Some(ScaleOp::Udiv(c)) if c.is_multiple_of(spacing) => {
                    count = count.div_ceil(c / spacing);
                    spacing = 1;
                }
                Some(ScaleOp::Udiv(_)) => return true,
                None => {}
            }
        }
        // A widening scale is modular at the output width too.
        match wrap(count, spacing, width_mask(function, v)) {
            Some((wrapped, _)) => iv.count() >= wrapped,
            None => true,
        }
    }

    /// Whether `v` is a load through a pointer: a non-constant address none of
    /// whose added terms is bounded at `site` like a table index or is a
    /// constant the ROM serves at or past [`FIRST_PAGE_END`], a table base.
    fn loads_a_field(&mut self, site: NodeId, v: ValueId) -> bool {
        let function = self.function;
        if !matches!(function.kind_of_value(v), NodeKind::Load(_)) {
            return false;
        }
        let Some(addr) = function.int_inputs(v).next() else {
            return false;
        };
        if function.int_const_u128(addr).is_some() {
            return false;
        }
        let mut stack = vec![addr];
        for _ in 0..MAX_WALK {
            let Some(term) = stack.pop() else {
                return true;
            };
            if let Some(c) = function.int_const_u128(term) {
                let served = u64::try_from(c)
                    .ok()
                    .filter(|&c| c >= FIRST_PAGE_END)
                    .zip(self.eval.rom())
                    .is_some_and(|(c, rom)| rom.try_read(c, &mut [0u8]));
                if served {
                    return false;
                }
                continue;
            }
            if matches!(
                function.kind_of_value(term),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            ) {
                stack.extend(function.int_inputs(term));
            } else if self.ranges.range_of(term, site).count() <= u128::from(MAX_TABLE_ENTRIES) {
                return false;
            }
        }
        stack.is_empty()
    }
}

/// The values that dominate `target` in its variability cone, nearest first,
/// or `None` when `target` is a constant.
///
/// The cone passes through a load [`foldable_load_address`] accepts and stops
/// at any other load, phis and opaque sources.
fn dominator_chain(function: &strider_ir::Function, target: ValueId) -> Option<Vec<ValueId>> {
    // `None` is the virtual ENTRY.
    let mut succs: rustc_hash::FxHashMap<Option<ValueId>, Vec<Option<ValueId>>> =
        rustc_hash::FxHashMap::default();
    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut stack = vec![target];
    while let Some(v) = stack.pop() {
        if function.int_const_u128(v).is_some() || !seen.insert(v) {
            continue;
        }
        let inputs: Vec<ValueId> = match function.kind_of_value(v) {
            NodeKind::Load(_) => foldable_load_address(function, v).into_iter().collect(),
            NodeKind::InitialVar(_)
            | NodeKind::Phi
            | NodeKind::Call { .. }
            | NodeKind::CallOther { .. }
            | NodeKind::New
            | NodeKind::CPoolRef => Vec::new(),
            _ => function.int_inputs(v).collect(),
        };
        let mut has_var_input = false;
        for p in inputs {
            // A decomposed base is skipped like a const, or it would be a
            // second root the index does not dominate.
            if is_base_operand(function, p) {
                continue;
            }
            has_var_input = true;
            succs.entry(Some(p)).or_default().push(Some(v));
            stack.push(p);
        }
        if !has_var_input {
            succs.entry(None).or_default().push(Some(v));
        }
    }
    if !seen.contains(&target) {
        return None;
    }
    let doms =
        graph_algorithms::dominance::dominators(None, |v| succs.remove(&v).unwrap_or_default());

    let mut chain: Vec<ValueId> = Vec::new();
    let mut at = target;
    while let Some(Some(up)) = doms.immediate_dominator(Some(at)) {
        chain.push(up);
        at = up;
    }
    Some(chain)
}

/// The load `v` strips to ([`strip_to_cell`]), when the evaluator can fold it.
fn foldable_cell_load(function: &strider_ir::Function, v: ValueId) -> Option<ValueId> {
    let cell = strip_to_cell(function, v, |_| true)?.cell;
    (matches!(function.kind_of_value(cell), NodeKind::Load(_))
        && foldable_load_address(function, cell).is_some())
    .then_some(cell)
}

/// Whether the constant offsets applied to `index` on its way to the address
/// keep `range` a contiguous unsigned interval.
///
/// `shallower` is the dominator chain between the target and `index`, nearest
/// the target first. An offset wrapping `range` past zero (`(x & 7) - 2`)
/// names slots below the table.
fn offsets_keep_range(
    function: &strider_ir::Function,
    shallower: &[ValueId],
    index: ValueId,
    mut range: Interval,
) -> bool {
    let mut below = index;
    for &value in shallower.iter().rev() {
        let producer = function.producer(value);
        match *function.node_kind(producer) {
            NodeKind::Extend(ExtendOp::ZeroExtend)
                if function.int_inputs(value).next() == Some(below) => {}
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let offset = crate::const_eval::var_and_const(function, producer)
                    .and_then(|(x, offset)| (x == below).then_some(offset));
                let (Some(offset), Some(mask)) = (
                    offset,
                    function
                        .value_type_opt(value)
                        .and_then(ValueType::int_mask_u128),
                ) else {
                    return true;
                };
                let lo = range.lo().wrapping_add(offset) & mask;
                let hi = range.hi().wrapping_add(offset) & mask;
                // Each end moves by `offset`, so they part only by a wrap.
                if lo > hi {
                    return false;
                }
                range = Interval::new(lo, hi, range.stride());
            }
            _ => return true,
        }
        below = value;
    }
    true
}

/// The address of a load whose address is a [base operand](is_base_operand)
/// or has one as an operand of its producer.
///
/// Operand-level, since a stack table's `(sp+base) + idx*stride` decomposes
/// only in part; it also passes loads the evaluator then fails to fold.
fn foldable_load_address(function: &strider_ir::Function, load: ValueId) -> Option<ValueId> {
    let addr = function.int_inputs(load).next()?;
    let foldable = is_base_operand(function, addr)
        || function
            .int_inputs(addr)
            .any(|op| is_base_operand(function, op));
    foldable.then_some(addr)
}

/// A constant, or an address [`decompose`](crate::mem_analysis::decompose)
/// roots at a stack or heap base. The evaluator folds a `Load` only through a
/// constant or the stack.
fn is_base_operand(function: &strider_ir::Function, v: ValueId) -> bool {
    function.int_const_u128(v).is_some() || crate::mem_analysis::decompose(function, v).is_some()
}

/// Is `iv` a non-empty, enumerable range narrower than the type mask `mask`?
fn index_bound_ok(mask: u128, iv: Interval) -> bool {
    !iv.is_empty() && iv.hi() < mask && iv.count() <= u128::from(MAX_TABLE_ENTRIES)
}

/// `count` values `spacing` apart from 0, taken modulo `mask + 1`: their count
/// and spacing, or `None` where that is no arithmetic progression. Past the
/// `u128` carrier nothing wraps.
fn wrap(count: u128, spacing: u128, mask: u128) -> Option<(u128, u128)> {
    if mask == u128::MAX
        || (count - 1)
            .checked_mul(spacing)
            .is_some_and(|top| top <= mask)
    {
        return Some((count, spacing));
    }
    // `spacing * i` covers every multiple of the power of two shared with the
    // modulus once `i` spans that many values.
    let shift = spacing.trailing_zeros().min(mask.count_ones());
    (count > mask >> shift).then_some(((mask >> shift) + 1, eval::bit(shift)))
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
