//! Table-dispatch arm of the indirect-branch classifier.
//!
//! A switch lowers to `branch f(index)` for one bounded index, whether the
//! table lives in rodata, on the stack, or behind a GP/GOT indirection.  Rather
//! than pattern-match each addressing shape, this arm finds the index and then
//! delegates the addressing to the abstract evaluator, pinning the index across
//! its proven strided range and folding the dispatch cone for each value.  A
//! cone whose every input is one phi of constants is folded for each constant
//! instead, which is exact where the range is their hull.
//!
//! The foldings the evaluator covers are listed on [`eval`](super::eval).  A
//! cone needing anything else resolves to `None` and the branch defers: sound
//! but less eager.
//!
//! # Soundness
//!
//! Two independent gates must hold before committing to `Multiple`.
//!
//! A BOUNDED INDEX, from an `if (idx < N)` guard, a mask, or a shift.  The cap
//! is on the entry COUNT, not the span, so a sparse scaled `idx*8` is
//! enumerable while a dense wide range is not.  A width-only range, one exactly
//! filling its extend-stripped type width, is a raw loaded table ENTRY and is
//! rejected, so table data is never enumerated as an index.  A load whose
//! address adds no bounded index reads a field through a pointer, not a
//! table, and its width is its bound.
//!
//! A COMPLETE FOLD: every value in the range must fold to a constant target.
//! Any failure returns `None`, because a `Multiple` omitting a real runtime
//! target would wire a CFG with missing edges.
//!
//! Over-approximating the bound never drops a real edge, but a surplus arm is a
//! live edge whose bytes are decoded like any other, so the range enumerated
//! must be one proven for the index the address scales.  Under-approximating
//! is unsound.  A KnownBits field bound beside a guard on overlapping bits of
//! the same value that neither encloses the field nor is connected to it is
//! not taken: the guard may size the table, and the field then reaches the
//! next one.

use super::index_range::{Interval, add_at, sub_at, widest_int};
use super::{MAX_TABLE_ENTRIES, SiteClassifier};
use strider_cfg::{ResolvedTarget, ResolvedTargets};
use strider_ir::IRViewer;
use strider_ir::IntBinaryOp;
use strider_ir::UBig;
use strider_ir::eval;
use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType};

/// The value the enumeration pins, and the values to pin it to.
struct Domain {
    pinned: ValueId,
    values: Vec<UBig>,
}

/// The value the enumeration pins, the values to pin it to, and the constant
/// those carry: the index takes `x - rebase` for each `x` in `range`.
///
/// `rebase` is non-zero where the bound came from a guarded sibling
/// `Add(index, rebase)` rather than from the index itself, so `range` is in the
/// sibling's coordinate and the offsets between index and address are checked
/// there too.
#[derive(Clone)]
struct PinnedIndex {
    index: ValueId,
    range: Interval,
    rebase: UBig,
    /// `index`'s integer type, the width `rebase` wraps at.  Carried because
    /// only [`SiteClassifier::bounded_index`], which admits an index only at
    /// an integer type, can read it without a fallible second look.
    ty: ValueType,
}

