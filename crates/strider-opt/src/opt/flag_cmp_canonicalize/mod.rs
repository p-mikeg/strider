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
//! CY   = Xor(IntLess(a, b), 1)    // C, at I1
//! OV   = IntSborrow(a, b)         // V
//!
//! EQ/NE  ZR                          HI/LS  BoolAnd(CY, Xor(ZR, 1)) / dual
//! CS/CC  CY                          GE/LT  Equal(NG, OV)
//! VS/VC  OV                          GT/LE  BoolAnd(Xor(ZR, 1), Equal(NG, OV)) / dual
//! ```
//!
//! `MI`/`PL` (bare `NG`) stays as lifted: `Sless(a-b, 0)` differs
//! from `Sless(a, b)` when the subtraction overflows.  `CS`/`CC` and `VS`/`VC`
//! are already in `(a, b)` form.
//!
//! Run after `ConstantFold` (so `Xor(Xor(x, 1), 1) -> x` at `I1` has collapsed)
//! and before `IfCondInversion` (so the cond carries at most one `Xor(_, 1)`
//! layer).

use crate::{BoxedRule, rewrite_rule};
use strider_ir::eval;
use strider_ir::node::{ExtendOp, IntBinaryOp, IntCmpOp, NodeId, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer, UBig};
use strider_pattern::{
    Bindings, Capture, CaptureExt, bool_and, bool_not, bool_or, capture_typed, int_add, int_const,
    int_const_with, int_eq, int_lt, int_neg, int_sborrow, int_scarry, int_shl, int_slt, int_xor,
    int_zero_extend, one_of, template, var,
};

use crate::Result;
use crate::opt::width_guards::{int_type, same_width, shift_count_in_range};
use crate::peephole::{PeepholePass, PeepholeRewrite, SeedOrder, first_matching_rule};

#[derive(Clone)]
pub struct FlagCmpCanonicalize;

thread_local! {
    /// Built once per thread, and held here rather than in the pass so the
    /// pass stays `Send`; see `ConstantFold`.
    static RULES: Vec<BoxedRule> = build_rules();
}

impl FlagCmpCanonicalize {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for FlagCmpCanonicalize {
    fn default() -> Self {
        Self::new()
    }
}

// Not `PROPAGATE`: a collapsed `IntCmpOp` cannot expose a fresh flag-tree
// shape to its consumers.
impl PeepholePass for FlagCmpCanonicalize {
    type Sweep = ();

    /// Every rule roots at a comparison, at the `And`/`Or`/`Xor` of a boolean
    /// tree, or at the `Truncate` / `Xor` a CR-bit test reaches here as.
    fn matches_kind(&self, kind: &NodeKind) -> bool {
        matches!(
            kind,
            NodeKind::IntCmpOp(_)
                | NodeKind::Truncate
                | NodeKind::IntBinaryOp(IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor)
        )
    }

    /// Outermost-first: a bottom-up seed would rewrite an inner sub-pattern and
    /// destroy the enclosing flag-tree match.
    fn seed_order(&self) -> SeedOrder {
        SeedOrder::Postorder
    }

