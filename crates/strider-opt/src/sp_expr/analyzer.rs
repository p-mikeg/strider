//! Stack-pointer analyzer: SP decomposition + address classification + alias
//! verdict, merged into one [`SpAnalyzer`] type.
//!
//! `decompose` is the workhorse: given an output that is `InitialVar(sp)`
//! transformed by `Add` of constants (subtraction appears as `Add(_, Neg(K))`)
//! or anchored at an alignment-masked `sp & mask`, it returns a single
//! `SpExpr { base, offset }` terminal (or `None`).
//! Callers thread a per-pass-call memo through it so repeated walks over the
//! same SP chain cost O(1) on cache hit.
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
//! the latter preferring the stack-offset side-table); the pure,
//! class-on-class verdict table is the free [`alias_verdict`].

use rustc_hash::FxHashMap;

use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker, IntBinaryOp};

use super::ranges::ranges_disjoint;
use crate::AliasMode;
use AddrClass::*;

/// Decomposed stack-pointer expression: `base + offset`, where `base` is an
/// SP-rooted node (`InitialVar(sp)` or an alignment-masked SP `And` output).
///
/// `decompose` returns `Option<SpExpr>`; `None` carries the
/// "not a provable SP terminal" case, so there is no separate variant for it.
#[derive(Clone, Copy, Debug)]
pub struct SpExpr {
    pub base: ValueId,
    pub offset: i64,
}

impl SpExpr {
    /// Adds `delta` to the offset, returning `None` (fail-closed: opaque
    /// base, no provable slot) on `i64` overflow rather than wrapping a deep
    /// Add chain into a wrong concrete offset that the alias oracle would then
    /// reason about as a valid nearby slot.  Real frames have small offsets;
    /// the decomposer is fed arbitrary lifted arithmetic.
    #[must_use]
    pub(crate) fn shifted(self, delta: i64) -> Option<Self> {
        Some(SpExpr {
            base: self.base,
            offset: self.offset.checked_add(delta)?,
        })
    }
}

/// Per-pass-call memo for `decompose`.
pub type SpExprMemo = FxHashMap<ValueId, Option<SpExpr>>;

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
    SpRooted { base: ValueId, offset: i64 },
    /// `NodeKind::IntConst(_)` address — a literal `.data`/`.rodata`/
    /// `.bss`/MMIO pointer.  Two `Constant` addresses with equal values
    /// refer to the same byte range; disjoint values are proven
    /// non-overlapping via [`ranges_disjoint`].
    Constant { addr: i64 },
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

/// Stack-pointer analyzer: holds the `function` and a per-pass-call `memo`,
/// and merges SP decomposition, address classification, and the store-alias
/// verdict.  The stack varnode to anchor on is the function's own
/// `default_cc().stack_vn`, read on demand — never stored, so it cannot drift
/// from the function under analysis.
pub(crate) struct SpAnalyzer<'a> {
    function: &'a Function,
    memo: &'a mut SpExprMemo,
}

impl<'a> SpAnalyzer<'a> {
    pub(crate) fn new(function: &'a Function, memo: &'a mut SpExprMemo) -> Self {
        Self { function, memo }
    }

    /// Decomposes `value` into `InitialVar(sp) + K` (or per-branch equivalent),
    /// caching every classified node (`Some` *and* `None`) in the memo.
    ///
    /// Implemented as a single defs-before-uses (reverse-post-order) sweep over
    /// the address cone: because every operand is classified before the node
    /// that consumes it, each arm is a local map lookup.  `Phi` nodes are not
    /// SP terminals (they classify to `None`), so the cone the sweep traverses
    /// is a DAG of `InitialVar` / `Add` / `And` nodes.
    ///
    /// Caching `None` is sound because a node's verdict is a deterministic
    /// function of the *fixed* graph within a memo's lifetime, independent of
    /// which `value` (call path) was queried:
    ///
    /// * The RPO sweep visits the queried value's entire backward operand cone
    ///   (`compute_full` is closed under data inputs) in defs-before-uses order,
    ///   so every `Add` / `And` arm reads its operands' *already-classified*
    ///   memo entries — never a spuriously-absent one.  A node's verdict is
    ///   therefore fixed by its kind, its input edges, and its operands'
    ///   verdicts — all graph-determined.
    /// * The only cyclic data dependency in SSA is loop-carried through
    ///   `Phi` / `MemPhi`, which classify to `None` from their kind alone with
    ///   no operand recursion.  There is no acyclic depth-truncation (the sweep
    ///   has no recursion-depth budget; it visits the finite live set once), so
    ///   no node's `None` is an artefact of where the walk started.
    /// * Callers hold the memo only against an immutable `&Function`; the
    ///   pipeline clears `sp_memo` after every graph mutation, so a cached
    ///   verdict never outlives the graph it was computed against.
    pub(crate) fn decompose(&mut self, value: ValueId) -> Option<SpExpr> {
        if let Some(cached) = self.memo.get(&value) {
            return *cached;
        }
        let graph = self.function.graph();
        let rpo = match self.function.walk_info(Some(graph.producer(value))) {
            Some(info) => self.function.reverse_postorder(&info),
            None => Vec::new(),
        };
        for node in rpo {
            let Ok([node_out]) = self.function.node_outputs_exact::<1>(node) else {
                continue;
            };
            if self.memo.contains_key(&node_out) {
                continue;
            }
            let expr = self.classify_sp_node(node, node_out);
            self.memo.insert(node_out, expr);
        }
        // The sweep only inserts entries for single-output producers, so a
        // queried `value` whose producer has ≠1 output (never an SP terminal)
        // would otherwise stay absent and force a full re-walk on every repeat
        // query.  Memoise its `None` so the second query is a hit; `or_insert`
        // hands back the now-present entry, so no second lookup is needed.
        *self.memo.entry(value).or_insert(None)
    }