impl SiteClassifier<'_> {
    /// The table-dispatch arm, over the dispatch value of an `IndirectBranch` or
    /// the selector of an already-seated `Switch`.
    ///
    /// `site` is the `IndirectBranch` or `Switch` being resolved; it scopes the
    /// index-range query to that node, never the first one that happens to share
    /// the dispatch value.
    pub(crate) fn classify_dispatch_value(
        &mut self,
        site: NodeId,
        target_value: ValueId,
        mode_value: Option<ValueId>,
    ) -> Option<ResolvedTargets> {
        let function = self.function();
        // A `Load[reg]` function pointer has no bounded dominator and defers here.
        let Domain { pinned, values } = self.dispatch_domain(target_value, site)?;

        let ev = &mut self.eval;
        let pruned = super::eval::cone_order_pruned(function, target_value, pinned);
        // The branch's committed ISA mode (an interworking `bx`/`jr`-dispatch),
        // `(entry & 1)`, evaluated per index so each arm carries its own mode.
        let mode_pruned = mode_value.map(|mv| super::eval::cone_order_pruned(function, mv, pinned));
        // `collect::<Option<_>>` bails the moment a value fails to fold, which
        // fails closed.
        let targets: Vec<ResolvedTarget> = values
            .into_iter()
            .map(|x| {
                // One memo for both roots: they are siblings over one dispatch
                // word (`And(word, 1)` and `And(word, ~1)`), so everything under
                // `word` folds once.
                ev.begin_index(pinned, x.clone());
                let addr = ev.eval_root(&pruned, target_value)?;
                let isa_bit = match (mode_value, &mode_pruned) {
                    // The branch commits an ISA mode per target; that mode MUST fold.
                    // If it does not, fail closed (`?` defers the whole branch),
                    // rather than decode the target in a guessed mode.
                    (Some(mv), Some(order)) => Some(ev.eval_root(order, mv)? != 0),
                    // No mode switch bound to this branch: an in-mode jump table
                    // that inherits the mode flowing into the branch.
                    _ => None,
                };
                Some(ResolvedTarget::new(addr, isa_bit))
            })
            .collect::<Option<_>>()?;
        // `table` sorts by `(addr, isa_bit)` and drops exact duplicates, so two
        // arms left on one address carry opposite modes.  `addr` is
        // mode-bit-masked, so words `X` and `X | 1` land on one address that way.
        // A disagreement defers the whole site: `None` is the mode FLOWING into
        // the branch, which a mode-committing branch contradicts.
        let seated = ResolvedTargets::table(targets);
        let arms = seated.arms();
        // Nothing to enumerate defers the site too: `table([])` is `Withdrawn`,
        // which would overrule a caller's seed where `None` only defers.
        (!arms.is_empty() && arms.windows(2).all(|w| w[0].addr != w[1].addr)).then_some(seated)
    }

    /// What to enumerate for `target`: a bounded index across its range, or the
    /// phi every input of the dispatch cone passes through across its constants.
    ///
    /// The phi's constants are the values it really takes, where its range is
    /// their hull: an index in `{0, 3, 5}` ranges over `0..=5`.  One outside the
    /// phi's range at the site, or ruled out by an `== k` guard on the way, is
    /// dropped.  They are taken where the phi bounds no index too, as for a jump
    /// to a phi of labels.  Where a bounded index is found as well, a constant
    /// the index range excludes is dropped, since both bound the same runtime
    /// value; where the index does not fold under some constant, the range is
    /// enumerated as it is without them.
    fn dispatch_domain(&mut self, target: ValueId, site: NodeId) -> Option<Domain> {
        let function = self.function();
        let chain = dominator_chain(function, target)?;
        let source = chain.last().copied().unwrap_or(target);
        // A constant the site's range or an equality guard on the way excludes
        // never reaches the branch.
        let constants = super::index_range::phi_constants(
            function,
            source,
            usize::try_from(MAX_TABLE_ENTRIES).ok()?,
        )
        .map(|mut constants| {
            let site_range = self.ranges.range_of(source, site);
            let ruled_out = self.ranges.ruled_out(source, site);
            constants.retain(|k| site_range.contains(k) && !ruled_out.contains(k));
            constants
        });
        let hit = self.decompose_index(&chain, site);
        let (hit, constants) = match (hit, constants) {
            (None, None) => return None,
            (None, Some(values)) => {
                return Some(Domain {
                    pinned: source,
                    values,
                });
            }
            (Some(hit), constants) => (hit, constants),
        };
        if let Some(constants) = constants {
            let order = super::eval::cone_order_pruned(function, hit.index, source);
            let ev = &mut self.eval;
            let values: Option<Vec<UBig>> = constants
                .into_iter()
                .filter_map(|k| {
                    ev.begin_index(source, k.clone());
                    match ev.eval_root(&order, hit.index) {
                        Some(index) => hit
                            .range
                            .contains(&add_at(&UBig::from(index), &hit.rebase, hit.ty))
                            .then_some(Some(k)),
                        None => Some(None),
                    }
                })
                .collect();
            if let Some(values) = values {
                return Some(Domain {
                    pinned: source,
                    values,
                });
            }
        }
        // `stride` is a KnownBits MUST-divisor of the value spacing, so stepping by
        // it visits exactly the reachable indices.  `index_bound_ok` has already
        // held the count to `MAX_TABLE_ENTRIES`, which bounds the walk.
        debug_assert!(hit.range.count() <= UBig::from(MAX_TABLE_ENTRIES));
        let mut values = Vec::new();
        let mut x = hit.range.lo().clone();
        while x <= *hit.range.hi() {
            values.push(sub_at(&x, &hit.rebase, hit.ty));
            x += hit.range.stride();
        }
        Some(Domain {
            pinned: hit.index,
            values,
        })
    }

    /// The shallowest genuinely-bounded, non-width-only, non-constant value of
    /// `chain`, the values that DOMINATE the target in its variability cone.
    ///
    /// Requiring dominance excludes a bypassed sub-branch: in a rotate
    /// `(x<<2) | (x>>30)`, `x>>30` has the tightest interval but does not dominate.
    ///
    /// SHALLOWEST here is target-rooted, nearest the dispatch value, not
    /// entry-rooted, and it is the one enumerated rather than the deepest: it sits
    /// just below the address arithmetic with every guard/mask/shift applied, so
    /// enumerating it visits exactly the reachable slots.  A deeper
    /// (further-from-dispatch) node's bound can be looser, and enumerating
    /// out-of-bounds slots defers a branch that should have resolved.
    fn decompose_index(&mut self, chain: &[ValueId], site: NodeId) -> Option<PinnedIndex> {
        let function = self.function();
        let hits: Vec<(usize, PinnedIndex)> = chain
            .iter()
            .enumerate()
            .filter_map(|(depth, &v)| self.bounded_index(site, v).map(|hit| (depth, hit)))
            .collect();
        let shallowest = hits.first()?.clone();
        // A candidate that strips to a cell of a table the evaluator folds bounds
        // the table DATA, not the index the address scales.  A narrowing mask on
        // the cell is what puts its range below its load width and past
        // `bounded_index`.  Where the chain carries a bounded value BELOW that
        // load, inside its address, that one is the index; a second mask on the
        // cell itself is not, so the depth comparison is against the load.
        let (depth, hit) = foldable_cell_load(function, shallowest.1.index)
            .and_then(|cell| chain.iter().position(|&v| v == cell))
            .and_then(|at| hits.iter().find(|&&(depth, _)| depth > at).cloned())
            .unwrap_or(shallowest);
        offsets_keep_range(function, &chain[..depth], hit.index, hit.range.clone()).then_some(hit)
    }

    /// Is `iv`, the range of `value` at `site`, one a table index can be
    /// enumerated over?  `bound` is the index type's all-ones.
    fn enumerable(&mut self, site: NodeId, bound: &UBig, value: ValueId, iv: &Interval) -> bool {
        index_bound_ok(bound, iv)
            && !self.is_width_only(site, value, iv)
            && !self.ranges.guard_left_unconnected(value, site, iv)
    }

    /// A genuinely-bounded non-constant integer whose bound is a real narrowing,
    /// not just its type width.  A loaded table ENTRY, bounded only by its load
    /// width, is excluded: enumerating one folds to bogus targets.
    ///
    /// The guard can sit on a sibling `v + k` the dispatch cone does not contain,
    /// which is what `switch (x)` over a negative case base lowers to once
    /// ConstantFold has merged `k` into the table base.  `Add` is modular at one
    /// width, so a bound on the sibling is a bound on `v` pinned `k` lower, and the
    /// tightest such sibling is taken.
    ///
    /// A range [`RangeMap::guard_left_unconnected`] flags is not one.
    ///
    /// [`RangeMap::guard_left_unconnected`]: super::index_range::RangeMap::guard_left_unconnected
    fn bounded_index(&mut self, site: NodeId, v: ValueId) -> Option<PinnedIndex> {
        let function = self.function();
        debug_assert!(
            function.int_const(v).is_none(),
            "the dominator chain skips constants"
        );
        let ty = function.value_type_opt(v).filter(|t| t.is_integer())?;
        let bound = eval::all_ones(ty);
        let iv = self.ranges.range_of(v, site);
        if self.enumerable(site, &bound, v, &iv) {
            return Some(PinnedIndex {
                index: v,
                range: iv,
                rebase: UBig::ZERO,
                ty,
            });
        }
        let siblings: Vec<(ValueId, UBig)> = function
            .value_uses(v)
            .filter(|&(user, _)| {
                matches!(
                    function.node_kind(user),
                    NodeKind::IntBinaryOp(IntBinaryOp::Add)
                )
            })
            .filter_map(|(user, _)| {
                let sum = function.first_value_output_of(user)?;
                let (operand, addend) = strip_commutative_const_scale(function, sum)?;
                (operand == v && function.value_type_opt(sum) == Some(ty)).then_some((sum, addend))
            })
            .collect();
        siblings
            .into_iter()
            .filter_map(|(sum, addend)| {
                let iv = self.ranges.range_of(sum, site);
                self.enumerable(site, &bound, sum, &iv)
                    .then(|| PinnedIndex {
                        index: v,
                        range: iv,
                        rebase: addend,
                        ty,
                    })
            })
            .min_by_key(|hit| hit.range.count())
    }

    /// Is `v`'s range merely its type width rather than a real narrowing?  A raw
    /// byte load fills its cell width exactly, making it table DATA, not an index,
    /// unless it is a field read through a pointer ([`Self::loads_a_field`]) that no
    /// dominating test bounds unread
    /// ([`RangeMap::cell_test_left_unread`](super::index_range::RangeMap::cell_test_left_unread)).
    ///
    /// Keying on the RANGE rather than on "is it load-derived" is load-bearing: a
    /// guarded raw load, `if (Load < N) switch(Load)`, is a genuine index even
    /// though it strips to a `Load`.
    ///
    /// Zero-extends, truncates, masks clearing only known-zero bits and constant
    /// scalings are stripped first ([`strip_to_cell`]), then replayed
    /// innermost-first over the cell's `1 << w` consecutive values, tracking their
    /// spacing: a divide collapses the set only by however much it outruns the
    /// spacing a preceding multiply built up, so `(cell << 2) >> 1` keeps all
    /// `1 << w`.  A truncate, and then the OUTPUT width, wrap the set
    /// ([`wrap`]): `zext(i8) << 25` at `I32` has 128 distinct values, not 256,
    /// and `trunc8(i32)` has 256.  A scale past the `u128` carrier, a floor over
    /// an unrelated spacing, a wrap that leaves no progression, and a strip
    /// longer than [`MAX_SCALE_STRIP`] all answer "width-only", which defers the
    /// site rather than enumerating a cell.
    /// An unrecognised producer is not a failure: the strip ends there and the
    /// count is taken at that value's own width.
    fn is_width_only(&mut self, site: NodeId, v: ValueId, iv: &Interval) -> bool {
        let function = self.function();
        let Some((base, chain)) =
            strip_to_cell(function, v, |and| self.ranges.identity_mask_operand(and))
        else {
            return true;
        };
        if self.loads_a_field(site, base) {
            return self.ranges.cell_test_left_unread(v, base, site, iv);
        }
        // Defer, as every other bail here does: with no integer width there is
        // no cell count to compare the range against, so nothing disproves
        // table DATA.
        let Some(w) = function
            .value_type_opt(base)
            .filter(|t| t.is_integer())
            .map(ValueType::bit_width)
        else {
            return true;
        };
        // No value set spreads past the widest type and still counts as one
        // cell's worth; the cap also keeps a binary's own constant from
        // sizing the spacing.
        let spread_cap = UBig::ONE << widest_int().bit_width();
        let mut count = UBig::ONE << w;
        let mut spacing = UBig::ONE;
        for scale in chain.iter().rev() {
            match scale {
                Scale::Widen(m) => {
                    spacing *= m;
                    if spacing > spread_cap {
                        return true;
                    }
                }
                Scale::Narrow(c) if &spacing % c == UBig::ZERO => spacing /= c,
                Scale::Narrow(c) if c % &spacing == UBig::ZERO => {
                    count = div_ceil(count, &(c / &spacing));
                    spacing = UBig::ONE;
                }
                // Floor over a spacing that neither divides nor is divided leaves
                // no arithmetic progression to count.
                Scale::Narrow(_) => return true,
                Scale::Wrap(bits) => match wrap(count, spacing, *bits) {
                    Some(wrapped) => (count, spacing) = wrapped,
                    None => return true,
                },
            }
        }
        // A widening scale is modular at the output width too.
        if let Some(out_w) = function
            .value_type_opt(v)
            .filter(|t| t.is_integer())
            .map(ValueType::bit_width)
        {
            match wrap(count, spacing, out_w) {
                Some((wrapped, _)) => count = wrapped,
                None => return true,
            }
        }
        iv.count() == count
    }

    /// Whether `v` is a load through a pointer: its address is no constant, and
    /// no term it adds ranges at `site` over few enough values to index a table.
    /// A cell of a table sits at the table's base plus a bounded index, wherever
    /// the table lives; a field of a struct, or a byte through a pointer, sits at
    /// an unbounded register or phi plus an offset.
    fn loads_a_field(&mut self, site: NodeId, v: ValueId) -> bool {
        let function = self.function();
        if !matches!(function.node_kind(function.producer(v)), NodeKind::Load(_)) {
            return false;
        }
        let Some(addr) = function.int_inputs(v).next() else {
            return false;
        };
        if function.int_const(addr).is_some() {
            return false;
        }
        let mut stack = vec![addr];
        let mut steps = MAX_SCALE_STRIP;
        while let Some(term) = stack.pop() {
            let Some(left) = steps.checked_sub(1) else {
                return false;
            };
            steps = left;
            if function.int_const(term).is_some() {
                continue;
            }
            if matches!(
                function.node_kind(function.producer(term)),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            ) {
                stack.extend(function.int_inputs(term));
            } else if self.ranges.range_of(term, site).count() <= UBig::from(MAX_TABLE_ENTRIES) {
                return false;
            }
        }
        true
    }
}

