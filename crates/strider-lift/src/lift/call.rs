use anyhow::Context as _;
use anyhow::{Result, anyhow};

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    /// Lifts a user-op. `terminate` is the CFG's verdict that it ends control
    /// ([`Self::ends_no_return_region`]).
    pub(super) fn handle_call_other(
        &mut self,
        insn: &rsleigh::Insn,
        terminate: bool,
    ) -> Result<()> {
        let (user_op_id, name) = decode_user_op(insn, self.lifter.user_op_names())?;
        if self.call_other_overrides.get(name).is_none()
            && strider_target::call_other_abi::is_msr_register_mode(
                self.lifter.arch().preset(),
                name,
            )
        {
            return self.lift_msr_register_mode(insn);
        }
        let class = strider_target::call_other_abi::classify_with(
            self.call_other_overrides,
            self.lifter.arch().preset(),
            name,
        )
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unknown CallOther user-op {name:?}; classify it per analysis \
                 (Python: CfgOptions(call_other_abis={{{name:?}: \
                 strider.sleigh.CallOtherAbi.pure()}}), Rust: \
                 CfgOptions::call_other_overrides), or add an entry to \
                 strider_target::call_other_abi::classify"
            )
        })?;

        // `None` is the NoOp class: no IR node, control and memory unchanged,
        // and the pcode-explicit output dropped.  Otherwise the ABI carries the
        // footprint, so one path serves side-effecting and terminating ops
        // alike.
        let Some(abi) = class.built(&self.lifter.sleigh_regs)? else {
            anyhow::ensure!(
                !terminate,
                "CallOther {name:?} ends its region, but its class builds no node to \
                 end it; lift under the options the cfg was built with"
            );
            return Ok(());
        };

        // Slot 0 is the user-op id, not an argument.
        let explicit_args = self.read_vns(insn.inputs.get(1..).unwrap_or(&[]))?;
        let output_vn: Option<rsleigh::Vn> = insn.output.as_ref().copied();
        self.build_abi_call_other(user_op_id, name, &explicit_args, &abi, output_vn, terminate)?;
        Ok(())
    }

    /// `out = msrRegisterMode(new, current)`: `current` under
    /// [`strider_ir::AssumptionOptions::msr_register_keeps_mode`], else `new`.
    fn lift_msr_register_mode(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let (Some(out), [_, new, current]) = (insn.output.as_ref(), insn.inputs.as_slice()) else {
            anyhow::bail!("msrRegisterMode takes two inputs and an output");
        };
        let keeps = self
            .builder
            .function()
            .assumptions()
            .msr_register_keeps_mode;
        let value = self.read_vn(if keeps { current } else { new })?;
        self.write_vn(out, value)
    }

    /// Whether the CFG sealed `region` `NoReturn` at the op at `addr`. Called
    /// only for a `CallOther`: every other `NoReturn` seal ends on a `Call` or
    /// `CallIndirect`.
    pub(super) fn ends_no_return_region(
        &self,
        region: strider_cfg::RegionId,
        addr: strider_cfg::PcodeInsnAddr,
    ) -> bool {
        self.cfg
            .region_graph()
            .node_weight(region)
            .is_some_and(|r| {
                matches!(r.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && r.insns.last().is_some_and(|last| last.addr == addr)
            })
    }

    /// Implicit-read registers come FIRST in the arg list, before the explicit
    /// pcode operands.
    pub(super) fn build_abi_call_other(
        &mut self,
        user_op_id: u64,
        name: &str,
        explicit_args: &[strider_ir::Value],
        abi: &strider_target::BuiltCallOtherAbi,
        output: Option<rsleigh::Vn>,
        terminate: bool,
    ) -> Result<Option<strider_ir::Value>> {
        let mut args: Vec<strider_ir::Value> =
            Vec::with_capacity(abi.implicit_reads.len() + explicit_args.len());
        for vn in &abi.implicit_reads {
            // An ABI footprint names a register the function's tracked set may
            // not carry: the set is built from the decoded instructions and the
            // calling convention, so a register neither touches is absent.
            args.push(self.read_vn(vn).with_context(|| {
                let reg = self
                    .lifter
                    .sleigh_regs()
                    .vn_to_name(*vn)
                    .map_or_else(|| format!("{vn:?}"), str::to_owned);
                format!("CallOther {name:?}: implicit read of {reg}")
            })?);
        }
        args.extend_from_slice(explicit_args);

        // Output vns are the 0-or-1 result then the clobbers. A clobber is
        // canonicalized to its largest tracked container (the builder validates
        // that) and deduplicated: sub-registers of one container collapse to a
        // single slot, and the result wins ties over a clobber.
        //
        // The result keeps the pcode output's OWN width only where
        // [`strider_target::SleighArch::callother_output_is_exact_in`] says
        // so, which carries the rule; elsewhere the result slot is the
        // container and the write clobbers it whole, which is opaque rather
        // than wrong. Decided per container, since one arch can hold register
        // files that differ.
        let result_vn = match output.map(|vn| {
            let container = self.container_of(&vn);
            if self.lifter.arch().callother_output_is_exact_in(&container) {
                vn
            } else {
                container
            }
        }) {
            // An intrinsic that writes a memory operand (x86 `sgdt [mem]`) has
            // its output in ram, which no output slot can carry. A memory
            // clobber already advances the chain over the write, so drop the
            // slot rather than fail the function.
            Some(vn)
                if abi.clobbers_memory
                    && !matches!(
                        vn.addr_space,
                        rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE
                    ) =>
            {
                None
            }
            other => other,
        };
        let mut clobber_vns: Vec<rsleigh::Vn> = Vec::new();
        for vn in &abi.implicit_writes {
            let c = self.container_of(vn);
            if Some(c) == result_vn || clobber_vns.contains(&c) {
                continue;
            }
            clobber_vns.push(c);
        }
        let mut output_vns: Vec<rsleigh::Vn> = result_vn.into_iter().collect();
        output_vns.extend_from_slice(&clobber_vns);

        let (_, outputs) = self
            .builder
            .build_call_other(
                user_op_id,
                &args,
                &output_vns,
                abi.clobbers_memory,
                terminate,
            )
            .with_context(|| format!("CallOther {name:?}: output and clobber vns"))?;
        self.builder
            .function_mut()
            .set_call_other_name(user_op_id, name);
        let (ret_vals, clobbers) = outputs.split_at(result_vn.iter().count());

        // An implicit clobber defines its whole container, so it is written
        // whole.  The result goes through the masked-insert path, which is the
        // full-container write when the slot IS the container.  Result last, so
        // an aliased clobber cannot re-clobber it.
        //
        // Skipped for a terminating op: `build_call_other` already closed the
        // region, so the write would land in a terminated region and no
        // successor could read the binding anyway.  The node's output slots
        // still exist and dangle, which matches `no_return` semantics.
        let result = ret_vals.first().copied();
        if !terminate {
            for (vn, v) in core::iter::zip(&clobber_vns, clobbers) {
                self.write_variable(vn, *v)?;
            }
            if let (Some(c), Some(v)) = (result_vn, result) {
                self.write_vn(&c, v)?;
            }
        }
        Ok(result)
    }
}

