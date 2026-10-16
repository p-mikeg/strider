//! Random x86-64 control flow, lifted, validated and run on the reference
//! interpreter against a direct simulation of the generated code.
//!
//! A branch may land on a block's terminator, past its body. That splits the
//! block's region, and the head takes a later `RegionId` than the tail it
//! dominates, so ascending region order stops being a dominator preorder.

use std::collections::BTreeSet;

use strider_ir::node::NodeKind;
use strider_ir_test_utils::IrWalkerEx;
use strider_ir_test_utils::graph_oracle;
use strider_ir_test_utils::interp::{self, Divergence, EntryVar, Env, Outcome, Slot};
use strider_lift::LiftOptions;

const BASE: u64 = 0x1000;
/// Bytes per generated function.
const SPAN: u64 = 0x1000;
/// Bytes per block: the longest block is 22.
const STRIDE: u64 = 32;
const MAX_BLOCKS: u64 = 12;
/// Bytes of a `Mov` or `Add` body.
const BODY_LEN: u64 = 5;
/// Bytes of `test edi, imm32; jz rel32`, after which a `Cond` falls through.
const TEST_JZ_LEN: u64 = 12;
const STACK: u128 = 0x7fff_1000;
const CASES: u64 = if cfg!(debug_assertions) { 48 } else { 1024 };
const EDIS: [u32; 4] = [0, 0x5a, 0xa5, 0xff];

#[derive(Clone, Copy, Debug)]
enum Body {
    None,
    Mov(u32),
    Add(u32),
}

/// A branch target: a block's first instruction, or with `skip` its
/// terminator.
#[derive(Clone, Copy, Debug)]
struct To {
    block: usize,
    skip: bool,
}

#[derive(Clone, Copy, Debug)]
enum Term {
    Ret,
    Jmp(To),
    /// `test edi, mask; jz taken; jmp other`.
    Cond {
        mask: u32,
        taken: To,
        other: To,
    },
}

impl Term {
    fn targets(self) -> Vec<To> {
        match self {
            Term::Ret => Vec::new(),
            Term::Jmp(t) => vec![t],
            Term::Cond { taken, other, .. } => vec![taken, other],
        }
    }
}

fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn random_blocks(seed: u64) -> Vec<(Body, Term)> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let n = 1 + next(&mut state) % MAX_BLOCKS;
    let to = |state: &mut u64| To {
        block: (next(state) % n) as usize,
        skip: next(state).is_multiple_of(4),
    };
    (0..n)
        .map(|_| {
            let body = match next(&mut state) % 3 {
                0 => Body::None,
                1 => Body::Mov(next(&mut state) as u32),
                _ => Body::Add(next(&mut state) as u32),
            };
            let term = match next(&mut state) % 10 {
                0 | 1 => Term::Ret,
                2..=4 => Term::Jmp(to(&mut state)),
                _ => Term::Cond {
                    mask: 1 << (next(&mut state) % 8),
                    taken: to(&mut state),
                    other: to(&mut state),
                },
            };
            (body, term)
        })
        .collect()
}

/// Program points: `2b` is block `b`'s first instruction, `2b + 1` its
/// terminator when it has a body. A `To` past an empty body is `2b`.
fn point(blocks: &[(Body, Term)], to: To) -> usize {
    2 * to.block + usize::from(to.skip && !matches!(blocks[to.block].0, Body::None))
}

fn terminator_point(blocks: &[(Body, Term)], b: usize) -> usize {
    2 * b + usize::from(!matches!(blocks[b].0, Body::None))
}

fn point_addr(base: u64, p: usize) -> u64 {
    base + (p / 2) as u64 * STRIDE + (p % 2) as u64 * BODY_LEN
}

/// `rel32` from the end of an instruction of `len` bytes starting at `at`.
fn rel32(at: u64, len: u64, target: u64) -> [u8; 4] {
    ((target as i64 - (at + len) as i64) as i32).to_le_bytes()
}

fn assemble(blocks: &[(Body, Term)], base: u64) -> Vec<u8> {
    let addr = |t: To| point_addr(base, point(blocks, t));
    let mut out = vec![0xcc; SPAN as usize];
    for (b, &(body, term)) in blocks.iter().enumerate() {
        let start = point_addr(base, 2 * b);
        let mut code: Vec<u8> = Vec::new();
        match body {
            Body::None => {}
            Body::Mov(k) => {
                code.push(0xb8);
                code.extend(k.to_le_bytes());
            }
            Body::Add(k) => {
                code.push(0x05);
                code.extend(k.to_le_bytes());
            }
        }
        match term {
            Term::Ret => code.push(0xc3),
            Term::Jmp(t) => {
                let at = start + code.len() as u64;
                code.push(0xe9);
                code.extend(rel32(at, 5, addr(t)));
            }
            Term::Cond { mask, taken, other } => {
                code.extend([0xf7, 0xc7]);
                code.extend(mask.to_le_bytes());
                let at = start + code.len() as u64;
                code.extend([0x0f, 0x84]);
                code.extend(rel32(at, 6, addr(taken)));
                let at = start + code.len() as u64;
                code.push(0xe9);
                code.extend(rel32(at, 5, addr(other)));
            }
        }
        let offset = (start - base) as usize;
        out[offset..offset + code.len()].copy_from_slice(&code);
    }
    out
}

