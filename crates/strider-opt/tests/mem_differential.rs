//! Memory differential fuzzer: random CFGs of loads, stores and calls, run on
//! the reference interpreter before and after `default_pipeline()`.
//!
//! Accesses are 1 to 16 bytes (24-bit included) at `sp + [-32, 8)` (partial
//! overlaps included), at constant addresses `0x1000 + [0, 24)` in RAM and in
//! the REGISTER space, off an `sp & -16` base, and at two anchors
//! (`sp - 32 + (v & 0x18)` and `0x1000 | (v & 0x18)`), under both byte
//! orders. Three modes: plain under `AssumptionOptions::none()` and
//! `::default()`; `escape_analysis` over a 64-byte frame every callee keeps;
//! and a `noalias_allocators` callee whose pointer the function addresses,
//! which keeps the frame and earlier allocations. The frame is excluded from
//! the compared writes.
//!
//! `FUZZ_N` and `FUZZ_START` scale a run; the `#[ignore]`d test is the long
//! one.

use strider_ir::node::{ExtendOp, NodeKind, ValueId, ValueType};
use strider_ir::{FunctionBuilder, IRBuilderExt, IRViewer, IntBinaryOp, IntCmpOp, UBig, eval};
use strider_ir_test_utils::interp::{self, EntryVar, Keeps, Outcome, Slot};
use strider_ir_test_utils::{Rng, reg_vn};

const SP: u64 = 0x7fff_0000_0000;
const ALLOCATOR: u64 = 0x5000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Plain,
    Escape,
    Alloc,
}

fn fit(b: &mut FunctionBuilder, v: ValueId, to: ValueType) -> Option<ValueId> {
    let from = b.function().value_type_opt(v)?;
    Some(if from == to {
        v
    } else if from.bit_width() > to.bit_width() {
        b.build_single_output_pure(NodeKind::Truncate, [v], to)
    } else {
        b.build_single_output_pure(NodeKind::Extend(ExtendOp::ZeroExtend), [v], to)
    })
}

