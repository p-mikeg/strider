//! Fixtures the resolve loop's unit tests share.

use strider_cfg::{MachineInsnAddr, PcodeInsnAddr, ResolvedTargets};
use strider_ir::node::NodeId;

thread_local! {
    /// Targets [`super::visit`] read on this thread.
    pub(crate) static TARGET_VISITS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// `run(n)`'s target visits at two sizes 8x apart: linear is 8x the visits,
/// quadratic 64x.
pub(crate) fn assert_linear_visits(what: &str, run: impl Fn(u64)) {
    let visits = |n| {
        TARGET_VISITS.with(|v| v.set(0));
        run(n);
        TARGET_VISITS.with(std::cell::Cell::get)
    };
    let small = visits(1_000);
    let large = visits(8_000);
    assert!(
        small >= 1_000,
        "{what}: {small} visits cannot read every arm"
    );
    assert!(
        large < small * 9,
        "{what}: 8x the arms cost {large} visits against {small}",
    );
}

pub(crate) fn pcode_addr(machine: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr::from(machine),
        insn_index: 0,
    }
}

/// The `BRANCHIND` of an ARM `bx`, a MIPS `jr` or an x86 `jmp [reg]` is not
/// the instruction's first p-code op, so its anchor and the only key a caller
/// can spell differ.
pub(crate) fn mid_insn_addr(machine: u64) -> PcodeInsnAddr {
    PcodeInsnAddr {
        machine_addr: MachineInsnAddr::from(machine),
        insn_index: 2,
    }
}

pub(crate) fn multiple(targets: &[(u64, Option<bool>)]) -> ResolvedTargets {
    ResolvedTargets::table(
        targets
            .iter()
            .map(|&(addr, bit)| strider_cfg::ResolvedTarget::new(addr, bit)),
    )
}

/// `t`'s successors as a set: what the convergence check compares.
pub(crate) fn successor_set(
    t: &ResolvedTargets,
) -> std::collections::BTreeSet<(Option<u64>, Option<bool>)> {
    t.successors().collect()
}

/// Minimal valid function: entry region terminating in one `IndirectBranch`
/// over an `IntConst`. Returns the placeholder's `NodeId` alongside it.
pub(crate) fn fn_with_live_indirect_branch() -> (strider_ir::Function, NodeId) {
    use strider_ir::IRBuilderExt;
    let mut b = strider_ir_test_utils::empty_builder().expect("builder");
    let region = b.create_region_all().expect("region");
    b.set_entry_region_all(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let target = b
        .build_int_const(0u128, strider_ir::ValueType::I64)
        .expect("const");
    let placeholder = b.build_indirect_branch(target).expect("indirect");
    b.set_lift_addr(None);
    let function = b.build().expect("build");
    (function, placeholder)
}
