//! `find_all` and `match_at` against a brute-force tree matcher, on random
//! single-region IR and random patterns: commutative swaps, `.ordered()`,
//! captures reused across positions, `one_of`, `first_of`, `ignore_casts`
//! and a root guard. The oracle settles a `first_of` or cast level per
//! (pattern position, offered value), lowest arm or outermost level first,
//! `first_of`s before casts, in pattern-node order.

use std::collections::{BTreeMap, BTreeSet};

use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp, IntUnaryOp};
use strider_ir_test_utils::{RegisterSet, Rng, reg_vn};
use strider_pattern::*;

#[derive(Clone, Debug)]
enum P {
    Any,
    Var(usize),
    Const(u128),
    Bin {
        op: IntBinaryOp,
        l: Box<P>,
        r: Box<P>,
        ordered: bool,
        cap: Option<usize>,
    },
    Neg(Box<P>, Option<usize>),
    OneOf(Vec<P>),
    FirstOf(Vec<P>),
}

const OPS: [IntBinaryOp; 5] = [
    IntBinaryOp::Add,
    IntBinaryOp::Mul,
    IntBinaryOp::Xor,
    IntBinaryOp::ShiftLeft,
    IntBinaryOp::And,
];

fn gen_pat(rng: &mut Rng, depth: u32, ncap: usize) -> P {
    if depth == 0 || rng.chance(25) {
        return match rng.below(4) {
            0 => P::Any,
            1 => P::Const(1 + rng.below(2) as u128),
            _ => P::Var(rng.below(ncap)),
        };
    }
    let cap = rng.chance(20).then(|| rng.below(ncap));
    match rng.below(10) {
        0 => P::Neg(Box::new(gen_pat(rng, depth - 1, ncap)), cap),
        1 => P::OneOf(
            (0..2 + rng.below(2))
                .map(|_| gen_pat(rng, depth - 1, ncap))
                .collect(),
        ),
        2 if rng.chance(50) => P::FirstOf(
            (0..2 + rng.below(2))
                .map(|_| gen_pat(rng, depth - 1, ncap))
                .collect(),
        ),
        _ => P::Bin {
            op: OPS[rng.below(OPS.len())],
            l: Box::new(gen_pat(rng, depth - 1, ncap)),
            r: Box::new(gen_pat(rng, depth - 1, ncap)),
            ordered: rng.chance(15),
            cap,
        },
    }
}

fn to_expr(p: &P, caps: &[Capture]) -> Expr {
    let e = match p {
        P::Any => anything().into_expr(),
        P::Var(c) => var(caps[*c]).into_expr(),
        P::Const(k) => int_const(*k).into_expr(),
        P::Bin {
            op, l, r, ordered, ..
        } => {
            let e = int_binary(*op, to_expr(l, caps), to_expr(r, caps));
            if *ordered {
                e.ordered().into_expr()
            } else {
                e.into_expr()
            }
        }
        P::Neg(i, _) => int_neg(to_expr(i, caps)).into_expr(),
        P::OneOf(arms) => {
            OneOf::new(arms.iter().map(|a| boxed_alt(to_expr(a, caps))).collect()).into_expr()
        }
        P::FirstOf(arms) => {
            OneOf::first(arms.iter().map(|a| boxed_alt(to_expr(a, caps))).collect()).into_expr()
        }
    };
    match p {
        P::Bin { cap: Some(c), .. } | P::Neg(_, Some(c)) => e.capture(caps[*c]).into_expr(),
        _ => e,
    }
}

/// Capture index to the bound value's id.
type Map = BTreeMap<usize, u32>;

fn merge(a: &Map, b: &Map) -> Option<Map> {
    let mut out = a.clone();
    for (k, v) in b {
        if let Some(old) = out.insert(*k, *v)
            && old != *v
        {
            return None;
        }
    }
    Some(out)
}

/// (choice: 0 first_of / 1 cast, path, offered value, arm or depth).
type Trail = Vec<(u8, Vec<u8>, u32, usize)>;

fn cast_input(f: &Function, v: ValueId) -> Option<ValueId> {
    let n = f.producer(v);
    matches!(f.node_kind(n), NodeKind::Truncate | NodeKind::Extend(_)).then(|| f.node_inputs(n)[0])
}

