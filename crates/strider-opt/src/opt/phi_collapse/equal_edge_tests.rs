use super::PhiCollapse;
use crate::test_support::{Rng, run_pass};
use strider_ir::node::{NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, UBig, eval};
use strider_ir_test_utils::interp::{self, EntryVar, Outcome};
use strider_ir_test_utils::{RegisterSet, reg_vn};

const I64: ValueType = ValueType::int(64);

fn add(fb: &mut FunctionBuilder, v: ValueId, k: u64) -> crate::Result<ValueId> {
    let k = fb.build_int_const(k, I64)?;
    fb.build_int_binary_operation(v, k, IntBinaryOp::Add, I64)
}

/// Whether the cone of `value` reads a phi.
fn reads_phi(f: &strider_ir::Function, value: ValueId) -> bool {
    let mut stack = vec![value];
    let mut seen = rustc_hash::FxHashSet::default();
    while let Some(v) = stack.pop() {
        let node = f.producer(v);
        if !seen.insert(node) {
            continue;
        }
        match f.node_kind(node) {
            NodeKind::Phi => return true,
            NodeKind::MemPhi => {}
            _ => stack.extend(f.node_inputs(node)),
        }
    }
    false
}

fn returned(f: &strider_ir::Function) -> Vec<ValueId> {
    let ret = f
        .graph()
        .all_node_ids()
        .find(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .expect("one Return");
    f.node_inputs(ret).into_iter().skip(2).collect()
}

fn outcome(f: &strider_ir::Function, inputs: &[u64]) -> Outcome {
    let entry = |e: EntryVar| eval::mask(&UBig::from(inputs[e.index]), e.ty);
    let env = interp::Env {
        entry: &entry,
        rom: None,
        seed: 0,
        step_budget: 4_000,
        private: None,
        keeps: None,
    };
    interp::run(f, &env).expect("the interpreter runs every kind built here")
}

/// The probe-loop shape: `bound = sp - span`, then `sp -= 0x1000` until
/// `sp == bound` (`negate`: until `!(sp != bound)`, an `If` on the negation
/// with swapped arms). The exit returns the loop's last `sp` before the step,
/// and `sp + span`.
fn probe_loop(negate: bool, op: IntCmpOp) -> crate::Result<strider_ir::Function> {
    let (sp, bound, last) = (reg_vn(0x20, 8), reg_vn(0x58, 8), reg_vn(0x60, 8));
    let mut fb = RegisterSet::new()
        .tracked(sp)
        .arg(sp)
        .tracked(bound)
        .tracked(last)
        .build_fn()?;
    let [entry, header, exit] = [(); 3].map(|()| fb.create_region_all().unwrap());
    fb.set_entry_region_all(entry)?;
    fb.set_region(entry);
    let sp0 = fb.read_variable(&sp)?;
    let limit = add(&mut fb, sp0, 0x9000u64.wrapping_neg())?;
    fb.write_variable(&bound, limit)?;
    fb.build_branch(header)?;

    fb.set_region(header);
    let cur = fb.read_variable(&sp)?;
    fb.write_variable(&last, cur)?;
    let next = add(&mut fb, cur, 0x1000u64.wrapping_neg())?;
    fb.write_variable(&sp, next)?;
    let limit = fb.read_variable(&bound)?;
    let mut cond = fb.build_int_cmp_operation(next, limit, op, I64)?;
    if negate {
        let one = fb.build_boolean_const(true);
        cond = fb.build_int_binary_operation(cond, one, IntBinaryOp::Xor, ValueType::I1)?;
        fb.build_if(cond, header, exit)?;
    } else {
        fb.build_if(cond, exit, header)?;
    }

    fb.set_region(exit);
    let before = fb.read_variable(&last)?;
    let top = fb.read_variable(&sp)?;
    let top = add(&mut fb, top, 0x9000)?;
    fb.build_return(None, &[before, top])?;
    fb.set_lift_addr(None);
    fb.build()
}

/// Below the exit edge the loop's SP is the bound, so the exit reads only
/// values fixed before the loop; the loop keeps its own reads.
#[test]
fn a_probe_loop_exit_reads_the_bound() -> crate::Result<()> {
    for negate in [false, true] {
        let mut f = probe_loop(negate, IntCmpOp::Equal)?;
        let want: Vec<Outcome> = [0x10_0000u64, 0x7fff_f000]
            .iter()
            .map(|&sp| outcome(&f, &[sp]))
            .collect();
        run_pass(&PhiCollapse, &mut f)?;
        for v in returned(&f) {
            assert!(!reads_phi(&f, v), "negate={negate}:\n{}", f.to_text(false));
        }
        let branch = crate::test_support::find_unique_if(f.graph());
        assert!(
            reads_phi(&f, f.if_cond(branch)),
            "the loop keeps its own reads"
        );
        for (sp, want) in [0x10_0000u64, 0x7fff_f000].iter().zip(&want) {
            assert_eq!(&outcome(&f, &[*sp]), want, "negate={negate}");
        }
    }
    Ok(())
}

/// Only `Equal` gives an equality: under `Less` the exit keeps the phi.
#[test]
fn a_loop_exiting_on_less_keeps_its_phi_reads() -> crate::Result<()> {
    let mut f = probe_loop(false, IntCmpOp::Less)?;
    run_pass(&PhiCollapse, &mut f)?;
    assert!(returned(&f).into_iter().all(|v| reads_phi(&f, v)));
    Ok(())
}

/// `x` is a phi of `1` and `2`, then `if (x == 2) { t } else { e }`, each
/// arm built by `arms` from `x` and returned; with `join`, written to `y`
/// and `y` and `x` returned past the join.
fn diamond_then_test(
    join: bool,
    arms: impl Fn(&mut FunctionBuilder, ValueId, bool) -> crate::Result<ValueId>,
) -> crate::Result<strider_ir::Function> {
    let (a, x, y) = (reg_vn(0x8, 8), reg_vn(0x10, 8), reg_vn(0x18, 8));
    let mut fb = RegisterSet::new()
        .tracked(a)
        .arg(a)
        .tracked(x)
        .tracked(y)
        .build_fn()?;
    let [entry, left, right, mid, then, other, end] =
        [(); 7].map(|()| fb.create_region_all().unwrap());
    fb.set_entry_region_all(entry)?;
    fb.set_region(entry);
    let arg = fb.read_variable(&a)?;
    let zero = fb.build_int_const(0u64, I64)?;
    let c = fb.build_int_cmp_operation(arg, zero, IntCmpOp::Equal, I64)?;
    fb.build_if(c, left, right)?;
    for (region, value) in [(left, 1u64), (right, 2)] {
        fb.set_region(region);
        let v = fb.build_int_const(value, I64)?;
        fb.write_variable(&x, v)?;
        fb.build_branch(mid)?;
    }
    fb.set_region(mid);
    let phi = fb.read_variable(&x)?;
    let two = fb.build_int_const(2u64, I64)?;
    let c = fb.build_int_cmp_operation(phi, two, IntCmpOp::Equal, I64)?;
    fb.build_if(c, then, other)?;
    let mut outs = Vec::new();
    for (region, is_then) in [(then, true), (other, false)] {
        fb.set_region(region);
        let out = arms(&mut fb, phi, is_then)?;
        if join {
            fb.write_variable(&y, out)?;
            fb.build_branch(end)?;
        } else {
            outs.push(out);
            fb.build_return(None, &[out])?;
        }
    }
    if join {
        fb.set_region(end);
        let out = fb.read_variable(&y)?;
        let phi = fb.read_variable(&x)?;
        fb.build_return(None, &[out, phi])?;
    }
    fb.set_lift_addr(None);
    fb.build()
}

/// The reads of each Return, in node order.
fn returns(f: &strider_ir::Function) -> Vec<Vec<ValueId>> {
    f.graph()
        .all_node_ids()
        .filter(|&n| matches!(f.node_kind(n), NodeKind::Return))
        .map(|ret| f.node_inputs(ret).into_iter().skip(2).collect())
        .collect()
}

/// The equal edge's arm reads the constant; the other arm keeps the phi.
#[test]
fn only_the_equal_arm_reads_the_constant() -> crate::Result<()> {
    let mut f = diamond_then_test(false, |fb, phi, then| {
        add(fb, phi, if then { 5 } else { 6 })
    })?;
    let want: Vec<Outcome> = [0u64, 1].iter().map(|&a| outcome(&f, &[a])).collect();
    run_pass(&PhiCollapse, &mut f)?;
    let rets = returns(&f);
    assert_eq!(
        rets.iter().filter(|r| reads_phi(&f, r[0])).count(),
        1,
        "{}",
        f.to_text(false)
    );
    for (a, want) in [0u64, 1].iter().zip(&want) {
        assert_eq!(&outcome(&f, &[*a]), want);
    }
    Ok(())
}

/// A read below the join of both arms, and one floating node read in both
/// arms, keep the phi: neither point is dominated by the equal edge alone.
#[test]
fn reads_past_the_join_or_on_both_arms_keep_the_phi() -> crate::Result<()> {
    let mut f = diamond_then_test(
        true,
        |fb, phi, then| {
            if then { Ok(phi) } else { add(fb, phi, 7) }
        },
    )?;
    run_pass(&PhiCollapse, &mut f)?;
    let [out, x] = returned(&f)[..] else {
        panic!("two returned values");
    };
    let mid = f.producer(x);
    assert!(
        matches!(f.node_kind(mid), NodeKind::Phi),
        "{}",
        f.to_text(false)
    );
    let arms: Vec<ValueId> = f.phi_data_inputs(f.producer(out)).collect();
    assert_eq!(
        f.int_const_u128(arms[0]),
        Some(2),
        "the equal arm's read moves"
    );
    assert!(reads_phi(&f, arms[1]));

    // One `Add` node: the builder dedups the second.
    let mut f = diamond_then_test(false, |fb, phi, _| add(fb, phi, 3))?;
    run_pass(&PhiCollapse, &mut f)?;
    assert!(
        returns(&f).iter().all(|r| reads_phi(&f, r[0])),
        "{}",
        f.to_text(false)
    );
    Ok(())
}

/// Random loops and diamonds testing `Add(var, c) == var` or `== c` keep
/// their outcomes under `PhiCollapse` alone and under the default pipeline,
/// and most move a read.
#[test]
fn random_equal_edges_keep_their_outcomes() -> crate::Result<()> {
    let (mut built, mut moved) = (0, 0);
    for seed in 0..400u64 {
        let Some(f0) = random_cfg(seed) else {
            continue;
        };
        built += 1;
        let mut rng = Rng(seed | 1);
        let vecs: Vec<[u64; 3]> = (0..12)
            .map(|_| {
                let pick =
                    |rng: &mut Rng| [0, 1, 2, rng.next_u64() % 8, rng.next_u64()][rng.below(5)];
                [pick(&mut rng), pick(&mut rng), pick(&mut rng)]
            })
            .collect();
        let want: Vec<Outcome> = vecs.iter().map(|v| outcome(&f0, v)).collect();
        let mut single = f0.clone();
        let mut edit = crate::EditFunction::new(&mut single);
        let generation = edit.generation();
        super::equal_edge::fold_equal_edges(&mut edit)?;
        moved += usize::from(edit.generation() != generation);
        edit.clean();
        let mut alone = f0.clone();
        run_pass(&PhiCollapse, &mut alone)?;
        let mut piped = f0.clone();
        crate::default_pipeline().run(&mut piped, &crate::OptCtx::new(None))?;
        for (f, what) in [
            (&single, "fold"),
            (&alone, "PhiCollapse"),
            (&piped, "pipeline"),
        ] {
            if let Err(errors) = strider_ir::validate::validate(f) {
                panic!("seed {seed} {what}: {}", errors.report(f));
            }
            for (v, want) in vecs.iter().zip(&want) {
                let got = outcome(f, v);
                assert!(
                    matches!((want, &got), (Outcome::Diverge(_), Outcome::Diverge(_)))
                        || &got == want,
                    "seed {seed} {what} inputs {v:?}: {want:?} became {got:?}\nBEFORE:\n{}\nAFTER:\n{}",
                    f0.to_text(false),
                    f.to_text(false)
                );
            }
        }
    }
    assert!(built >= 100, "built {built}");
    println!("moved in {moved} of {built}");
    assert!(moved >= built / 4, "moved in {moved} of {built}");
    Ok(())
}

/// Three 32-bit vars over 3..8 regions; each region adds a constant to a var
/// or copies one, then returns, branches, or tests `Add(x, c) == y` or `== c`.
fn random_cfg(seed: u64) -> Option<strider_ir::Function> {
    let ty = ValueType::int(32);
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let vns = [0x1000, 0x1004, 0x1008].map(|off| reg_vn(off, 4));
    let mut set = RegisterSet::new();
    for vn in vns {
        set = set.tracked(vn).arg(vn);
    }
    let mut b = set.build_fn().ok()?;
    let n = 3 + rng.below(6);
    let regions: Vec<_> = (0..n).map(|_| b.create_region_all().unwrap()).collect();
    b.set_entry_region_all(regions[0]).ok()?;
    for (i, &region) in regions.iter().enumerate() {
        b.set_region(region);
        for _ in 0..rng.below(3) {
            let j = rng.below(3);
            let src = b.read_variable(&vns[rng.below(3)]).ok()?;
            let k = b.build_int_const(rng.next_u64() % 4, ty).ok()?;
            let v = if rng.chance(70) {
                b.build_int_binary_operation(src, k, IntBinaryOp::Add, ty)
                    .ok()?
            } else {
                src
            };
            b.write_variable(&vns[j], v).ok()?;
        }
        let last = i + 1 == n;
        match if last { 0 } else { rng.below(6) } {
            0 => {
                let vals: Vec<ValueId> =
                    vns.iter().map(|vn| b.read_variable(vn).unwrap()).collect();
                let k = b.build_int_const(rng.next_u64() % 3, ty).ok()?;
                let mut vals = vals;
                let first = vals[0];
                vals.push(
                    b.build_int_binary_operation(first, k, IntBinaryOp::Add, ty)
                        .ok()?,
                );
                b.build_return(None, &vals).ok()?;
            }
            1 => b.build_branch(regions[rng.below(n)]).ok()?,
            _ => {
                let x = b.read_variable(&vns[rng.below(3)]).ok()?;
                let c = b.build_int_const(rng.next_u64() % 3, ty).ok()?;
                let lhs = if rng.chance(50) {
                    b.build_int_binary_operation(x, c, IntBinaryOp::Add, ty)
                        .ok()?
                } else {
                    x
                };
                let rhs = if rng.chance(50) {
                    b.read_variable(&vns[rng.below(3)]).ok()?
                } else {
                    b.build_int_const(rng.next_u64() % 4, ty).ok()?
                };
                let mut cond = b
                    .build_int_cmp_operation(lhs, rhs, IntCmpOp::Equal, ty)
                    .ok()?;
                if rng.chance(30) {
                    let one = b.build_boolean_const(true);
                    cond = b
                        .build_int_binary_operation(cond, one, IntBinaryOp::Xor, ValueType::I1)
                        .ok()?;
                }
                let (t, e) = (regions[rng.below(n)], regions[rng.below(n)]);
                if t == e {
                    b.build_branch(t).ok()?;
                } else {
                    b.build_if(cond, t, e).ok()?;
                }
            }
        }
    }
    b.set_lift_addr(None);
    b.build().ok()
}

/// A loop whose header phi `x` an `n`-long else-if chain tests against
/// distinct constants; each equal arm writes `x + 1` and loops.
fn if_chain(n: usize) -> crate::Result<strider_ir::Function> {
    let x = reg_vn(0x10, 8);
    let mut fb = RegisterSet::new().tracked(x).build_fn()?;
    let [entry, header, exit] = [(); 3].map(|()| fb.create_region_all().unwrap());
    let tests: Vec<_> = (0..n).map(|_| fb.create_region_all().unwrap()).collect();
    let arms: Vec<_> = (0..n).map(|_| fb.create_region_all().unwrap()).collect();
    fb.set_entry_region_all(entry)?;
    fb.set_region(entry);
    let zero = fb.build_int_const(0u64, I64)?;
    fb.write_variable(&x, zero)?;
    fb.build_branch(header)?;
    fb.set_region(header);
    fb.build_branch(tests[0])?;
    for k in 0..n {
        fb.set_region(tests[k]);
        let v = fb.read_variable(&x)?;
        let c = fb.build_int_const(k as u64 * 3, I64)?;
        let cond = fb.build_int_cmp_operation(v, c, IntCmpOp::Equal, I64)?;
        fb.build_if(cond, arms[k], tests.get(k + 1).copied().unwrap_or(exit))?;
        fb.set_region(arms[k]);
        let v = fb.read_variable(&x)?;
        let v = add(&mut fb, v, 1)?;
        fb.write_variable(&x, v)?;
        fb.build_branch(header)?;
    }
    fb.set_region(exit);
    let v = fb.read_variable(&x)?;
    fb.build_return(None, &[v])?;
    fb.set_lift_addr(None);
    fb.build()
}

/// `n` tests of one phi cost reads examined linear in `n`, not one scan of
/// the phi's reads per test.
#[test]
fn many_tests_of_one_phi_scan_its_reads_near_linearly() -> crate::Result<()> {
    crate::test_support::assert_near_linear(
        "equal-edge reads",
        500,
        |n| {
            let mut f = if_chain(n)?;
            crate::test_support::steps(&[&super::equal_edge::READS_SCANNED], || {
                run_pass(&PhiCollapse, &mut f).map(|_| ())
            })
        },
        250,
    )
}

/// `if (x + 1 == x)`: the other side roots at the same phi, where `x - 1`
/// would read the phi and moving that read would close a data cycle, so
/// nothing moves.
#[test]
fn an_equality_with_the_same_phi_on_both_sides_moves_nothing() -> crate::Result<()> {
    let x = reg_vn(0x10, 8);
    let mut fb = RegisterSet::new().tracked(x).arg(x).build_fn()?;
    let [entry, header, then, exit] = [(); 4].map(|()| fb.create_region_all().unwrap());
    fb.set_entry_region_all(entry)?;
    fb.set_region(entry);
    fb.build_branch(header)?;
    fb.set_region(header);
    let v = fb.read_variable(&x)?;
    let lhs = add(&mut fb, v, 1)?;
    let cond = fb.build_int_cmp_operation(lhs, v, IntCmpOp::Equal, I64)?;
    fb.build_if(cond, then, exit)?;
    fb.set_region(then);
    let two = fb.build_int_const(2u64, I64)?;
    let doubled = fb.build_int_binary_operation(v, two, IntBinaryOp::Mul, I64)?;
    fb.build_return(None, &[doubled])?;
    fb.set_region(exit);
    let next = add(&mut fb, v, 3)?;
    fb.write_variable(&x, next)?;
    fb.build_branch(header)?;
    fb.set_lift_addr(None);
    let mut f = fb.build()?;
    while run_pass(&PhiCollapse, &mut f)? {}
    assert!(reads_phi(&f, returned(&f)[0]), "{}", f.to_text(false));
    Ok(())
}

/// `x = phi(a, b); if (x + 16 == *p) return x + 16`: the other side is a
/// load, and the read keeps `x + 16`, the value the memory analysis follows
/// (an on-stack list head tested against the pointer it holds).
#[test]
fn an_equality_with_a_load_moves_nothing() -> crate::Result<()> {
    let (a, b, x) = (reg_vn(0x8, 8), reg_vn(0x10, 8), reg_vn(0x18, 8));
    let mut fb = RegisterSet::new()
        .tracked(a)
        .arg(a)
        .tracked(b)
        .arg(b)
        .tracked(x)
        .build_fn()?;
    let [entry, left, right, mid, then, other] = [(); 6].map(|()| fb.create_region_all().unwrap());
    fb.set_entry_region_all(entry)?;
    fb.set_region(entry);
    let first = fb.read_variable(&a)?;
    let zero = fb.build_int_const(0u64, I64)?;
    let c = fb.build_int_cmp_operation(first, zero, IntCmpOp::Equal, I64)?;
    fb.build_if(c, left, right)?;
    for (region, arg) in [(left, a), (right, b)] {
        fb.set_region(region);
        let v = fb.read_variable(&arg)?;
        fb.write_variable(&x, v)?;
        fb.build_branch(mid)?;
    }
    fb.set_region(mid);
    let phi = fb.read_variable(&x)?;
    let head = add(&mut fb, phi, 16)?;
    let addr = fb.build_int_const(0x100u64, I64)?;
    let next = fb.build_load(addr, rsleigh::VnSpace::RAM, I64)?;
    let c = fb.build_int_cmp_operation(head, next, IntCmpOp::Equal, I64)?;
    fb.build_if(c, then, other)?;
    fb.set_region(then);
    let read = add(&mut fb, head, 8)?;
    fb.build_return(None, &[read])?;
    fb.set_region(other);
    fb.build_return(None, &[zero])?;
    fb.set_lift_addr(None);
    let mut f = fb.build()?;
    run_pass(&PhiCollapse, &mut f)?;
    let rets = returns(&f);
    assert!(
        rets.iter().any(|r| reads_phi(&f, r[0])),
        "{}",
        f.to_text(false)
    );
    Ok(())
}