    fn try_rewrite(
        &self,
        _sweep: &mut (),
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<PeepholeRewrite> {
        let rewrite = rewrite_at(edit, root)?;
        if let PeepholeRewrite::Changed { .. } = rewrite {
            // Outermost-first, a redirect lands on a value a later root
            // redirects again, carrying every consumer the earlier ones moved
            // onto it; merging twins and culling the orphaned cone now keeps a
            // chain of nested flag tests linear.
            edit.clean();
        }
        Ok(rewrite)
    }
}

fn rewrite_at(edit: &mut crate::EditFunction<'_>, root: NodeId) -> Result<PeepholeRewrite> {
    // Imperative arm: the variable-arity CR pack
    // `Or(ShiftLeft(ZeroExtend(cmp_i), pos_i)...)` doesn't fit the
    // fixed-shape `rewrite_rule` DSL.
    if let Some(cmp) = canonicalize_cr_bit_test(edit, root)? {
        return Ok(PeepholeRewrite::from_new_value(edit, Some(cmp)));
    }
    let opt = RULES.with(|rules| first_matching_rule(rules, edit, root))?;
    Ok(PeepholeRewrite::from_new_value(edit, opt))
}

/// Every capture in `caps` as a `UBig`, with the integer type `width_src`
/// carries. `None` unless all of them resolve.
fn uints<const N: usize>(
    b: &Bindings,
    f: &Function,
    caps: [Capture; N],
    width_src: Capture,
) -> Option<([UBig; N], ValueType)> {
    let mut vals: Vec<UBig> = Vec::with_capacity(N);
    for cap in caps {
        vals.push(b.get_uint(cap, f)?);
    }
    Some((vals.try_into().ok()?, int_type(f, b, width_src)?))
}

/// `-v` at `ty`, which is total for an integer `ty`.
fn negate(v: &UBig, ty: ValueType) -> UBig {
    eval::complement(v, ty) + UBig::ONE
}

/// `M == -N` at `width_src`'s width.
fn neg_relation(b: &Bindings, f: &Function, m: Capture, n: Capture, width_src: Capture) -> bool {
    let Some(([m, n], ty)) = uints(b, f, [m, n], width_src) else {
        return false;
    };
    eval::mask(&m, ty) == eval::mask(&negate(&n, ty), ty)
}

/// `N` is not the signed minimum at `width_src`'s width, so `-N` is
/// representable there.
fn negation_fits(b: &Bindings, f: &Function, n: Capture, width_src: Capture) -> bool {
    let Some(([n], ty)) = uints(b, f, [n], width_src) else {
        return false;
    };
    eval::mask(&n, ty) != UBig::ONE << (ty.bit_width() - 1)
}

/// `M == C1 - N` at `width_src`'s width.
fn sub_relation(
    b: &Bindings,
    f: &Function,
    m: Capture,
    n: Capture,
    c1: Capture,
    width_src: Capture,
) -> bool {
    let Some(([m, n, c1], ty)) = uints(b, f, [m, n, c1], width_src) else {
        return false;
    };
    eval::mask(&m, ty) == eval::mask(&(c1 + negate(&n, ty)), ty)
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
    //   LS:  Or(less, eq)                  -> Xor(cmp, 1)
    //   HI:  And(Xor(less, 1), Xor(eq, 1)) -> cmp
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

    // Emits a signed GE rule and its LT negation over the flag-tree term:
    //   GE:  tree          -> Xor(cmp, 1)
    //   LT:  Xor(tree, 1)  -> cmp
    macro_rules! ge_lt_pair {
        ($tree:expr, $cmp:expr $(,)?) => {
            [
                rewrite_rule($tree, template::bool_not($cmp)),
                rewrite_rule(bool_not($tree), $cmp),
            ]
        };
    }

    let mut rules: Vec<BoxedRule> = vec![
        // EQ:  Equal(Add(a, Neg(b)), 0) -> Equal(a, b)
        rewrite_rule(
            int_eq(int_add(var(a), int_neg(var(b))), int_const(0u128)),
            template::int_eq(var(a), var(b)),
        ),
        // HI -> IntLess(b, a), in either arch shape:
        //    raw NZCV:    BoolAnd(Xor(IntLess(a, b), 1), Xor(Equal(diff, 0), 1))
        //    decomposed:  BoolAnd(Xor(Equal(a, b), 1), Xor(IntLess(a, b), 1))
        rewrite_rule(
            one_of![
                bool_and(
                    bool_not(int_lt(var(a), var(b))),
                    bool_not(int_eq(int_add(var(a), int_neg(var(b))), int_const(0u128))),
                ),
                bool_and(
                    bool_not(int_eq(var(a), var(b))),
                    bool_not(int_lt(var(a), var(b))),
                ),
            ],
            template::int_lt(var(b), var(a)),
        ),
        // LS -> Xor(IntLess(b, a), 1), in either arch shape:
        //    raw NZCV:    BoolOr(IntLess(a, b), Equal(diff, 0))
        //    decomposed:  BoolOr(Equal(a, b), IntLess(a, b))
        // The raw form requires ConstantFold to have already cancelled the
        // `Xor(Xor(IntLess, 1), 1)` chain that `Xor(CY, 1)` produces.
        rewrite_rule(
            one_of![
                bool_or(
                    int_lt(var(a), var(b)),
                    int_eq(int_add(var(a), int_neg(var(b))), int_const(0u128)),
                ),
                bool_or(int_eq(var(a), var(b)), int_lt(var(a), var(b))),
            ],
            template::bool_not(template::int_lt(var(b), var(a))),
        ),
        // LT:  Xor(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))), 1) -> IntSless(a, b)
        rewrite_rule(
            bool_not(int_eq(
                int_zero_extend(int_slt(int_add(var(a), int_neg(var(b))), int_const(0u128))),
                int_zero_extend(int_sborrow(var(a), var(b))),
            )),
            template::int_slt(var(a), var(b)),
        ),
        // GE:  Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))) -> Xor(IntSless(a, b), 1)
        rewrite_rule(
            int_eq(
                int_zero_extend(int_slt(int_add(var(a), int_neg(var(b))), int_const(0u128))),
                int_zero_extend(int_sborrow(var(a), var(b))),
            ),
            template::bool_not(template::int_slt(var(a), var(b))),
        ),
        // GT -> IntSless(b, a), in either arch shape:
        //    raw NZCV:    BoolAnd(Xor(Equal(diff, 0), 1),
        //                    Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))))
        //    decomposed:  BoolAnd(Xor(Equal(a, b), 1), Xor(IntSless(a, b), 1))
        rewrite_rule(
            one_of![
                bool_and(
                    bool_not(int_eq(int_add(var(a), int_neg(var(b))), int_const(0u128))),
                    int_eq(
                        int_zero_extend(int_slt(
                            int_add(var(a), int_neg(var(b))),
                            int_const(0u128)
                        )),
                        int_zero_extend(int_sborrow(var(a), var(b))),
                    ),
                ),
                bool_and(
                    bool_not(int_eq(var(a), var(b))),
                    bool_not(int_slt(var(a), var(b))),
                ),
            ],
            template::int_slt(var(b), var(a)),
        ),
        // LE -> Xor(IntSless(b, a), 1), in either arch shape:
        //    raw NZCV:    BoolOr(Equal(diff, 0),
        //                    Xor(Equal(ZeroExtend(IntSless(diff, 0)), ZeroExtend(IntSborrow(a, b))), 1))
        //    decomposed:  BoolOr(Equal(a, b), IntSless(a, b))
        rewrite_rule(
            one_of![
                bool_or(
                    int_eq(int_add(var(a), int_neg(var(b))), int_const(0u128)),
                    bool_not(int_eq(
                        int_zero_extend(int_slt(
                            int_add(var(a), int_neg(var(b))),
                            int_const(0u128)
                        )),
                        int_zero_extend(int_sborrow(var(a), var(b))),
                    )),
                ),
                bool_or(int_eq(var(a), var(b)), int_slt(var(a), var(b))),
            ],
            template::bool_not(template::int_slt(var(b), var(a))),
        ),
        // Thumb "false" flag test (BNE / BCC / BPL / BVC):
        //    IntEqual(ZeroExtend(b), 0) -> Xor(b, 1)
        // `of_width(1)` is load-bearing: `zext(b) == 0` equals `!b` only for an
        // `I1` `b`.  Unguarded, a chained zero-extend (I1->I8->I32) binds `b` to
        // the wider intermediate and yields a malformed complement.
        rewrite_rule(
            int_eq(int_zero_extend(var(b).of_width(1)), int_const(0u128)),
            template::bool_not(var(b)),
        ),
        // The Thumb "true" flag test, `Xor(IntEqual(ZeroExtend(b:I1), 0), 1)
        // -> b`, is the CR-bit arm's one-term pack.
    ];

    // Constant compare operand (`cmp a, N; ja`/`jbe`), after ConstantFold
    // collapsed `Equal(Add(a, Neg(N)), 0)` to `Equal(Add(a, M), 0)`, `M = -N`:
    //   LS:  Or(Less(a, N), Equal(Add(a, M), 0))                 -> Xor(Less(N, a), 1)
    //   HI:  And(Xor(Less(a, N), 1), Xor(Equal(Add(a, M), 0), 1)) -> Less(N, a)
    // The guard pins `M == -N` mod width.
    rules.extend(ls_hi_pair!(
        int_lt(var(a), int_const(n)),
        int_eq(int_add(var(a), int_const(m)), int_const(0u128)),
        move |edit, _ty, binds| neg_relation(binds, edit.function(), m, n, a),
        template::int_lt(var(n), var(a)),
    ));
    // Signed constant compare operand (`cmp a, N; jl`/`jge`), after
    // ConstantFold collapsed `Add(a, Neg(N))` to `Add(a, M)`, `M = -N`:
    //   GE:  Equal(ZeroExtend(Sless(Add(a, M), 0)), ZeroExtend(Sborrow(a, N)))         -> Xor(Sless(a, N), 1)
    //   LT:  Xor(Equal(ZeroExtend(Sless(Add(a, M), 0)), ZeroExtend(Sborrow(a, N))), 1) -> Sless(a, N)
    // The guard pins `M == -N` mod width. GT/LE follow through the decomposed
    // rules once the EQ term has solved for `a`. `N = 0` is not this shape:
    // `Add(a, 0)` folds to `a` and `Sborrow(a, 0)` to false.
    let sborrow_nv = || {
        int_eq(
            int_zero_extend(int_slt(int_add(var(a), int_const(m)), int_const(0u128))),
            int_zero_extend(int_sborrow(var(a), int_const(n))),
        )
        .when_match(move |edit, _ty, binds| neg_relation(binds, edit.function(), m, n, a))
    };
    rules.extend(ge_lt_pair!(sborrow_nv(), template::int_slt(var(a), var(n))));
    // The `cmn a, N` sibling, a compare against `-N`:
    //   GE:  Equal(ZeroExtend(Sless(Add(a, N), 0)), ZeroExtend(Scarry(a, N)))         -> Xor(Sless(a, -N), 1)
    //   LT:  Xor(Equal(ZeroExtend(Sless(Add(a, N), 0)), ZeroExtend(Scarry(a, N))), 1) -> Sless(a, -N)
    // Guarded to `N != INT_MIN`, whose negation `2^(W-1)` no `Sless` names.
    let scarry_nv = || {
        int_eq(
            int_zero_extend(int_slt(int_add(var(a), int_const(n)), int_const(0u128))),
            int_zero_extend(int_scarry(var(a), int_const(n))),
        )
        .when_match(move |edit, _ty, binds| negation_fits(binds, edit.function(), n, a))
    };
    let neg_n = || capture_typed(a, int_const_with!([n: uint, ty] => negate(&n, ty)));
    rules.extend(ge_lt_pair!(scarry_nv(), template::int_slt(var(a), neg_n())));
    // Offset-base sibling, `cmp X, N` on `X = Add(b, C1)` (a loop's `i + 1`),
    // after ConstantFold reassociated the flag operand `Add(X, -N)` into
    // `Add(b, C2)`:
    //   GE:  Equal(ZeroExtend(Sless(Add(b, C2), 0)), ZeroExtend(Sborrow(X, N))) -> Xor(Sless(X, N), 1)
    //   LT:  Xor(<the GE tree>, 1)                                              -> Sless(X, N)
    // and, once the EQ term has solved for `b`:
    //   LE:  Or(Equal(b, C3), Sless(X, N))                  -> Xor(Sless(N, X), 1)
    //   GT:  And(Xor(Equal(b, C3), 1), Xor(Sless(X, N), 1)) -> Sless(N, X)
    // The guards pin `C2 == C1 - N` and `C3 == N - C1`. Not covered: `C1 == N`,
    // whose `Add(b, 0)` folds to `b`, and the `cmn` offset form.
    let offset_nv = || {
        int_eq(
            int_zero_extend(int_slt(int_add(var(b), int_const(m)), int_const(0u128))),
            int_zero_extend(int_sborrow(
                int_add(var(b), int_const(c1)).capture(x),
                int_const(n),
            )),
        )
        .when_match(move |edit, _ty, binds| sub_relation(binds, edit.function(), m, n, c1, b))
    };
    rules.extend(ge_lt_pair!(offset_nv(), template::int_slt(var(x), var(n))));
    let offset_lt = || int_slt(int_add(var(b), int_const(c1)).capture(x), int_const(n));
    let offset_eq_guard = move |edit: &strider_pattern::Matcher, _ty, binds: &Bindings| {
        sub_relation(binds, edit.function(), m, c1, n, b)
    };
    rules.push(rewrite_rule(
        bool_or(int_eq(var(b), int_const(m)), offset_lt()).when_match(offset_eq_guard),
        template::bool_not(template::int_slt(var(n), var(x))),
    ));
    rules.push(rewrite_rule(
        bool_and(
            bool_not(int_eq(var(b), int_const(m))),
            bool_not(offset_lt()),
        )
        .when_match(offset_eq_guard),
        template::int_slt(var(n), var(x)),
    ));
    // Offset-base siblings, for a switch whose cases start at a nonzero base.
    // The Less operand `X = Add(b, C1)` and the Equal base `Add(b, C2)` are
    // DISTINCT nodes, so these key on the shared base `b`:
    //   LS:  Or(Less(X, N), Equal(Add(b, C2), 0))                 -> Xor(Less(N, X), 1)
    //   HI:  And(Xor(Less(X, N), 1), Xor(Equal(Add(b, C2), 0), 1)) -> Less(N, X)
    // The guard pins `C2 == C1 - N`.
    rules.extend(ls_hi_pair!(
        int_lt(int_add(var(b), int_const(c1)).capture(x), int_const(n),),
        int_eq(int_add(var(b), int_const(m)), int_const(0u128)),
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
        int_eq(int_add(var(a), int_const(n)), int_const(m)).when_match(move |edit, _ty, binds| {
            int_type(edit.function(), binds, a).is_some()
                && same_width(edit.function(), binds, &[a, n, m]).is_some()
        }),
        template::int_eq(
            var(a),
            // The fresh const takes `a`'s width, not the `Equal` root's `I1`.
            capture_typed(
                a,
                int_const_with!([n: uint, m: uint, ty] => m + negate(&n, ty)),
            ),
        ),
    ));

    // `Equal(Xor(x, C1), C2) -> Equal(x, C1 ^ C2)`: xor-with-C1 is a bijection,
    // so applying it to both sides is value-preserving.
    rules.push(rewrite_rule(
        int_eq(int_xor(var(a), int_const(n)), int_const(m)).when_match(move |edit, _ty, binds| {
            same_width(edit.function(), binds, &[a, n, m]).is_some()
        }),
        template::int_eq(
            var(a),
            capture_typed(a, int_const_with!([n: uint, m: uint] => n ^ m)),
        ),
    ));

    // `Equal(Neg(x), C) -> Equal(x, -C)`: two's-complement negation is a
    // bijection, so it moves across `Equal` value-preservingly.
    rules.push(rewrite_rule(
        int_eq(int_neg(var(a)), int_const(m)).when_match(move |edit, _ty, binds| {
            int_type(edit.function(), binds, a).is_some()
                && same_width(edit.function(), binds, &[a, m]).is_some()
        }),
        template::int_eq(
            var(a),
            capture_typed(a, int_const_with!([m: uint, ty] => negate(&m, ty))),
        ),
    ));

    // `Sless(ShiftLeft(x, C), 0):I1 -> Xor(Equal(And(x, mask), 0), 1):I1`,
    // mask = 1 << (W-1-C): a signed `< 0` on a left-shifted value tests bit
    // (W-1-C) of `x`.  The `And`/mask/`0` are width `W` via `capture_typed(x,
    // ..)`; the `Xor`/`1` are `I1`.  Guarded to `C < W`: at or above the width
    // `x << C` is 0 and the test is const-false, a different rewrite.
    rules.push(rewrite_rule(
        int_slt(int_shl(var(x), int_const(n)), int_const(0u128)).when_match(move |edit, _ty, b| {
            let f = edit.function();
            b.get_type(x, f)
                .is_some_and(|ty| shift_count_in_range(f, b, ty, n))
        }),
        template::bool_not(template::int_eq(
            capture_typed(
                x,
                template::int_and(
                    var(x),
                    capture_typed(
                        x,
                        // `ty` is this const's own width, `x`'s, which is the
                        // width the guard bounds `n` against; the `Sless`
                        // slot-0 type is a second width that only a validated
                        // graph agrees with.
                        int_const_with!([n: uint, ty] => eval::ones_at(
                            1,
                            ty.bit_width()
                                - 1
                                - usize::try_from(&n).map_err(|_| strider_pattern::skip())?,
                        )),
                    ),
                ),
            ),
            capture_typed(x, int_const(0u128)),
        )),
    ));

    rules
}

/// Rewrites a PowerPC CR-bit test to the boolean sitting at the tested bit:
/// `Truncate(ShiftRight(Or(ShiftLeft(ZeroExtend(cmp_i:I1), pos_i) ...), k)):I1`
/// -> `cmp_i` where `pos_i == k`.  A one-term pack is the Thumb "true" flag
/// test, `Xor(IntEqual(ZeroExtend(b:I1), 0), 1)` -> `b`.  `None` on any shape
/// it cannot prove a true identity for.
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
    edit.replace_value(cond_out, cmp);
    Ok(Some(cmp))
}

/// The kinds [`cr_bit_comparison`] traverses between the tested bit and the
/// comparisons at the pack's terms.  Everything else is a leaf: an `IntCmpOp`
/// because below one are the compared values, and any other producer because
/// `input_producers_iter` follows memory and control edges, which from the
/// opaque operand of an `And(v, 2^p)` term reach `InitialMemory` / `Entry`.
fn builds_a_cr_pack(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::IntConst(_)
            | NodeKind::Truncate
            | NodeKind::Extend(_)
            | NodeKind::IntBinaryOp(
                IntBinaryOp::Or
                    | IntBinaryOp::And
                    | IntBinaryOp::Xor
                    | IntBinaryOp::ShiftLeft
                    | IntBinaryOp::ShiftRight
            )
    )
}

