//! The optimiser must not change what a function computes.
//!
//! Structural diffing cannot answer that: the pipeline rewrites through
//! algebraic identities, so raw and optimised graphs for the same code look
//! nothing alike. This runs both instead. A concrete environment (one value
//! per entry varnode, one byte oracle for unwritten memory) is fed to the raw
//! graph and to the optimised one, and every return slot plus every memory
//! byte the function writes has to come out the same.
//!
//! The evaluator covers acyclic control flow over integer dataflow and RAM
//! loads and stores; a function reaching anything else (a loop, a call, a
//! Sleigh user-op) is reported as skipped rather than silently passing, and
//! the tests assert a floor on how many were really compared.

mod common;

use common::{Arch, analyze as optimized, lift_for_pipeline};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use strider_ir::node::{NodeId, NodeKind, ValueId};
use strider_ir::{ExtendOp, Function, IRViewer, IntBinaryOp, IntCmpOp};
use strider_target::Endianness;

/// Where the stack pointer is planted, and how wide a window either side of it
/// counts as the callee's own frame. A spill there dies with the frame, so
/// deleting one preserves semantics and the window is left out of the memory
/// comparison. Anything written through a pointer argument lands outside it.
const STACK_BASE: u64 = 0x7fff_0000_1000;
const FRAME_HALF_WIDTH: u64 = 0x1_0000;

/// Splitmix64, so raw and optimised agree on every entry value without
/// carrying a table between them.
fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// The byte at `addr` before the function writes anything.
fn initial_byte(addr: u64) -> u8 {
    (mix(addr ^ 0xa5a5_a5a5_a5a5_a5a5) >> 24) as u8
}

fn entry_value(vn: rsleigh::Vn, stack_vn: rsleigh::Vn) -> u128 {
    if vn == stack_vn {
        return u128::from(STACK_BASE);
    }
    let seed = mix(vn.addr_off.rotate_left(17)) ^ mix(u64::from(vn.size) << 40);
    u128::from(seed) | (u128::from(mix(seed)) << 64)
}

/// Why a function could not be run.
#[derive(Debug, PartialEq, Eq)]
struct Unsupported(String);

type Eval<T> = Result<T, Unsupported>;

fn unsupported<T>(what: impl std::fmt::Display) -> Eval<T> {
    Err(Unsupported(what.to_string()))
}

fn mask(v: u128, width: usize) -> u128 {
    if width >= 128 {
        v
    } else {
        v & ((1u128 << width) - 1)
    }
}

fn sext(v: u128, width: usize) -> i128 {
    let v = mask(v, width);
    if width < 128 && v >> (width - 1) == 1 {
        (v as i128).wrapping_sub(1i128 << width)
    } else {
        v as i128
    }
}

/// Bytes a function wrote, keyed by address. Absent means the byte oracle
/// still answers.
type Mem = BTreeMap<u64, u8>;

struct Machine<'f> {
    f: &'f Function,
    stack_vn: rsleigh::Vn,
    /// Which of a `Region`'s control inputs the run arrived on.
    taken: FxHashMap<NodeId, usize>,
    values: FxHashMap<ValueId, u128>,
    mems: FxHashMap<ValueId, Mem>,
}

impl<'f> Machine<'f> {
    fn new(f: &'f Function, stack_vn: rsleigh::Vn) -> Self {
        Self {
            f,
            stack_vn,
            taken: FxHashMap::default(),
            values: FxHashMap::default(),
            mems: FxHashMap::default(),
        }
    }

    fn kind(&self, node: NodeId) -> &NodeKind {
        self.f.node_kind(node)
    }

    fn inputs(&self, node: NodeId) -> Vec<ValueId> {
        self.f.node_inputs(node).into_iter().collect()
    }

    /// The one node a control value flows into, and the input slot it lands in.
    fn sole_use(&self, value: ValueId) -> Eval<(NodeId, usize)> {
        let mut uses = self.f.graph().value_uses(value);
        let Some((node, slot)) = uses.next() else {
            return unsupported("a control value with no consumer");
        };
        if uses.next().is_some() {
            return unsupported("a control value with several consumers");
        }
        Ok((node, slot as usize))
    }

