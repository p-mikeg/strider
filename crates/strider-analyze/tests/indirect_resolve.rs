//! Tests for [`strider_analyze::indirect_resolver::resolve_indirect_target`] —
//! the lazy mini-IR resolver.
//!
//! Each test:
//!   1. Builds a hand-crafted sequence of `RegionInstruction`s.  No machine
//!      code is decoded — pcode is typed by hand to keep the failure
//!      modes attributable to the resolver, not to the lifter.
//!   2. Calls the resolver and asserts on the returned [`ResolvedTargets`]
//!      or the error variant.
//!
//! The resolver lives at
//! `crates/strider-analyze/src/indirect_resolver.rs`; the integration
//! between it and `RegionBuilder::process_new_insn` is covered by
//! `cfg/tests/indirect_dispatch.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use strider_lift::cfg::{MachineInsnAddr, PcodeInsnAddr, RegionInstruction, ResolvedTargets};
use rsleigh::mem_readers::BufMemReader;
use rsleigh::{Insn, Opcode, Vn, VnSpace};
use strider_analyze::indirect_resolver::resolve_indirect_target;
use strider_ir_test_utils::MockRom;

type TestReader = BufMemReader<Vec<u8>>;

/// x86 (32-bit) Sleigh, sufficient for register-aliasing tests
/// (`eax`/`ax`/`al` overlap inside `eax`'s 4-byte container) without
/// the full x86_64 machinery.
fn make_x86_sleigh() -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("create x86 Sleigh")
}

/// Build an `x86-64` Sleigh — needed for sub-register aliasing tests
/// where we want to write a 4-byte sub-register (`eax`) and read an
/// 8-byte container (`rax`) so the resolver exercises pcode-lift's
/// `Piece`/`Insert` aliasing logic.
fn make_x86_64_sleigh() -> rsleigh::Sleigh<TestReader> {
    let reader = BufMemReader::new(Vec::<u8>::new(), 0x0);
    rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86_64,
        rsleigh::pspec::PSPEC_X86_64,
        reader,
    )
    .expect("create x86-64 Sleigh")
}

/// 4-byte REGISTER varnode at the given Sleigh register-space offset.
fn reg4(off: u64) -> Vn {
    Vn { size: 4, addr_off: off, addr_space: VnSpace::REGISTER }
}

/// Look up an x86 / x86-64 Sleigh register by name; panics if the name
/// is unknown — every name used in these tests is guaranteed by the
/// Sleigh registry.
fn vn_for_name<R: rsleigh::MemReader>(sleigh: &rsleigh::Sleigh<R>, name: &str) -> Vn {
    sleigh
        .regs()
        .expect("regs")
        .name_to_vn(name)
        .unwrap_or_else(|| panic!("unknown reg: {name}"))
}

/// A short way to construct a CONST varnode of declared `size` carrying
/// integer `val`.
fn const_vn(val: u64, size: u32) -> Vn {
    Vn { size, addr_off: val, addr_space: VnSpace::CONST }
}

/// Wrap an [`rsleigh::Insn`] into a [`RegionInstruction`] at the given
/// pcode address.
fn ri(machine: u64, insn_index: u64, insn: Insn) -> RegionInstruction {
    RegionInstruction {
        addr: PcodeInsnAddr { machine_addr: MachineInsnAddr::from(machine), insn_index },
        insn,
    }
}

/// Trailing `BranchIndirect target_vn` so the resolver naturally stops
/// lifting at this op (the lifter returns `Ok(false)` for control-flow
/// opcodes).
fn branch_indirect(target_vn: Vn) -> Insn {
    Insn {
        opcode: Opcode::BranchIndirect,
        output: None,
        inputs: vec![target_vn].into(),
    }
}

// ── ResolvedTargets::Single ───────────────────────────────────────────

/// `Copy reg, K; BranchIndirect reg` resolves to `Single(K)`.
#[test]
fn resolves_direct_const_to_single() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0); // EAX in x86 sleigh — actual offset doesn't matter
    let region = vec![
        ri(
            0x1000,
            0,
            Insn {
                opcode: Opcode::Copy,
                output: Some(target),
                inputs: vec![const_vn(0xdead_beef, 4)].into(),
            },
        ),
        ri(0x1004, 0, branch_indirect(target)),
    ];
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("resolver");
    assert_eq!(res, Some(ResolvedTargets::Single(0xdead_beef)));
}