/// Folds every CR-pack interior node's asm-fingerprint into the surviving
/// boolean.  The descent is confined to [`builds_a_cr_pack`], plus the
/// `IntEqual(_, 0)` of the `Xor` spelling, and stops at the boolean.
fn absorb_cr_pack_fingerprints(
    edit: &mut crate::EditFunction<'_>,
    cond_out: ValueId,
    cmp: ValueId,
) {
    let into = edit.producer(cmp);
    let root = edit.producer(cond_out);
    let tested_eq = match edit.node_kind(root) {
        NodeKind::IntBinaryOp(IntBinaryOp::Xor) => {
            crate::peephole::input_producers_iter(edit, root)
                .find(|&n| matches!(edit.node_kind(n), NodeKind::IntCmpOp(IntCmpOp::Equal)))
        }
        _ => None,
    };
    let mut stack = vec![root];
    let mut visited: entity_utils::DenseEntitySet<NodeId> = entity_utils::DenseEntitySet::new();
    let mut interior: Vec<NodeId> = Vec::new();
    while let Some(n) = stack.pop() {
        if n == into || !visited.insert(n) {
            continue;
        }
        interior.push(n);
        if builds_a_cr_pack(edit.node_kind(n)) || Some(n) == tested_eq {
            stack.extend(crate::peephole::input_producers_iter(edit, n));
        }
    }
    for n in interior {
        edit.absorb_node_fingerprint(into, n);
    }
}

