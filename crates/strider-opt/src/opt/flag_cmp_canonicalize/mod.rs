//! Collapses the flag-tree conditions of a `cmp`-then-branch into a single
//! [`strider_ir::IntCmpOp`] on the original `(a, b)` pair.
//!
//! AArch64 `cmp a, b` lifts to four flags, and each cond code reads a fixed
//! boolean tree of them:
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
//! Bottom-up, `ZR` and the `GE` tree become `Equal(a, b)` and
//! `Xor(Sless(a, b), 1)`, then each cond code's tree of those collapses.
//! `MI`/`PL` stays as lifted: `Sless(a-b, 0)` differs from `Sless(a, b)` on
//! overflow.
//!
//! Runs before `IfCondInversion`, so the cond carries at most one `Xor(_, 1)`.

use crate::rewrite_rule::{Rule, RuleTable};
use strider_ir::eval;
use strider_ir::node::{
    ExtendOp, IntBinaryOp, IntCmpOp, IntUnaryOp, NodeId, NodeKind, ValueId, ValueType,
};
use strider_ir::{Function, IRViewer, UBig};
use strider_pattern::{
    Bindings, Capture, CaptureExt, bool_and, bool_not, bool_or, capture_typed, int_add, int_const,
    int_const_with, int_eq, int_lt, int_neg, int_sborrow, int_scarry, int_shl, int_slt, int_xor,
    int_zero_extend, template, var,
};

use crate::Result;
use crate::opt::width_guards::{int_type, same_width, shift_count_in_range};
use crate::peephole::PeepholePass;

pub struct FlagCmpCanonicalize;

thread_local! {
    /// Built once per thread; held outside the pass so it stays
    /// `Sync`, as `Optimizer` requires.
    static RULES: RuleTable = RuleTable::new(build_rules());
}

impl PeepholePass for FlagCmpCanonicalize {
    type Sweep = ();
    /// An atom's comparison is what the enclosing combiner reads.
    const PROPAGATE: bool = true;

    fn matches_kind(&self, kind: &NodeKind) -> bool {
        RULES.with(|rules| rules.admits(kind))
    }

