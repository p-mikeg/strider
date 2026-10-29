//! Random x86-64 control flow, lifted, validated and run on the reference
//! interpreter against a direct simulation of the generated code.
//!
//! A branch may land on a block's first instruction, on its terminator past
//! the body, or on the `jmp` a `jz` falls through to. Landing past a body
//! splits the block's region, and the head takes a later `RegionId` than the
//! tail it dominates, so ascending region order stops being a dominator
//! preorder. A `jz` onto its own fall-through is a true multi-edge.
//! Conditions test `edi` or a register the bodies change, so a loop can exit
//! after some iterations and return what its phis carried.

use std::collections::{BTreeSet, HashSet};
use std::ops::Range;

use strider_ir::node::{NodeKind, ValueId};
use strider_ir::walk::NodeIdSet;
use strider_ir::{Function, IRViewer};
use strider_ir_test_utils::interp::{self, Divergence, EntryVar, Env, Outcome, Slot};
use strider_ir_test_utils::{Rng, graph_oracle};

const BASE: u64 = 0x1000;
/// Bytes per generated function.
const SPAN: u64 = 0x1000;
/// Bytes per block: the longest block is 23.
const STRIDE: u64 = 32;
const MAX_BLOCKS: u64 = 12;
/// Bytes of `test r32, imm32; jz rel32`, after which a `Cond` falls through.
const TEST_JZ_LEN: u64 = 12;
const STACK: u128 = 0x7fff_1000;
const CASES: u64 = if cfg!(debug_assertions) { 192 } else { 4096 };
const EDIS: [u32; 4] = [0, 0x5a, 0xa5, 0xff];
/// Program points a simulated run may pass before the simulation gives up.
const STEP_CAP: usize = 256;
/// A program point costs the interpreter at most a `Region`, an `If` and an
/// exit-free sink's `If`, so every run the simulation finishes fits.
const STEP_BUDGET: usize = 4 * STEP_CAP;
/// A region spans at most two points, so an interpreter run that returns
/// within this many control nodes is one the simulation would have finished.
const CAPPED_BUDGET: usize = STEP_CAP / 2;
/// The fewest points an interpreter run that spends all of [`CAPPED_BUDGET`]
/// has passed, since a point costs at most three control nodes.
const CAPPED_POINTS: usize = CAPPED_BUDGET / 3;

#[derive(Clone, Copy, Debug)]
enum Op {
    MovEax(u32),
    AddEax(u32),
    AddEaxEcx,
    MovEcxEax,
    AddEcx(u32),
}

impl Op {
    fn bytes(self) -> Vec<u8> {
        match self {
            Op::MovEax(k) => [&[0xb8][..], &k.to_le_bytes()].concat(),
            Op::AddEax(k) => [&[0x05][..], &k.to_le_bytes()].concat(),
            Op::AddEaxEcx => vec![0x01, 0xc8],
            Op::MovEcxEax => vec![0x89, 0xc1],
            Op::AddEcx(k) => [&[0x81, 0xc1][..], &k.to_le_bytes()].concat(),
        }
    }

    fn writes(self) -> Reg {
        match self {
            Op::MovEax(_) | Op::AddEax(_) | Op::AddEaxEcx => Reg::Eax,
            Op::MovEcxEax | Op::AddEcx(_) => Reg::Ecx,
        }
    }

    /// Runs as the last point of `path`. A 32-bit write zero-extends.
    fn run(self, rax: &mut Val, rcx: &mut Val, path: &[usize]) {
        let now = path.len() - 1;
        // Whether this op already ran since `origin`.
        let again = |origin: usize| path[origin..now].contains(&path[now]);
        let (eax, ecx) = (rax.bits as u32, rcx.bits as u32);
        match self {
            Op::MovEax(k) => {
                *rax = Val {
                    bits: u64::from(k),
                    origin: now,
                    accumulated: false,
                };
            }
            Op::AddEax(k) => {
                rax.bits = u64::from(eax.wrapping_add(k));
                rax.accumulated |= again(rax.origin);
            }
            Op::AddEaxEcx => {
                let origin = rax.origin.min(rcx.origin);
                *rax = Val {
                    bits: u64::from(eax.wrapping_add(ecx)),
                    origin,
                    accumulated: rax.accumulated || rcx.accumulated || again(origin),
                };
            }
            Op::MovEcxEax => {
                *rcx = Val {
                    bits: u64::from(eax),
                    ..*rax
                }
            }
            Op::AddEcx(k) => {
                rcx.bits = u64::from(ecx.wrapping_add(k));
                rcx.accumulated |= again(rcx.origin);
            }
        }
    }
}

