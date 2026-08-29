use anyhow::Context as _;
use anyhow::{Result, anyhow};
use strider_ir::IRViewer as _;

use super::FunctionLifter;

impl<R: rsleigh::MemReader> FunctionLifter<'_, R> {
    pub(super) fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let (user_op_id, name) = decode_user_op(insn, self.lifter.user_op_names())?;
        let class = strider_target::call_other_abi::classify_with(
            self.call_other_overrides,
            self.lifter.arch.preset(),
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
        // footprint and whether control returns, so one path serves
        // side-effecting and terminating ops alike.  A `BUG_ON`-class trap is
        // the empty-footprint `no_return` case.
        let Some(abi) = class.built(&self.lifter.sleigh_regs)? else {
            return Ok(());
        };

        // Slot 0 is the user-op id, not an argument.
        let explicit_args = self.read_vns(insn.inputs.get(1..).unwrap_or(&[]))?;
        let output_vn: Option<rsleigh::Vn> = insn.output.as_ref().copied();

        // A PowerPC trap whose TO mask names every relation fires
        // unconditionally, so the fall-through is dead. The mask reaches the op
        // through a temporary, not as a literal operand, so read it back as a
        // folded constant rather than off the pcode.
        let to_mask = explicit_args
            .first()
            .and_then(|&v| self.builder.function().int_const_u128(v));
        let terminate =
            abi.no_return || strider_target::call_other_abi::trap_is_unconditional(name, to_mask);

        self.build_abi_call_other(user_op_id, name, &explicit_args, &abi, output_vn, terminate)?;
        Ok(())
    }

    /// Implicit-read registers come FIRST in the arg list, before the explicit
    /// pcode operands.
    fn build_abi_call_other(
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

        // Output vns are the 0-or-1 result then the clobbers, each canonicalized
        // to its largest tracked container (the builder validates that) and
        // deduplicated: sub-registers of one container collapse to a single
        // slot, and the result wins ties over a clobber.
        let result_vn = match output.map(|vn| self.container_of(&vn)) {
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

        let (node, outputs) = self
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
            .side_tables_mut()
            .set_call_other_name(node, name);
        let (ret_vals, clobbers) = outputs.split_at(result_vn.iter().count());

        // Full-container writes: an opaque intrinsic defines the whole
        // container.  Result last, so an aliased clobber cannot re-clobber it.
        //
        // Skipped for a terminating op: `build_call_other` already closed the
        // region, so `write_variable` would insert into a terminated region and
        // no successor could read the bindings anyway.  The node's output slots
        // still exist and dangle, which matches `no_return` semantics.
        let result = ret_vals.first().copied();
        if !terminate {
            for (vn, v) in core::iter::zip(&clobber_vns, clobbers) {
                self.builder.write_variable(vn, *v)?;
            }
            if let (Some(c), Some(v)) = (result_vn, result) {
                self.builder.write_variable(&c, v)?;
            }
        }
        Ok(result)
    }
}

pub(super) fn decode_user_op<'a>(
    insn: &rsleigh::Insn,
    user_op_names: &'a [String],
) -> Result<(u64, &'a str)> {
    let id_vn = crate::lift::pcode_util::nth_input_or_err(insn, 0)?;
    crate::lift::pcode_util::ensure_const_space(id_vn, insn.opcode, "input 0")?;
    let user_op_id = id_vn.addr_off;
    let user_op_id_u32 = u32::try_from(user_op_id)
        .map_err(|_| anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32"))?;
    let name = user_op_names
        .get(user_op_id_u32 as usize)
        .map(String::as_str)
        .ok_or_else(|| {
            anyhow!("CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table")
        })?;
    Ok((user_op_id, name))
}

#[cfg(test)]
mod tests {
    use strider_target::call_other_abi::{CallOtherAbi, CallOtherClass};

    fn x86_64_sleigh_regs() -> rsleigh::SleighRegs {
        let arch = strider_target::SleighArch::x86_64();
        arch.probe_regs()
            .expect("probe_regs must succeed for x86_64")
    }

    /// Helper: lifts `code` at 0x1000 under the x86-64 SysV convention.
    fn lift_x86_64(
        code: Vec<u8>,
        opts: &crate::LiftOptions,
    ) -> anyhow::Result<strider_ir::Function> {
        let arch = strider_target::SleighArch::x86_64();
        let regs = x86_64_sleigh_regs();
        let cc = strider_target::CallingConvention::x86_64_systemv().build(&regs)?;
        let reader = rsleigh::mem_readers::BufMemReader::new(code, 0x1000);
        let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), reader)?;
        let mut lifter = crate::lift::Lifter::new(arch, sleigh)?;
        let cfg = lifter.build_cfg(0x1000u64.into(), &opts.cfg, &opts.per_address_ccs)?;
        Ok(lifter.build_ir_with(&cfg, cc, opts)?.function)
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
        let f = lift_x86_64(vec![0x0f, 0x05, 0xc3], &crate::LiftOptions::default())
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
        let mut opts = crate::LiftOptions::default();
        opts.cfg.call_other_overrides =
            strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "syscall".to_owned(),
                strider_target::call_other_abi::CallOtherOverride::Built(abi),
            )])
            .expect("unique override names");
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

    /// Folding ABI footprints into the tracked universe must not preempt the
    /// two errors that belong to lifting the op: an unclassified name, and a
    /// footprint register outside this arch's register table. Both still fail
    /// where they name the op.
    #[test]
    fn universe_construction_defers_both_call_other_errors() {
        // 1000: 0f 34    sysenter    ; x86-64 classifies no such user-op
        // 1002: c3       ret
        let Err(err) = lift_x86_64(vec![0x0f, 0x34, 0xc3], &crate::LiftOptions::default()) else {
            panic!("an unclassified user-op must fail the lift");
        };
        let msg = format!("{err:?}");
        assert!(
            msg.contains("unknown CallOther user-op \"sysenter\""),
            "an unclassified op keeps its own message, got {msg}"
        );

        let mut opts = crate::LiftOptions::default();
        opts.cfg.call_other_overrides =
            strider_target::call_other_abi::CallOtherOverrides::new(vec![(
                "syscall".to_owned(),
                CallOtherClass::Call(CallOtherAbi {
                    implicit_reads: &["NONEXISTENT_REG_XYZZY"],
                    implicit_writes: &[],
                    clobbers_memory: false,
                    no_return: false,
                })
                .into(),
            )])
            .expect("unique override names");
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
