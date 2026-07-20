//! Collapses the multi-node "flag-tree" conditions that flag-register
//! architectures emit for `cmp`-then-branch into a single
//! [`strider_ir::IntCmpOp`] on the original `(a, b)` pair.
//!
//! AArch64 `cmp a, b` lifts (post `IntSub` / `IntLessEqual` canonicalisation)
//! to four flags, and each cond code reads a fixed boolean tree of them:
//!
//! ```text
//! diff = Add(a, Neg(b))
//! ZR   = Equal(diff, 0)           // Z
//! NG   = IntSless(diff, 0)        // N
//! CY   = BitNot(IntLess(a, b))    // C, at I1
//! OV   = IntSborrow(a, b)         // V
//!
//! EQ/NE  ZR                          HI/LS  BoolAnd(CY, BitNot(ZR)) / dual
//! CS/CC  CY                          GE/LT  Equal(NG, OV)
//! VS/VC  OV                          GT/LE  BoolAnd(BitNot(ZR), Equal(NG, OV)) / dual
//! ```
//!
//! `MI`/`PL` (bare `NG`) is deliberately left alone: `Sless(a-b, 0)` differs
//! from `Sless(a, b)` when the subtraction overflows.  `CS`/`CC` and `VS`/`VC`
//! are already in `(a, b)` form.
//!
//! Run after `ConstantFold` (so `BitNot(BitNot(x)) -> x` at `I1` has collapsed)
//! and before `IfCondInversion` (so the cond carries at most one BitNot layer).

use std::rc::Rc;

use crate::{BoxedRule, apply_rules_in_order, rewrite_rule};
use strider_ir::IRViewer;
use strider_ir::node::{ExtendOp, IntBinaryOp, NodeId, NodeKind, ValueId, ValueType};
use strider_pattern::{
    Bindings, Capture, CaptureExt, add, any_int_const, bool_and, bool_not, bool_or, capture_typed,
    int_const, int_const_with, int_eq, int_lt, int_sborrow, int_slt, neg, one_of, shl, template,
    var, xor, zero_extend,
};

use crate::error::Result;
use crate::peephole::{PeepholePass, PeepholeRewrite, SeedOrder};

#[derive(Clone)]
pub struct FlagCmpCanonicalize {
    rules: Rc<Vec<BoxedRule>>,
}

impl FlagCmpCanonicalize {
    pub fn new() -> Self {
        Self {
            rules: Rc::new(build_rules()),
        }
    }
}

impl Default for FlagCmpCanonicalize {
    fn default() -> Self {
        Self::new()
    }
}

impl PeepholePass for FlagCmpCanonicalize {
    fn matches_kind(&self, _kind: &NodeKind) -> bool {
        true
    }

    /// Outermost-first: a bottom-up seed would rewrite an inner sub-pattern and
    /// destroy the enclosing flag-tree match.
    fn seed_order(&self) -> SeedOrder {
        SeedOrder::Postorder
    }

    fn try_rewrite(
        &self,
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &mut crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        // Imperative arm: the variable-arity CR pack
        // `Or(ShiftLeft(ZeroExtend(cmp_i), pos_i)...)` doesn't fit the
        // fixed-shape `rewrite_rule` DSL.
        if let Some(cmp) = canonicalize_cr_bit_test(edit, root)? {
            return Ok(PeepholeRewrite::from_new_value(edit, Some(cmp)));
        }
        let opt = apply_rules_in_order(&self.rules)(edit, root)?;
        Ok(PeepholeRewrite::from_new_value(edit, opt))
    }

    /// A collapsed `IntCmpOp` cannot expose a fresh flag-tree shape to its
    /// consumers, so skip the re-enqueue.
    fn propagate_to_consumers(&self) -> bool {
        false
    }
}

/// `M == -N` at `width_src`'s width.  `false` unless all bindings resolve.
fn neg_relation(
    binds: &Bindings,
    func: &strider_ir::Function,
    m: Capture,
    n: Capture,
    width_src: Capture,
) -> bool {
    let (Some(m_val), Some(n_val), Some(width)) = (
        binds.get_uint(m, func),
        binds.get_uint(n, func),
        binds.get_type(width_src, func).map(|t| t.bit_mask_u128()),
    ) else {
        return false;
    };
    (m_val & width) == (n_val.wrapping_neg() & width)
}