/// `Some(rax)` at the `ret`; `None` when the run comes back to a program
/// point, which with `edi` fixed is a loop forever. `path` gets the points
/// run, in order.
fn simulate(blocks: &[(Body, Term)], edi: u32, rax: u64, path: &mut Vec<usize>) -> Option<u64> {
    let mut rax = rax;
    let mut seen = vec![false; 2 * blocks.len()];
    let mut at = 0;
    loop {
        if std::mem::replace(&mut seen[at], true) {
            return None;
        }
        path.push(at);
        let (body, term) = blocks[at / 2];
        if at.is_multiple_of(2) && !matches!(body, Body::None) {
            rax = match body {
                Body::None => rax,
                Body::Mov(k) => u64::from(k),
                Body::Add(k) => u64::from((rax as u32).wrapping_add(k)),
            };
            at += 1;
            continue;
        }
        let to = match term {
            Term::Ret => return Some(rax),
            Term::Jmp(t) => t,
            Term::Cond { mask, taken, other } => {
                if edi & mask == 0 {
                    taken
                } else {
                    other
                }
            }
        };
        at = point(blocks, to);
    }
}

/// Each program point's successors: a body falls through to its terminator.
fn successors(blocks: &[(Body, Term)]) -> Vec<Vec<u32>> {
    let mut succs = vec![Vec::new(); 2 * blocks.len()];
    for (b, &(body, term)) in blocks.iter().enumerate() {
        if !matches!(body, Body::None) {
            succs[2 * b].push(2 * b as u32 + 1);
        }
        succs[terminator_point(blocks, b)] = term
            .targets()
            .into_iter()
            .map(|t| point(blocks, t) as u32)
            .collect();
    }
    succs
}

/// Where the CFG must start a region: the entry, every branch target of a
/// reachable terminator, and the `jmp` a reachable `jz` falls through to.
fn region_starts(blocks: &[(Body, Term)], base: u64, reach: &[bool]) -> BTreeSet<u64> {
    let mut starts = BTreeSet::from([base]);
    for (b, &(_, term)) in blocks.iter().enumerate() {
        let at = terminator_point(blocks, b);
        if !reach[at] {
            continue;
        }
        let targets = term.targets().into_iter();
        starts.extend(targets.map(|t| point_addr(base, point(blocks, t))));
        if matches!(term, Term::Cond { .. }) {
            starts.insert(point_addr(base, at) + TEST_JZ_LEN);
        }
    }
    starts
}

/// How many draws carry each shape in their reachable control flow, so a
/// generator change cannot quietly drop one.
#[derive(Debug, Default)]
struct Shapes {
    cyclic: u64,
    irreducible: u64,
    self_loop: u64,
    multi_edge: u64,
    back_to_entry: u64,
    exit_free: u64,
    /// A branch lands past a block's body.
    split: u64,
    /// Some region's immediate dominator has the larger `RegionId`.
    idom_after: u64,
    with_phi: u64,
    /// Returning runs that passed a point with two predecessors.
    joined_returns: u64,
    most_points: usize,
}

impl Shapes {
    fn tally(&mut self, blocks: &[(Body, Term)], succs: &[Vec<u32>], reach: &[bool]) {
        let live: Vec<usize> = (0..succs.len()).filter(|&p| reach[p]).collect();
        let any = |pred: &dyn Fn(usize) -> bool| u64::from(live.iter().any(|&p| pred(p)));
        let returns = |p: usize| {
            let from = graph_oracle::reachable(succs, &[p as u32], None);
            (0..blocks.len())
                .any(|b| matches!(blocks[b].1, Term::Ret) && from[terminator_point(blocks, b)])
        };
        let is_terminator = |p: usize| terminator_point(blocks, p / 2) == p;
        self.cyclic += any(&|p| graph_oracle::reaches(succs, p as u32, p as u32));
        self.irreducible += u64::from(!graph_oracle::is_reducible(succs, 0));
        self.self_loop += any(&|p| succs[p].contains(&(p as u32)));
        self.multi_edge += any(&|p| succs[p].len() == 2 && succs[p][0] == succs[p][1]);
        self.back_to_entry += any(&|p| succs[p].contains(&0));
        self.exit_free += any(&|p| !returns(p));
        self.split += any(&|p| is_terminator(p) && succs[p].iter().any(|&s| s % 2 == 1));
        self.most_points = self.most_points.max(live.len());
    }

    /// The rarest shape's count.
    fn rarest(&self) -> u64 {
        [
            self.cyclic,
            self.irreducible,
            self.self_loop,
            self.multi_edge,
            self.back_to_entry,
            self.exit_free,
            self.split,
            self.idom_after,
            self.with_phi,
            self.joined_returns,
        ]
        .into_iter()
        .min()
        .unwrap_or(0)
    }
}