    /// Walks control from `Entry` to the `Return`, recording which edge each
    /// `Region` was entered on. Refuses a repeated `Region`, which is a loop.
    fn run_control(&mut self) -> Eval<NodeId> {
        let entry = self.f.entry();
        let mut current = self.f.node_outputs(entry)[0];
        loop {
            let (node, slot) = self.sole_use(current)?;
            match self.kind(node) {
                NodeKind::Return => return Ok(node),
                NodeKind::Region => {
                    if self.taken.insert(node, slot).is_some() {
                        return unsupported("a Region entered twice (a loop)");
                    }
                    current = self.f.node_outputs(node)[0];
                }
                NodeKind::If => {
                    let cond = self.inputs(node)[1];
                    let taken = self.value(cond)? & 1;
                    // Output 0 is the true arm, output 1 the false arm.
                    current = self.f.node_outputs(node)[usize::from(taken == 0)];
                }
                other => return unsupported(format!("{other:?} on the control path")),
            }
        }
    }

    /// The input a phi selects, given the edge its `Region` was entered on.
    fn phi_choice(&self, node: NodeId) -> Eval<ValueId> {
        let inputs = self.inputs(node);
        let region = self.f.producer(inputs[0]);
        let Some(&slot) = self.taken.get(&region) else {
            return unsupported("a phi whose Region the run never entered");
        };
        inputs
            .get(slot + 1)
            .copied()
            .map_or_else(|| unsupported("a phi shorter than its Region"), Ok)
    }

    fn memory(&mut self, value: ValueId) -> Eval<Mem> {
        if let Some(m) = self.mems.get(&value) {
            return Ok(m.clone());
        }
        let node = self.f.producer(value);
        let mem = match *self.kind(node) {
            NodeKind::InitialMemory => Mem::new(),
            NodeKind::MemPhi => {
                let chosen = self.phi_choice(node)?;
                self.memory(chosen)?
            }
            NodeKind::Store(space) => {
                if space != rsleigh::VnSpace::RAM {
                    return unsupported(format!("Store to {space:?}"));
                }
                let [prior, addr, data] = [0, 1, 2].map(|i| self.inputs(node)[i]);
                let mut mem = self.memory(prior)?;
                let at = self.value(addr)? as u64;
                let width = self.width(data)?;
                let bytes = self.value(data)?.to_le_bytes();
                let size = width.div_ceil(8);
                for i in 0..size {
                    let offset = match self.f.endianness() {
                        Endianness::Little => i,
                        Endianness::Big => size - 1 - i,
                    };
                    mem.insert(at.wrapping_add(i as u64), bytes[offset]);
                }
                mem
            }
            other => return unsupported(format!("{other:?} as a memory state")),
        };
        self.mems.insert(value, mem.clone());
        Ok(mem)
    }

    fn width(&self, value: ValueId) -> Eval<usize> {
        match self.f.value_type_opt(value) {
            Some(ty) if ty.is_integer() => Ok(ty.bit_width()),
            other => unsupported(format!("{other:?} where an integer was needed")),
        }
    }

