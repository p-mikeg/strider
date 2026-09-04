use anyhow::Result;

use super::Lifter;

/// Per-function translation context, borrowing the shared [`Lifter`] engine
/// and owning a fresh [`strider_ir::FunctionBuilder`].
pub(crate) struct FunctionLifter<'a, R: rsleigh::MemReader> {
    pub(crate) lifter: &'a Lifter<R>,
    pub(crate) builder: strider_ir::FunctionBuilder,
    pub(crate) cfg: &'a strider_cfg::Cfg,
    /// Maps each `BranchIndirect`'s pcode address to its `IndirectBranch`
    /// placeholder node.
    pub(crate) unresolved_branches: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    /// Seated `Switch` sites, so the resolver can re-derive and widen one.
    pub(crate) switch_anchors: Vec<(strider_cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    pub(crate) per_address_ccs:
        &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
    /// Caller classifications for user-op names, from
    /// [`strider_cfg::CfgOptions`]; consulted before the built-in tables.
    pub(crate) call_other_overrides: &'a strider_target::call_other_abi::CallOtherOverrides,
    /// O(1) varnode-to-largest-container map, built over the raw varnode set
    /// plus every CC register.  Varnodes absent from it fall through to the
    /// scan in `container_of`.
    pub(crate) container_map: vn_container::ContainerMap,
    /// Constant values of the current machine instruction's temporaries, fed
    /// every op in `region.insns` order. `collect_def_sites` runs an identical
    /// one over the same sequence, so a register store resolves to the same
    /// varnode on both sides.
    pub(crate) pcode_consts: super::pcode_consts::PcodeConsts,
    /// The FIRST value committed to `ISAModeSwitch` at a machine address (ARM's
    /// `setISAMode` reads it, MIPS's `JXWritePC` writes it) and that address:
    /// the mode the instruction decodes its branch target(s) in. First, not
    /// last, because the ARMv7/v8 sla writes it twice for `mov pc, rN` and the
    /// second write re-derives it from an already-masked value; see
    /// `vn_io::write_vn`. The region's terminating `IndirectBranch` takes it as
    /// its mode input iff they share that address.
    pub(crate) pending_isa_mode: Option<(strider_ir::node::ValueId, u64)>,
    /// The `ISAModeSwitch` register vn (the ISA-mode commit target on ARM/MIPS),
    /// resolved once; `None` on arches without one.
    pub(crate) isa_mode_switch_vn: Option<rsleigh::Vn>,
    /// Each `Region` node's last machine address: the branch leaving it.
    pub(crate) region_last_addrs: rustc_hash::FxHashMap<strider_ir::node::NodeId, u64>,
    /// `ret_and_clobber_vns` under the function-default CC.  The tracked set
    /// and the container map are both frozen for the lift, so every call site
    /// without a `per_address_ccs` override yields this same split.
    pub(super) default_ret_clobber_vns: (Vec<rsleigh::Vn>, Vec<rsleigh::Vn>),
    /// [`super::cc_projection::float_arg_prefix`] under the function-default
    /// CC, cached for the same reason.
    pub(super) default_float_arg_vns: Vec<rsleigh::Vn>,
}

