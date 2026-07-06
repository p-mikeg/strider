//! Stack-pointer analyzer: SP decomposition + address classification + alias
//! verdict, merged into one [`SpAnalyzer`] type.
//!
//! `decompose_readonly` is the workhorse: given an output that is
//! `InitialVar(sp)` transformed by `Add` of constants (subtraction appears as
//! `Add(_, Neg(K))`) or anchored at an alignment-masked `sp & mask`, it returns
//! a single `SpExpr { base, offset }` terminal (or `None`).  It caches nothing
//! itself but reads the function's `stack_offsets` side-table (the persistent
//! decomposition cache, filled once the graph is frozen by `StackOffsetDetect`)
//! before walking the live SP spine.
//!
//! The decomposer does **not** look through `Phi` nodes — a stack-tagged
//! `Phi(sp)` (loop-header join, or the single-predecessor phi the lifter wraps
//! around `read_variable(sp)`) decomposes to `None`.  By the time any SP-aware
//! pass runs `decompose`, `PhiCollapse` / `RedundantPhis` have already
//! collapsed those single-predecessor phis to their `InitialVar(sp)` input, so
//! the decomposer only ever meets real terminals.  A `None` reads as "not a
//! provable SP terminal", which every caller already treats conservatively
//! (may-alias / opaque base).
//!
//! On top of the decomposition, [`SpAnalyzer`] also classifies a load / store
//! address into a coarse [`AddrClass`] (`classify_addr` / `classify_store_addr`,
//! both decomposing via the cache-backed `decompose_readonly`); the pure,
//! class-on-class verdict table is the free [`alias_verdict`].


use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IntBinaryOp, SpDecomp};

use super::ranges::ranges_disjoint;
use crate::AliasMode;
use AddrClass::*;

/// Decomposed stack-pointer expression: `base + offset`, where `base` is an
/// SP-rooted node (`InitialVar(sp)` or an alignment-masked SP `And` output).
///
/// `decompose` returns `Option<SpExpr>`; `None` carries the
/// "not a provable SP terminal" case, so there is no separate variant for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SpExpr {
    pub base: ValueId,
    pub offset: i128,
}

/// Coarse classification of a Load / Store address.  The verdict table in
/// [`alias_verdict`] is keyed on the `(load_class, store_class)` pair:
/// matching addresses use the diagonal of the table, disjointness uses the
/// off-diagonal.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AddrClass {
    /// `decompose` returned a terminal `{ base, offset }`.  Two
    /// `SpRooted` addresses refer to the same byte range only when they
    /// share the same `base` (the SP-derived terminal node) AND offset;
    /// disjoint offsets on the SAME base are proven non-overlapping via
    /// [`ranges_disjoint`].  Different bases — e.g. `InitialVar(sp)` vs an
    /// alignment-masked `sp & -16` — differ by an unknown amount (the
    /// caller-dependent `sp mod align`), so their offsets are in different
    /// coordinate systems and are treated as may-alias.
    SpRooted { base: ValueId, offset: i128 },
    /// `NodeKind::IntConst(_)` address — a literal `.data`/`.rodata`/
    /// `.bss`/MMIO pointer.  Two `Constant` addresses with equal values
    /// refer to the same byte range; disjoint values are proven
    /// non-overlapping via [`ranges_disjoint`].
    Constant { addr: i128 },
    /// Anything else (`Load`-of-pointer, `Add` of opaque values, a
    /// non-collapsing `Phi`-of-offsets, …).  Two `Anchor` addresses are
    /// proven equal only by `ValueId` equality; different ids can compute
    /// to the same address at runtime, so we treat them as
    /// possibly-aliasing.
    Anchor { value: ValueId },
}

/// Pairwise verdict between a Load's address class + size and an
/// intervening Store's address class + size.  Implements the table
/// described in the [`AliasMode`] module docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AliasVerdict {
    /// Same byte range — a `load_forward` caller treats this Store as the
    /// forwarding source.
    Match,
    /// Provably non-overlapping byte range — caller steps through.
    Disjoint,
    /// Cannot prove either; caller bails (shadow / no-forward).
    MayAlias,
}

/// Stack-pointer analyzer over an immutable function: SP decomposition +
/// address classification + the store-alias verdict.  The stack varnode to
/// anchor on is the function's own `default_cc().stack_vn`, read on demand — so
/// it cannot drift from the function under analysis.
///
/// Decomposition results are cached in the function's `stack_offsets`
/// side-table, written once the graph is frozen by `StackOffsetDetect`.
/// This read-only analyzer
/// consults that cache and otherwise recomputes a cone *without* mutating, so
/// it composes with any shared `&Function` borrow (the memory-SSA walk, the
/// range-scoped indirect-branch classifier).
pub(crate) struct SpAnalyzer<'a> {
    function: &'a Function,
}