    fn value(&mut self, value: ValueId) -> Eval<u128> {
        if let Some(&v) = self.values.get(&value) {
            return Ok(v);
        }
        let node = self.f.producer(value);
        let width = self.width(value)?;
        let inputs = self.inputs(node);
        let result = match *self.kind(node) {
            NodeKind::IntConst(_) => self
                .f
                .int_const_u128(value)
                .map_or_else(|| unsupported("an unreadable IntConst"), Ok)?,
            NodeKind::InitialVar(id) => {
                let vn = self.f.initial_vn(id);
                mask(entry_value(vn, self.stack_vn), width)
            }
            NodeKind::Phi => {
                let chosen = self.phi_choice(node)?;
                self.value(chosen)?
            }
            NodeKind::Truncate => mask(self.value(inputs[0])?, width),
            NodeKind::Extend(op) => {
                let src_width = self.width(inputs[0])?;
                let src = self.value(inputs[0])?;
                match op {
                    ExtendOp::ZeroExtend => mask(mask(src, src_width), width),
                    ExtendOp::SignExtend => mask(sext(src, src_width) as u128, width),
                }
            }
            NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::Neg) => {
                mask(0u128.wrapping_sub(self.value(inputs[0])?), width)
            }
            NodeKind::Popcount => {
                let src_width = self.width(inputs[0])?;
                u128::from(mask(self.value(inputs[0])?, src_width).count_ones())
            }
            NodeKind::Lzcount => {
                let src_width = self.width(inputs[0])?;
                let src = mask(self.value(inputs[0])?, src_width);
                (src_width - (128 - src.leading_zeros() as usize)) as u128
            }
            NodeKind::IntBinaryOp(op) => {
                let a = mask(self.value(inputs[0])?, width);
                let b = mask(self.value(inputs[1])?, width);
                self.int_binary(op, a, b, width)?
            }
            NodeKind::IntCmpOp(op) => {
                // The lifter equalises the operand widths before the compare,
                // so a mismatch here would mean guessing at the extension.
                let operand_width = self.width(inputs[0])?;
                if operand_width != self.width(inputs[1])? {
                    return unsupported("a comparison of mismatched widths");
                }
                let a = mask(self.value(inputs[0])?, operand_width);
                let b = mask(self.value(inputs[1])?, operand_width);
                u128::from(int_cmp(op, a, b, operand_width))
            }
            NodeKind::Load(space) => {
                if space != rsleigh::VnSpace::RAM {
                    return unsupported(format!("Load from {space:?}"));
                }
                let mem = self.memory(inputs[0])?;
                let at = self.value(inputs[1])? as u64;
                let size = width.div_ceil(8);
                let mut bytes = [0u8; 16];
                for i in 0..size {
                    let addr = at.wrapping_add(i as u64);
                    let byte = mem
                        .get(&addr)
                        .copied()
                        .unwrap_or_else(|| initial_byte(addr));
                    let offset = match self.f.endianness() {
                        Endianness::Little => i,
                        Endianness::Big => size - 1 - i,
                    };
                    bytes[offset] = byte;
                }
                mask(u128::from_le_bytes(bytes), width)
            }
            other => return unsupported(format!("{other:?} as a value")),
        };
        self.values.insert(value, result);
        Ok(result)
    }

    fn int_binary(&self, op: IntBinaryOp, a: u128, b: u128, width: usize) -> Eval<u128> {
        let sa = sext(a, width);
        let sb = sext(b, width);
        let out = match op {
            IntBinaryOp::Add => a.wrapping_add(b),
            IntBinaryOp::Mul => a.wrapping_mul(b),
            IntBinaryOp::And => a & b,
            IntBinaryOp::Or => a | b,
            IntBinaryOp::Xor => a ^ b,
            IntBinaryOp::Div => {
                if b == 0 {
                    return unsupported("an unsigned divide by zero");
                }
                a / b
            }
            IntBinaryOp::Rem => {
                if b == 0 {
                    return unsupported("an unsigned remainder by zero");
                }
                a % b
            }
            IntBinaryOp::Sdiv => {
                if sb == 0 {
                    return unsupported("a signed divide by zero");
                }
                sa.wrapping_div(sb) as u128
            }
            IntBinaryOp::Srem => {
                if sb == 0 {
                    return unsupported("a signed remainder by zero");
                }
                sa.wrapping_rem(sb) as u128
            }
            // P-code clears the result once the count reaches the width.
            IntBinaryOp::ShiftLeft => {
                if b >= width as u128 {
                    0
                } else {
                    a << b
                }
            }
            IntBinaryOp::ShiftRight => {
                if b >= width as u128 {
                    0
                } else {
                    a >> b
                }
            }
            IntBinaryOp::SShiftRight => {
                if b >= width as u128 {
                    if sa < 0 { u128::MAX } else { 0 }
                } else {
                    (sa >> b) as u128
                }
            }
        };
        Ok(mask(out, width))
    }
}

fn int_cmp(op: IntCmpOp, a: u128, b: u128, width: usize) -> bool {
    let (sa, sb) = (sext(a, width), sext(b, width));
    match op {
        IntCmpOp::Equal => a == b,
        IntCmpOp::Less => a < b,
        IntCmpOp::Sless => sa < sb,
        IntCmpOp::Carry => mask(a.wrapping_add(b), width) < a,
        IntCmpOp::Scarry => sext(mask(a.wrapping_add(b), width), width) != sa + sb,
        IntCmpOp::Sborrow => sext(mask(a.wrapping_sub(b), width), width) != sa - sb,
    }
}

/// What one graph computed: a value per return slot, and the memory it wrote
/// outside the callee's own frame.
struct Outcome {
    returns: Vec<Option<u128>>,
    writes: Mem,
}