/// A random function and its SP varnode; `None` when the builder refuses
/// the drawn shape.
fn build(
    seed: u64,
    big_endian: bool,
    calls: bool,
    mode: Mode,
) -> Option<(strider_ir::Function, rsleigh::Vn)> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let sp = strider_ir_test_utils::stack_vn_x86_64();
    let i64t = ValueType::int(64);
    let nvars = 1 + rng.below(3);
    let mut rs = strider_ir_test_utils::sp_frame(sp);
    if big_endian {
        rs = rs.byte_order(strider_target::Endianness::Big);
    }
    let mut vns = Vec::new();
    for i in 0..nvars {
        let vn = reg_vn(0x1000 * (i as u64 + 1), [1u32, 4, 8][rng.below(3)]);
        rs = rs.tracked(vn).arg(vn);
        vns.push(vn);
    }
    let ret = reg_vn(0x8000, 8);
    if mode == Mode::Alloc {
        rs = rs.tracked(ret).ret(ret);
    }
    // The share of `sp + [0, 64)` addresses, frame-placed, that escape mode
    // draws in place of anchors.
    let frame_pct = if mode == Mode::Escape {
        100 - rng.below(2) * 10
    } else {
        0
    };
    let mut b = rs.build_fn().ok()?;
    let nregions = 2 + rng.below(7);
    let regions: Vec<_> = (0..nregions)
        .map(|_| b.create_region_all())
        .collect::<Result<_, _>>()
        .ok()?;
    b.set_entry_region_all(regions[0]).ok()?;
    let widths = [8, 16, 24, 32, 64, 128].map(ValueType::int);
    // Outside plain mode the function opens a 64-byte frame and closes it
    // before each return.
    let mut entry_sp = None;
    if mode != Mode::Plain {
        b.set_region(regions[0]);
        let spv = b.read_variable(&sp).ok()?;
        entry_sp = Some(spv);
        let low = b
            .build_const_binop((-64i128) as u128, spv, IntBinaryOp::Add, i64t)
            .ok()?;
        b.write_variable(&sp, low).ok()?;
    }
    for (i, &region) in regions.iter().enumerate() {
        b.set_region(region);
        for _ in 0..=rng.below(6) {
            let spv = b.read_variable(&sp).ok()?;
            let mut space = rsleigh::VnSpace::RAM;
            let addr = match rng.below(14) {
                10 => {
                    let m = b
                        .build_const_binop((-16i128) as u128, spv, IntBinaryOp::And, i64t)
                        .ok()?;
                    let off = rng.below(48) as i128 - 16;
                    b.build_const_binop(off as u128, m, IntBinaryOp::Add, i64t)
                        .ok()?
                }
                11 => {
                    space = rsleigh::VnSpace::REGISTER;
                    b.build_int_const(0x1000 + rng.below(24) as u128, i64t)
                        .ok()?
                }
                12 | 13 if mode == Mode::Alloc => {
                    let p = b.read_variable(&ret).ok()?;
                    let off = rng.below(24) as u128;
                    b.build_const_binop(off, p, IntBinaryOp::Add, i64t).ok()?
                }
                0..=5 | 12 | 13 => {
                    let off = if mode == Mode::Plain {
                        rng.below(40) as i128 - 32
                    } else if rng.chance(70) {
                        8 * rng.below(8) as i128 - 24
                    } else {
                        rng.below(96) as i128 - 24
                    };
                    b.build_const_binop(off as u128, spv, IntBinaryOp::Add, i64t)
                        .ok()?
                }
                8 | 9 if rng.below(100) < frame_pct => {
                    let off = rng.below(64) as i128;
                    b.build_const_binop(off as u128, spv, IntBinaryOp::Add, i64t)
                        .ok()?
                }
                6 | 7 => b
                    .build_int_const(0x1000 + rng.below(24) as u128, i64t)
                    .ok()?,
                k => {
                    let vj = b.read_variable(&vns[rng.below(nvars)]).ok()?;
                    let vj = fit(&mut b, vj, i64t)?;
                    let m = b
                        .build_const_binop(0x18u64, vj, IntBinaryOp::And, i64t)
                        .ok()?;
                    if k == 8 {
                        let base = b
                            .build_const_binop((-32i128) as u128, spv, IntBinaryOp::Add, i64t)
                            .ok()?;
                        b.build_int_binary_operation(base, m, IntBinaryOp::Add, i64t)
                            .ok()?
                    } else {
                        b.build_const_binop(0x1000u64, m, IntBinaryOp::Or, i64t)
                            .ok()?
                    }
                }
            };
            let ty = if mode != Mode::Plain && rng.chance(50) {
                i64t
            } else {
                widths[rng.below(widths.len())]
            };
            let var = vns[rng.below(nvars)];
            let vj = b.read_variable(&var).ok()?;
            let vty = b.function().value_type_opt(vj)?;
            match rng.below(12) {
                0..=4 => {
                    let data = if rng.chance(30) {
                        b.build_int_const(u128::from(rng.next_u64()), ty).ok()?
                    } else {
                        fit(&mut b, vj, ty)?
                    };
                    b.build_store(addr, data, space).ok()?;
                }
                5..=9 => {
                    let l = b.build_load(addr, space, ty).ok()?;
                    let l = fit(&mut b, l, vty)?;
                    let nv = b
                        .build_int_binary_operation(vj, l, IntBinaryOp::Xor, vty)
                        .ok()?;
                    b.write_variable(&var, nv).ok()?;
                }
                10 | 11 if calls && (mode != Mode::Plain || rng.chance(50)) => {
                    if mode == Mode::Alloc && rng.chance(60) {
                        let t = b.build_int_const(u128::from(ALLOCATOR), i64t).ok()?;
                        let (_, outs) = b.build_call(t, &[], &[ret], 0).ok()?;
                        b.write_variable(&ret, outs[0]).ok()?;
                    } else {
                        let t = b.build_int_const(0x4000u128, i64t).ok()?;
                        b.build_call(t, &[], &[], 0).ok()?;
                    }
                }
                _ => {
                    let c = b
                        .build_int_const(u128::from(rng.next_u64() % 5), vty)
                        .ok()?;
                    let nv = b
                        .build_int_binary_operation(vj, c, IntBinaryOp::Add, vty)
                        .ok()?;
                    b.write_variable(&var, nv).ok()?;
                }
            }
        }
        let choice = if i + 1 == nregions { 0 } else { rng.below(10) };
        match choice {
            0 | 1 => {
                if let Some(v) = entry_sp {
                    b.write_variable(&sp, v).ok()?;
                }
                let vals: Vec<ValueId> = vns
                    .iter()
                    .map(|vn| b.read_variable(vn))
                    .collect::<Result<_, _>>()
                    .ok()?;
                b.build_return(None, &vals).ok()?;
            }
            3 | 4 => {
                b.build_branch(regions[rng.below(nregions)]).ok()?;
            }
            _ => {
                let vj = b.read_variable(&vns[rng.below(nvars)]).ok()?;
                let ty = b.function().value_type_opt(vj)?;
                let c = b.build_int_const(u128::from(rng.next_u64() % 5), ty).ok()?;
                let op = [IntCmpOp::Equal, IntCmpOp::Less, IntCmpOp::Sless][rng.below(3)];
                let cond = b.build_int_cmp_operation(vj, c, op, ty).ok()?;
                let (t, e) = (regions[rng.below(nregions)], regions[rng.below(nregions)]);
                if t == e {
                    b.build_branch(t).ok()?;
                } else {
                    b.build_if(cond, t, e).ok()?;
                }
            }
        }
    }
    b.set_lift_addr(None);
    Some((b.build().ok()?, sp))
}