/// The value `root` tests, paired with whether it observes EVERY bit of that
/// value rather than bit 0 alone.
///
/// Two spellings reach here: the `Truncate(_):I1` narrowing, which is bit 0,
/// and the `Xor(IntEqual(_, 0), 1):I1` that the `cond != 0` branch lowering
/// emits, which is the OR of every bit.  A peeled `And(y, 1)` layer selects
/// bit 0 under both, so it settles the flag either way.
fn bit_zero_source(f: &impl IRViewer, root: NodeId) -> Option<(ValueId, bool)> {
    let (src, whole_value) = match f.node_kind(root) {
        NodeKind::Truncate => {
            let [inner] = f.node_inputs_exact::<1>(root).ok()?;
            (inner, false)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::Xor) => {
            let [l, r] = f.node_inputs_exact::<2>(root).ok()?;
            let eq = const_operand_is(f, l, r, 1)?;
            if !matches!(f.kind_of_value(eq), NodeKind::IntCmpOp(IntCmpOp::Equal)) {
                return None;
            }
            let [a, b] = f.producer_inputs_exact::<2>(eq).ok()?;
            (const_operand_is(f, a, b, 0)?, true)
        }
        _ => return None,
    };
    if let NodeKind::IntBinaryOp(IntBinaryOp::And) = f.kind_of_value(src)
        && let Ok([a, b]) = f.producer_inputs_exact::<2>(src)
        && let Some(y) = const_operand_is(f, a, b, 1)
    {
        return Some((y, false));
    }
    Some((src, whole_value))
}