/// A register's value and its dependence chain: the path index the chain
/// starts at (the `mov` of a constant that began it, or the entry), and
/// whether an op of the chain ran at a point the path had already passed
/// since that origin, so the value went round a loop and changed.
#[derive(Clone, Copy, Debug)]
struct Val {
    bits: u64,
    origin: usize,
    accumulated: bool,
}

#[derive(Clone, Copy, Debug)]
enum Reg {
    Eax,
    Ecx,
    Edi,
}

impl Reg {
    /// The ModRM byte of `test reg, imm32`.
    fn modrm(self) -> u8 {
        match self {
            Reg::Eax => 0xc0,
            Reg::Ecx => 0xc1,
            Reg::Edi => 0xc7,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum At {
    /// The first instruction.
    Head,
    /// The terminator, past the body.
    Terminator,
    /// The `jmp` a `Cond`'s `jz` falls through to.
    Tail,
}

#[derive(Clone, Copy, Debug)]
struct To {
    block: usize,
    at: At,
}

#[derive(Clone, Copy, Debug)]
enum Term {
    Ret,
    Jmp(To),
    /// `test reg, mask; jz taken; jmp other`.
    Cond {
        reg: Reg,
        mask: u32,
        taken: To,
        other: To,
    },
}

type Block = (Option<Op>, Term);

fn random_blocks(seed: u64) -> Vec<Block> {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let n = 1 + (rng.next_u64() % MAX_BLOCKS) as usize;
    let to = |rng: &mut Rng, blocks: Range<usize>| To {
        block: blocks.start + (rng.next_u64() % blocks.len() as u64) as usize,
        at: match rng.next_u64() % 8 {
            0 | 1 => At::Terminator,
            2 | 3 => At::Tail,
            _ => At::Head,
        },
    };
    // Sparse definitions leave joins whose phi only the iterated frontier
    // places.
    let empty = if rng.next_u64().is_multiple_of(2) {
        6
    } else {
        1
    };
    (0..n)
        .map(|b| {
            let k = rng.next_u64() as u32;
            let op = match rng.next_u64() % (9 + empty) {
                0 => Some(Op::MovEax(k)),
                1..=3 => Some(Op::AddEax(k)),
                4 | 5 => Some(Op::AddEaxEcx),
                6 => Some(Op::MovEcxEax),
                7 | 8 => Some(Op::AddEcx(k)),
                _ => None,
            };
            let term = match rng.next_u64() % 10 {
                _ if b + 1 == n => Term::Ret,
                0 => Term::Ret,
                1..=3 => Term::Jmp(to(&mut rng, 0..n)),
                // A latch: test what the body wrote, branch back, exit forward.
                4 | 5 if op.is_some() => Term::Cond {
                    reg: op.map_or(Reg::Edi, Op::writes),
                    mask: latch_mask(op, rng.next_u64() as u32 % 4),
                    taken: To {
                        block: if rng.next_u64().is_multiple_of(2) {
                            b
                        } else {
                            (rng.next_u64() % (b as u64 + 1)) as usize
                        },
                        at: At::Head,
                    },
                    other: to(&mut rng, b + 1..n),
                },
                _ => Term::Cond {
                    reg: [Reg::Edi, Reg::Eax, Reg::Ecx][(rng.next_u64() % 3) as usize],
                    mask: 1 << (rng.next_u64() % 4),
                    taken: if rng.next_u64().is_multiple_of(4) {
                        To {
                            block: b,
                            at: At::Tail,
                        }
                    } else {
                        to(&mut rng, 0..n)
                    },
                    other: to(&mut rng, 0..n),
                },
            };
            (op, term)
        })
        .collect()
}

/// The lowest bit the body adds, shifted up by `shift` while it stays in
/// the register: adding `k` takes that bit through both values within
/// `2 << shift` additions, so a loop of the latch alone exits.
fn latch_mask(op: Option<Op>, shift: u32) -> u32 {
    match op {
        Some(Op::AddEax(k) | Op::AddEcx(k)) if k != 0 => {
            let low = k & k.wrapping_neg();
            if low.leading_zeros() >= shift {
                low << shift
            } else {
                low
            }
        }
        _ => 1 << shift,
    }
}

/// Program points: `3b` is block `b`'s first instruction, `3b + 1` its
/// terminator past a body, `3b + 2` a `Cond`'s trailing `jmp`. Past an empty
/// body is the first instruction; a `Tail` without a `Cond` is the terminator.
fn point(blocks: &[Block], to: To) -> usize {
    let (op, term) = blocks[to.block];
    let head = 3 * to.block;
    match to.at {
        At::Head => head,
        At::Tail if matches!(term, Term::Cond { .. }) => head + 2,
        At::Terminator | At::Tail => head + usize::from(op.is_some()),
    }
}

fn terminator_point(blocks: &[Block], b: usize) -> usize {
    3 * b + usize::from(blocks[b].0.is_some())
}

fn point_addr(blocks: &[Block], base: u64, p: usize) -> u64 {
    let body = blocks[p / 3].0.map_or(0, |op| op.bytes().len() as u64);
    let offset = match p % 3 {
        0 => 0,
        1 => body,
        _ => body + TEST_JZ_LEN,
    };
    base + (p / 3) as u64 * STRIDE + offset
}

/// `rel32` from the end of an instruction of `len` bytes starting at `at`.
fn rel32(at: u64, len: u64, target: u64) -> [u8; 4] {
    ((target as i64 - (at + len) as i64) as i32).to_le_bytes()
}

fn assemble(blocks: &[Block], base: u64) -> Vec<u8> {
    let addr = |t: To| point_addr(blocks, base, point(blocks, t));
    let mut out = vec![0xcc; SPAN as usize];
    for (b, &(op, term)) in blocks.iter().enumerate() {
        let start = point_addr(blocks, base, 3 * b);
        let mut code = op.map_or_else(Vec::new, Op::bytes);
        match term {
            Term::Ret => code.push(0xc3),
            Term::Jmp(t) => {
                let at = start + code.len() as u64;
                code.push(0xe9);
                code.extend(rel32(at, 5, addr(t)));
            }
            Term::Cond {
                reg,
                mask,
                taken,
                other,
            } => {
                code.extend([0xf7, reg.modrm()]);
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

#[derive(Debug)]
enum Run {
    /// `rax` at the `ret`; whether its dependence chain crossed a back edge
    /// (the path from where the chain starts repeats a point); whether the
    /// value changed going round.
    Returns {
        rax: u64,
        carried: bool,
        accumulated: bool,
    },
    /// Came back to a state it had been in: a loop forever.
    Loops,
    /// Passed [`STEP_CAP`] points without returning or repeating a state.
    TooLong,
}

/// What the simulation saw: how the run ended, the points it passed and
/// whether each `jz` was taken, in order, with the path index it ran at.
struct Sim {
    run: Run,
    path: Vec<usize>,
    taken: Vec<bool>,
    taken_at: Vec<usize>,
}

/// The run from the entry, for up to [`STEP_CAP`] points. A state is the
/// point and the registers (the flags are set before every use); a run that
/// repeats one is [`Run::Loops`], but goes on to the cap so `taken` is long.
fn simulate(blocks: &[Block], edi: u32, rax: u64, rcx: u64) -> Sim {
    let entry = |bits| Val {
        bits,
        origin: 0,
        accumulated: false,
    };
    let (mut rax, mut rcx) = (entry(rax), entry(rcx));
    let (mut path, mut taken, mut taken_at) = (Vec::new(), Vec::new(), Vec::new());
    let mut seen = HashSet::new();
    let mut loops = false;
    let mut at = 0;
    while path.len() < STEP_CAP {
        loops |= !seen.insert((at, rax.bits, rcx.bits));
        path.push(at);
        let (op, term) = blocks[at / 3];
        if let (0, Some(op)) = (at % 3, op) {
            op.run(&mut rax, &mut rcx, &path);
            at += 1;
            continue;
        }
        let to = match term {
            Term::Ret => {
                let mut chain = path[rax.origin..].to_vec();
                chain.sort_unstable();
                chain.dedup();
                let carried = chain.len() < path.len() - rax.origin;
                let run = Run::Returns {
                    rax: rax.bits,
                    carried,
                    accumulated: rax.accumulated,
                };
                return Sim {
                    run,
                    path,
                    taken,
                    taken_at,
                };
            }
            Term::Jmp(to) => to,
            Term::Cond { other, .. } if at % 3 == 2 => other,
            Term::Cond {
                reg,
                mask,
                taken: to,
                ..
            } => {
                let value = match reg {
                    Reg::Eax => rax.bits as u32,
                    Reg::Ecx => rcx.bits as u32,
                    Reg::Edi => edi,
                };
                taken.push(value & mask == 0);
                taken_at.push(path.len() - 1);
                if value & mask == 0 {
                    to
                } else {
                    To {
                        block: at / 3,
                        at: At::Tail,
                    }
                }
            }
        };
        at = point(blocks, to);
    }
    let run = if loops { Run::Loops } else { Run::TooLong };
    Sim {
        run,
        path,
        taken,
        taken_at,
    }
}

/// Each program point's successors: a body falls through to its terminator,
/// a `jz` lists `taken` then its fall-through.
fn successors(blocks: &[Block]) -> Vec<Vec<u32>> {
    let mut succs = vec![Vec::new(); 3 * blocks.len()];
    let target = |to| point(blocks, to) as u32;
    for (b, &(op, term)) in blocks.iter().enumerate() {
        let t = terminator_point(blocks, b);
        if op.is_some() {
            succs[3 * b].push(t as u32);
        }
        succs[t] = match term {
            Term::Ret => Vec::new(),
            Term::Jmp(to) => vec![target(to)],
            Term::Cond { taken, other, .. } => {
                succs[3 * b + 2] = vec![target(other)];
                vec![target(taken), 3 * b as u32 + 2]
            }
        };
    }
    succs
}

/// The regions the CFG must build, each as the points it runs through. A
/// region starts at the entry and at each successor of a reachable branch,
/// and runs on through a body's fall-through.
fn oracle_regions(blocks: &[Block], succs: &[Vec<u32>], reach: &[bool]) -> Vec<Vec<usize>> {
    let branches: Vec<bool> = (0..succs.len())
        .map(|p| p % 3 == 2 || p == terminator_point(blocks, p / 3))
        .collect();
    let mut starts = BTreeSet::from([0]);
    for p in (0..succs.len()).filter(|&p| reach[p] && branches[p]) {
        starts.extend(succs[p].iter().map(|&q| q as usize));
    }
    starts
        .iter()
        .map(|&s| {
            let mut run = vec![s];
            let mut p = s;
            while !branches[p] && !starts.contains(&(succs[p][0] as usize)) {
                p = succs[p][0] as usize;
                run.push(p);
            }
            run
        })
        .collect()
}

/// The CFG's regions as an index graph: successors per region, one per edge,
/// the entry's index, and each region's start address.
fn region_graph(cfg: &strider_cfg::Cfg) -> (Vec<Vec<u32>>, u32, Vec<u64>) {
    let ids: Vec<strider_cfg::RegionId> = cfg.region_ids().collect();
    let index = |r| ids.iter().position(|&x| x == r).expect("a region") as u32;
    let succs = ids
        .iter()
        .map(|&r| cfg.region_graph().neighbors(r).map(index).collect())
        .collect();
    let starts = ids
        .iter()
        .map(|&r| cfg.region_graph()[r].start_addr.machine_addr.addr)
        .collect();
    (succs, index(cfg.entry()), starts)
}

/// The nodes value `v` is computed from, not looking past a `Region`.
fn cone(f: &Function, v: ValueId) -> NodeIdSet {
    let mut seen = NodeIdSet::new();
    let mut stack = vec![f.producer(v)];
    while let Some(n) = stack.pop() {
        if seen.insert(n) && !matches!(f.node_kind(n), NodeKind::Region) {
            stack.extend(f.node_inputs(n).into_iter().map(|i| f.producer(i)));
        }
    }
    seen
}

/// Whether `v` is computed through a value `Phi`.
fn through_phi(f: &Function, v: ValueId) -> bool {
    cone(f, v)
        .iter()
        .any(|n| matches!(f.node_kind(n), NodeKind::Phi))
}

/// Counts that keep the draws honest: region-graph shapes per draw, then
/// runs by outcome and by what the compared value went through.
#[derive(Debug, Default)]
struct Tally {
    cyclic: u64,
    irreducible: u64,
    self_loop: u64,
    multi_edge: u64,
    /// A multi-edge whose target has another predecessor too (the entry's is
    /// its caller).
    shared_multi_edge: u64,
    back_to_entry: u64,
    exit_free: u64,
    /// Some region's immediate dominator has the larger `RegionId`.
    idom_after: u64,
    /// `eax` needs a phi that only the iterated dominance frontier places.
    deep_eax_phi: u64,
    returned: u64,
    diverged: u64,
    /// Runs the simulation gave up on; the interpreter must not return
    /// sooner.
    capped: u64,
    /// Returning runs that took a back edge.
    loop_returns: u64,
    /// Returning runs whose `rax` is computed through a value `Phi`.
    phi_returns: u64,
    /// Returning runs whose `rax` chain crossed a back edge.
    carried_returns: u64,
    /// ... and changed going round.
    accumulated_returns: u64,
    /// `jz` outcomes compared with the interpreter's `If` conditions.
    branches_compared: u64,
    /// ... of them in runs that repeat a state.
    branches_in_looping_runs: u64,
    /// ... of them in runs the simulation gave up on.
    branches_in_capped_runs: u64,
}

impl Tally {
    fn shapes(&mut self, succs: &[Vec<u32>], root: u32, eax_defs: &[u32], ids_after: bool) {
        let n = succs.len() as u32;
        let any = |pred: &dyn Fn(u32) -> bool| u64::from((0..n).any(pred));
        let preds = graph_oracle::preds_of(succs);
        let dup = |v: u32| {
            let s = &succs[v as usize];
            (0..s.len())
                .find(|&i| s[i + 1..].contains(&s[i]))
                .map(|i| s[i])
        };
        let returns = |v: u32| {
            let from = graph_oracle::reachable(succs, &[v], None);
            (0..succs.len()).any(|w| from[w] && succs[w].is_empty())
        };
        self.cyclic += any(&|v| graph_oracle::reaches(succs, v, v));
        self.irreducible += u64::from(!graph_oracle::is_reducible(succs, root));
        self.self_loop += any(&|v| succs[v as usize].contains(&v));
        self.multi_edge += any(&|v| dup(v).is_some());
        self.shared_multi_edge += any(&|v| {
            dup(v).is_some_and(|t| preds[t as usize].iter().any(|&p| p != v) || t == root)
        });
        self.back_to_entry += any(&|v| succs[v as usize].contains(&root));
        self.exit_free += any(&|v| !returns(v));
        self.idom_after += u64::from(ids_after);
        let df = graph_oracle::frontiers(succs, &graph_oracle::dom_matrix(succs, root), root);
        let one_level: BTreeSet<u32> = eax_defs
            .iter()
            .flat_map(|&d| df[d as usize].iter().copied())
            .collect();
        self.deep_eax_phi += u64::from(graph_oracle::iterated_frontier(&df, eax_defs) != one_level);
    }

    /// The counts that must each reach the floor.
    fn floored(&self) -> [u64; 15] {
        [
            self.cyclic,
            self.irreducible,
            self.self_loop,
            self.multi_edge,
            self.shared_multi_edge,
            self.back_to_entry,
            self.exit_free,
            self.idom_after,
            self.deep_eax_phi,
            self.loop_returns,
            self.phi_returns,
            self.carried_returns,
            self.accumulated_returns,
            self.branches_in_looping_runs,
            self.branches_in_capped_runs,
        ]
    }
}

#[test]
fn random_control_flow_lifts_to_what_the_code_computes() {
    let functions: Vec<Vec<Block>> = (0..CASES).map(random_blocks).collect();
    let image: Vec<u8> = functions
        .iter()
        .enumerate()
        .flat_map(|(g, blocks)| assemble(blocks, BASE + g as u64 * SPAN))
        .collect();
    let arch = strider_target::SleighArch::x86_64();
    let mut lifter =
        strider_lift::lift::Lifter::new(arch, rsleigh::mem_readers::BufMemReader::new(image, BASE))
            .expect("lifter");
    let mut tally = Tally::default();
    for (g, blocks) in functions.iter().enumerate() {
        let base = BASE + g as u64 * SPAN;
        let cc = strider_target::CallingConvention::x86_64_systemv()
            .build(lifter.sleigh_regs())
            .expect("systemv");
        // RCX is the fourth SysV argument register.
        let (rdi, rcx) = (cc.arg_passing_regs[0], cc.arg_passing_regs[3]);
        let (rax, rsp) = (cc.ret_val_regs[0], cc.stack_ptr);
        let cfg = lifter
            .build_cfg(base.into(), &strider_cfg::CfgOptions::default())
            .unwrap_or_else(|e| panic!("seed {g}: cfg: {e:#}\n{blocks:?}"));
        let succs = successors(blocks);
        let reach = graph_oracle::reachable(&succs, &[0], None);
        let (rsuccs, root, starts) = region_graph(&cfg);
        let mut edges: Vec<(u64, u64)> = rsuccs
            .iter()
            .enumerate()
            .flat_map(|(v, ss)| ss.iter().map(move |&w| (v, w as usize)))
            .map(|(v, w)| (starts[v], starts[w]))
            .collect();
        edges.sort_unstable();
        let runs = oracle_regions(blocks, &succs, &reach);
        let addr = |p: usize| point_addr(blocks, base, p);
        let mut want: Vec<(u64, u64)> = runs
            .iter()
            .flat_map(|run| {
                let last = run[run.len() - 1];
                succs[last]
                    .iter()
                    .map(move |&q| (addr(run[0]), addr(q as usize)))
            })
            .collect();
        want.sort_unstable();
        assert_eq!(edges, want, "seed {g}: region graph\n{blocks:?}");
        // Regions writing eax, and the entry, which defines everything.
        let eax_defs: Vec<u32> = (0..rsuccs.len() as u32)
            .filter(|&v| {
                let run = runs.iter().find(|run| addr(run[0]) == starts[v as usize]);
                run.expect("a region the oracle builds").iter().any(|&p| {
                    let op = blocks[p / 3].0.filter(|_| p % 3 == 0);
                    op.is_some_and(|op| matches!(op.writes(), Reg::Eax))
                })
            })
            .chain([root])
            .collect();
        let idom = graph_oracle::idoms(&graph_oracle::dom_matrix(&rsuccs, root), root);
        let ids: Vec<strider_cfg::RegionId> = cfg.region_ids().collect();
        let ids_after = (0..ids.len()).any(|v| idom[v].is_some_and(|d| ids[d as usize] > ids[v]));
        tally.shapes(&rsuccs, root, &eax_defs, ids_after);

        let f = lifter
            .build_ir(&cfg, cc)
            .unwrap_or_else(|e| panic!("seed {g}: lift: {e:#}\n{blocks:?}"))
            .function;
        if let Err(e) = strider_ir::validate::validate(&f) {
            panic!("seed {g}: invalid IR: {e:?}\n{blocks:?}");
        }
        let slot = f
            .ret_val_regs()
            .iter()
            .position(|&v| v == rax)
            .expect("rax is returned");
        for (k, &edi) in EDIS.iter().enumerate() {
            let rax0 = interp::mix((g as u64) ^ ((k as u64) << 32));
            let rcx0 = interp::mix(rax0);
            let sim = simulate(blocks, edi, rax0, rcx0);
            let entry = |e: EntryVar| -> u128 {
                match e.vn {
                    v if v == rdi => u128::from(edi),
                    v if v == rax => u128::from(rax0),
                    v if v == rcx => u128::from(rcx0),
                    v if v == rsp => STACK,
                    _ => 0,
                }
            };
            let env = Env {
                entry: &entry,
                rom: None,
                seed: g as u64,
                step_budget: if matches!(sim.run, Run::TooLong) {
                    CAPPED_BUDGET
                } else {
                    STEP_BUDGET
                },
                private: None,
            };
            let mut ret = None;
            let mut ifs = Vec::new();
            let got = interp::run_observed(&f, &env, &mut |node, v, c| match f.node_kind(node) {
                NodeKind::Return => ret = Some(node),
                // An exit-free sink's `If` tests a constant.
                NodeKind::If
                    if v == f.node_inputs(node)[1]
                        && !matches!(f.node_kind(f.producer(v)), NodeKind::IntConst(_)) =>
                {
                    ifs.push(c != 0);
                }
                _ => {}
            })
            .unwrap_or_else(|e| panic!("seed {g}, edi {edi:#x}: {e:?}\n{blocks:?}"));
            match (&sim.run, got) {
                (
                    &Run::Returns {
                        rax: want,
                        carried,
                        accumulated,
                    },
                    Outcome::Return { values, .. },
                ) => {
                    assert_eq!(
                        values[slot],
                        Slot::Known(u128::from(want)),
                        "seed {g}, edi {edi:#x}\n{blocks:?}"
                    );
                    tally.returned += 1;
                    let mut points = sim.path.clone();
                    points.sort_unstable();
                    points.dedup();
                    tally.loop_returns += u64::from(points.len() < sim.path.len());
                    tally.carried_returns += u64::from(carried);
                    tally.accumulated_returns += u64::from(accumulated);
                    // A `Return`'s inputs are control, memory, then its slots.
                    let ret = ret.expect("a returned value is observed");
                    tally.phi_returns += u64::from(through_phi(&f, f.node_inputs(ret)[2 + slot]));
                }
                (Run::Loops, Outcome::Diverge(Divergence::StepBudget)) => tally.diverged += 1,
                (Run::TooLong, Outcome::Diverge(Divergence::StepBudget)) => tally.capped += 1,
                (want, got) => {
                    panic!("seed {g}, edi {edi:#x}: want {want:?}, got {got:?}\n{blocks:?}")
                }
            }
            // Each `jz` against the `If` the interpreter took for it, as far
            // as both ran: a looping run's budget covers every point the
            // simulation passed, a capped one's the first `CAPPED_POINTS`.
            let n = sim.taken.len().min(ifs.len());
            let need = match sim.run {
                Run::Returns { .. } | Run::Loops => sim.taken.len(),
                Run::TooLong => sim.taken_at.partition_point(|&i| i < CAPPED_POINTS),
            };
            let lengths = match sim.run {
                Run::Returns { .. } => ifs.len() == need,
                Run::Loops | Run::TooLong => ifs.len() >= need,
            };
            let first = (0..n).find(|&k| ifs[k] != sim.taken[k]);
            assert!(
                first.is_none() && lengths,
                "seed {g}, edi {edi:#x}: {:?}: first differing jz {first:?}; {} Ifs for {} jz, \
                 {need} needed\n{blocks:?}",
                sim.run,
                ifs.len(),
                sim.taken.len()
            );
            tally.branches_compared += n as u64;
            match sim.run {
                Run::Returns { .. } => {}
                Run::Loops => tally.branches_in_looping_runs += n as u64,
                Run::TooLong => tally.branches_in_capped_runs += n as u64,
            }
        }
    }
    let floor = (CASES / 64).max(4);
    assert!(
        tally.returned >= CASES
            && tally.diverged > 0
            && tally.floored().iter().all(|&c| c >= floor),
        "too few draws of some kind (floor {floor}): {tally:#?}"
    );
}