fn input_value(rng: &mut Rng, ty: ValueType) -> UBig {
    let m = eval::all_ones(ty);
    let w = ty.bit_width();
    let v = match rng.below(6) {
        0 => UBig::ZERO,
        1 => m,
        2 => UBig::ONE << (w - 1),
        3 => UBig::from(rng.next_u64() % 8),
        _ => UBig::from(rng.next_u64()),
    };
    eval::mask(&v, ty)
}

/// Whether the optimised `got` may stand for the raw `want`: equal, except
/// that anything refines an undefined raw slot.  `None` when the reference
/// cannot decide.
fn refines(want: &Outcome, got: &Outcome) -> Option<bool> {
    let slots = |w: &[Slot], g: &[Slot]| -> Option<bool> {
        if w.len() != g.len() {
            return Some(false);
        }
        for (w, g) in w.iter().zip(g) {
            if !w.admits(g)? {
                return Some(false);
            }
        }
        Some(true)
    };
    let (
        Outcome::Return {
            values: wv,
            writes: ww,
            calls: wc,
        },
        Outcome::Return {
            values: gv,
            writes: gw,
            calls: gc,
        },
    ) = (want, got)
    else {
        return Some(false);
    };
    if ww != gw || wc.len() != gc.len() || !slots(wv, gv)? {
        return Some(false);
    }
    for (w, g) in wc.iter().zip(gc) {
        if w.target != g.target || w.writes != g.writes || !slots(&w.args, &g.args)? {
            return Some(false);
        }
    }
    Some(true)
}