fn run(f: &Function, stack_vn: rsleigh::Vn) -> Eval<Outcome> {
    let mut m = Machine::new(f, stack_vn);
    let ret = m.run_control()?;
    let inputs = m.inputs(ret);
    let mut writes = m.memory(inputs[1])?;
    writes.retain(|&addr, _| addr.abs_diff(STACK_BASE) > FRAME_HALF_WIDTH);
    let mut returns = Vec::new();
    for slot in inputs.into_iter().skip(2) {
        // A float return slot has no integer value; both sides skip it alike.
        returns.push(match f.value_type_opt(slot) {
            Some(ty) if ty.is_integer() => Some(m.value(slot)?),
            _ => None,
        });
    }
    Ok(Outcome { returns, writes })
}

/// `Ok(true)` when both graphs ran and agreed, `Ok(false)` when neither could
/// run, and `Err` on a real divergence.
fn compare(arch: Arch, case: &str, name: &str) -> Result<bool, String> {
    let (outcome, _lifter, cc, _sleigh_arch, _rom) = lift_for_pipeline(arch, case, name);
    let raw = outcome.function;
    let opt = optimized(arch, case, name);
    let stack_vn = cc.stack_vn;
    match (run(&raw, stack_vn), run(&opt, stack_vn)) {
        (Ok(a), Ok(b)) => {
            if a.returns != b.returns {
                return Err(format!(
                    "{}/{case}::{name} returns {:x?} raw vs {:x?} optimised",
                    arch.name(),
                    a.returns,
                    b.returns
                ));
            }
            if a.writes != b.writes {
                return Err(format!(
                    "{}/{case}::{name} wrote {:x?} raw vs {:x?} optimised",
                    arch.name(),
                    a.writes,
                    b.writes
                ));
            }
            Ok(true)
        }
        (Err(_), Err(_)) => Ok(false),
        (Ok(_), Err(Unsupported(why))) => Err(format!(
            "{}/{case}::{name} ran raw but not optimised: {why}",
            arch.name()
        )),
        (Err(Unsupported(why)), Ok(_)) => Err(format!(
            "{}/{case}::{name} ran optimised but not raw: {why}",
            arch.name()
        )),
    }
}

fn sweep(cases: &[(&str, &[&str])], floor: usize) {
    let arches = [
        Arch::X64,
        Arch::Aarch64,
        Arch::Arm,
        Arch::Mips32be,
        Arch::Ppc32be,
    ];
    let mut compared = 0usize;
    let mut failures = Vec::new();
    for &arch in &arches {
        for &(case, names) in cases {
            for &name in names {
                match compare(arch, case, name) {
                    Ok(true) => compared += 1,
                    Ok(false) => {}
                    Err(e) => failures.push(e),
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(
        compared >= floor,
        "only {compared} functions were actually run, expected at least {floor}"
    );
}

/// Straight-line integer arithmetic: the shapes the peephole rules rewrite
/// most aggressively.
#[test]
fn arithmetic_survives_the_pipeline() {
    sweep(
        &[(
            "arithmetic",
            &[
                "add", "sub", "mul", "bit_and", "bit_or", "bit_xor", "bit_not", "shl", "lshr",
                "ashr", "negate", "udiv", "umod", "sdiv", "smod",
            ],
        )],
        71,
    );
}

/// Branches and merges, where the optimiser rewrites conditions and folds
/// phis.
#[test]
fn branching_control_flow_survives_the_pipeline() {
    sweep(
        &[("control", &["abs_val", "max_val", "clamp", "select_three"])],
        20,
    );
}

/// Loads and stores, where load forwarding and store elimination rewire the
/// memory chain.
#[test]
fn memory_effects_survive_the_pipeline() {
    sweep(
        &[(
            "memory",
            &[
                "pointer_chase",
                "struct_field_load",
                "struct_field_store",
                "tagged_union_read",
            ],
        )],
        20,
    );
}

/// A guard against the machinery agreeing because it computed nothing: two
/// different functions must come out different.
#[test]
fn the_comparison_can_tell_two_functions_apart() {
    let (outcome, _lifter, cc, _arch, _rom) = lift_for_pipeline(Arch::X64, "arithmetic", "add");
    let add = run(&outcome.function, cc.stack_vn).expect("add runs");
    let sub = run(&optimized(Arch::X64, "arithmetic", "sub"), cc.stack_vn).expect("sub runs");
    assert_ne!(add.returns, sub.returns);
}