/// The values that dominate `target` in its variability cone, nearest first,
/// or `None` when `target` is a constant.
///
/// The cone traverses THROUGH a load [`foldable_load_address`] passes into its
/// address, and stops at any other load, phis and opaque sources.  A virtual
/// ENTRY feeds every root, so the deepest value of the chain, where it is a
/// root, is the only one.
fn dominator_chain(function: &strider_ir::Function, target: ValueId) -> Option<Vec<ValueId>> {
    // Vertex 0 is the virtual ENTRY; `value[i]` is vertex `i`'s cone value,
    // `None` for the ENTRY.
    const ENTRY: u32 = 0;
    let mut succs: Vec<Vec<u32>> = vec![Vec::new()];
    let mut value: Vec<Option<ValueId>> = vec![None];
    let mut nidx: rustc_hash::FxHashMap<ValueId, u32> = rustc_hash::FxHashMap::default();
    let node_of = |succs: &mut Vec<Vec<u32>>,
                   value: &mut Vec<Option<ValueId>>,
                   nidx: &mut rustc_hash::FxHashMap<ValueId, u32>,
                   v: ValueId| {
        *nidx.entry(v).or_insert_with(|| {
            succs.push(Vec::new());
            value.push(Some(v));
            u32::try_from(value.len() - 1).expect("cone vertex count fits u32")
        })
    };

    let mut seen: rustc_hash::FxHashSet<ValueId> = rustc_hash::FxHashSet::default();
    let mut stack = vec![target];
    while let Some(v) = stack.pop() {
        if function.int_const(v).is_some() || !seen.insert(v) {
            continue;
        }
        let vi = node_of(&mut succs, &mut value, &mut nidx, v);
        // The addressing arithmetic, a foldable load's address, or nothing for
        // an opaque source.
        let inputs: Vec<ValueId> = match function.node_kind(function.producer(v)) {
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
            // An SP-decomposable base is a symbolic BASE, not a variable, so it
            // is skipped like a const; otherwise `sp` becomes a second root and
            // the real index stops dominating the target.  `decompose` accepts
            // `sp + const` and alignment masks but rejects `sp & 0xF`, which is
            // a bounded VALUE and must stay a candidate index.
            if is_base_operand(function, p) {
                continue;
            }
            has_var_input = true;
            let pi = node_of(&mut succs, &mut value, &mut nidx, p);
            succs[pi as usize].push(vi);
            stack.push(p);
        }
        if !has_var_input {
            succs[ENTRY as usize].push(vi); // a cone root
        }
    }

    let target_idx = *nidx.get(&target)?;
    // Each vertex is asked for its successors once, as it is discovered.
    let doms =
        graph_algorithms::dominance::dominators(ENTRY, |v| std::mem::take(&mut succs[v as usize]));

    // The idom chain above the target is the target-rooted shallow-to-deep
    // order, so the first bounded hit IS the shallowest; the ENTRY carries no
    // cone value and drops out.
    let mut chain: Vec<ValueId> = Vec::new();
    let mut at = target_idx;
    while let Some(up) = doms.immediate_dominator(at) {
        if let Some(v) = value[up as usize] {
            chain.push(v);
        }
        at = up;
    }
    Some(chain)
}