fn env_or(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Checks one seed; `Ok(None)` when the builder refused it, else the number
/// of compared runs and whether the pipeline removed a load.
fn check(seed: u64) -> Result<Option<(usize, bool)>, String> {
    let big = seed % 3 == 1;
    let mode = [Mode::Plain, Mode::Escape, Mode::Alloc][(seed / 4 % 3) as usize];
    let calls = seed.is_multiple_of(2) || mode != Mode::Plain;
    let Some((mut f, sp)) = build(seed, big, calls, mode) else {
        return Ok(None);
    };
    let frame = Some(SP - 0x1000..SP);
    let keeps = match mode {
        Mode::Plain => {
            if seed % 4 < 2 {
                f.assume(&strider_opt::AssumptionOptions::default());
            }
            None
        }
        Mode::Escape => {
            f.assume(&strider_opt::AssumptionOptions {
                escape_analysis: true,
                ..Default::default()
            });
            Some(Keeps {
                frame,
                allocators: vec![],
                all_calls: true,
            })
        }
        Mode::Alloc => {
            let mut claims = strider_opt::AssumptionOptions::default();
            claims.noalias_allocators.insert(ALLOCATOR);
            f.assume(&claims);
            Some(Keeps {
                frame,
                allocators: vec![u128::from(ALLOCATOR)],
                all_calls: false,
            })
        }
    };
    let before = f.to_text(false);
    let loads = |f: &strider_ir::Function| f.to_text(false).matches("= load[").count();
    let loads_before = loads(&f);
    let mut rng = Rng(seed | 1);
    let inputs: Vec<Vec<u64>> = (0..12)
        .map(|_| (0..8).map(|_| rng.next_u64()).collect())
        .collect();
    let run = |f: &strider_ir::Function, input: &[u64]| {
        let entry = |e: EntryVar| {
            if e.vn == sp {
                UBig::from(SP)
            } else {
                input_value(&mut Rng(input[e.index] | 1), e.ty)
            }
        };
        interp::run(
            f,
            &interp::Env {
                entry: &entry,
                rom: None,
                seed: 7,
                // Each call copies the kept memory, which allocations grow.
                step_budget: if mode == Mode::Plain { 20_000 } else { 2_000 },
                private: Some(SP - 0x1000..=SP + 0x1000),
                keeps: keeps.as_ref(),
            },
        )
    };
    let wants: Vec<_> = inputs.iter().map(|i| run(&f, i).ok()).collect();
    strider_opt::default_pipeline()
        .run(&mut f, &strider_opt::OptCtx::new(None))
        .map_err(|e| format!("seed {seed}: pipeline error {e:#}\n{before}"))?;
    let mut compared = 0;
    for (input, want) in inputs.iter().zip(&wants) {
        let Some(want @ Outcome::Return { .. }) = want else {
            continue;
        };
        let dump = |what: String| {
            format!(
                "seed {seed} big={big} {mode:?}: inputs {input:?} {what}\nBEFORE:\n{before}\nAFTER:\n{}",
                f.to_text(false)
            )
        };
        let got = run(&f, input).map_err(|why| dump(format!("after: {why:?}")))?;
        compared += 1;
        if refines(want, &got) == Some(false) {
            return Err(dump(format!("want {want:?} got {got:?}")));
        }
    }
    Ok(Some((compared, loads(&f) < loads_before)))
}

fn sweep(default_seeds: u64) {
    let start = env_or("FUZZ_START", 0);
    let n = env_or("FUZZ_N", default_seeds);
    let (mut built, mut compared, mut forwarded) = (0, 0, 0);
    let mut failures = Vec::new();
    for seed in start..start + n {
        match std::panic::catch_unwind(|| check(seed)) {
            Ok(Ok(None)) => {}
            Ok(Ok(Some((c, f)))) => {
                built += 1;
                compared += c;
                forwarded += usize::from(f);
            }
            Ok(Err(e)) => failures.push(e),
            Err(_) => failures.push(format!("seed {seed}: panic")),
        }
        if failures.len() >= 3 {
            break;
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n=====\n"));
    assert!(
        built * 4 >= n as usize && compared >= built && forwarded * 20 >= built,
        "too thin a corpus: {built} built, {compared} runs compared, {forwarded} forwarded"
    );
}

#[test]
fn the_pipeline_preserves_memory_semantics() {
    sweep(if cfg!(debug_assertions) { 100 } else { 400 });
}

#[test]
#[ignore = "long run"]
fn the_pipeline_preserves_memory_semantics_long() {
    sweep(6000);
}