/// `Copy reg, K1; IntAdd reg, reg, K2; BranchIndirect reg` resolves to
/// `Single(K1+K2)` after `ConstantFold`.
#[test]
fn resolves_arithmetic_chain_to_single() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region = vec![
        ri(
            0x1000,
            0,
            Insn {
                opcode: Opcode::Copy,
                output: Some(target),
                inputs: vec![const_vn(0x100, 4)].into(),
            },
        ),
        ri(
            0x1004,
            0,
            Insn {
                opcode: Opcode::IntAdd,
                output: Some(target),
                inputs: vec![target, const_vn(0x33, 4)].into(),
            },
        ),
        ri(0x1008, 0, branch_indirect(target)),
    ];
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("resolver");
    assert_eq!(res, Some(ResolvedTargets::Single(0x133)));
}

/// Sub-register aliasing: write a 4-byte `EAX` constant on x86_64, then
/// branch through the 8-byte container `RAX`.  After
/// `KnownBits` simplifies the `Piece`/`Insert` chain, the target should
/// resolve to `Single(K)`.  The upper 4 bytes of `RAX` are
/// `InitialVar(rax) >> 32` masked; `KnownBits` does NOT prove those
/// bits are zero (an `InitialVar` carries no static knowledge), so a
/// real upper-half write would block resolution.  Here the test
/// expresses the canonical `mov eax, K` shape via `Subpiece(rax, 0)` /
/// `Insert` — which on x86_64 represents the architectural "writes to
/// 32-bit register zero-extend to 64 bits" semantics.  `KnownBits` then
/// proves the upper 32 bits zero, leaving the constant.
///
/// Implementation detail: x86_64 SLEIGH lowers `mov eax, imm32` to a
/// `Copy eax, imm32` followed by an implicit zero-extension into RAX.
/// The cleanest way to model that here is with two pcode ops.  We
/// hand-roll the Copy + the `IntZext rax, eax` follow-up so we don't
/// depend on the SLEIGH lifter.
#[test]
fn resolves_sub_register_aliasing_to_single() {
    let sleigh = make_x86_64_sleigh();
    let eax = vn_for_name(&sleigh, "EAX");
    let rax = vn_for_name(&sleigh, "RAX");
    // The branch target is RAX (8-byte).
    let region = vec![
        // Copy eax, K  — pcode-lift's write_reg_vn handles the
        // sub-register insert into rax.
        ri(
            0x1000,
            0,
            Insn {
                opcode: Opcode::Copy,
                output: Some(eax),
                inputs: vec![const_vn(0xdead_beef, 4)].into(),
            },
        ),
        // mov eax, K on x86-64 zero-extends into rax — model that
        // explicitly with IntZext so KnownBits doesn't have to lean on
        // architectural-semantics that the mini-graph doesn't know.
        ri(
            0x1004,
            0,
            Insn {
                opcode: Opcode::IntZext,
                output: Some(rax),
                inputs: vec![eax].into(),
            },
        ),
        ri(0x1008, 0, branch_indirect(rax)),
    ];
    let res = resolve_indirect_target(
        &region,
        rax,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("resolver");
    assert_eq!(res, Some(ResolvedTargets::Single(0xdead_beef)));
}

// ── ResolvedTargets::LinkRegister ─────────────────────────────────────

/// `BranchIndirect lr` with no prior write to `lr` resolves to
/// `LinkRegister`.  `cc_link_register_vn` is set to the same varnode
/// the BranchIndirect names — i.e. the canonical ARM AAPCS link
/// register.  Modelled here against an x86 Sleigh by reusing a
/// dedicated `lr_like` varnode at a non-overlapping register offset:
/// the resolver's classification only cares about
/// `NodeKind::InitialVar(vn) == cc_link_register_vn`, not whether the
/// architecture has a real link register.
#[test]
fn resolves_link_register_to_link_register() {
    let sleigh = make_x86_sleigh();
    // Pick a synthetic varnode for the link register — any unique
    // REGISTER offset works because the resolver only matches by Vn
    // equality, not by name.  Use offset 0x100 so it doesn't overlap
    // any real x86 register the empty region might inadvertently touch.
    let lr_like = Vn {
        size: 4,
        addr_off: 0x100, addr_space: VnSpace::REGISTER,
    };
    let region = vec![ri(0x2000, 0, branch_indirect(lr_like))];
    let res = resolve_indirect_target(
        &region,
        lr_like,
        &sleigh,
        Some(lr_like),
        None,
        strider_target::Endianness::Little,
    )
    .expect("resolver");
    assert_eq!(res, Some(ResolvedTargets::LinkRegister));
}

// ── ResolvedTargets::Single via LoadReadOnly ──────────────────────────

/// `Load reg, [const_addr]; BranchIndirect reg` with a `ReadOnlyMemory`
/// covering `const_addr` resolves to `Single(K)` where `K` is the
/// loaded value.
///
/// We can't synthesise a `Load` pcode insn by hand — its `inputs[0]`
/// encodes the target address space as a raw FFI pointer, which only
/// the SLEIGH lifter can produce safely (see comment in
/// `crates/pcode-lift/tests/value_lifter.rs` for the full reasoning).
/// So we lift real x86 bytes for `mov eax, [0x4000]; jmp eax` and pull
/// the resulting pcode insns out as our `RegionInstruction` slice.
#[test]
fn resolves_rodata_load_to_single() {
    // 0xA1 imm32       — `mov eax, [imm32]` (absolute load into EAX)
    // 0xFF 0xE0        — `jmp eax`
    let bytes: Vec<u8> = vec![0xA1, 0x00, 0x40, 0x00, 0x00, 0xFF, 0xE0];
    let reader = BufMemReader::new(bytes, 0x1000);
    let mut sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("create x86 Sleigh");
    let region = lift_region(&mut sleigh, 0x1000, 7);
    let target = find_branch_indirect_target(&region);
    // Tiny ROM: covers a single 4-byte read at addr 0x4000 returning
    // 0xcafe_babe; everything else is None.
    let rom = MockRom::limited(0x4000, 4, 0xcafe_babe);
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        Some(&rom),
        strider_target::Endianness::Little,
    )
    .expect("resolver");
    assert_eq!(res, Some(ResolvedTargets::Single(0xcafe_babe)));
}