/// `M == C1 - N` at `width_src`'s width.  `false` unless all bindings resolve.
fn sub_relation(
    binds: &Bindings,
    func: &strider_ir::Function,
    m: Capture,
    n: Capture,
    c1: Capture,
    width_src: Capture,
) -> bool {
    let (Some(m_val), Some(n_val), Some(c1_val), Some(width)) = (
        binds.get_uint(m, func),
        binds.get_uint(n, func),
        binds.get_uint(c1, func),
        binds.get_type(width_src, func).map(|t| t.bit_mask_u128()),
    ) else {
        return false;
    };
    (m_val & width) == (c1_val.wrapping_sub(n_val) & width)
}

fn build_rules() -> Vec<BoxedRule> {
    // Captures may be shared across rules: each rule matches with fresh
    // `Bindings`, so "same node everywhere" binds intra-rule only.
    let a = Capture::new();
    let b = Capture::new();
    // The `Less` constant `N` and the `Add` constant `M`.
    let n = Capture::new();
    let m = Capture::new();
    // The compare-base offset `C1` and the whole offset value `X = Add(b, C1)`.
    let c1 = Capture::new();
    let x = Capture::new();

    // Emits an LS rule and its De-Morgan HI dual:
    //   LS:  Or(less, eq)                          -> BitNot(cmp)
    //   HI:  And(BitNot(less), BitNot(eq))         -> cmp
    macro_rules! ls_hi_pair {
        ($less:expr, $eq:expr, $guard:expr, $cmp:expr $(,)?) => {
            [
                rewrite_rule(
                    bool_or($less, $eq).when_match($guard),
                    template::bool_not($cmp),
                ),
                rewrite_rule(
                    bool_and(bool_not($less), bool_not($eq)).when_match($guard),
                    $cmp,
                ),
            ]
        };
    }

    let mut rules: Vec<BoxedRule> = vec![
        // EQ:  Equal(Add(a, Neg(b)), 0) -> Equal(a, b)
        rewrite_rule(
            int_eq(add(var(a), neg(var(b))), int_const(0u128)),
            template::int_eq(var(a), var(b)),
        ),
        // HI -> IntLess(b, a), in either arch shape:
        //    raw NZCV:    BoolAnd(BitNot(IntLess(a, b)), BitNot(Equal(diff, 0)))
        //    decomposed:  BoolAnd(BitNot(Equal(a, b)), BitNot(IntLess(a, b)))
        rewrite_rule(
            one_of![
                bool_and(
                    bool_not(int_lt(var(a), var(b))),
                    bool_not(int_eq(add(var(a), neg(var(b))), int_const(0u128))),
                ),
                bool_and(
                    bool_not(int_eq(var(a), var(b))),
                    bool_not(int_lt(var(a), var(b))),
                ),
            ],
            template::int_lt(var(b), var(a)),
        ),
        // LS -> BitNot(IntLess(b, a)), in either arch shape:
        //    raw NZCV:    BoolOr(IntLess(a, b), Equal(diff, 0))
        //    decomposed:  BoolOr(Equal(a, b), IntLess(a, b))
        // The raw form requires ConstantFold to have already cancelled the
        // `BitNot(BitNot(IntLess))` chain that `BitNot(CY)` produces.
        rewrite_rule(
            one_of![
                bool_or(
                    int_lt(var(a), var(b)),
                    int_eq(add(var(a), neg(var(b))), int_const(0u128)),
                ),
                bool_or(int_eq(var(a), var(b)), int_lt(var(a), var(b))),
            ],
            template::bool_not(template::int_lt(var(b), var(a))),
        ),
        // LT:  BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))) -> IntSless(a, b)
        rewrite_rule(
            bool_not(int_eq(
                zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                zero_extend(int_sborrow(var(a), var(b))),
            )),
            template::int_slt(var(a), var(b)),
        ),
        // GE:  Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))) -> BitNot(IntSless(a, b))
        rewrite_rule(
            int_eq(
                zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                zero_extend(int_sborrow(var(a), var(b))),
            ),
            template::bool_not(template::int_slt(var(a), var(b))),
        ),
        // GT -> IntSless(b, a), in either arch shape:
        //    raw NZCV:    BoolAnd(BitNot(Equal(diff, 0)),
        //                    Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))))
        //    decomposed:  BoolAnd(BitNot(Equal(a, b)), BitNot(IntSless(a, b)))
        rewrite_rule(
            one_of![
                bool_and(
                    bool_not(int_eq(add(var(a), neg(var(b))), int_const(0u128))),
                    int_eq(
                        zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                        zero_extend(int_sborrow(var(a), var(b))),
                    ),
                ),
                bool_and(
                    bool_not(int_eq(var(a), var(b))),
                    bool_not(int_slt(var(a), var(b))),
                ),
            ],
            template::int_slt(var(b), var(a)),
        ),
        // LE -> BitNot(IntSless(b, a)), in either arch shape:
        //    raw NZCV:    BoolOr(Equal(diff, 0),
        //                    BitNot(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b)))))
        //    decomposed:  BoolOr(Equal(a, b), IntSless(a, b))
        rewrite_rule(
            one_of![
                bool_or(
                    int_eq(add(var(a), neg(var(b))), int_const(0u128)),
                    bool_not(int_eq(
                        zero_extend(int_slt(add(var(a), neg(var(b))), int_const(0u128))),
                        zero_extend(int_sborrow(var(a), var(b))),
                    )),
                ),
                bool_or(int_eq(var(a), var(b)), int_slt(var(a), var(b))),
            ],
            template::bool_not(template::int_slt(var(b), var(a))),
        ),
        // Thumb "false" flag test (BNE / BCC / BPL / BVC):
        //    IntEqual(ZeroExtend(b), 0) -> BitNot(b)
        // `of_width(1)` is load-bearing: `zext(b) == 0` equals `!b` only for an
        // `I1` `b`.  Unguarded, a chained zero-extend (I1->I8->I32) binds `b` to
        // the wider intermediate and yields a malformed BitNot.
        rewrite_rule(
            int_eq(zero_extend(var(b).of_width(1)), int_const(0u128)),
            template::bool_not(var(b)),
        ),
        // Thumb "true" flag test (BEQ / BCS / BMI / BVS):
        //    BitNot(IntEqual(ZeroExtend(b), 0)) -> b
        // Same `I1` guard as above.
        rewrite_rule(
            bool_not(int_eq(zero_extend(var(b).of_width(1)), int_const(0u128))),
            var(b),
        ),
    ];

    // Constant compare operand (`cmp a, N; ja`/`jbe`), after ConstantFold
    // collapsed `Equal(Add(a, Neg(N)), 0)` to `Equal(Add(a, M), 0)`, `M = -N`:
    //   LS:  Or(Less(a, N), Equal(Add(a, M), 0))                  -> BitNot(Less(N, a))
    //   HI:  And(BitNot(Less(a, N)), BitNot(Equal(Add(a, M), 0)))  -> Less(N, a)
    // The guard pins `M == -N` mod width.
    rules.extend(ls_hi_pair!(
        int_lt(var(a), any_int_const().capture(n)),
        int_eq(add(var(a), any_int_const().capture(m)), int_const(0u128)),
        move |edit, _ty, binds| neg_relation(binds, edit.function(), m, n, a),
        template::int_lt(var(n), var(a)),
    ));
    // Offset-base siblings, for a switch whose cases start at a nonzero base.
    // The Less operand `X = Add(b, C1)` and the Equal base `Add(b, C2)` are
    // DISTINCT nodes, so these key on the shared base `b`:
    //   LS:  Or(Less(X, N), Equal(Add(b, C2), 0))                  -> BitNot(Less(N, X))
    //   HI:  And(BitNot(Less(X, N)), BitNot(Equal(Add(b, C2), 0)))  -> Less(N, X)
    // The guard pins `C2 == C1 - N`.
    rules.extend(ls_hi_pair!(
        int_lt(
            add(var(b), any_int_const().capture(c1)).capture(x),
            any_int_const().capture(n),
        ),
        int_eq(add(var(b), any_int_const().capture(m)), int_const(0u128)),
        move |edit, _ty, binds| sub_relation(binds, edit.function(), m, n, c1, b),
        template::int_lt(var(n), var(x)),
    ));

    // Solve for x: `Equal(Add(x, C1), C2) -> Equal(x, C2 - C1)`.  Sound at any
    // width/signedness, since fixed-width add wraps mod 2^W and `Equal` tests
    // that residue.
    //
    // Must NOT run before the offset-base rules above, whose `Equal(diff, 0)`
    // term it would consume.  Outermost-first seeding gives that: the `Or` root
    // folds before this rule can reach the inner `Equal`.
    rules.push(rewrite_rule(
        int_eq(
            add(var(a), any_int_const().capture(n)),
            any_int_const().capture(m),
        ),
        template::int_eq(
            var(a),
            // The fresh const takes `a`'s width, not the `Equal` root's `I1`.
            capture_typed(a, int_const_with!([n: uint, m: uint] => m.wrapping_sub(n))),
        ),
    ));

    // `Equal(Xor(x, C1), C2) -> Equal(x, C1 ^ C2)`: xor-with-C1 is a bijection,
    // so applying it to both sides is value-preserving.
    rules.push(rewrite_rule(
        int_eq(
            xor(var(a), any_int_const().capture(n)),
            any_int_const().capture(m),
        ),
        template::int_eq(
            var(a),
            capture_typed(a, int_const_with!([n: uint, m: uint] => n ^ m)),
        ),
    ));

    // `Equal(Neg(x), C) -> Equal(x, -C)`: two's-complement negation is a
    // bijection, so it moves across `Equal` value-preservingly.
    rules.push(rewrite_rule(
        int_eq(neg(var(a)), any_int_const().capture(m)),
        template::int_eq(
            var(a),
            capture_typed(a, int_const_with!([m: uint] => m.wrapping_neg())),
        ),
    ));

    // `Sless(ShiftLeft(x, C), 0):I1 -> Xor(Equal(And(x, mask), 0), 1):I1`,
    // mask = 1 << (W-1-C): a signed `< 0` on a left-shifted value tests bit
    // (W-1-C) of `x`.  The `And`/mask/`0` are width `W` via `capture_typed(x,
    // ..)`; the `Xor`/`1` are `I1`.  Guarded to `C < W`: at or above the width
    // `x << C` is 0 and the test is const-false, a different rewrite.
    rules.push(rewrite_rule(
        int_slt(shl(var(x), any_int_const().capture(n)), int_const(0u128)).when_match(
            move |edit, _ty, b| {
                let (Some(c), Some(ty)) = (
                    b.get_uint(n, edit.function()),
                    b.get_type(x, edit.function()),
                ) else {
                    return false;
                };
                c < ty.bit_width() as u128
            },
        ),
        template::bool_not(template::int_eq(
            capture_typed(
                x,
                template::and(
                    var(x),
                    capture_typed(
                        x,
                        int_const_with!([n: uint, in_ty] => {
                            let w = in_ty.ok_or_else(strider_pattern::skip)?.bit_width() as u128;
                            1u128 << (w - 1 - n)
                        }),
                    ),
                ),
            ),
            capture_typed(x, int_const(0u128)),
        )),
    ));

    rules
}