    fn try_rewrite(
        &self,
        _sweep: &mut (),
        edit: &mut crate::EditFunction<'_>,
        _opt_ctx: &crate::pipeline::OptCtx<'_>,
        root: NodeId,
    ) -> Result<Option<ValueId>> {
        RULES.with(|rules| rules.first_match(edit, root))
    }
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

/// `-v` at the integer `ty`; `None` for a float.
fn negate(v: &UBig, ty: ValueType) -> Option<UBig> {
    eval::int_unary(IntUnaryOp::Neg, v, ty)
}

/// `M == -N` at `width_src`'s width.
fn neg_relation(b: &Bindings, f: &Function, m: Capture, n: Capture, width_src: Capture) -> bool {
    let Some(([m, n], ty)) = uints(b, f, [m, n], width_src) else {
        return false;
    };
    Some(eval::mask(&m, ty)) == negate(&n, ty)
}

/// `N` is not the signed minimum at `width_src`'s width, so `-N` is
/// representable there.
fn negation_fits(b: &Bindings, f: &Function, n: Capture, width_src: Capture) -> bool {
    let Some(([n], ty)) = uints(b, f, [n], width_src) else {
        return false;
    };
    eval::mask(&n, ty) != UBig::ONE << (ty.bit_width() - 1)
}

/// `M == minuend - subtrahend` at `width_src`'s width.
fn sub_relation(
    b: &Bindings,
    f: &Function,
    m: Capture,
    minuend: Capture,
    subtrahend: Capture,
    width_src: Capture,
) -> bool {
    let Some(([m, minuend, subtrahend], ty)) = uints(b, f, [m, minuend, subtrahend], width_src)
    else {
        return false;
    };
    negate(&subtrahend, ty)
        .is_some_and(|neg| eval::mask(&m, ty) == eval::mask(&(minuend + neg), ty))
}

fn build_rules() -> Vec<Rule> {
    // Captures are shared across rules; each rule binds them afresh.
    let a = Capture::new();
    let b = Capture::new();
    // The compared constant `N` and a second constant `M`.
    let n = Capture::new();
    let m = Capture::new();
    // The compare-base offset `C1` and the whole offset value `X = Add(b, C1)`.
    let c1 = Capture::new();
    let x = Capture::new();

    // The atoms: flag terms to comparisons on `(a, b)`.
    let mut rules: Vec<Rule> = vec![
        // The variable-arity CR pack does not fit the fixed-shape DSL.
        Rule::Fold {
            roots: |k| {
                matches!(
                    k,
                    NodeKind::Truncate | NodeKind::IntBinaryOp(IntBinaryOp::Xor)
                )
            },
            fold: canonicalize_cr_bit_test,
        },
        // `!!x -> x`
        Rule::rewrite(bool_not(bool_not(var(a))), var(a)),
        // Z:  Equal(Add(a, Neg(b)), 0) -> Equal(a, b)
        Rule::rewrite(
            int_eq(int_add(var(a), int_neg(var(b))), int_const(0u128)),
            template::int_eq(var(a), var(b)),
        ),
        // The Thumb "false" flag test (BNE / BCC / BPL / BVC):
        //    IntEqual(ZeroExtend(b), 0) -> Xor(b, 1)
        // `zext(b) == 0` equals `!b` only for an `I1` `b`.
        Rule::rewrite(
            int_eq(int_zero_extend(var(b).of_width(1)), int_const(0u128)),
            template::bool_not(var(b)),
        ),
        // GE:  Equal(ZeroExtend(Sless(Add(a, Neg(b)), 0)), ZeroExtend(Sborrow(a, b)))
        //      -> Xor(Sless(a, b), 1)
        Rule::rewrite(
            int_eq(
                int_zero_extend(int_slt(int_add(var(a), int_neg(var(b))), int_const(0u128))),
                int_zero_extend(int_sborrow(var(a), var(b))),
            ),
            template::bool_not(template::int_slt(var(a), var(b))),
        ),
    ];
    // GE on a constant operand (`cmp a, N`), `M = -N`:
    //   Equal(ZeroExtend(Sless(Add(a, M), 0)), ZeroExtend(Sborrow(a, N))) -> Xor(Sless(a, N), 1)
    rules.push(Rule::rewrite(
        int_eq(
            int_zero_extend(int_slt(int_add(var(a), int_const(m)), int_const(0u128))),
            int_zero_extend(int_sborrow(var(a), int_const(n))),
        )
        .when_match(move |edit, _ty, binds| neg_relation(binds, edit.function(), m, n, a)),
        template::bool_not(template::int_slt(var(a), var(n))),
    ));
    // The `cmn a, N` sibling, a compare against `-N`:
    //   Equal(ZeroExtend(Sless(Add(a, N), 0)), ZeroExtend(Scarry(a, N))) -> Xor(Sless(a, -N), 1)
    // Guarded to `N != INT_MIN`, whose negation `2^(W-1)` no `W`-bit signed
    // value holds.
    rules.push(Rule::rewrite(
        int_eq(
            int_zero_extend(int_slt(int_add(var(a), int_const(n)), int_const(0u128))),
            int_zero_extend(int_scarry(var(a), int_const(n))),
        )
        .when_match(move |edit, _ty, binds| negation_fits(binds, edit.function(), n, a)),
        template::bool_not(template::int_slt(
            var(a),
            capture_typed(
                a,
                int_const_with!([n: uint, ty] => negate(&n, ty).ok_or_else(strider_pattern::skip)?),
            ),
        )),
    ));
    // Offset-base sibling, `cmp X, N` on `X = Add(b, C1)`, guarded to
    // `C2 == C1 - N`:
    //   Equal(ZeroExtend(Sless(Add(b, C2), 0)), ZeroExtend(Sborrow(X, N))) -> Xor(Sless(X, N), 1)
    rules.push(Rule::rewrite(
        int_eq(
            int_zero_extend(int_slt(int_add(var(b), int_const(m)), int_const(0u128))),
            int_zero_extend(int_sborrow(
                int_add(var(b), int_const(c1)).capture(x),
                int_const(n),
            )),
        )
        .when_match(move |edit, _ty, binds| sub_relation(binds, edit.function(), m, c1, n, b)),
        template::bool_not(template::int_slt(var(x), var(n))),
    ));

    // The combiners, unsigned and signed alike, where `less` binds `a < b`:
    //   LS/LE:  Or(less, Equal(a, b))                  -> Xor(less(b, a), 1)
    //   HI/GT:  And(Xor(less, 1), Xor(Equal(a, b), 1)) -> less(b, a)
    // and the offset-base form, `X = Add(b, C1)` against `N` with the Z term
    // solved to `Equal(b, C3)`, guarded to `C3 == N - C1`.
    let offset_guard = move |edit: &strider_pattern::Matcher, _ty, binds: &Bindings| {
        sub_relation(binds, edit.function(), m, n, c1, b)
    };
    macro_rules! combiners {
        ($less:ident) => {
            rules.extend([
                Rule::rewrite(
                    bool_or($less(var(a), var(b)), int_eq(var(a), var(b))),
                    template::bool_not(template::$less(var(b), var(a))),
                ),
                Rule::rewrite(
                    bool_and(
                        bool_not($less(var(a), var(b))),
                        bool_not(int_eq(var(a), var(b))),
                    ),
                    template::$less(var(b), var(a)),
                ),
                Rule::rewrite(
                    bool_or(
                        $less(int_add(var(b), int_const(c1)).capture(x), int_const(n)),
                        int_eq(var(b), int_const(m)),
                    )
                    .when_match(offset_guard),
                    template::bool_not(template::$less(var(n), var(x))),
                ),
                Rule::rewrite(
                    bool_and(
                        bool_not($less(
                            int_add(var(b), int_const(c1)).capture(x),
                            int_const(n),
                        )),
                        bool_not(int_eq(var(b), int_const(m))),
                    )
                    .when_match(offset_guard),
                    template::$less(var(n), var(x)),
                ),
            ]);
        };
    }
    combiners!(int_lt);
    combiners!(int_slt);

    // `Equal(op(x), C) -> Equal(x, op^-1(C))` for a bijective `op` at one
    // width; the fresh const takes `x`'s width.
    macro_rules! solve {
        ($op_x:expr, [$($cap:expr),*], $inverse:expr $(,)?) => {
            rules.push(Rule::rewrite(
                int_eq($op_x, int_const(m)).when_match(move |edit, _ty, binds| {
                    same_width(edit.function(), binds, &[a, $($cap,)* m]).is_some()
                }),
                template::int_eq(var(a), capture_typed(a, $inverse)),
            ));
        };
    }
    // `Equal(Add(x, C1), C2) -> Equal(x, C2 - C1)`
    solve!(
        int_add(var(a), int_const(n)),
        [n],
        int_const_with!([n: uint, m: uint, ty] =>
            m + negate(&n, ty).ok_or_else(strider_pattern::skip)?),
    );
    // `Equal(Xor(x, C1), C2) -> Equal(x, C1 ^ C2)`.
    solve!(
        int_xor(var(a), int_const(n)),
        [n],
        int_const_with!([n: uint, m: uint] => n ^ m),
    );
    // `Equal(Neg(x), C) -> Equal(x, -C)`.
    solve!(
        int_neg(var(a)),
        [],
        int_const_with!([m: uint, ty] => negate(&m, ty).ok_or_else(strider_pattern::skip)?),
    );

    // `Sless(ShiftLeft(x, C), 0):I1 -> Xor(Equal(And(x, 1 << (W-1-C)), 0), 1):I1`,
    // guarded to `C < W`.
    rules.push(Rule::rewrite(
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
                        // `ty` is `x`'s width, the one the guard bounds `n` by.
                        int_const_with!([n: uint, ty] => eval::ones_at(
                            1,
                            ty.bit_width()
                                - 1
                                - usize::try_from(&n)
                                    .expect("the guard bounds the count by the width"),
                        )),
                    ),
                ),
            ),
            capture_typed(x, int_const(0u128)),
        )),
    ));

    rules
}