// ── Unresolved paths (`Ok(None)`, no error) ───────────────────────────

/// Same load shape as `resolves_rodata_load_to_single` but no ROM →
/// the resolver cannot fold the load, so target is `Ok(None)`.
#[test]
fn unknown_memory_returns_ok_none() {
    let bytes: Vec<u8> = vec![0xA1, 0x00, 0x40, 0x00, 0x00, 0xFF, 0xE0];
    let reader = BufMemReader::new(bytes, 0x1000);
    let mut sleigh = rsleigh::Sleigh::new(
        rsleigh::sla_spec::SLA_SPEC_X86,
        rsleigh::pspec::PSPEC_X86,
        reader,
    )
    .expect("create x86 Sleigh");
    let region = lift_region(&mut sleigh, 0x1000, 7);
    let target = find_branch_indirect_target(&region);
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("soft contract: cfg-time resolver returns Ok rather than Err on unresolved");
    assert!(res.is_none(), "got {res:?}");
}

// ── Helpers used by the `Load`-via-real-bytes tests ──────────────────

/// Lift every machine instruction in the byte buffer between
/// `[start, start + total_len)` and concatenate every produced pcode
/// op into a single `Vec<RegionInstruction>` in program order.
fn lift_region<R: rsleigh::MemReader>(
    sleigh: &mut rsleigh::Sleigh<R>,
    start: u64,
    total_len: u64,
) -> Vec<RegionInstruction> {
    let mut out = Vec::new();
    let mut cur = start;
    while cur < start + total_len {
        let lift = sleigh.lift_one(cur).expect("lift_one");
        for (i, insn) in lift.insns.iter().enumerate() {
            out.push(RegionInstruction {
                addr: PcodeInsnAddr { machine_addr: MachineInsnAddr::from(cur), insn_index: i as u64 },
                insn: insn.clone(),
            });
        }
        cur += lift.machine_insn_len as u64;
    }
    out
}