    /// Classifies a single node in the address cone given that all of its
    /// operands have already been classified into the memo (guaranteed by the
    /// defs-before-uses `rpo` order).  `Phi` is not an SP terminal and falls
    /// through to `None`.
    fn classify_sp_node(&self, node: NodeId, node_value: ValueId) -> Option<SpExpr> {
        let function = self.function;
        match *function.node_kind(node) {
            NodeKind::InitialVar(id)
                if function.initial_vn(id) == function.default_cc().stack_vn =>
            {
                Some(SpExpr {
                    base: node_value,
                    offset: 0,
                })
            }
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => {
                // IntBinaryOp has exactly 2 inputs (validated structural invariant).
                let [lhs, rhs] = function
                    .node_inputs_exact::<2>(node)
                    .expect("IntBinaryOp(Add) has 2 inputs (validated)");
                // SP + const in either operand order; the constant shifts the
                // other operand's decomposed offset.  Right operand checked
                // first (post-ConstantFold an Add never carries two constants).
                let (sp_operand, c) =
                    match (function.int_const_i64(rhs), function.int_const_i64(lhs)) {
                        (Some(c), _) => (lhs, c),
                        (None, Some(c)) => (rhs, c),
                        _ => return None,
                    };
                self.memo
                    .get(&sp_operand)
                    .copied()
                    .flatten()
                    .and_then(|e| e.shifted(c))
            }
            // x86 cdecl alignment dance: `and $0xfffffff8, %esp` (or wider
            // `0xfffffff0` for SSE-aligned frames).  The And's output is
            // runtime-aligned `(SP & mask)` — its exact value depends on the
            // entry SP's alignment, so the offset relative to `InitialVar(sp)`
            // is unknown.  But within the function the And's output is *fixed*
            // and serves as a stable opaque base for every subsequent stack
            // address.  Return `Terminal { base: <And output>, offset: 0 }`
            // so downstream Adds / Subs of constants chain through normally
            // and `StackOffsetDetect` can classify the post-alignment stores
            // as stack-aliased using this base.
            //
            // Only matches when the non-mask operand is itself an SP-rooted
            // expression — guards against `And(rax, mask)` accidentally
            // producing a fake stack base.
            NodeKind::IntBinaryOp(IntBinaryOp::And) => {
                // IntBinaryOp has exactly 2 inputs (validated structural invariant).
                let [l, r] = function
                    .node_inputs_exact::<2>(node)
                    .expect("IntBinaryOp(And) has 2 inputs (validated)");
                // Require the constant operand to be an *alignment* mask —
                // a contiguous run of high 1-bits (e.g. 0xFFFF_FFF0).  A
                // low-bit mask like `And(sp, 0xF)` is a bit-extraction (value
                // in [0,15]), NOT a stack base; treating it as one would feed
                // a bogus opaque base to `distinct_sp_bases_disjoint`.
                let sp_value = if function.int_const_u128(r).is_some_and(is_alignment_mask) {
                    l
                } else if function.int_const_u128(l).is_some_and(is_alignment_mask) {
                    r
                } else {
                    return None;
                };
                // The And's output is a fresh opaque base (offset 0) for
                // downstream walkers; we only require the non-mask operand to
                // be SP-rooted, discarding its concrete decomposition.
                self.memo.get(&sp_value).copied().flatten().map(|_| SpExpr {
                    base: node_value,
                    offset: 0,
                })
            }
            _ => None,
        }
    }