/// Rewrites a CR-bit test (`Truncate(v):I1` of bit 0, or `Xor(Equal(v, 0),
/// 1):I1` with all but one bit known zero) to the `I1` [`BitTracer`] proves
/// sits at the tested bit: a PowerPC CR pack or the Thumb "true" flag test.
fn canonicalize_cr_bit_test(
    edit: &mut crate::EditFunction<'_>,
    root: NodeId,
) -> Result<Option<ValueId>> {
    let mut tracer = BitTracer {
        f: &*edit,
        budget: MAX_BIT_VISITS,
        proof: Vec::new(),
    };
    let Some((cond_out, cmp)) = tracer.tested_boolean(root) else {
        return Ok(None);
    };
    // `replace_value` absorbs only the root's fingerprint; add the proof's.
    let proof = tracer.proof;
    let into = edit.producer(cmp);
    for n in proof {
        if n != into {
            edit.absorb_node_fingerprint(into, n);
        }
    }
    edit.replace_value(cond_out, cmp);
    Ok(Some(cmp))
}

/// Nodes one CR-bit test may trace through, every bit of a `!= 0` test
/// included.
const MAX_BIT_VISITS: usize = 512;

/// One bit of a value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bit {
    Zero,
    One,
    /// Equal to this `I1`.
    Is(ValueId),
    Unknown,
}