/// Whether some region's immediate dominator, by definition, has the larger
/// `RegionId`.
fn idom_after(cfg: &strider_cfg::Cfg) -> bool {
    let ids: Vec<strider_cfg::RegionId> = cfg.region_ids().collect();
    let index = |r| ids.iter().position(|&x| x == r).expect("a region") as u32;
    let succs: Vec<Vec<u32>> = ids
        .iter()
        .map(|&r| cfg.region_graph().neighbors(r).map(index).collect())
        .collect();
    let root = index(cfg.entry());
    let idom = graph_oracle::idoms(&graph_oracle::dom_matrix(&succs, root), root);
    (0..ids.len()).any(|v| idom[v].is_some_and(|d| ids[d as usize] > ids[v]))
}

#[test]
fn random_control_flow_lifts_to_what_the_code_computes() {
    let functions: Vec<Vec<(Body, Term)>> = (0..CASES).map(random_blocks).collect();
    let image: Vec<u8> = functions
        .iter()
        .enumerate()
        .flat_map(|(g, blocks)| assemble(blocks, BASE + g as u64 * SPAN))
        .collect();
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        rsleigh::mem_readers::BufMemReader::new(image, BASE),
    )
    .expect("sleigh");
    let mut lifter = strider_lift::lift::Lifter::new(arch, sleigh).expect("lifter");
    let opts = LiftOptions::default();
    let (mut returned, mut diverged) = (0u64, 0u64);
    let mut shapes = Shapes::default();
    for (g, blocks) in functions.iter().enumerate() {
        let base = BASE + g as u64 * SPAN;
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(lifter.sleigh_regs())
            .expect("systemv");
        let (rdi, rax, rsp) = (cc.arg_passing_regs[0], cc.ret_val_regs[0], cc.stack_vn);
        let cfg = lifter
            .build_cfg(base.into(), &opts.cfg, opts.per_address_ccs)
            .unwrap_or_else(|e| panic!("seed {g}: cfg: {e:#}\n{blocks:?}"));
        let starts: BTreeSet<u64> = cfg
            .regions()
            .map(|r| r.start_addr.machine_addr.addr)
            .collect();
        let succs = successors(blocks);
        let reach = graph_oracle::reachable(&succs, &[0], None);
        assert_eq!(
            starts,
            region_starts(blocks, base, &reach),
            "seed {g}: region starts\n{blocks:?}"
        );
        shapes.tally(blocks, &succs, &reach);
        shapes.idom_after += u64::from(idom_after(&cfg));
        let preds = graph_oracle::preds_of(&succs);
        // The entry also has the call edge in.
        let is_join = |p: usize| {
            let from: BTreeSet<u32> = preds[p]
                .iter()
                .copied()
                .filter(|&q| reach[q as usize])
                .collect();
            from.len() + usize::from(p == 0) >= 2
        };
        let f = lifter
            .build_ir_with(&cfg, cc, &opts)
            .unwrap_or_else(|e| panic!("seed {g}: lift: {e:#}\n{blocks:?}"))
            .function;
        if let Err(e) = strider_ir::validate::validate(&f) {
            panic!("seed {g}: invalid IR: {e:?}\n{blocks:?}");
        }
        shapes.with_phi += u64::from(f.has_kind(|k| matches!(k, NodeKind::Phi)));
        let slot = f
            .ret_val_regs()
            .iter()
            .position(|&v| v == rax)
            .expect("rax is returned");
        for (k, &edi) in EDIS.iter().enumerate() {
            let rax0 = interp::mix((g as u64) ^ ((k as u64) << 32));
            let entry = |e: EntryVar| -> u128 {
                if e.vn == rdi {
                    u128::from(edi)
                } else if e.vn == rax {
                    u128::from(rax0)
                } else if e.vn == rsp {
                    STACK
                } else {
                    0
                }
            };
            let env = Env {
                entry: &entry,
                rom: None,
                seed: g as u64,
                step_budget: 4096,
                private: None,
            };
            let got = interp::run(&f, &env)
                .unwrap_or_else(|e| panic!("seed {g}, edi {edi:#x}: {e:?}\n{blocks:?}"));
            let mut path = Vec::new();
            match (simulate(blocks, edi, rax0, &mut path), got) {
                (Some(want), Outcome::Return { values, .. }) => {
                    assert_eq!(
                        values[slot],
                        Slot::Known(u128::from(want)),
                        "seed {g}, edi {edi:#x}\n{blocks:?}"
                    );
                    returned += 1;
                    shapes.joined_returns += u64::from(path.iter().any(|&p| is_join(p)));
                }
                (None, Outcome::Diverge(Divergence::StepBudget)) => diverged += 1,
                (want, got) => {
                    panic!("seed {g}, edi {edi:#x}: want {want:?}, got {got:?}\n{blocks:?}")
                }
            }
        }
    }
    assert!(
        returned >= CASES && diverged > 0 && shapes.rarest() >= (CASES / 32).max(1),
        "too few runs compared: {returned} returned, {diverged} diverged, {shapes:?}"
    );
}