impl<'a> SpAnalyzer<'a> {
    pub(crate) fn new(function: &'a Function) -> Self {
        Self { function }
    }

    /// Decomposes `value` into `InitialVar(sp) + K` (or an alignment-masked SP
    /// base) — a cache hit when the value was already decomposed, else a
    /// read-only recompute of its cone.  See [`decompose_readonly`].
    pub(crate) fn decompose(&self, value: ValueId) -> Option<SpExpr> {
        decompose_readonly(self.function, value)
    }
}

/// Defensive backstop on the SP-spine walk length.  The spine (`InitialVar(sp)`
/// through `Add`-of-const / alignment-`And`) is acyclic in valid SSA — data
/// cycles pass only through `Phi`, which terminates the walk — so this is never
/// reached in practice; it guards against a malformed graph.
const MAX_SP_SPINE: u32 = 100_000;

/// The stack decomposition of `value`, MEMOIZED: a cache hit returns O(1), else
/// it walks the SP spine and caches the verdict (positive or `NotStack`) so the
/// next query on `value` is a hit.
///
/// The cache lives behind a `RefCell` on the function's side-tables, so this
/// fills it through a shared `&Function` — its memory-SSA / range-scoped callers
/// can't hand it `&mut`.  The optimizer clears the cache on every graph mutation
/// (so a memoized verdict never outlives its graph), which is why filling here
/// is sound during the fixed point.
pub(crate) fn decompose_readonly(function: &Function, value: ValueId) -> Option<SpExpr> {
    // Cache hit for the queried value → done, no re-walk.
    match function.side_tables().stack_slot(value) {
        SpDecomp::Stack(_) => return resolve_slot(function, value),
        SpDecomp::NotStack => return None,
        SpDecomp::Unknown => {}
    }
    // Miss: walk the spine (short-circuiting on any already-cached ancestor),
    // then memoize `value`'s verdict.
    let result = spine_walk(function, value);
    match result {
        Some(e) => function.side_tables().set_stack_slot(value, e.base, e.offset),
        None => function.side_tables().set_stack_slot_not(value),
    }
    result
}

/// The read-only SP-spine walk backing [`decompose_readonly`]: the
/// `Add`-of-const / alignment-`And` chain down to `InitialVar(sp)`, accumulating
/// the constant offset.  O(spine depth), NOT O(cone) — it follows only the
/// single SP-bearing operand at each step.  Mutates nothing; short-circuits on
/// any cached ancestor slot.
fn spine_walk(function: &Function, value: ValueId) -> Option<SpExpr> {
    let mut cur = value;
    let mut acc: i128 = 0;
    // Once an alignment-`And` is met, the base is fixed to *that* And output (an
    // opaque, entry-alignment-dependent base) carrying the offset accrued above
    // it; the rest of the walk only *confirms* the operand is SP-rooted, so
    // offsets below the And are ignored.  Iterative (not recursive) so a deep
    // `And`-of-`And` chain cannot overflow the stack.
    let mut anchor: Option<SpExpr> = None;
    for _ in 0..MAX_SP_SPINE {
        // A committed verdict short-circuits the walk.
        match function.side_tables().stack_slot(cur) {
            SpDecomp::Stack(_) => {
                let hit = resolve_slot(function, cur)?;
                return Some(match anchor {
                    // Below an anchor, the committed hit only confirms SP-rooting.
                    Some(a) => a,
                    None => SpExpr {
                        base: hit.base,
                        offset: hit.offset.checked_add(acc)?,
                    },
                });
            }
            SpDecomp::NotStack => return None,
            SpDecomp::Unknown => {}
        }
        let node = function.producer(cur);
        match *function.node_kind(node) {
            NodeKind::InitialVar(id)
                if function.initial_vn(id) == function.default_cc().stack_vn =>
            {
                return Some(anchor.unwrap_or(SpExpr {
                    base: cur,
                    offset: acc,
                }));
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                let [lhs, rhs] = function.node_inputs_exact::<2>(node).ok()?;
                // SP + const in either order; the const shifts the accumulated
                // offset (only while above the anchor) and the walk continues
                // down the other operand.
                let (sp_operand, c) =
                    match (function.int_const_i128(rhs), function.int_const_i128(lhs)) {
                        (Some(c), _) => (lhs, c),
                        (None, Some(c)) => (rhs, c),
                        _ => return None,
                    };
                if anchor.is_none() {
                    acc = acc.checked_add(c)?; // fail-closed on overflow
                }
                cur = sp_operand;
            }
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                let [l, r] = function.node_inputs_exact::<2>(node).ok()?;
                // Alignment-masked SP is a fresh opaque base (its exact value
                // depends on entry-SP alignment).  Require the non-mask operand
                // to be SP-rooted; fix the base to this And output the first time.
                let sp_operand = if function.int_const_u128(r).is_some_and(is_alignment_mask) {
                    l
                } else if function.int_const_u128(l).is_some_and(is_alignment_mask) {
                    r
                } else {
                    return None;
                };
                anchor.get_or_insert(SpExpr {
                    base: cur,
                    offset: acc,
                });
                cur = sp_operand;
            }
            _ => return None,
        }
    }
    None
}