/// Rewrites a PowerPC CR-bit test to the comparison sitting at the tested bit:
/// `Truncate(ShiftRight(Or(ShiftLeft(ZeroExtend(cmp_i:I1), pos_i) ...), k)):I1`
/// -> `cmp_i` where `pos_i == k`.  `None` on any shape it cannot prove a true
/// identity for.
fn canonicalize_cr_bit_test(
    edit: &mut crate::EditFunction<'_>,
    root: NodeId,
) -> Result<Option<ValueId>> {
    let Some((cond_out, cmp)) = cr_bit_comparison(edit, root) else {
        return Ok(None);
    };
    // `replace_value` absorbs only the immediate `Truncate`'s fingerprint, so
    // fold the rest of the pack in first; otherwise the `crset`/`cror`/`cmpwi`
    // addresses vanish when the pack is culled, breaking the superset-only
    // contract.
    absorb_cr_pack_fingerprints(edit, cond_out, cmp);
    edit.replace_value(cond_out, cmp)?;
    Ok(Some(cmp))
}

/// Folds every CR-pack interior node's asm-fingerprint into the surviving
/// comparison.  The descent stops at each `IntCmpOp`; below one are the compared
/// values themselves, not pack-building instructions.
fn absorb_cr_pack_fingerprints(
    edit: &mut crate::EditFunction<'_>,
    cond_out: ValueId,
    cmp: ValueId,
) {
    let into = edit.producer(cmp);
    let mut stack = vec![edit.producer(cond_out)];
    let mut visited: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
    let mut interior: Vec<NodeId> = Vec::new();
    while let Some(n) = stack.pop() {
        if !visited.insert(n) {
            continue;
        }
        interior.push(n);
        if matches!(edit.node_kind(n), NodeKind::IntCmpOp(_)) {
            continue;
        }
        stack.extend(crate::peephole::input_producers_iter(edit, n));
    }
    for n in interior {
        if n != into {
            edit.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint_from(into, n);
        }
    }
}