/// Locate the first `BranchIndirect` in `region` and return its
/// `inputs[0]` (the dispatch-target varnode).  Panics if absent.
fn find_branch_indirect_target(region: &[RegionInstruction]) -> Vn {
    region
        .iter()
        .find_map(|ri| {
            if ri.insn.opcode == Opcode::BranchIndirect {
                Some(ri.insn.inputs[0])
            } else {
                None
            }
        })
        .expect("region has no BranchIndirect")
}

/// `BranchIndirect reg` with no prior write to `reg` and no
/// link-register classification → `Ok(None)`.  The producer is
/// `InitialVar(reg)` but `cc_link_register_vn` is `None`, so the
/// LinkRegister arm doesn't fire and cfg-time resolver defers to the
/// strider-level outer loop.
#[test]
fn runtime_input_returns_ok_none() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region = vec![ri(0x1000, 0, branch_indirect(target))];
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("soft contract: Ok(None) on unresolved");
    assert!(res.is_none(), "got {res:?}");
}

/// Empty region (no instructions before the BranchIndirect) →
/// `Ok(None)`.  Equivalent to `runtime_input_returns_ok_none`
/// but pinning the empty-region path explicitly.
#[test]
fn empty_region_returns_ok_none() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region: Vec<RegionInstruction> = Vec::new();
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("soft contract: Ok(None) on unresolved");
    assert!(res.is_none(), "got {res:?}");
}

/// Malformed BranchIndirect: caller looks up `inputs[0]` ahead of
/// passing the target VN to the resolver, so an empty-input
/// BranchIndirect cannot reach the resolver.  This test pins that the
/// resolver itself does NOT panic on malformed shapes by passing an
/// otherwise-valid `target_vn` together with a region whose only insn
/// has no inputs/output.  The `BranchIndirect` arm of `ValueLifter::lift`
/// returns `Ok(false)` so lifting stops; resolution proceeds but the
/// target VN is never written → `Ok(None)`.
///
/// In production, the BranchIndirect-without-target check in
/// `RegionBuilder::process_new_insn` short-circuits before we ever call
/// the resolver — see
/// `crates/cfg/src/cfg/builder/region_builder.rs`.
#[test]
fn malformed_branch_indirect_returns_ok_none() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region = vec![ri(
        0x1000,
        0,
        Insn {
            opcode: Opcode::BranchIndirect,
            output: None,
            inputs: vec![].into(),
        },
    )];
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("soft contract: Ok(None) on unresolved");
    assert!(res.is_none(), "got {res:?}");
}

/// cfg-time unresolved cases return `Ok(None)`; the strict-failure
/// semantic lives at the strider-level outer loop (cfg-build defers
/// the branch via `RegionTerminator::UnresolvedIndirectBranch`).
///
/// Same input shape as `runtime_input_returns_ok_none` (a bare
/// `BranchIndirect reg` with no prior write and no link-register
/// classification).
#[test]
fn cfg_time_unresolved_returns_ok_none() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region = vec![ri(0x1000, 0, branch_indirect(target))];
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("soft contract: cfg-time resolver returns Ok rather than Err on unresolved");
    assert!(
        res.is_none(),
        "unresolvable target must produce Ok(None), got {res:?}"
    );
}

/// cfg-time resolved cases return `Ok(Some(ResolvedTargets))`.  Same
/// input shape as `resolves_direct_const_to_single`.
#[test]
fn cfg_time_resolved_const_returns_ok_some_single() {
    let sleigh = make_x86_sleigh();
    let target = reg4(0);
    let region = vec![
        ri(
            0x1000,
            0,
            Insn {
                opcode: Opcode::Copy,
                output: Some(target),
                inputs: vec![const_vn(0xdead_beef, 4)].into(),
            },
        ),
        ri(0x1004, 0, branch_indirect(target)),
    ];
    let res = resolve_indirect_target(
        &region,
        target,
        &sleigh,
        None,
        None,
        strider_target::Endianness::Little,
    )
    .expect("resolver");
    assert_eq!(res, Some(ResolvedTargets::Single(0xdead_beef)));
}