/// An operand: the sub-pattern at `v`, then (under casts) at each cast input below it.
fn operand(f: &Function, p: &P, v: ValueId, path: &mut Vec<u8>, casts: bool) -> Vec<(Map, Trail)> {
    let mut out = Vec::new();
    let has_cast = casts && cast_input(f, v).is_some();
    let mut level = Some(v);
    let mut depth = 0;
    while let Some(l) = level {
        for (m, mut t) in at(f, p, l, path, casts) {
            if has_cast {
                t.push((1, path.clone(), v.as_u32(), depth));
            }
            out.push((m, t));
        }
        if !casts {
            break;
        }
        level = cast_input(f, l);
        depth += 1;
    }
    out
}

fn at(f: &Function, p: &P, v: ValueId, path: &mut Vec<u8>, casts: bool) -> Vec<(Map, Trail)> {
    let n = f.producer(v);
    let capf = |set: Vec<(Map, Trail)>, cap: Option<usize>| -> Vec<(Map, Trail)> {
        let Some(c) = cap else { return set };
        let one: Map = [(c, v.as_u32())].into();
        set.into_iter()
            .filter_map(|(m, t)| merge(&m, &one).map(|m| (m, t)))
            .collect()
    };
    match p {
        P::Any => vec![(Map::new(), Vec::new())],
        P::Var(c) => vec![([(*c, v.as_u32())].into(), Vec::new())],
        P::Const(k) => {
            if matches!(f.node_kind(n), NodeKind::IntConst(_)) && f.int_const_u128(v) == Some(*k) {
                vec![(Map::new(), Vec::new())]
            } else {
                Vec::new()
            }
        }
        P::Neg(i, cap) => {
            if *f.node_kind(n) != NodeKind::IntUnaryOp(IntUnaryOp::Neg) {
                return Vec::new();
            }
            path.push(0);
            let r = operand(f, i, f.node_inputs(n)[0], path, casts);
            path.pop();
            capf(r, *cap)
        }
        P::Bin {
            op,
            l,
            r,
            ordered,
            cap,
        } => {
            let kind = NodeKind::IntBinaryOp(*op);
            if *f.node_kind(n) != kind {
                return Vec::new();
            }
            let ins = f.node_inputs(n);
            let mut out = Vec::new();
            let mut pair = |a: ValueId, b: ValueId, path: &mut Vec<u8>| {
                path.push(0);
                let sa = operand(f, l, a, path, casts);
                path.pop();
                path.push(1);
                let sb = operand(f, r, b, path, casts);
                path.pop();
                for (ma, ta) in &sa {
                    for (mb, tb) in &sb {
                        if let Some(m) = merge(ma, mb) {
                            out.push((m, ta.iter().chain(tb).cloned().collect()));
                        }
                    }
                }
            };
            pair(ins[0], ins[1], path);
            if !*ordered && kind.is_commutative() && ins[0] != ins[1] {
                pair(ins[1], ins[0], path);
            }
            capf(out, *cap)
        }
        P::OneOf(arms) | P::FirstOf(arms) => {
            let first = matches!(p, P::FirstOf(_));
            let mut out = Vec::new();
            for (i, a) in arms.iter().enumerate() {
                let me = path.clone();
                path.push(100 + i as u8);
                for (m, mut t) in at(f, a, v, path, casts) {
                    if first {
                        t.push((0, me.clone(), v.as_u32(), i));
                    }
                    out.push((m, t));
                }
                path.pop();
            }
            out
        }
    }
}

/// Postorder rank of each path: the order the lowering creates pattern nodes.
fn ranks(p: &P, path: &mut Vec<u8>, out: &mut BTreeMap<Vec<u8>, usize>) {
    match p {
        P::Any | P::Var(_) | P::Const(_) => {}
        P::Neg(i, _) => {
            path.push(0);
            ranks(i, path, out);
            path.pop();
        }
        P::Bin { l, r, .. } => {
            path.push(0);
            ranks(l, path, out);
            path.pop();
            path.push(1);
            ranks(r, path, out);
            path.pop();
        }
        P::OneOf(a) | P::FirstOf(a) => {
            for (i, x) in a.iter().enumerate() {
                path.push(100 + i as u8);
                ranks(x, path, out);
                path.pop();
            }
        }
    }
    let n = out.len();
    out.insert(path.clone(), n);
}