    /// Classifies a load / store address.  Cheap: `decompose` is memoised
    /// across the function, the `IntConst` peek is a single match.
    pub(super) fn classify_addr(&mut self, addr: ValueId) -> AddrClass {
        match self.decompose(addr) {
            Some(SpExpr { base, offset }) => AddrClass::SpRooted { base, offset },
            None => {
                if let Some(c) = self.function.int_const_u128(addr) {
                    AddrClass::Constant { addr: c as i64 }
                } else {
                    AddrClass::Anchor { value: addr }
                }
            }
        }
    }

    /// Classifies a raw `NodeKind::Store`'s address into an [`AddrClass`],
    /// preferring the `Function::stack_offsets` side-table (the SSoT populated
    /// by `StackOffsetDetect`) over a fresh `decompose`.  The side-table offset
    /// survives address rewrites that leave `decompose` unable to re-derive it
    /// (an earlier pass folding the address into an opaque shape), so it is
    /// consulted first.  The store-side counterpart of [`Self::classify_addr`];
    /// callers feed the result straight into [`alias_verdict`].
    pub(crate) fn classify_store_addr(&mut self, store_node: NodeId) -> AddrClass {
        match self.function.stack_offset(store_node) {
            Some((base, offset)) => AddrClass::SpRooted { base, offset },
            None => {
                let addr = self.function.store_addr(store_node);
                self.classify_addr(addr)
            }
        }
    }
}

/// Is `m` a stack-*alignment* mask — a contiguous run of high 1-bits with at
/// least one low 0-bit (e.g. `0xFFFF_FFF8`, `0xFFFF_FFF0`)?  An alignment mask
/// clears only the low-order bits; a low-bit mask (`0xF`) is a bit-extraction,
/// not a base.  `0` and all-ones masks are rejected (no alignment effect / not
/// a low-clearing mask).
fn is_alignment_mask(m: u128) -> bool {
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
    load_off: i64,
    load_size: i64,
    store_off: i64,
    store_size: i64,
) -> AliasVerdict {
    if load_off == store_off {
        AliasVerdict::Match
    } else if ranges_disjoint(load_off, load_size, store_off, store_size) {
        AliasVerdict::Disjoint
    } else {
        AliasVerdict::MayAlias
    }
}