/// The cell `v` reduces to, through the width changes, masks and constant
/// scalings between a load and the address arithmetic, when that is a load of
/// a table the evaluator can fold.
fn foldable_cell_load(function: &strider_ir::Function, v: ValueId) -> Option<ValueId> {
    let (cell, _) = strip_to_cell(function, v, |and| {
        strip_commutative_const_scale(function, and).map(|(next, _)| next)
    })?;
    (matches!(
        function.node_kind(function.producer(cell)),
        NodeKind::Load(_)
    ) && foldable_load_address(function, cell).is_some())
    .then_some(cell)
}

/// What `v` reduces to through zero-extends, truncates, the constant masks
/// `through_mask` strips and constant scalings, with the steps that change
/// the value set, outermost first. `None` past [`MAX_SCALE_STRIP`] steps.
fn strip_to_cell(
    function: &strider_ir::Function,
    v: ValueId,
    mut through_mask: impl FnMut(ValueId) -> Option<ValueId>,
) -> Option<(ValueId, Vec<Scale>)> {
    let mut base = v;
    let mut chain: Vec<Scale> = Vec::new();
    for _ in 0..MAX_SCALE_STRIP {
        let inner = match *function.node_kind(function.producer(base)) {
            NodeKind::Extend(ExtendOp::ZeroExtend) => {
                function.int_inputs(base).next().map(|next| (next, None))
            }
            NodeKind::Truncate => function
                .int_inputs(base)
                .next()
                .zip(function.value_type_opt(base))
                .map(|(next, ty)| (next, Some(Scale::Wrap(ty.bit_width())))),
            NodeKind::IntBinaryOp(IntBinaryOp::And) => through_mask(base).map(|next| (next, None)),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => strip_const_scale(function, base)
                .and_then(|(next, k)| Some((next, Some(Scale::Widen(pow2(&k)?))))),
            NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => strip_const_scale(function, base)
                .and_then(|(next, k)| Some((next, Some(Scale::Narrow(pow2(&k)?))))),
            NodeKind::IntBinaryOp(IntBinaryOp::Div) => strip_const_scale(function, base)
                .and_then(|(next, c)| (c != UBig::ZERO).then_some((next, Some(Scale::Narrow(c))))),
            NodeKind::IntBinaryOp(IntBinaryOp::Mul) => {
                strip_commutative_const_scale(function, base).and_then(|(next, c)| {
                    (c != UBig::ZERO).then_some((next, Some(Scale::Widen(c))))
                })
            }
            _ => None,
        };
        match inner {
            Some((next, scale)) => {
                base = next;
                chain.extend(scale);
            }
            None => return Some((base, chain)),
        }
    }
    None
}