pub(super) fn decode_user_op<'a>(
    insn: &rsleigh::Insn,
    user_op_names: &'a [String],
) -> Result<(u64, &'a str)> {
    if let Some((id, name)) = strider_cfg::user_op_of(insn, user_op_names) {
        return Ok((u64::from(id), name));
    }
    // Why it names none.
    let id_vn = crate::lift::pcode_util::nth_input_or_err(insn, 0)?;
    crate::lift::pcode_util::ensure_const_space(id_vn, insn.opcode, "input 0")?;
    let id = u32::try_from(id_vn.addr_off)
        .map_err(|_| anyhow!("CallOther user-op id {:#x} exceeds u32", id_vn.addr_off))?;
    anyhow::bail!("CallOther user-op id {id} not in Sleigh's user_op table")
}

#[cfg(test)]
mod tests {
    use strider_ir::IRViewer as _;
    use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass};

    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = strider_target::SleighArch::x86_64();
        arch.probe_regs()
    }

    /// Helper: lifts `code` at 0x1000 under the x86-64 SysV convention.
    fn lift_x86_64(
        code: Vec<u8>,
        opts: &strider_cfg::CfgOptions,
    ) -> anyhow::Result<strider_ir::Function> {
        let arch = strider_target::SleighArch::x86_64();
        let regs = x86_64_sleigh_regs();
        let cc = strider_target::CallingConvention::x86_64_systemv().build(&regs)?;
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)?;
        let mut lifter = crate::lift::Lifter::new(arch, sleigh)?;
        let cfg = lifter.build_cfg(0x1000u64.into(), opts)?;
        Ok(lifter
            .build_ir_with(
                &cfg,
                cc,
                opts,
                &rustc_hash::FxHashMap::default(),
                &strider_ir::AssumptionOptions::none(),
            )?
            .function)
    }

    /// The register names behind a `CallOther` node's argument inputs and its
    /// result / clobber outputs, in slot order. Both edge lists carry the
    /// structural `[CTRL, MEM]` pair ahead of the tail.
    fn call_other_arg_and_out_regs(
        f: &strider_ir::Function,
        regs: &rsleigh::SleighRegs,
    ) -> (Vec<String>, Vec<String>) {
        use strider_ir::{IRViewer as _, IRWalker as _};
        let node = f
            .walk_kind(|k| matches!(k, strider_ir::node::NodeKind::CallOther { .. }))
            .next()
            .expect("a CallOther node");
        // An argument is the tracked register's `InitialVar`; an output slot
        // carries its vn on the value.
        let name_of = |v: strider_ir::node::ValueId| {
            match f.node_kind(f.producer(v)) {
                strider_ir::node::NodeKind::InitialVar(id) => Some(f.initial_vn(*id)),
                _ => f.get_vn_for_value(v),
            }
            .and_then(|vn| regs.vn_to_name(vn))
            .map_or_else(|| "<untagged>".to_owned(), str::to_owned)
        };
        let args = f
            .node_inputs(node)
            .into_iter()
            .skip(2)
            .map(name_of)
            .collect();
        let outs = f
            .node_outputs(node)
            .iter()
            .skip(2)
            .map(|v| name_of(*v))
            .collect();
        (args, outs)
    }

    /// A bare `syscall` names no register in its pcode, which is the whole
    /// reason its ABI row exists. `R10` (read) and `R11` (write) are in neither
    /// the SysV argument registers nor its callee-saved set, so nothing but the
    /// ABI footprint itself can put them in the tracked-varnode universe.
    #[test]
    fn syscall_lifts_with_its_full_implicit_footprint() {
        // 1000: 0f 05    syscall
        // 1002: c3       ret
        let f = lift_x86_64(vec![0x0f, 0x05, 0xc3], &strider_cfg::CfgOptions::default())
            .expect("a function containing `syscall` must lift");
        let regs = x86_64_sleigh_regs();
        let (args, outs) = call_other_arg_and_out_regs(&f, &regs);
        assert_eq!(
            args,
            vec!["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
            "every implicit read of the syscall ABI must be wired as an input"
        );
        assert_eq!(
            outs,
            vec!["RAX", "RCX", "R11"],
            "every implicit write of the syscall ABI must be wired as an output"
        );
    }

    /// A caller-supplied footprint is the case the tracked universe cannot
    /// cover incidentally: `R10` and `RBX` have no SysV argument or return
    /// role, and the two instructions name neither.
    #[test]
    fn custom_call_other_abi_names_registers_the_code_never_touches() {
        let regs = x86_64_sleigh_regs();
        let vn = |n: &str| regs.name_to_vn(n).expect("register must resolve");
        let abi = strider_target::BuiltCallOtherAbi {
            implicit_reads: vec![vn("R10")],
            implicit_writes: vec![vn("EBX")],
            clobbers_memory: false,
            no_return: false,
        };
        let opts = strider_cfg::CfgOptions {
            call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "syscall".to_owned(),
                strider_target::call_other_abi::CallOtherOverride::Built(abi),
            )])
            .expect("unique override names"),
            ..Default::default()
        };
        // 1000: 0f 05    syscall     ; classified by the override above
        // 1002: c3       ret
        let f = lift_x86_64(vec![0x0f, 0x05, 0xc3], &opts)
            .expect("a caller-supplied footprint must not need the code to name its registers");
        let (args, outs) = call_other_arg_and_out_regs(&f, &regs);
        assert_eq!(args, vec!["R10"], "the declared read is wired as an input");
        // Nothing names the 64-bit container, so the declared `EBX` is itself
        // the tracked variable rather than a slice of `RBX`.
        assert_eq!(
            outs,
            vec!["EBX"],
            "the declared write is wired as an output"
        );
    }

    /// Lifts `code` at 0x1000 under the PowerPC 32-bit System V convention.
    fn lift_ppc32be(
        code: Vec<u8>,
        opts: &strider_cfg::CfgOptions,
    ) -> anyhow::Result<strider_ir::Function> {
        let arch = strider_target::SleighArch::ppc32be();
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)?;
        let mut lifter = crate::lift::Lifter::new(arch, sleigh)?;
        let cc = strider_target::CallingConvention::powerpc_sysv32().build(lifter.sleigh_regs())?;
        let cfg = lifter.build_cfg(0x1000u64.into(), opts)?;
        Ok(lifter
            .build_ir_with(
                &cfg,
                cc,
                opts,
                &rustc_hash::FxHashMap::default(),
                &strider_ir::AssumptionOptions::none(),
            )?
            .function)
    }

    /// The unconditional-trap rule reads `twi`'s TO mask off the pcode, so a
    /// caller override for `trapWord` must reach it: the emitted node and the
    /// region terminator answer to one classification, or the lifter walks
    /// into a region the CFG kept open.
    #[test]
    fn a_call_other_override_outranks_the_unconditional_ppc_trap_rule() {
        // 1000: 38 60 00 01    li r3,1
        // 1004: 0f e3 00 00    twi 31,r3,0
        // 1008: 38 60 00 02    li r3,2
        // 100c: 4e 80 00 20    blr
        let bytes = vec![
            0x38, 0x60, 0x00, 0x01, 0x0f, 0xe3, 0x00, 0x00, 0x38, 0x60, 0x00, 0x02, 0x4e, 0x80,
            0x00, 0x20,
        ];
        let unreachable_count = |opts: &strider_cfg::CfgOptions| {
            let f =
                lift_ppc32be(bytes.clone(), opts).expect("a function containing `twi` must lift");
            f.graph()
                .all_node_ids()
                .filter(|n| matches!(f.node_kind(*n), strider_ir::node::NodeKind::Unreachable))
                .count()
        };

        assert_eq!(
            unreachable_count(&strider_cfg::CfgOptions::default()),
            1,
            "TO=31 is unconditional, so control sinks at the trap"
        );

        let opts = strider_cfg::CfgOptions {
            call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "trapWord".to_owned(),
                CallOtherClass::MEM_CLOBBER.into(),
            )])
            .expect("unique override names"),
            ..Default::default()
        };
        assert_eq!(
            unreachable_count(&opts),
            0,
            "a returning override for `trapWord` must keep control flowing"
        );
    }

    /// The lift sinks a `CallOther` into `Unreachable` exactly where the CFG
    /// ended its region `NoReturn`: over all 32 `twi` TO masks, and under an
    /// override that keeps the unconditional one returning.
    #[test]
    fn a_call_other_ends_the_ir_exactly_where_the_cfg_ends_its_region() {
        const LI_R3_1: u32 = 0x3860_0001;
        const LI_R3_2: u32 = 0x3860_0002;
        const BLR: u32 = 0x4e80_0020;
        let twi = |to: u32| 0x0c00_0000 | (to << 21) | (3 << 16);
        // Program k, 16 bytes at 0x1000 + 0x10 * k: `li r3,1 ; twi k,r3,0 ;
        // li r3,2 ; blr`, the 33rd repeating TO=31 under the override.
        let words: Vec<u32> = (0..32)
            .chain([31])
            .flat_map(|to| [LI_R3_1, twi(to), LI_R3_2, BLR])
            .collect();
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
        let arch = strider_target::SleighArch::ppc32be();
        let reader = rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let mut lifter = crate::lift::Lifter::new(arch, sleigh).expect("lifter");
        let cc = strider_target::CallingConvention::powerpc_sysv32()
            .build(lifter.sleigh_regs())
            .expect("cc");
        let plain = strider_cfg::CfgOptions::default();
        let keep_returning = strider_cfg::CfgOptions {
            call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "trapWord".to_owned(),
                CallOtherClass::MEM_CLOBBER.into(),
            )])
            .expect("unique override names"),
            ..Default::default()
        };
        let no_ccs = rustc_hash::FxHashMap::default();
        let mut ended = 0;
        for k in 0..33u64 {
            let opts = if k < 32 { &plain } else { &keep_returning };
            let cfg = lifter
                .build_cfg((0x1000 + 0x10 * k).into(), opts)
                .expect("cfg");
            let cfg_ends = cfg.regions().any(|r| {
                matches!(r.terminator, strider_cfg::RegionTerminator::NoReturn)
                    && r.insns
                        .last()
                        .is_some_and(|w| w.insn.opcode == rsleigh::Opcode::CallOther)
            });
            let f = lifter
                .build_ir_with(
                    &cfg,
                    cc.clone(),
                    opts,
                    &no_ccs,
                    &strider_ir::AssumptionOptions::none(),
                )
                .expect("lift")
                .function;
            let sinks = f
                .graph()
                .all_node_ids()
                .filter(|n| matches!(f.node_kind(*n), strider_ir::node::NodeKind::Unreachable))
                .count();
            assert_eq!(sinks, usize::from(cfg_ends), "program {k}");
            ended += usize::from(cfg_ends);
        }
        assert_eq!(
            ended, 7,
            "the seven masks naming a trichotomy end it, none under the override"
        );
    }

    /// A `Cfg` lifts as it was built, whatever options `build_ir_with` is
    /// handed: one built with `trapWord` kept returning has no sink under
    /// plain options, and one built plain sinks under that override.
    #[test]
    fn a_call_other_lifts_as_its_cfg_was_built() {
        // 1000: 38 60 00 01    li r3,1
        // 1004: 0f e3 00 00    twi 31,r3,0
        // 1008: 38 60 00 02    li r3,2
        // 100c: 4e 80 00 20    blr
        let bytes = vec![
            0x38, 0x60, 0x00, 0x01, 0x0f, 0xe3, 0x00, 0x00, 0x38, 0x60, 0x00, 0x02, 0x4e, 0x80,
            0x00, 0x20,
        ];
        let arch = strider_target::SleighArch::ppc32be();
        let reader = rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
        let mut lifter = crate::lift::Lifter::new(arch, sleigh).expect("lifter");
        let cc = strider_target::CallingConvention::powerpc_sysv32()
            .build(lifter.sleigh_regs())
            .expect("cc");
        let plain = strider_cfg::CfgOptions::default();
        let keep_returning = strider_cfg::CfgOptions {
            call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "trapWord".to_owned(),
                CallOtherClass::MEM_CLOBBER.into(),
            )])
            .expect("unique override names"),
            ..Default::default()
        };
        let no_ccs = rustc_hash::FxHashMap::default();
        let mut sinks = |built: &strider_cfg::CfgOptions, lifted: &strider_cfg::CfgOptions| {
            let cfg = lifter.build_cfg(0x1000u64.into(), built).expect("cfg");
            let f = lifter
                .build_ir_with(
                    &cfg,
                    cc.clone(),
                    lifted,
                    &no_ccs,
                    &strider_ir::AssumptionOptions::none(),
                )
                .expect("a CallOther lifts as its cfg was built")
                .function;
            f.graph()
                .all_node_ids()
                .filter(|n| matches!(f.node_kind(*n), strider_ir::node::NodeKind::Unreachable))
                .count()
        };
        assert_eq!(
            sinks(&keep_returning, &plain),
            0,
            "the cfg kept the trap returning, so the lift does too"
        );
        assert_eq!(
            sinks(&plain, &keep_returning),
            1,
            "the cfg ended the region at the trap, so the lift sinks there"
        );

        let overrides = |class: CallOtherClass| {
            strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "trapWord".to_owned(),
                class.into(),
            )])
            .expect("unique override names")
        };
        let (ends, noop) = (
            overrides(CallOtherClass::NO_RETURN),
            overrides(CallOtherClass::NoOp),
        );
        let overriding = |call_other_overrides| strider_cfg::CfgOptions {
            call_other_overrides,
            ..Default::default()
        };
        let cfg = lifter
            .build_cfg(0x1000u64.into(), &overriding(&ends))
            .expect("cfg");
        let Err(err) = lifter.build_ir_with(
            &cfg,
            cc,
            &overriding(&noop),
            &no_ccs,
            &strider_ir::AssumptionOptions::none(),
        ) else {
            panic!("a CallOther the cfg ended must not lift as a NoOp");
        };
        assert!(
            format!("{err:?}").contains("ends its region, but its class builds no node"),
            "got {err:?}"
        );
    }

    /// Folding ABI footprints into the tracked universe must not preempt the
    /// two errors that belong to lifting the op: an unclassified name, and a
    /// footprint register outside this arch's register table. Both still fail
    /// where they name the op.
    #[test]
    fn universe_construction_defers_both_call_other_errors() {
        // 1000: 0f 34    sysenter    ; x86-64 classifies no such user-op
        // 1002: c3       ret
        let Err(err) = lift_x86_64(vec![0x0f, 0x34, 0xc3], &strider_cfg::CfgOptions::default())
        else {
            panic!("an unclassified user-op must fail the lift");
        };
        let msg = format!("{err:?}");
        assert!(
            msg.contains("unknown CallOther user-op \"sysenter\""),
            "an unclassified op keeps its own message, got {msg}"
        );

        let opts = strider_cfg::CfgOptions {
            call_other_overrides: &strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "syscall".to_owned(),
                CallOtherClass::Call(CallOtherAbi {
                    implicit_reads: &["NONEXISTENT_REG_XYZZY"],
                    implicit_writes: &[],
                    clobbers_memory: false,
                    no_return: false,
                })
                .into(),
            )])
            .expect("unique override names"),
            ..Default::default()
        };
        // 1000: 0f 05    syscall     ; classified by the override above
        // 1002: c3       ret
        let Err(err) = lift_x86_64(vec![0x0f, 0x05, 0xc3], &opts) else {
            panic!("an unresolvable footprint register must fail the lift");
        };
        let msg = format!("{err:?}");
        assert!(
            msg.contains("NONEXISTENT_REG_XYZZY"),
            "an unresolvable ABI register keeps being named, got {msg}"
        );
    }
}
