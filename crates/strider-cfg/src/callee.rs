//! What a direct call's callee does to its caller beyond a calling
//! convention's defaults, read off the callee's own decoded code. Only on an
//! ISA whose call pushes the return address, where a return pops it.

use rustc_hash::FxHashMap;

use crate::types::{Region, RegionTerminator};
use crate::{Builder, Cfg, CfgOptions};

/// A callee's effect on its caller's frame and registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CalleeEffect {
    /// What each of the callee's returns adds to SP, its return address
    /// included: 4 for an i386 `ret`, 8 for `ret $4`. `None` unless every
    /// return is decoded, reads the same, and no path leaves the callee
    /// another way (a tail call, an unresolved indirect branch).
    pub stack_pop: Option<i64>,
    /// The register the callee copies its return address into and returns
    /// (`__x86.get_pc_thunk.bx`: `mov (%esp),%ebx; ret`).
    pub return_address_reg: Option<rsleigh::Vn>,
}

/// One [`CalleeEffect`] per distinct direct call target in `regions` that
/// yields any, each read off a CFG of the callee built on `sleigh`. A callee
/// that does not build yields none.
pub(crate) fn callee_effects<'r, R: rsleigh::MemReader>(
    arch: &strider_target::SleighArch,
    sleigh: &mut rsleigh::Sleigh<R>,
    user_op_names: Option<&[String]>,
    regions: impl Iterator<Item = &'r Region>,
) -> FxHashMap<u64, CalleeEffect> {
    let code_space = sleigh.default_code_space();
    let mut targets: Vec<u64> = regions
        .flat_map(|region| &region.insns)
        .filter(|wrapped| wrapped.insn.opcode == rsleigh::Opcode::Call)
        .filter_map(|wrapped| wrapped.insn.inputs.first())
        .filter(|target| target.addr_space == code_space)
        .map(|target| target.addr_off)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let opts = CfgOptions::default();
    let mut effects = FxHashMap::default();
    for target in targets {
        let mut callee = Builder::for_arch(arch, sleigh, target, &opts);
        callee.probe_callees = false;
        callee.user_op_names = user_op_names.map(std::borrow::Cow::Borrowed);
        let Ok(cfg) = callee.build() else { continue };
        let effect = CalleeEffect {
            stack_pop: stack_pop(&cfg),
            return_address_reg: return_address_reg(&cfg),
        };
        if effect.stack_pop.is_some() || effect.return_address_reg.is_some() {
            effects.insert(target, effect);
        }
    }
    effects
}

/// The p-code ops of the machine instruction ending `region`.
fn last_instruction(region: &Region) -> impl Iterator<Item = &rsleigh::Insn> {
    let last = region.insns.last().map(|w| w.addr.machine_addr);
    region
        .insns
        .iter()
        .filter(move |w| Some(w.addr.machine_addr) == last)
        .map(|w| &w.insn)
}

/// `true` for `Load _, ram, sp`.
fn loads_top_of_stack(insn: &rsleigh::Insn, sp: rsleigh::Vn, cfg: &Cfg) -> bool {
    insn.opcode == rsleigh::Opcode::Load
        && insn.inputs.len() == 2
        && cfg.space_ids.resolve(insn.inputs[0]) == Some(rsleigh::VnSpace::RAM)
        && insn.inputs[1] == sp
}

/// The SP a return instruction pops through and what it adds to it, for the
/// exact shape `Load t, ram, sp; (IntAdd sp, sp, const)*; Return t`.
fn return_pop<'i>(
    mut ops: impl Iterator<Item = &'i rsleigh::Insn>,
    cfg: &Cfg,
) -> Option<(rsleigh::Vn, i64)> {
    let load = ops.next()?;
    let sp = *load.inputs.get(1)?;
    if sp.addr_space != rsleigh::VnSpace::REGISTER || !loads_top_of_stack(load, sp, cfg) {
        return None;
    }
    let target = load.output?;
    let mut pop: i64 = 0;
    while let Some(op) = ops.next() {
        match op.opcode {
            rsleigh::Opcode::IntAdd
                if op.output == Some(sp)
                    && op.inputs.len() == 2
                    && op.inputs[0] == sp
                    && op.inputs[1].addr_space == rsleigh::VnSpace::CONST
                    && op.inputs[1].addr_off <= u64::from(u16::MAX) =>
            {
                pop += op.inputs[1].addr_off as i64;
            }
            rsleigh::Opcode::Return if op.inputs.first() == Some(&target) => {
                return ops.next().is_none().then_some((sp, pop));
            }
            _ => return None,
        }
    }
    None
}

fn stack_pop(cfg: &Cfg) -> Option<i64> {
    if !cfg.interior_branch_targets.is_empty() {
        return None;
    }
    let mut pop = None;
    for region in cfg.regions() {
        match region.terminator {
            RegionTerminator::Return => {
                let (_, this) = return_pop(last_instruction(region), cfg)?;
                if pop.is_some_and(|p| p != this) {
                    return None;
                }
                pop = Some(this);
            }
            RegionTerminator::Unconditional
            | RegionTerminator::CondBranch { .. }
            | RegionTerminator::NoReturn => {}
            RegionTerminator::TailCall { .. }
            | RegionTerminator::Switch { .. }
            | RegionTerminator::UnresolvedIndirectBranch { .. } => return None,
        }
    }
    pop
}

/// The whole callee is `reg = *sp; return` with a plain pointer-width pop.
fn return_address_reg(cfg: &Cfg) -> Option<rsleigh::Vn> {
    let mut regions = cfg.regions();
    let region = regions.next()?;
    if regions.next().is_some() || region.terminator != RegionTerminator::Return {
        return None;
    }
    let first = region.insns.first()?.addr.machine_addr;
    let split = region
        .insns
        .iter()
        .position(|w| w.addr.machine_addr != first)?;
    let (head, ret) = region.insns.split_at(split);
    if ret
        .iter()
        .any(|w| w.addr.machine_addr != ret[0].addr.machine_addr)
    {
        return None;
    }
    let (sp, pop) = return_pop(ret.iter().map(|w| &w.insn), cfg)?;
    if pop != i64::from(sp.size) {
        return None;
    }
    let reg = match head {
        [load] if loads_top_of_stack(&load.insn, sp, cfg) => load.insn.output?,
        [load, copy]
            if loads_top_of_stack(&load.insn, sp, cfg)
                && copy.insn.opcode == rsleigh::Opcode::Copy
                && copy.insn.inputs.len() == 1
                && Some(copy.insn.inputs[0]) == load.insn.output =>
        {
            copy.insn.output?
        }
        _ => return None,
    };
    let disjoint_from_sp = reg.addr_off + u64::from(reg.size) <= sp.addr_off
        || sp.addr_off + u64::from(sp.size) <= reg.addr_off;
    (reg.addr_space == rsleigh::VnSpace::REGISTER && reg.size == sp.size && disjoint_from_sp)
        .then_some(reg)
}