/// Whether the constant offsets applied to `index` on its way to the address
/// keep `range` a contiguous unsigned interval.
///
/// `shallower` is the dominator chain above `index`, target-rooted. An offset
/// that wraps `range` past zero (`(x & 7) - 2`) names slots below the table
/// that no guard excludes, since the guards the classifier could not read are
/// the ones on the offset value; enumerating `x` would seat them.
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
                let Ok([a, b]) = function.producer_inputs_exact::<2>(value) else {
                    return true;
                };
                let offset = match (a == below, b == below) {
                    (true, false) => function.int_const(b),
                    (false, true) => function.int_const(a),
                    _ => None,
                };
                let (Some(offset), Some(ty)) = (
                    offset,
                    function.value_type_opt(value).filter(|t| t.is_integer()),
                ) else {
                    return true;
                };
                let lo = add_at(range.lo(), offset, ty);
                let hi = add_at(range.hi(), offset, ty);
                if lo > hi || &hi - &lo != range.hi() - range.lo() {
                    return false;
                }
                range = Interval::new(lo, hi, range.stride().clone());
            }
            _ => return true,
        }
        below = value;
    }
    true
}

/// The address of a load whose address is a const or SP-rooted base, or has
/// one as an operand of its producer; `None` otherwise.
///
/// The check is operand-level because a stack table load
/// `Load[(sp+base) + idx*stride]` has an INDEX-DEPENDENT address that never
/// decomposes as a whole, though its `sp+base` operand does.  So it also
/// passes loads the evaluator cannot fold: `Load(reg + 8)`, a vtable slot or
/// struct field, and `Load(call())`, whose constant callee is an input.  Those
/// fold to nothing, so the site defers.
fn foldable_load_address(function: &strider_ir::Function, load: ValueId) -> Option<ValueId> {
    let addr = function.int_inputs(load).next()?;
    let foldable = is_base_operand(function, addr)
        || function
            .int_inputs(addr)
            .any(|op| is_base_operand(function, op));
    foldable.then_some(addr)
}