/// Resolves a value's cached `(base, offset)` slot into an [`SpExpr`], or `None`
/// when the slot is unknown / not-stack.
fn resolve_slot(function: &Function, value: ValueId) -> Option<SpExpr> {
    function
        .side_tables()
        .stack_slot_resolved(value)
        .map(|(base, offset)| SpExpr { base, offset })
}

impl SpAnalyzer<'_> {
    /// Classifies a load / store address.  Cheap: `decompose` is a cache hit /
    /// local recompute, the `IntConst` peek is a single match.
    pub(super) fn classify_addr(&self, addr: ValueId) -> AddrClass {
        match self.decompose(addr) {
            Some(SpExpr { base, offset }) => AddrClass::SpRooted { base, offset },
            None => {
                if let Some(c) = self.function.int_const_u128(addr) {
                    AddrClass::Constant { addr: c as i128 }
                } else {
                    AddrClass::Anchor { value: addr }
                }
            }
        }
    }

    /// Classifies a raw `NodeKind::Store`'s address into an [`AddrClass`] — the
    /// store-side counterpart of [`Self::classify_addr`], kept named so
    /// `def_clobbers` and the exact re-check in `verdict` classify a store
    /// identically.  `classify_addr` decomposes via `decompose_readonly`, which
    /// consults the `stack_offsets` cache itself, so there is no separate
    /// side-table preference here.
    pub(crate) fn classify_store_addr(&self, store_node: NodeId) -> AddrClass {
        self.classify_addr(self.function.store_addr(store_node))
    }
}

/// Is `m` a stack-*alignment* mask — a contiguous run of high 1-bits with at
/// least one low 0-bit (e.g. `0xFFFF_FFF8`, `0xFFFF_FFF0`)?  An alignment mask
/// clears only the low-order bits; a low-bit mask (`0xF`) is a bit-extraction,
/// not a base.  `0` and all-ones masks are rejected (no alignment effect / not
/// a low-clearing mask).
pub(crate) fn is_alignment_mask(m: u128) -> bool {
    let tz = m.trailing_zeros();
    // `tz == 0`: no low zero bits → not clearing any alignment (low mask or all-ones).
    // `tz == 128`: m is zero → all bits cleared, not a valid alignment mask.
    if tz == 0 || tz == 128 {
        return false;
    }
    // After dropping the low zero run, the remaining bits must be a contiguous
    // block of 1s (all-ones once shifted), i.e. `shifted + 1` is a power of two.
    let shifted = m >> tz;
    shifted != 0 && shifted & shifted.wrapping_add(1) == 0
}

/// Diagonal verdict for two in-class offsets: equal → `Match`,
/// range-disjoint → `Disjoint`, otherwise `MayAlias`.  Shared by the
/// `SpRooted`/`SpRooted` and `Constant`/`Constant` arms of
/// [`alias_verdict`] (the `Anchor`/`Anchor` arm uses `ValueId` equality
/// and has no offset/range shape).
fn offset_range_verdict(
    load_off: i128,
    load_size: i128,
    store_off: i128,
    store_size: i128,
) -> AliasVerdict {
    if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load_size, store_off, store_size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// An address class paired with the byte size of the access at it — one
/// operand (load or store) of the pairwise [`alias_verdict`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct SizedAddr {
    pub(crate) class: AddrClass,
    pub(crate) size: i128,
}