/// The one of `l` / `r` whose sibling is the constant `c`.
fn const_operand_is(f: &impl IRViewer, l: ValueId, r: ValueId, c: u128) -> Option<ValueId> {
    if f.int_const_u128(r) == Some(c) {
        Some(l)
    } else if f.int_const_u128(l) == Some(c) {
        Some(r)
    } else {
        None
    }
}

/// The `(condition-output, boolean)` pair for a CR-bit test.  `Some` only
/// when every OR term is a provable single-bit value at a DISTINCT position and
/// the one at the tested bit carries an `I1`; only then does that bit equal
/// the `I1` for all inputs.
fn cr_bit_comparison(f: &impl IRViewer, root: NodeId) -> Option<(ValueId, ValueId)> {
    let cond_out = *f.node_outputs(root).first()?;
    if f.value_type_opt(cond_out) != Some(ValueType::I1) {
        return None;
    }
    let (inner, whole_value) = bit_zero_source(f, root)?;
    // Bit 0 of `inner` is under test, so a `ShiftRight(x, k)` there means the
    // tested bit is bit k of `x`.
    let (pack, bit) = match *f.kind_of_value(inner) {
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            let [x, amt] = f.producer_inputs_exact::<2>(inner).ok()?;
            let k = u32::try_from(f.int_const_u128(amt)?).ok()?;
            // At or past the width the shift is 0, so no bit is under test.
            let width = u32::try_from(f.value_type_opt(inner)?.bit_width()).ok()?;
            if k >= width {
                return None;
            }
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
            // The tested bit must carry an `I1`, not an opaque masked bit.
            found = Some(cmp?);
        }
    }
    // `src != 0` is true for a bit ABOVE the tested one too, where the
    // comparison is false; a shift has already dropped everything below it.
    if whole_value && positions.iter().any(|&p| p > bit) {
        return None;
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

/// Classifies a CR-pack term as `(bit position, boolean at that bit)`,
/// proving structurally that it sets ONLY that bit; `None` for anything not
/// provably single-bit.  The boolean is `Some` for a bare `IntCmpOp` leaf or
/// a `ZeroExtend` of any `I1`.
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
            // p-code yields 0 once the shift reaches the output width, so a
            // term shifted that far carries no bit to name.
            let width = u32::try_from(f.value_type_opt(value)?.bit_width()).ok()?;
            let (q, cmp) = single_bit_term(f, v, depth + 1)?;
            let bit = q.checked_add(pos)?;
            (bit < width).then_some((bit, cmp))
        }
        // Zero-fill above the source leaves the set-bit position unchanged.
        NodeKind::Extend(ExtendOp::ZeroExtend) => {
            let [src] = f.producer_inputs_exact::<1>(value).ok()?;
            match f.value_type_opt(src) {
                Some(ValueType::I1) => Some((0, Some(src))),
                Some(ValueType::Int(_) | ValueType::Float(_)) | None => {
                    single_bit_term(f, src, depth + 1)
                }
            }
        }
        // `And(v, single-bit const)` keeps that bit; its value stays unknown.
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let [a, b] = f.producer_inputs_exact::<2>(value).ok()?;
            let mask = f.int_const_u128(a).or_else(|| f.int_const_u128(b))?;
            mask.is_power_of_two()
                .then(|| (mask.trailing_zeros(), None))
        }
        NodeKind::IntCmpOp(_) => Some((0, Some(value))),
        NodeKind::IntConst(_) => {
            let c = f.int_const_u128(value)?;
            c.is_power_of_two().then(|| (c.trailing_zeros(), None))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