impl<'a, R: rsleigh::MemReader> FunctionLifter<'a, R> {
    /// `all_vns` seeds the tracked set: every REGISTER / UNIQUE varnode the
    /// `cfg`'s instructions name, plus every `CallOther` ABI footprint
    /// register.  The stack vn and each override's argument registers are
    /// added here.
    pub(crate) fn new(
        lifter: &'a Lifter<R>,
        cc: strider_target::BuiltCallingConvention,
        cfg: &'a strider_cfg::Cfg,
        mut all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: &'a rustc_hash::FxHashMap<u64, strider_target::BuiltCallingConvention>,
        call_other_overrides: &'a strider_target::call_other_abi::CallOtherOverrides,
    ) -> Result<Self> {
        // Membership set over `all_vns`: callers pass hundreds of per-address
        // CCs, and a linear scan per override register is quadratic in the
        // tracked set.  The Vec stays the SSoT for ORDER.
        let mut seen: rustc_hash::FxHashSet<rsleigh::Vn> = all_vns.iter().copied().collect();
        let mut track = |vns: &mut Vec<rsleigh::Vn>, v: rsleigh::Vn| {
            if seen.insert(v) {
                vns.push(v);
            }
        };
        // The lifter is the SSoT for tracking SP: a function may never name
        // SP yet still need it tracked for stack analysis.
        track(&mut all_vns, cc.stack_vn);
        // `FunctionBuilder::new` seeds the function's OWN convention only, so
        // an override's argument registers would be untracked: an integer one
        // fails the lift outright in `read_vns`, a float one gaps
        // `float_arg_slots` and truncates the call's float arguments there.
        let overrides: Vec<rsleigh::Vn> = per_address_ccs
            .values()
            .flat_map(|o| {
                o.arg_passing_regs
                    .iter()
                    .chain(o.arg_passing_regs_float.iter())
            })
            .copied()
            .collect();
        for v in overrides {
            track(&mut all_vns, v);
        }
        // `FunctionBuilder::new` owns universe construction (seeding CC
        // registers, dropping enclosed sub-registers), so its `all_vns()` is
        // the canonical tracked set.  Resolving a varnode INTO that set is the
        // machine-register concern owned here: query every raw varnode plus
        // every CC register, so an ABI register narrower than its tracked
        // container (ABI says `eax`, the function tracks `rax`) resolves.
        //
        // `cc` threads by value into `FunctionBuilder::new` and on to
        // `Function::default_cc` with no clone, so every read of it is captured
        // into an owned local first.
        let stack_vn = cc.stack_vn;
        let cc_regs: Vec<rsleigh::Vn> = cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
            .chain(cc.arg_passing_regs_float.iter())
            .chain(cc.callee_saved_regs.iter())
            .chain(std::iter::once(&cc.stack_vn))
            .copied()
            .collect();
        let builder =
            strider_ir::FunctionBuilder::new(all_vns.clone(), cc, lifter.arch.endianness())?;
        let queries = all_vns.iter().copied().chain(cc_regs);
        let container_map =
            vn_container::ContainerMap::build(builder.function().all_vns(), queries);
        // Since the stack vn was added to `all_vns` above, after dedup it must
        // resolve to a tracked container: itself, or a larger tracked vn.
        debug_assert!(
            {
                let tracked = builder.function().all_vns();
                let sp_container = vn_container::largest_container_in(tracked, &stack_vn);
                tracked.contains(&sp_container)
                    && vn_container::vn_contains(&sp_container, &stack_vn)
            },
            "stack vn {stack_vn:?} did not resolve to a tracked container",
        );
        let default_ret_clobber_vns = {
            let f = builder.function();
            f.default_cc()
                .ret_and_clobber_vns(f.all_vns(), |v| container_map.container_of(f.all_vns(), v))
        };
        let default_float_arg_vns = {
            let f = builder.function();
            super::cc_projection::float_arg_prefix(f.default_cc(), f.all_vns(), |v| {
                container_map.container_of(f.all_vns(), v)
            })
        };
        let isa_mode_switch_vn = lifter.sleigh_regs().name_to_vn("ISAModeSwitch");
        // Capture and carry are paired BOTH ways. Carry without capture would
        // leave `enqueue_resolved`'s None-arch arm dropping the committed bit
        // and decoding a resolved target in the wrong mode; capture without
        // carry means the name above did not resolve, which `name_to_vn`
        // reports as `None` rather than as an error, so a misspelling would
        // otherwise satisfy a one-directional check vacuously while the mode
        // bit is silently never captured. Mirror of the carry-side check in
        // `Builder::with_flow_context`.
        debug_assert_eq!(
            isa_mode_switch_vn.is_some(),
            lifter.arch.isa_mode_var().is_some(),
            "arch {:?}: ISAModeSwitch resolved={}, isa_mode_var={:?}",
            lifter.arch.preset(),
            isa_mode_switch_vn.is_some(),
            lifter.arch.isa_mode_var(),
        );
        Ok(Self {
            lifter,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            switch_anchors: Vec::new(),
            per_address_ccs,
            call_other_overrides,
            container_map,
            pcode_consts: super::pcode_consts::PcodeConsts::default(),
            pending_isa_mode: None,
            isa_mode_switch_vn,
            region_last_addrs: rustc_hash::FxHashMap::default(),
            default_ret_clobber_vns,
            default_float_arg_vns,
        })
    }

    /// Every IR node born inside `f` picks up `addr` in its fingerprint
    /// side-table.  The address is cleared even on the error path.
    pub(crate) fn with_lift_addr<T>(
        &mut self,
        addr: Option<u64>,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.builder.set_lift_addr(addr);
        let res = f(self);
        self.builder.set_lift_addr(None);
        res
    }
    /// A zero-pcode-op entry region carries no instruction, so its start address
    /// is the fallback: a sink stamped with no address fails the fingerprint
    /// invariant.
    pub(crate) fn entry_machine_addr(&self) -> Option<u64> {
        let region = self.cfg.region_graph().node_weight(self.cfg.entry())?;
        Some(
            region
                .insns
                .first()
                .map_or(region.start_addr.machine_addr.addr, |wrapped| {
                    wrapped.addr.machine_addr.addr
                }),
        )
    }
}