/// Pairwise alias verdict between a load's class + size and a store's
/// class + size under the given [`AliasMode`].
pub(crate) fn alias_verdict(
    load_class: AddrClass,
    load_size: i64,
    store_class: AddrClass,
    store_size: i64,
    mode: AliasMode,
    distinct_sp_bases_disjoint: bool,
) -> AliasVerdict {
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
        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(live_sp);
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
        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(addr);
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
        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(addr);
        assert!(matches!(r, Some(SpExpr { offset: -4, .. })));
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_hit_returns_same_result() -> crate::Result<()> {
        // Calling decompose twice on the same out should populate the memo
        // and return the same answer.
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
        let mut memo = SpExprMemo::default();
        let r1 = SpAnalyzer::new(&fg, &mut memo).decompose(addr);
        // Memo should now be populated.
        assert!(memo.contains_key(&addr));
        let r2 = SpAnalyzer::new(&fg, &mut memo).decompose(addr);
        assert!(matches!(
            (&r1, &r2),
            (
                Some(SpExpr { offset: -4, .. }),
                Some(SpExpr { offset: -4, .. })
            )
        ));
        Ok(())
    }

    #[test]
    fn decompose_sp_non_sp_returns_none() -> crate::Result<()> {
        // An IntConst is not SP-rooted.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        assert!(SpAnalyzer::new(&fg, &mut memo).decompose(c).is_none());
        Ok(())
    }

    #[test]
    fn decompose_sp_memo_caches_intermediate_results() -> crate::Result<()> {
        // Edge case: decomposing the outermost node of a deep `sp - K1 - K2 - K3`
        // chain must populate the memo for ALL intermediate sub-expressions, so
        // a sibling walk hitting any of them gets a cache hit. The previous
        // `if visiting.is_empty()` predicate only fired at the outermost call
        // frame, so intermediates were never cached and the memo was useless
        // for cross-call sharing.
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

        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(s3);
        assert!(matches!(r, Some(SpExpr { offset: -24, .. })));

        // After one top-level walk, all three intermediate outputs must be
        // memoized. (sp_val itself is cached too, but its ValueId is
        // VarPhi-of-InitialVar, which we don't directly check here.)
        assert!(memo.contains_key(&s3), "expected memo entry for s3");
        assert!(memo.contains_key(&s2), "expected memo entry for s2");
        assert!(memo.contains_key(&s1), "expected memo entry for s1");
        Ok(())
    }

    #[test]
    fn decompose_sp_caches_none_results() -> crate::Result<()> {
        // A `None` verdict is a deterministic function of the fixed graph
        // (kind + input edges + operands' deterministic verdicts), not of the
        // query path: the iterative RPO sweep classifies every cone node from
        // already-classified operands and has no depth-truncation, and the
        // memo lives only against an immutable graph.  So a `None` verdict is
        // cached and reused, sparing the repeated cone walk for the common
        // non-SP (constant/global/heap) address case.
        let mut b = strider_ir_test_utils::empty_builder()?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let c = b.build_int_const(0x1000u64, ValueType::I32)?;
        b.build_return(Some(c), &[])?;
        b.set_lift_addr(None);
        let mut fg = b.build()?;
        collapse_phis(&mut fg);
        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(c);
        assert!(r.is_none());
        // The verdict is now cached: the key is present and maps to a `None`
        // verdict, so a repeat query short-circuits instead of re-walking.
        let cached = memo
            .get(&c)
            .expect("decompose must cache the None verdict so the cone is not re-walked");
        assert!(
            cached.is_none(),
            "cached verdict for a non-SP address is None"
        );
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
        let entry = b.create_region()?;
        let loop_hdr = b.create_region()?;
        let exit = b.create_region()?;
        b.set_entry_region(entry)?;

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
            let mut m = SpExprMemo::default();
            SpAnalyzer::new(&fg, &mut m).decompose(v)
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
            let mut shared = SpExprMemo::default();
            for v in order {
                let got = SpAnalyzer::new(&fg, &mut shared).decompose(v);
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
        let entry = b.create_region()?;
        let a = b.create_region()?;
        let bb = b.create_region()?;
        let c = b.create_region()?;
        b.set_entry_region(entry)?;

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

        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(sp_at_c);
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
        let mut memo = SpExprMemo::default();
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(aligned);
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
        let mut memo = SpExprMemo::default();
        let aligned_dec = SpAnalyzer::new(&fg, &mut memo)
            .decompose(aligned)
            .expect("aligned must decompose");
        let post_sub_dec = SpAnalyzer::new(&fg, &mut memo)
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
        let mut memo = SpExprMemo::default();
        // Iterative rpo sweep: the deep And chain re-bases at each level and
        // resolves to an opaque base without recursion, so no stack overflow.
        let r = SpAnalyzer::new(&fg, &mut memo).decompose(current);
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
        let mut memo = SpExprMemo::default();
        let SpExpr { offset, .. } = SpAnalyzer::new(&fg, &mut memo)
            .decompose(current)
            .expect("5000-node chain must decompose without stack-overflowing");
        assert_eq!(
            offset, N as i64,
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
    /// address (stack-offset SSoT before `decompose`), derive its size, then
    /// run the pure class-on-class [`alias_verdict`] — exactly what the
    /// production `SpAliasOracle` Store arm does now that the bespoke
    /// `store_alias_verdict` method was dissolved into `classify_store_addr`.
    fn store_alias_verdict(
        f: &Function,
        memo: &mut SpExprMemo,
        store: NodeId,
        load_class: AddrClass,
        load_size: i64,
        mode: AliasMode,
        distinct_sp_bases_disjoint: bool,
    ) -> AliasVerdict {
        let store_size = store_value_byte_size(f.graph(), f.store_data(store));
        let store_class = SpAnalyzer::new(f, memo).classify_store_addr(store);
        alias_verdict(
            load_class,
            load_size,
            store_class,
            store_size,
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
        let mut memo = SpExprMemo::default();
        let verdict = store_alias_verdict(
            &f,
            &mut memo,
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
        let mut memo = SpExprMemo::default();
        let verdict = store_alias_verdict(
            &f,
            &mut memo,
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
        let mut memo = SpExprMemo::default();
        // store at sp+8 (size 4) vs query at sp+0 (size 4): disjoint.
        let verdict = store_alias_verdict(
            &f,
            &mut memo,
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
        let mut memo = SpExprMemo::default();
        // store at sp+8 (size 4) vs query at sp+8 (size 4): exact match.
        let verdict = store_alias_verdict(
            &f,
            &mut memo,
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