fn rejected(guard: bool, map: &Map) -> bool {
    guard && matches!((map.get(&0), map.get(&1)), (Some(x), Some(y)) if x == y)
}

fn settle(f: &Function, p: &P, v: ValueId, guard: bool, casts: bool) -> BTreeSet<Map> {
    let mut all = at(f, p, v, &mut Vec::new(), casts);
    all.retain(|(m, _)| !rejected(guard, m));
    let mut rank = BTreeMap::new();
    ranks(p, &mut Vec::new(), &mut rank);
    // Groups in settle order: first_ofs, then casts, each by pattern-node rank.
    let mut groups: BTreeMap<(u8, usize, u32), Vec<(usize, usize)>> = BTreeMap::new();
    for (i, (_, t)) in all.iter().enumerate() {
        for (c, path, val, arm) in t {
            groups
                .entry((*c, rank[path], *val))
                .or_default()
                .push((i, *arm));
        }
    }
    let mut kept = vec![true; all.len()];
    for members in groups.values() {
        let Some(low) = members.iter().filter(|e| kept[e.0]).map(|e| e.1).min() else {
            continue;
        };
        for &(i, arm) in members {
            kept[i] &= arm == low;
        }
    }
    all.into_iter()
        .zip(kept)
        .filter(|e| e.1)
        .map(|e| (e.0).0)
        .collect()
}

fn gen_fn(rng: &mut Rng, casts: bool) -> Function {
    let t32 = ValueType::int(32);
    let t64 = ValueType::int(64);
    let (va, vb) = (reg_vn(0, 4), reg_vn(8, 4));
    let mut b = RegisterSet::new()
        .tracked(va)
        .tracked(vb)
        .arg(va)
        .arg(vb)
        .build_fn_single_region()
        .unwrap();
    // (value, is64)
    let mut vals: Vec<(ValueId, bool)> = vec![
        (b.read_variable(&va).unwrap(), false),
        (b.read_variable(&vb).unwrap(), false),
        (b.build_int_const(1u64, t32).unwrap(), false),
        (b.build_int_const(2u64, t32).unwrap(), false),
    ];
    if casts {
        vals.push((b.build_int_const(1u64, t64).unwrap(), true));
    }
    for _ in 0..(4 + rng.below(12)) {
        let wide = casts && rng.chance(50);
        let pool: Vec<ValueId> = vals.iter().filter(|e| e.1 == wide).map(|e| e.0).collect();
        let ty = if wide { t64 } else { t32 };
        if pool.is_empty() {
            continue;
        }
        let x = pool[rng.below(pool.len())];
        let y = pool[rng.below(pool.len())];
        let roll = rng.below(100);
        let v = if casts && roll < 25 {
            let src: Vec<ValueId> = vals.iter().filter(|e| e.1 != wide).map(|e| e.0).collect();
            if src.is_empty() {
                continue;
            }
            let s = src[rng.below(src.len())];
            let kind = if wide {
                NodeKind::Extend(strider_ir::node::ExtendOp::ZeroExtend)
            } else {
                NodeKind::Truncate
            };
            b.build_single_output_pure(kind, [s], ty)
        } else if roll < 40 {
            b.build_int_unary_operation(x, IntUnaryOp::Neg, ty).unwrap()
        } else {
            let op = OPS[rng.below(OPS.len())];
            b.build_int_binary_operation(x, y, op, ty).unwrap()
        };
        vals.push((v, wide));
    }
    // Keep everything reachable: or-fold each width, widen the narrow fold.
    let fold = |b: &mut strider_ir::FunctionBuilder, wide: bool| -> Option<ValueId> {
        let ty = if wide { t64 } else { t32 };
        let mut acc: Option<ValueId> = None;
        for &(v, w) in &vals {
            if w != wide {
                continue;
            }
            acc = Some(match acc {
                None => v,
                Some(a) => b
                    .build_int_binary_operation(a, v, IntBinaryOp::Or, ty)
                    .unwrap(),
            });
        }
        acc
    };
    let narrow = fold(&mut b, false).unwrap();
    let ret = match fold(&mut b, true) {
        Some(w) => {
            let t = b.build_single_output_pure(NodeKind::Truncate, [w], t32);
            b.build_int_binary_operation(narrow, t, IntBinaryOp::Or, t32)
                .unwrap()
        }
        None => narrow,
    };
    b.build_return(Some(ret), &[]).unwrap();
    b.build().unwrap()
}