/// The two bases the evaluator can fold a `Load` through: a const rodata base
/// or an SP-rooted stack base.
fn is_base_operand(function: &strider_ir::Function, v: ValueId) -> bool {
    function.int_const(v).is_some() || crate::mem_analysis::decompose(function, v).is_some()
}

/// Is `iv` a non-empty, enumerable, genuinely-narrowed range under the type's
/// all-ones `bound`?
fn index_bound_ok(bound: &UBig, iv: &Interval) -> bool {
    !iv.is_empty() && iv.hi() < bound && iv.count() <= UBig::from(MAX_TABLE_ENTRIES)
}

/// The variable operand and the constant of a non-commutative binop whose RHS
/// is constant.
fn strip_const_scale(function: &strider_ir::Function, v: ValueId) -> Option<(ValueId, UBig)> {
    let [lhs, rhs] = function.producer_inputs_exact::<2>(v).ok()?;
    function.int_const(rhs).map(|c| (lhs, c.clone()))
}

/// As [`strip_const_scale`], for a commutative binop: the constant may sit on
/// either side, and two constants are not a scaling of anything.
fn strip_commutative_const_scale(
    function: &strider_ir::Function,
    v: ValueId,
) -> Option<(ValueId, UBig)> {
    crate::const_eval::var_and_ubig(function, function.producer(v))
}