/// Pairwise alias verdict between a load's class + size and a store's
/// class + size under the given [`AliasMode`].
pub(crate) fn alias_verdict(
    load: SizedAddr,
    store: SizedAddr,
    mode: AliasMode,
    distinct_sp_bases_disjoint: bool,
) -> AliasVerdict {
    let SizedAddr {
        class: load_class,
        size: load_size,
    } = load;
    let SizedAddr {
        class: store_class,
        size: store_size,
    } = store;
    match (load_class, store_class) {
        // Diagonal: in-class equality + range-disjoint.  Two SP-rooted
        // addresses are only comparable when they share the same base node;
        // different SP bases (initial SP vs an alignment-masked SP) differ
        // by an unknown amount, so their offsets can't be related → normally
        // may-alias.  `distinct_sp_bases_disjoint` opts into the optimistic
        // assumption that distinct SP bases address disjoint regions (used by
        // stack-arg detection, where incoming-arg slots above the entry SP do
        // not overlap frame locals rooted at an alignment-masked SP).
        (
            SpRooted {
                base: lb,
                offset: lo,
            },
            SpRooted {
                base: sb,
                offset: so,
            },
        ) => {
            if lb == sb {
                offset_range_verdict(lo, load_size, so, store_size)
            } else if distinct_sp_bases_disjoint {
                AliasVerdict::Disjoint
            } else {
                AliasVerdict::MayAlias
            }
        }
        (Constant { addr: lo }, Constant { addr: so }) => {
            offset_range_verdict(lo, load_size, so, store_size)
        }
        (Anchor { value: lout }, Anchor { value: sout }) => {
            if lout == sout {
                AliasVerdict::Match
            } else {
                // Different ids can compute to the same address at runtime;
                // no disjointness proof available.
                AliasVerdict::MayAlias
            }
        }
        // Off-diagonal: cross-class.  Strict cannot prove disjoint;
        // StackGlobalDisjoint admits SP↔Constant pairs.
        (SpRooted { .. }, Constant { .. }) | (Constant { .. }, SpRooted { .. }) => match mode {
            AliasMode::Strict => AliasVerdict::MayAlias,
            AliasMode::StackGlobalDisjoint => AliasVerdict::Disjoint,
        },
        // Every other cross-class pair (Anchor vs anything) still bails
        // under both modes; closing this requires escape analysis.
        _ => AliasVerdict::MayAlias,
    }
}