/// The `(condition-output, comparison)` pair for a CR-bit test.  `Some` only
/// when every OR term is a provable single-bit value at a DISTINCT position and
/// the one at the tested bit carries a comparison; only then does that bit equal
/// the comparison for all inputs.
fn cr_bit_comparison(f: &impl IRViewer, root: NodeId) -> Option<(ValueId, ValueId)> {
    if !matches!(f.node_kind(root), NodeKind::Truncate) {
        return None;
    }
    let cond_out = *f.node_outputs(root).first()?;
    if f.value_type_opt(cond_out) != Some(ValueType::I1) {
        return None;
    }
    let [inner] = f.node_inputs_exact::<1>(root).ok()?;
    // `Truncate(_):I1` exposes bit 0, so a `ShiftRight(x, k)` input means the
    // tested bit is bit k of `x`.
    let (pack, bit) = match *f.kind_of_value(inner) {
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            let [x, amt] = f.producer_inputs_exact::<2>(inner).ok()?;
            let k = u32::try_from(f.int_const_u128(amt)?).ok()?;
            (x, k)
        }
        _ => (inner, 0),
    };

    let mut terms = Vec::new();
    flatten_or(f, pack, &mut terms, 0);
    let mut positions: Vec<u32> = Vec::new();
    let mut found: Option<ValueId> = None;
    for term in terms {
        // A structural zero contributes no bit.
        if f.int_const_u128(term) == Some(0) {
            continue;
        }
        let (pos, cmp) = single_bit_term(f, term, 0)?;
        if positions.contains(&pos) {
            return None; // overlapping terms: `bit` can't be isolated
        }
        positions.push(pos);
        if pos == bit {
            // The tested bit must carry a comparison, not an opaque masked bit.
            found = Some(cmp?);
        }
    }
    found.map(|cmp| (cond_out, cmp))
}