/// `a / b` rounded up.
fn div_ceil(a: UBig, b: &UBig) -> UBig {
    (a + b - UBig::ONE) / b
}

/// One step on the way from a cell to `v` that changes its value set.
enum Scale {
    /// `* m`: spreads the value set out, so a later divide collapses less.
    Widen(UBig),
    /// `/ c`: collapses the value set once it outruns the spacing.
    Narrow(UBig),
    /// A truncate to this many bits: the set taken modulo `2^bits`.
    Wrap(usize),
}

/// Producer steps [`strip_to_cell`] may take, matching the caps its sibling
/// walks carry.  Exhausting it answers "width-only", the same conservative
/// bail as an uncountable scale.
const MAX_SCALE_STRIP: u32 = 64;

/// `count` values `spacing` apart from 0, taken modulo `2^bits`: their count
/// and spacing, or `None` where that is no arithmetic progression.
///
/// `bits` is a declared type width, so the modulus it names is bounded.
fn wrap(count: UBig, spacing: UBig, bits: usize) -> Option<(UBig, UBig)> {
    debug_assert!(bits <= widest_int().bit_width(), "a declared width");
    let modulus = UBig::ONE << bits;
    if (&count - UBig::ONE) * &spacing < modulus {
        return Some((count, spacing));
    }
    // `spacing * i` covers every multiple of the power of two shared with the
    // modulus, once `i` runs over that many consecutive values.
    let shared = UBig::ONE
        << spacing
            .trailing_zeros()
            .expect("a spacing is at least 1")
            .min(bits);
    let full = modulus / shared.clone();
    (count >= full).then_some((full, shared))
}

/// `1 << k`, or `None` past the widest declared width, where no value set
/// scaled by it stays inside a type.
fn pow2(k: &UBig) -> Option<UBig> {
    let k = usize::try_from(k).ok()?;
    (k <= widest_int().bit_width()).then(|| UBig::ONE << k)
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod table_tests;