#[cfg(test)]
mod decompose_tests {
    use super::*;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR};

    fn sp() -> rsleigh::Vn {
        rsleigh::Vn {
            addr_off: 0x20,
            addr_space: rsleigh::VnSpace::REGISTER,
            size: 4,
        }
    }

    /// Collapses the single-predecessor `read_variable(sp)` phi so an SP
    /// address becomes a bare `InitialVar(sp) + k` terminal — the shape
    /// `decompose` sees in production (it no longer looks through phis;
    /// the pipeline's `PhiCollapse` has run by then).  ConstantFold is
    /// intentionally NOT run here: these tests build the canonical
    /// `Add(_, IntConst(-K))` offset shape directly (via [`sub_off`]) and the
    /// deep-chain / memo tests need the un-collapsed structure.
    fn collapse_phis(fg: &mut strider_ir::Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(fg, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    /// Builds the canonical `Add(x, IntConst(-k))` — the post-`ConstantFold`
    /// shape of `x - k`.  The lifter emits `Add(x, Neg(IntConst(k)))`, but
    /// `ConstantFold` folds the `Neg` to a single negative `IntConst` before
    /// any SP-aware pass runs, so the decomposer only ever meets this shape
    /// (it does not peel `Neg` itself).
    fn sub_off(
        b: &mut strider_ir::FunctionBuilder,
        x: ValueId,
        k: i64,
        ty: ValueType,
    ) -> crate::Result<ValueId> {
        let neg_k = b.build_int_const((-k) as u64, ty)?;
        b.build_int_binary_operation(x, neg_k, IntBinaryOp::Add, ty)
    }

    #[test]
    fn decompose_sp_initial_var() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        b.build_return(Some(sp_val), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        // `read_variable(sp)` wraps `InitialVar(sp)` in a single-predecessor
        // phi; PhiCollapse collapses it, so the live SP value (the Return's
        // value input) is the bare `InitialVar(sp)` that decomposes to
        // offset 0.  (Decomposing the now-detached phi output would be None.)
        collapse_phis(&mut fg);
        let live_sp = crate::test_support::return_value(fg.graph())?;
        let r = SpAnalyzer::new(&fg).decompose(live_sp);
        assert!(matches!(r, Some(SpExpr { offset: 0, .. })));
        let _ = sp_val;
        Ok(())
    }

    #[test]
    fn decompose_sp_sub_constant() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let addr = sub_off(&mut b, sp_val, 4, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = SpAnalyzer::new(&fg).decompose(addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_add_negative_unsigned() -> crate::Result<()> {
        // Add(sp, 0xFFFF_FFFC_U32) must decompose to -4 (sign-extended).
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let neg_four = b.build_int_const(0xFFFF_FFFCu64, ValueType::I32)?;
        let addr =
            b.build_int_binary_operation(sp_val, neg_four, IntBinaryOp::Add, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = SpAnalyzer::new(&fg).decompose(addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_readonly_is_idempotent_and_committed_slot_round_trips() -> crate::Result<()> {
        // Read-only decompose is idempotent, and committing a slot (as
        // `StackOffsetDetect` does) round-trips through the cache.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let addr = sub_off(&mut b, sp_val, 4, ValueType::I32)?;
        b.build_return(Some(addr), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r1 = SpAnalyzer::new(&fg).decompose(addr);
        let r2 = SpAnalyzer::new(&fg).decompose(addr);
        assert!(matches!(
            (&r1, &r2),
            (
                Some(SpExpr { offset: -4, .. }),
                Some(SpExpr { offset: -4, .. })
            )
        ));
        if let Some(SpExpr { base, offset }) = r1 {
            fg.side_tables_mut().set_stack_slot(addr, base, offset);
        }
        assert_eq!(
            fg.side_tables().stack_slot_resolved(addr).map(|(_, o)| o),
            Some(-4),
            "committed slot must round-trip"
        );
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
        // An IntConst is not SP-rooted.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region_all()?;
        b.set_entry_region_all(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        assert!(SpAnalyzer::new(&fg).decompose(c).is_none());
        Ok(())
    }

    #[test]
    fn decompose_readonly_walks_deep_offset_chain() -> crate::Result<()> {
        // The spine walk accumulates the constant offset down a deep
        // `sp - 4 - 8 - 12` chain, and each intermediate decomposes to its own
        // partial offset.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let s1 = sub_off(&mut b, sp_val, 4, ValueType::I32)?;
        let s2 = sub_off(&mut b, s1, 8, ValueType::I32)?;
        let s3 = sub_off(&mut b, s2, 12, ValueType::I32)?;
        b.build_return(Some(s3), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let off = |v| SpAnalyzer::new(&fg).decompose(v).map(|e| e.offset);
        assert_eq!(off(s1), Some(-4), "s1 = sp - 4");
        assert_eq!(off(s2), Some(-12), "s2 = sp - 12");
        assert_eq!(off(s3), Some(-24), "s3 = sp - 24");
        Ok(())
    }

    /// Determinism guarantee under a cycle: a loop-carried SP expression
    /// `Phi(InitialVar(sp), Add(phi, -K))` contains a data cycle through the
    /// `Phi`.  Every node in the cone (the `Phi`, the loop-carried `Add`, and a
    /// genuinely-non-SP address) must classify identically regardless of which
    /// value is queried first or whether a memo is shared across queries — the
    /// path-independence that makes caching `None` sound.
    #[test]
    fn decompose_sp_cycle_classifies_identically_regardless_of_query_order() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let loop_hdr = b.create_region_all()?;
        let exit = b.create_region_all()?;
        b.set_entry_region_all(entry)?;

        // entry: sp is the incoming value; branch into the loop header.
        b.set_region(entry);
        b.build_branch(loop_hdr)?;

        // loop header: read sp (a Phi joining the entry value and the
        // loop-carried decremented value), then sp = sp - 4, and conditionally
        // branch back to the header — a data cycle through the sp Phi.
        b.set_region(loop_hdr);
        let sp_phi = b.read_variable(&sp)?;
        let sp_dec = sub_off(&mut b, sp_phi, 4, ValueType::I32)?;
        b.write_variable(&sp, sp_dec)?;
        let keep_looping = b.build_boolean_const(true);
        b.build_if(keep_looping, loop_hdr, exit)?;

        // exit: a genuinely non-SP address in the same function cone.
        b.set_region(exit);
        let global = b.build_int_const(0x4000u64, ValueType::I32)?;
        b.build_return(Some(global), &[])?;
        b.set_lift_addr(None);
        let fg = b.build()?;
        // NB: do NOT collapse phis here — we want the genuine loop-header Phi
        // (multi-predecessor) intact so the cone really contains a cycle.

        // Compute each verdict in isolation (fresh memo per query) — the
        // ground-truth graph verdict for each value.
        let truth = |v: ValueId| -> Option<SpExpr> {
            SpAnalyzer::new(&fg).decompose(v)
        };
        let t_phi = truth(sp_phi);
        let t_dec = truth(sp_dec);
        let t_global = truth(global);

        // The cycle nodes are not provable SP terminals (the decomposer does
        // not look through phis), and the global is genuinely non-SP.
        assert!(t_phi.is_none(), "loop-header Phi(sp) is not an SP terminal");
        assert!(
            t_dec.is_none(),
            "loop-carried Add over a Phi is not provable"
        );
        assert!(t_global.is_none(), "global address is not SP-rooted");

        // Now query in every order through ONE shared memo and assert each
        // value's verdict matches its isolated ground truth — verdicts are
        // path-independent, so a cached `None` (or `Some`) is always correct.
        for order in [
            [sp_phi, sp_dec, global],
            [global, sp_dec, sp_phi],
            [sp_dec, global, sp_phi],
        ] {
            for v in order {
                let got = SpAnalyzer::new(&fg).decompose(v);
                let want = if v == sp_phi {
                    t_phi
                } else if v == sp_dec {
                    t_dec
                } else {
                    t_global
                };
                assert_eq!(
                    got.map(|e| e.offset),
                    want.map(|e| e.offset),
                    "verdict for {v:?} must be query-order-independent"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn decompose_sp_phi_with_non_sp_pred_returns_none() -> crate::Result<()> {
        // A VarPhi(sp) whose predecessor value is NOT SP-rooted must
        // decompose to None.  Previously decompose_sp_phi fabricated a
        // Terminal{base: phi_output, offset: 0} on this path; callers
        // ignored `base` but trusted `offset == 0`, which on conventions
        // where stack_arg_offsets[0] == 0 (AArch64/ARM AAPCS) could
        // misclassify a non-SP-rooted phi as the first stack argument or
        // wrongly forward a load over it.
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn()?;
        let entry = b.create_region_all()?;
        let a = b.create_region_all()?;
        let bb = b.create_region_all()?;
        let c = b.create_region_all()?;
        b.set_entry_region_all(entry)?;

        // entry: if cond goto a else goto bb
        b.set_region(entry);
        let cond = b.build_boolean_const(true);
        b.build_if(cond, a, bb)?;

        // a: sp = sp - 4 (SP-rooted)
        b.set_region(a);
        let sp_a = b.read_variable(&sp)?;
        let sp_minus_4 = sub_off(&mut b, sp_a, 4, ValueType::I32)?;
        b.write_variable(&sp, sp_minus_4)?;
        b.build_branch(c)?;

        // bb: sp = 0xDEAD_BEEF (NOT SP-rooted — a literal value pretending
        // to be a new SP).
        b.set_region(bb);
        let bogus = b.build_int_const(0xDEAD_BEEFu64, ValueType::I32)?;
        b.write_variable(&sp, bogus)?;
        b.build_branch(c)?;

        // c: read sp.  The phi at c has two predecessor values: the SP-rooted
        // one from `a` and the bogus const from `bb`.  decompose must
        // refuse to claim "this is sp + K" for that phi.
        b.set_region(c);
        let sp_at_c = b.read_variable(&sp)?;
        b.build_return(Some(sp_at_c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);

        let r = SpAnalyzer::new(&fg).decompose(sp_at_c);
        assert!(
            r.is_none(),
            "expected None for VarPhi(sp) with a non-SP-rooted predecessor, got {r:?}"
        );
        Ok(())
    }

    /// FreeBSD i386 10.0 prologue: `and $0xfffffff8, %esp` aligns the
    /// stack to 8 bytes after the saved-register pushes.  All subsequent
    /// stack arithmetic is anchored at the And's output, not at
    /// `InitialVar(sp)`, so `decompose` must recognise the And and
    /// treat its output as a stable opaque base (offset 0) — otherwise
    /// every store after the alignment dance is a non-decomposable
    /// `Store(_)`, and `CallStackArgCollect` walks past the call's args
    /// as "non-aliasing".
    #[test]
    fn decompose_sp_and_with_alignment_mask_yields_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        // Simulate `and $0xfffffff8, %esp`.
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        b.build_return(Some(aligned), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let r = SpAnalyzer::new(&fg).decompose(aligned);
        // The aligned output is a stable opaque base.  Offset = 0
        // because the alignment can shift the value by 0..7 bytes — we
        // can't pin a constant delta, but we *can* pin a stable
        // `ValueId` that subsequent decompositions reference.
        let Some(SpExpr { base, offset }) = r else {
            panic!("expected Terminal from And-aligned SP, got {r:?}");
        };
        assert_eq!(offset, 0, "And-aligned base offset must be 0");
        // Base must NOT be the InitialVar(sp) output — it's the And output.
        let base_node = fg.producer(base);
        assert!(
            matches!(
                *fg.node_kind(base_node),
                NodeKind::IntBinaryOp(IntBinaryOp::And)
            ),
            "And-aligned base must point to the And node, got {:?}",
            fg.node_kind(base_node)
        );
        Ok(())
    }

    /// Following the alignment dance, the function does
    /// `sub $0x1d0, %esp` (the local-frame reservation).  The post-Sub
    /// SP must decompose to the *same* opaque base (the And output),
    /// just with a non-zero offset.  Without this, every cdecl call
    /// site after the alignment dance has args at addresses that
    /// `decompose` cannot relate to each other, breaking
    /// `CallStackArgCollect`.
    #[test]
    fn decompose_sp_sub_after_and_chains_offset_through_opaque_base() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let sp_val = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        let aligned =
            b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
        let post_sub = sub_off(&mut b, aligned, 0x1D0, ValueType::I32)?;
        b.build_return(Some(post_sub), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let aligned_dec = SpAnalyzer::new(&fg)
            .decompose(aligned)
            .expect("aligned must decompose");
        let post_sub_dec = SpAnalyzer::new(&fg)
            .decompose(post_sub)
            .expect("post_sub must decompose");
        let SpExpr {
            base: aligned_base,
            offset: aligned_off,
        } = aligned_dec;
        let SpExpr {
            base: post_sub_base,
            offset: post_sub_off,
        } = post_sub_dec;
        assert_eq!(
            aligned_base, post_sub_base,
            "post-Sub base must equal post-And base (opaque base shared)"
        );
        assert_eq!(aligned_off, 0);
        assert_eq!(post_sub_off, -0x1D0, "Sub by 0x1D0 shifts offset by -0x1D0");
        Ok(())
    }

    /// Deep nested-`And` shape: the iterative `rpo` sweep re-bases at
    /// each level and resolves to an opaque base without recursion, so a
    /// pathologically deep chain terminates cleanly (no stack overflow,
    /// no recursion-depth budget) with an opaque `Terminal` base.
    #[test]
    fn decompose_sp_deep_and_chain_terminates_without_overflow() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
        const N: usize = 6000;
        for _ in 0..N {
            current =
                b.build_int_binary_operation(current, mask, IntBinaryOp::And, ValueType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        // Iterative rpo sweep: the deep And chain re-bases at each level and
        // resolves to an opaque base without recursion, so no stack overflow.
        let r = SpAnalyzer::new(&fg).decompose(current);
        assert!(matches!(r, Some(SpExpr { offset: 0, .. })));
        Ok(())
    }

    /// Regression: `decompose`
    /// must not blow the thread stack on a deep `sp + K1 + K2 + ... + KN`
    /// chain.  The recursive form overflowed at ~4-8k nodes; the
    /// iterative form must walk a 5000-node chain without panic AND
    /// produce the correct cumulative offset.
    #[test]
    fn decompose_sp_does_not_stack_overflow_on_deep_chain() -> crate::Result<()> {
        let sp = sp();
        let mut b = RegisterSet::new()
            .tracked(sp)
            .arg(sp)
            .stack_vn(sp)
            .build_fn_single_region()?;
        let mut current = b.read_variable(&sp)?;
        const N: usize = 5000;
        for _ in 0..N {
            let one = b.build_int_const(1u64, ValueType::I32)?;
            current =
                b.build_int_binary_operation(current, one, IntBinaryOp::Add, ValueType::I32)?;
        }
        b.build_return(Some(current), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let SpExpr { offset, .. } = SpAnalyzer::new(&fg)
            .decompose(current)
            .expect("5000-node chain must decompose without stack-overflowing");
        assert_eq!(
            offset, N as i128,
            "cumulative offset must equal N adds of +1"
        );
        Ok(())
    }
}

#[cfg(test)]
mod alias_tests {
    use super::super::ranges::store_value_byte_size;
    use super::*;
    use strider_ir::node::{NodeKind, ValueType};
    use strider_ir::{IRBuilderExt, IntBinaryOp};
    use strider_ir_test_utils::{make_sp_fn, stack_vn_x86};

    /// The `InitialVar(sp)` output — the canonical entry-SP terminal base
    /// that `decompose` returns for any clean `sp + k` address.
    fn entry_sp_value(f: &Function, sp: rsleigh::Vn) -> ValueId {
        let node = f
            .graph()
            .all_node_ids()
            .find(
                |&n| matches!(*f.node_kind(n), NodeKind::InitialVar(id) if f.initial_vn(id) == sp),
            )
            .expect("InitialVar(sp) exists");
        f.node_outputs_exact::<1>(node)
            .expect("InitialVar has 1 output")[0]
    }

    fn only_store(f: &Function) -> NodeId {
        f.graph()
            .all_node_ids()
            .find(|&n| matches!(f.node_kind(n), NodeKind::Store(_)))
            .expect("one store")
    }

    /// Test composition of the store→load alias verdict: classify the store
    /// address, derive its size, then
    /// run the pure class-on-class [`alias_verdict`] — exactly what the
    /// production `SpAliasOracle` Store arm does now that the bespoke
    /// `store_alias_verdict` method was dissolved into `classify_store_addr`.
    fn store_alias_verdict(
        f: &Function,
        store: NodeId,
        load_class: AddrClass,
        load_size: i128,
        mode: AliasMode,
        distinct_sp_bases_disjoint: bool,
    ) -> AliasVerdict {
        let store_size = store_value_byte_size(f.graph(), f.store_data(store));
        let store_class = SpAnalyzer::new(f).classify_store_addr(store);
        alias_verdict(
            SizedAddr {
                class: load_class,
                size: load_size,
            },
            SizedAddr {
                class: store_class,
                size: store_size,
            },
            mode,
            distinct_sp_bases_disjoint,
        )
    }

    /// Collapse the single-predecessor `read_variable(sp)` phi so SP
    /// addresses are bare `InitialVar(sp) + k` terminals — the shape these
    /// alias helpers see in production (the decomposer no longer looks
    /// through phis).
    fn collapse(f: &mut Function) {
        let mut p = crate::OptimizerPipeline::new();
        p.add(crate::PhiCollapse);
        p.add(crate::RegionCollapse);
        p.run(f, &mut crate::OptCtx::new(None))
            .expect("phi collapse");
    }

    /// Regression for the two-terminal base bug: a `Store` whose address is
    /// an *alignment-masked* SP base (`(sp & mask) + 8`) must NOT be proven
    /// disjoint from a query slot rooted at the *entry* SP just because
    /// their offsets don't overlap.  The two bases differ by the runtime
    /// alignment delta `sp mod align`, so the offset comparison is
    /// meaningless and the verdict must be may-alias (not `Disjoint`).
    #[test]
    fn different_base_terminal_store_may_alias() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            // aligned = sp & 0xFFFF_FFF8  (a distinct SP base)
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            // store at aligned + 8
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(
            verdict,
            AliasVerdict::MayAlias,
            "store at an alignment-masked base must may-alias an entry-SP query \
             (different bases are not offset-comparable)"
        );
    }

    /// With the `distinct_sp_bases_disjoint` opt-in (used by stack-arg
    /// detection), the SAME different-base store is instead treated as
    /// `Disjoint`: incoming-arg slots above the entry SP are assumed not to
    /// overlap frame locals rooted at an alignment-masked SP.
    #[test]
    fn different_base_terminal_store_disjoint_when_opted_in() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let mask = b.build_int_const(0xFFFF_FFF8u64, ValueType::I32)?;
            let aligned =
                b.build_int_binary_operation(sp_val, mask, IntBinaryOp::And, ValueType::I32)?;
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(aligned, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            true,
        );
        assert_eq!(
            verdict,
            AliasVerdict::Disjoint,
            "with distinct_sp_bases_disjoint, a different-base store is assumed disjoint"
        );
    }

    /// Sanity: same base, non-overlapping offsets are provably disjoint.
    #[test]
    fn same_base_disjoint_offsets_is_disjoint() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        // store at sp+8 (size 4) vs query at sp+0 (size 4): disjoint.
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 0,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Disjoint);
    }

    /// Sanity: same base, same offset is an exact `Match`.
    #[test]
    fn same_base_same_offset_is_match() {
        let sp = stack_vn_x86();
        let mut f = make_sp_fn(sp, |b, sp_val| {
            let eight = b.build_int_const(8u64, ValueType::I32)?;
            let store_addr =
                b.build_int_binary_operation(sp_val, eight, IntBinaryOp::Add, ValueType::I32)?;
            let data = b.build_int_const(0xAAu64, ValueType::I32)?;
            b.build_store(store_addr, data, rsleigh::VnSpace::RAM)?;
            // Load + return so the store (and its SP-address phi) are reachable
            // and PhiCollapse collapses the read_variable phi.
            let loaded = b.build_load(store_addr, rsleigh::VnSpace::RAM, ValueType::I32)?;
            b.build_return(Some(loaded), &[])?;
            Ok(())
        })
        .unwrap();
        collapse(&mut f);

        let store = only_store(&f);
        let query_base = entry_sp_value(&f, sp);
        // store at sp+8 (size 4) vs query at sp+8 (size 4): exact match.
        let verdict = store_alias_verdict(
            &f,
            store,
            AddrClass::SpRooted {
                base: query_base,
                offset: 8,
            },
            4,
            AliasMode::StackGlobalDisjoint,
            false,
        );
        assert_eq!(verdict, AliasVerdict::Match);
    }
}