/// Flattens an `Or` tree into its terms.  A well-formed 4-bit CR pack is at
/// most 3 `Or` nodes deep; anything past the cap is pushed as an opaque leaf.
fn flatten_or(f: &impl IRViewer, value: ValueId, out: &mut Vec<ValueId>, depth: u32) {
    const MAX_OR_DEPTH: u32 = 4;
    if depth <= MAX_OR_DEPTH
        && let NodeKind::IntBinaryOp(IntBinaryOp::Or) = f.kind_of_value(value)
        && let Ok([a, b]) = f.producer_inputs_exact::<2>(value)
    {
        flatten_or(f, a, out, depth + 1);
        flatten_or(f, b, out, depth + 1);
        return;
    }
    out.push(value);
}

/// Classifies a CR-pack term as `(bit position, comparison at that bit)`,
/// proving structurally that it sets ONLY that bit; `None` for anything not
/// provably single-bit.  The comparison is `Some` for a bare `IntCmpOp` leaf
/// or a `ZeroExtend` of one.
fn single_bit_term(
    f: &impl IRViewer,
    value: ValueId,
    depth: u32,
) -> Option<(u32, Option<ValueId>)> {
    const MAX_DEPTH: u32 = 32;
    if depth > MAX_DEPTH {
        return None;
    }
    match *f.kind_of_value(value) {
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let [v, amt] = f.producer_inputs_exact::<2>(value).ok()?;
            let pos = u32::try_from(f.int_const_u128(amt)?).ok()?;
            let (q, cmp) = single_bit_term(f, v, depth + 1)?;
            Some((q.checked_add(pos)?, cmp))
        }
        // Zero-fill above the source leaves the set-bit position unchanged.
        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            let [src] = f.producer_inputs_exact::<1>(value).ok()?;
            match f.value_type_opt(src) {
                Some(ValueType::I1) => Some((
                    0,
                    matches!(f.kind_of_value(src), NodeKind::IntCmpOp(_)).then_some(src),
                )),
                _ => single_bit_term(f, src, depth + 1),
            }
        }
        // `And(v, single-bit const)` keeps that bit; its value stays unknown.
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let [a, b] = f.producer_inputs_exact::<2>(value).ok()?;
            let mask = f.int_const_u128(a).or_else(|| f.int_const_u128(b))?;
            (mask.count_ones() == 1).then(|| (mask.trailing_zeros(), None))
        }
        NodeKind::IntCmpOp(_) => Some((0, Some(value))),
        NodeKind::IntConst(_) => {
            let c = f.int_const_u128(value)?;
            (c.count_ones() == 1).then(|| (c.trailing_zeros(), None))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