fn has_cap(p: &P) -> bool {
    match p {
        P::Any | P::Const(_) => false,
        P::Var(_) | P::Bin { cap: Some(_), .. } | P::Neg(_, Some(_)) => true,
        P::Bin { l, r, .. } => has_cap(l) || has_cap(r),
        P::Neg(i, None) => has_cap(i),
        P::OneOf(a) | P::FirstOf(a) => a.iter().any(has_cap),
    }
}

/// Every match `find_all` returns is one the oracle finds, once, and no
/// other; `match_at` answers at exactly the roots the oracle matches. A set
/// comparison, so enumeration order is not checked.
#[test]
fn find_all_equals_brute_force_with_first_of_and_casts() {
    let cases = if cfg!(debug_assertions) { 3000 } else { 40_000 };
    let mut rng = Rng(0xBADC_0FFE_E0DD_F00D);
    let mut nonempty = 0usize;
    for case in 0..cases {
        let depth = 3 + (case % 3) as u32;
        let casts = rng.chance(60);
        let f = gen_fn(&mut rng, casts);
        let ncap = 1 + rng.below(3);
        let caps: Vec<Capture> = (0..ncap).map(|_| Capture::new()).collect();
        let mut p = gen_pat(&mut rng, depth, ncap);
        if !matches!(p, P::Bin { .. } | P::Neg(..)) {
            p = P::Bin {
                op: IntBinaryOp::Add,
                l: Box::new(p),
                r: Box::new(P::Var(0)),
                ordered: false,
                cap: None,
            };
        }
        let guard = ncap >= 2 && rng.chance(30);
        let mut pat = to_expr(&p, &caps).into_pattern();
        if casts {
            pat = pat.ignore_casts();
        }
        if guard {
            let (c0, c1) = (caps[0], caps[1]);
            pat = pat.with_root_post_match(Box::new(move |_m, _n, _ty, b: &Bindings| {
                match (b.get_value(c0), b.get_value(c1)) {
                    (Some(x), Some(y)) => x != y,
                    _ => true,
                }
            }));
        }
        let m = Matcher::new(&f);
        let got = m.find_all(&pat).unwrap();
        let mut got_set: BTreeSet<(u32, Map)> = BTreeSet::new();
        for hit in &got {
            let map: Map = caps
                .iter()
                .enumerate()
                .filter_map(|(i, &c)| hit.value(c).map(|v| (i, v.as_u32())))
                .collect();
            assert!(
                got_set.insert((hit.root().as_u32(), map.clone())),
                "case {case}: duplicate {map:?}\npat {p:?}\n{}",
                f.to_text(false)
            );
        }
        let mut want: BTreeSet<(u32, Map)> = BTreeSet::new();
        for n in f.walk() {
            for &out in f.node_outputs(n) {
                for map in settle(&f, &p, out, guard, casts) {
                    want.insert((n.as_u32(), map));
                }
            }
        }
        let msg = || {
            format!(
                "case {case}: casts {casts} guard {guard} pat {p:?}\n{}",
                f.to_text(false)
            )
        };
        if has_cap(&p) {
            assert_eq!(got_set, want, "{}", msg());
        } else {
            let a: BTreeSet<u32> = got_set.iter().map(|e| e.0).collect();
            let b: BTreeSet<u32> = want.iter().map(|e| e.0).collect();
            assert_eq!(a, b, "{}", msg());
        }
        for n in f.walk() {
            let hit = m.match_at(n, &pat).unwrap();
            assert_eq!(
                hit.is_some(),
                want.iter().any(|e| e.0 == n.as_u32()),
                "match_at {}",
                msg()
            );
        }
        nonempty += usize::from(!got.is_empty());
    }
    // About a fifth of the cases match: a floor at half that.
    assert!(
        nonempty * 10 >= cases,
        "only {nonempty} of {cases} cases matched"
    );
}