/// Traces single bits down through casts, constant shifts and bitwise ops,
/// recording every node it reads as the proof.
struct BitTracer<'a, V: IRViewer + ?Sized> {
    f: &'a V,
    budget: usize,
    proof: Vec<NodeId>,
}

impl<V: IRViewer + ?Sized> BitTracer<'_, V> {
    /// `(root's output, the I1 it equals)` for a CR-bit test at `root`.
    fn tested_boolean(&mut self, root: NodeId) -> Option<(ValueId, ValueId)> {
        let f = self.f;
        let cond_out = *f.node_outputs(root).first()?;
        if f.value_type_opt(cond_out) != Some(ValueType::I1) {
            return None;
        }
        let found = match f.node_kind(root) {
            NodeKind::Truncate => {
                let [v] = f.node_inputs_exact::<1>(root).ok()?;
                self.bit(v, 0)
            }
            // `v != 0` is the boolean when every other bit is zero.
            NodeKind::IntBinaryOp(IntBinaryOp::Xor) => {
                let eq = self.operand_beside(root, 1)?;
                if !matches!(f.kind_of_value(eq), NodeKind::IntCmpOp(IntCmpOp::Equal)) {
                    return None;
                }
                let v = self.operand_beside(f.producer(eq), 0)?;
                self.proof.push(f.producer(eq));
                let width = f.value_type_opt(v)?.bit_width();
                let mut found = Bit::Zero;
                for k in 0..u32::try_from(width).ok()? {
                    match (found, self.bit(v, k)) {
                        (_, Bit::Zero) => {}
                        (Bit::Zero, Bit::Is(b)) => found = Bit::Is(b),
                        _ => return None,
                    }
                }
                found
            }
            _ => return None,
        };
        match found {
            Bit::Is(b) => Some((cond_out, b)),
            _ => None,
        }
    }

    /// The operand of the two-input `node` whose sibling is the constant
    /// `c`, recording the constant.
    fn operand_beside(&mut self, node: NodeId, c: u128) -> Option<ValueId> {
        let [l, r] = self.f.node_inputs_exact::<2>(node).ok()?;
        let (v, k) = if self.f.int_const_u128(r) == Some(c) {
            (l, r)
        } else if self.f.int_const_u128(l) == Some(c) {
            (r, l)
        } else {
            return None;
        };
        self.proof.push(self.f.producer(k));
        Some(v)
    }

    /// Bit `k` of `v`; [`Bit::Unknown`] once the budget is spent.
    fn bit(&mut self, v: ValueId, k: u32) -> Bit {
        let f = self.f;
        let Some(ty) = f.value_type_opt(v) else {
            return Bit::Unknown;
        };
        if eval::shift_out_of_range(k.into(), ty) {
            return Bit::Zero;
        }
        let Some(budget) = self.budget.checked_sub(1) else {
            return Bit::Unknown;
        };
        self.budget = budget;
        let node = f.producer(v);
        if let Some(c) = f.int_const_u128(v) {
            self.proof.push(node);
            return if c.checked_shr(k).is_some_and(|s| s & 1 == 1) {
                Bit::One
            } else {
                Bit::Zero
            };
        }
        if ty == ValueType::I1 {
            return Bit::Is(v);
        }
        self.proof.push(node);
        let width = ty.bit_width();
        match *f.node_kind(node) {
            NodeKind::Truncate => match f.node_inputs_exact::<1>(node) {
                Ok([src]) => self.bit(src, k),
                Err(_) => Bit::Unknown,
            },
            NodeKind::Extend(op) => {
                let Ok([src]) = f.node_inputs_exact::<1>(node) else {
                    return Bit::Unknown;
                };
                let Some(src_width) = f.value_type_opt(src).map(ValueType::bit_width) else {
                    return Bit::Unknown;
                };
                match (op, u32::try_from(src_width)) {
                    (_, Ok(w)) if k < w => self.bit(src, k),
                    (ExtendOp::ZeroExtend, _) => Bit::Zero,
                    (ExtendOp::SignExtend, Ok(w)) if w > 0 => self.bit(src, w - 1),
                    (ExtendOp::SignExtend, _) => Bit::Unknown,
                }
            }
            NodeKind::IntBinaryOp(op @ (IntBinaryOp::ShiftLeft | IntBinaryOp::ShiftRight)) => {
                let Ok([x, count]) = f.node_inputs_exact::<2>(node) else {
                    return Bit::Unknown;
                };
                let Some(c) = f.int_const_u128(count) else {
                    return Bit::Unknown;
                };
                self.proof.push(f.producer(count));
                if eval::shift_out_of_range(c, ty) {
                    return Bit::Zero;
                }
                let c = c as u32;
                match op {
                    IntBinaryOp::ShiftLeft if k < c => Bit::Zero,
                    IntBinaryOp::ShiftLeft => self.bit(x, k - c),
                    _ if u128::from(k) + u128::from(c) >= width as u128 => Bit::Zero,
                    _ => self.bit(x, k + c),
                }
            }
            NodeKind::IntBinaryOp(op @ (IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor)) => {
                let Ok([mut a, mut b]) = f.node_inputs_exact::<2>(node) else {
                    return Bit::Unknown;
                };
                if f.int_const_u128(b).is_some() {
                    std::mem::swap(&mut a, &mut b);
                }
                // An absorbing bit decides alone; only its trace is proof.
                let absorbing = match op {
                    IntBinaryOp::And => Bit::Zero,
                    IntBinaryOp::Or => Bit::One,
                    _ => Bit::Unknown,
                };
                let mark = self.proof.len();
                let x = self.bit(a, k);
                if x == absorbing && x != Bit::Unknown {
                    return x;
                }
                let mid = self.proof.len();
                let y = self.bit(b, k);
                if y == absorbing && y != Bit::Unknown {
                    self.proof.drain(mark..mid);
                    return y;
                }
                combine(op, x, y)
            }
            _ => Bit::Unknown,
        }
    }
}

/// Bit `op` of two traced bits.
fn combine(op: IntBinaryOp, x: Bit, y: Bit) -> Bit {
    use Bit::{One, Unknown, Zero};
    let (identity, absorbing) = match op {
        IntBinaryOp::And => (One, Zero),
        IntBinaryOp::Or => (Zero, One),
        _ => (Zero, Unknown),
    };
    match (x, y) {
        _ if x == absorbing || y == absorbing => absorbing,
        _ if x == identity => y,
        _ if y == identity => x,
        // `p & p == p | p == p`, `p ^ p == 0`.
        (Bit::Is(p), Bit::Is(q)) if p == q => {
            if op == IntBinaryOp::Xor {
                Zero
            } else {
                x
            }
        }
        (One, One) => Zero,
        _ => Unknown,
    }
}

#[cfg(test)]
mod tests;
