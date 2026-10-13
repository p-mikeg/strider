//! A return instruction is a jump to whatever its target register or stack
//! slot holds. Only one that jumps to the address the function was entered
//! with is a return; any other is an indirect branch and must be reported.

mod common;

use common::Arch;

const BASE: u64 = 0x10_000;

fn le32(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn be32(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

fn le16(words: &[u16]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn analyze(arch: Arch, bytes: Vec<u8>) -> strider_orchestrator::AnalyzeResult {
    analyze_with(arch, bytes, &Default::default())
}

fn analyze_with(
    arch: Arch,
    bytes: Vec<u8>,
    opt_opts: &strider_opt::OptOptions,
) -> strider_orchestrator::AnalyzeResult {
    let size = bytes.len() as u64;
    // The entry's low bit selects Thumb.
    let entry = if matches!(arch, Arch::ArmThumb) {
        BASE | 1
    } else {
        BASE
    };
    let (mut strider, cc) = common::strider_over_bytes(arch, bytes, BASE, None);
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(size),
            ..Default::default()
        },
        ..Default::default()
    };
    strider
        .analyze(entry, &cc, &lift_opts, opt_opts, None)
        .expect("analyze")
}

/// The sound floor: every claim about the program cleared.
fn no_assumptions() -> strider_opt::OptOptions {
    strider_opt::OptOptions {
        assumptions: strider_opt::AssumptionOptions::none(),
        ..Default::default()
    }
}

/// The return at `BASE + ret_offset` is reported under
/// `AssumptionOptions::none()`, and passes for lack of that floor under the
/// defaults.
fn assert_reported_only_without_assumptions(
    arch: Arch,
    bytes: Vec<u8>,
    ret_offset: u64,
    what: &str,
) {
    let floor = analyze_with(arch, bytes.clone(), &no_assumptions());
    assert_eq!(
        unresolved(&floor),
        vec![BASE + ret_offset],
        "{what}: the return rests on a cleared claim"
    );
    assert!(!floor.is_complete(), "{what}");
    assert!(
        analyze(arch, bytes).is_complete(),
        "{what}: the default claim is what lets it pass"
    );
}

fn seeded(result: &strider_orchestrator::AnalyzeResult) -> Vec<u64> {
    result
        .unverified_seeded_sites
        .iter()
        .map(|a| a.machine_addr.addr)
        .collect()
}

fn unresolved(result: &strider_orchestrator::AnalyzeResult) -> Vec<u64> {
    result
        .unresolved_indirect_branches
        .iter()
        .map(|a| a.machine_addr.addr)
        .collect()
}

/// The return at `BASE + ret_offset` is reported, and nothing else is.
fn assert_reported(arch: Arch, bytes: Vec<u8>, ret_offset: u64, what: &str) {
    let result = analyze(arch, bytes);
    assert_eq!(
        unresolved(&result),
        vec![BASE + ret_offset],
        "{what}: the return is an indirect branch"
    );
    assert!(!result.is_complete(), "{what}");
}

fn assert_complete(arch: Arch, bytes: Vec<u8>, what: &str) {
    let result = analyze(arch, bytes);
    assert!(
        result.is_complete(),
        "{what}: a genuine return is complete; unresolved {:x?}",
        unresolved(&result)
    );
}

#[test]
fn x86_64_returns_through_a_rewritten_slot_are_reported() {
    // push %rsi ; ret
    assert_reported(Arch::X64, vec![0x56, 0xc3], 1, "push %rsi; ret");
    // mov %rsi,(%rsp) ; ret
    assert_reported(
        Arch::X64,
        vec![0x48, 0x89, 0x34, 0x24, 0xc3],
        4,
        "mov %rsi,(%rsp); ret",
    );
}

#[test]
fn x86_64_genuine_returns_stay_complete() {
    // push %rbx ; call ; pop %rbx ; ret
    assert_complete(
        Arch::X64,
        vec![0x53, 0xe8, 0xfa, 0x0f, 0x00, 0x00, 0x5b, 0xc3],
        "a callee-saved push around a call",
    );
    // push $0 ; add $8,%rsp ; ret $8
    assert_complete(
        Arch::X64,
        vec![0x6a, 0x00, 0x48, 0x83, 0xc4, 0x08, 0xc2, 0x08, 0x00],
        "ret imm16 past a discarded push",
    );
}

#[test]
fn arm_returns_through_a_rewritten_link_register_are_reported() {
    // mov lr, r1 ; bx lr
    assert_reported(
        Arch::Arm,
        le32(&[0xe1a0_e001, 0xe12f_ff1e]),
        4,
        "mov lr, r1; bx lr",
    );
    // push {r4, lr} ; str r1, [sp, #4] ; pop {r4, pc}
    assert_reported(
        Arch::Arm,
        le32(&[0xe92d_4010, 0xe58d_1004, 0xe8bd_8010]),
        8,
        "the saved lr slot overwritten",
    );
    // Thumb: mov lr, r1 ; bx lr
    assert_reported(
        Arch::ArmThumb,
        le16(&[0x468e, 0x4770]),
        2,
        "Thumb mov lr, r1; bx lr",
    );
}

#[test]
fn arm_a_single_register_pop_into_the_pc_is_a_return() {
    // push {lr} ; bl ; pop {pc}: a `BranchIndirect` over the stack slot, which
    // the call between the two keeps from folding back to the link register.
    let result = analyze(Arch::Arm, le32(&[0xe52d_e004, 0xeb00_03fd, 0xe49d_f004]));
    assert_eq!(
        unresolved(&result),
        Vec::<u64>::new(),
        "ldr pc, [sp], #4 with the saved lr intact across a call is a return",
    );

    // Nothing between the push and the pop: the slot is proven, not claimed.
    assert_complete(
        Arch::Arm,
        le32(&[0xe52d_e004, 0xe49d_f004]),
        "ldr pc, [sp], #4 straight off the push",
    );
}

/// The slot surviving the call rests on
/// `return_slots_survive_untracked_writes`, so the seat it decides is named on
/// the unverified channel: the answer is one target, but nothing proved it.
#[test]
fn arm_a_pop_into_the_pc_seated_on_a_claim_is_reported_as_unverified() {
    let result = analyze(Arch::Arm, le32(&[0xe52d_e004, 0xeb00_03fd, 0xe49d_f004]));
    assert_eq!(
        seeded(&result),
        vec![BASE + 8],
        "the proof passed a call on a claim",
    );
    assert!(!result.is_complete());
    // A return the lifter seated carries no claimed CFG edge, so the identical
    // claim on an ordinary `pop {r4, pc}` stays off the channel.
    assert_eq!(
        seeded(&analyze(
            Arch::Arm,
            le32(&[0xe92d_4010, 0xeb00_03fd, 0xe8bd_8010]),
        )),
        Vec::<u64>::new(),
    );
}

#[test]
fn arm_a_single_register_pop_into_the_pc_of_another_value_is_reported() {
    // push {r1} ; pop {pc}: the identical shape, dispatching to r1.
    assert_reported(
        Arch::Arm,
        le32(&[0xe52d_1004, 0xe49d_f004]),
        4,
        "push {r1} ; ldr pc, [sp], #4",
    );
}

#[test]
fn arm_genuine_returns_stay_complete() {
    // push {r4, lr} ; bl ; pop {r4, pc}
    assert_complete(
        Arch::Arm,
        le32(&[0xe92d_4010, 0xeb00_03fd, 0xe8bd_8010]),
        "pop {r4, pc} with the saved lr intact",
    );
    // Thumb: push {r4, lr} ; bl ; pop {r4, pc}
    assert_complete(
        Arch::ArmThumb,
        le16(&[0xb510, 0xf000, 0xfffd, 0xbd10]),
        "Thumb pop {r4, pc}",
    );
    // Thumb: bx lr
    assert_complete(Arch::ArmThumb, le16(&[0x4770]), "Thumb interworking bx lr");
}

#[test]
fn aarch64_returns_through_a_rewritten_link_register_are_reported() {
    // mov x30, x1 ; ret
    assert_reported(
        Arch::Aarch64,
        le32(&[0xaa01_03fe, 0xd65f_03c0]),
        4,
        "mov x30, x1; ret",
    );
    // stp x29, x30, [sp, #-16]! ; str x1, [sp, #8] ; ldp x29, x30, [sp], #16 ; ret
    assert_reported(
        Arch::Aarch64,
        le32(&[0xa9bf_7bfd, 0xf900_07e1, 0xa8c1_7bfd, 0xd65f_03c0]),
        12,
        "the saved x30 slot overwritten",
    );
}

#[test]
fn aarch64_genuine_returns_stay_complete() {
    // stp ; bl ; ldp ; ret
    assert_complete(
        Arch::Aarch64,
        le32(&[0xa9bf_7bfd, 0x9400_03ff, 0xa8c1_7bfd, 0xd65f_03c0]),
        "ldp restoring x30 across a call",
    );
}

#[test]
fn mips_returns_through_a_rewritten_ra_are_reported() {
    // move ra, a1 ; jr ra ; nop
    assert_reported(
        Arch::Mips32be,
        be32(&[0x00a0_f825, 0x03e0_0008, 0x0000_0000]),
        4,
        "move ra, a1; jr ra",
    );
    // addiu sp,-8 ; sw ra,4(sp) ; sw a1,4(sp) ; lw ra,4(sp) ; jr ra ; addiu sp,8
    assert_reported(
        Arch::Mips32be,
        be32(&[
            0x27bd_fff8,
            0xafbf_0004,
            0xafa5_0004,
            0x8fbf_0004,
            0x03e0_0008,
            0x27bd_0008,
        ]),
        16,
        "the saved ra slot overwritten",
    );
}

#[test]
fn mips_genuine_returns_stay_complete() {
    // addiu sp,-8 ; sw ra,4(sp) ; jal ; nop ; lw ra,4(sp) ; jr ra ; addiu sp,8
    assert_complete(
        Arch::Mips32be,
        be32(&[
            0x27bd_fff8,
            0xafbf_0004,
            0x0c00_0403,
            0x0000_0000,
            0x8fbf_0004,
            0x03e0_0008,
            0x27bd_0008,
        ]),
        "jr ra with a delay slot, ra restored across a call",
    );
}

#[test]
fn ppc_returns_through_a_rewritten_link_register_are_reported() {
    // mtlr r4 ; blr
    assert_reported(
        Arch::Ppc32be,
        be32(&[0x7c88_03a6, 0x4e80_0020]),
        4,
        "mtlr r4; blr",
    );
    // mflr r0 ; stw r0,4(r1) ; stwu r1,-16(r1) ; stw r4,20(r1) ; addi r1,r1,16 ;
    // lwz r0,4(r1) ; mtlr r0 ; blr
    assert_reported(
        Arch::Ppc32be,
        be32(&[
            0x7c08_02a6,
            0x9001_0004,
            0x9421_fff0,
            0x9081_0014,
            0x3821_0010,
            0x8001_0004,
            0x7c08_03a6,
            0x4e80_0020,
        ]),
        28,
        "the saved LR slot overwritten",
    );
}

#[test]
fn ppc_genuine_returns_stay_complete() {
    // mflr ; stw ; stwu ; bl ; addi ; lwz ; mtlr ; blr
    assert_complete(
        Arch::Ppc32be,
        be32(&[
            0x7c08_02a6,
            0x9001_0004,
            0x9421_fff0,
            0x4800_0ff5,
            0x3821_0010,
            0x8001_0004,
            0x7c08_03a6,
            0x4e80_0020,
        ]),
        "blr with LR restored across a call",
    );
}

#[test]
fn x86_64_a_return_slot_rewritten_with_itself_in_a_loop_stays_complete() {
    // mov (%rsp),%rax ; mov %rax,(%rsp) ; test %edi,%edi ; jne 0 ; ret
    assert_complete(
        Arch::X64,
        vec![
            0x48, 0x8b, 0x04, 0x24, 0x48, 0x89, 0x04, 0x24, 0x85, 0xff, 0x75, 0xf4, 0xc3,
        ],
        "the return slot rewritten with its own value in a loop",
    );
}

#[test]
fn ppc_a_link_register_resaved_in_a_loop_stays_complete() {
    // mflr r0 ; stw r0,4(r1) ; loop: lwz r0,4(r1) ; mtlr r0 ; cmpwi r3,0 ; beqlr ;
    // stw r0,4(r1) ; bl ; b loop
    assert_complete(
        Arch::Ppc32be,
        be32(&[
            0x7c08_02a6,
            0x9001_0004,
            0x8001_0004,
            0x7c08_03a6,
            0x2c03_0000,
            0x4d82_0020,
            0x9001_0004,
            0x4800_1001,
            0x4bff_ffe8,
        ]),
        "a conditional blr in a loop that re-saves the LR it restored",
    );
}

#[test]
fn x86_64_a_return_slot_rewritten_in_a_loop_from_an_argument_is_reported() {
    // mov (%rsp),%rax ; test %edi,%edi ; je 1f ; mov %rsi,%rax ; 1: mov %rax,(%rsp) ;
    // test %edx,%edx ; jne 0 ; ret
    assert_reported(
        Arch::X64,
        vec![
            0x48, 0x8b, 0x04, 0x24, 0x85, 0xff, 0x74, 0x03, 0x48, 0x89, 0xf0, 0x48, 0x89, 0x04,
            0x24, 0x85, 0xd2, 0x75, 0xed, 0xc3,
        ],
        0x13,
        "the return slot takes an argument on one path around the loop",
    );
}

// netdev_upper_dev_unlink, arm/6.1-clang.
const ARM_STACK_PROTECTED_UNLINK: [u32; 14] = [
    0xe92d_4800, // push {fp, lr}
    0xe24d_d010, // sub sp, sp, #16
    0xee1d_2f70, // mrc 15, 0, r2, cr13, cr0, {3}
    0xe592_24f8, // ldr r2, [r2, #1272]
    0xe58d_200c, // str r2, [sp, #12]
    0xe1a0_200d, // mov r2, sp
    0xeb00_0006, // bl
    0xe59d_000c, // ldr r0, [sp, #12]
    0xee1d_1f70, // mrc 15, 0, r1, cr13, cr0, {3}
    0xe591_14f8, // ldr r1, [r1, #1272]
    0xe151_0000, // cmp r1, r0
    0x028d_d010, // addeq sp, sp, #16
    0x08bd_8800, // popeq {fp, pc}
    0xeb06_6be4, // bl __stack_chk_fail
];

#[test]
fn arm_a_conditional_pop_after_a_conditional_sp_adjustment_on_the_same_flags_stays_complete() {
    assert_complete(
        Arch::Arm,
        le32(&ARM_STACK_PROTECTED_UNLINK),
        "addeq sp, sp, #16 ; popeq {fp, pc}",
    );
}

#[test]
fn arm_a_conditional_pop_on_the_opposite_flags_of_the_sp_adjustment_is_reported() {
    let mut words = ARM_STACK_PROTECTED_UNLINK;
    // addne sp, sp, #16: the eq path pops pc from a slot nothing saved.
    words[11] = 0x128d_d010;
    assert_reported(Arch::Arm, le32(&words), 0x30, "addne sp ; popeq {fp, pc}");
}

#[test]
fn arm_a_conditional_pop_on_flags_recomputed_after_the_sp_adjustment_is_reported() {
    let mut words: Vec<u32> = ARM_STACK_PROTECTED_UNLINK.to_vec();
    // cmp r2, #0 between the two: popeq no longer tests addeq's flags.
    words.insert(12, 0xe352_0000);
    assert_reported(
        Arch::Arm,
        le32(&words),
        0x34,
        "addeq sp ; cmp r2, #0 ; popeq {fp, pc}",
    );
}

// tcp_skb_shift, thumb/6.1-clang: a conditional pop and tail call in one IT
// block, and a pop on the path that took neither.
const THUMB_CONDITIONAL_TAIL_CALL: [u16; 23] = [
    0xb580, // push {r7, lr}
    0xf8d0, 0xc050, // ldr.w ip, [r0, #80]
    0xf64f, 0x7ef7, // movw lr, #65527
    0xf2c0, 0x0e07, // movt lr, #7
    0x449c, // add ip, r3
    0x45f4, // cmp ip, lr
    0xd80a, // bhi.n -> movs r0, #0
    0xf8b0, 0xc020, // ldrh.w ip, [r0, #32]
    0x4462, // add r2, ip
    0xf5b2, 0x3f80, // cmp.w r2, #65536
    0xbfbe, // ittt lt
    0x461a, // movlt r2, r3
    0xe8bd, 0x4080, // ldmialt.w sp!, {r7, lr}
    0xf797, 0xb840, // blt.w skb_shift
    0x2000, // movs r0, #0
    0xbd80, // pop {r7, pc}
];

#[test]
fn thumb_a_pop_past_a_conditional_pop_on_the_flags_of_its_skipped_branch_stays_complete() {
    assert_complete(
        Arch::ArmThumb,
        le16(&THUMB_CONDITIONAL_TAIL_CALL),
        "ittt lt ; ldmialt sp!, {r7, lr} ; blt.w ; ... ; pop {r7, pc}",
    );
}

#[test]
fn thumb_a_pop_past_a_conditional_pop_on_the_opposite_flags_is_reported() {
    let mut words = THUMB_CONDITIONAL_TAIL_CALL;
    // itet lt: ldmiage pops on the path the blt does not take.
    words[15] = 0xbfb6;
    assert_reported(
        Arch::ArmThumb,
        le16(&words),
        0x2c,
        "itet lt ; ldmiage ; blt.w ; pop",
    );
}

// cycles_2_ns, x64/4.4-gcc: a DRAP frame, the stack realigned and restored
// from a pointer saved in the aligned frame.
const X86_64_DRAP_CYCLES_2_NS: [u8; 115] = [
    0x4c, 0x8d, 0x54, 0x24, 0x08, // lea 0x8(%rsp),%r10
    0x48, 0x83, 0xe4, 0xf0, // and $-16,%rsp
    0x41, 0xff, 0x72, 0xf8, // push -0x8(%r10)
    0x55, // push %rbp
    0x48, 0x89, 0xe5, // mov %rsp,%rbp
    0x41, 0x52, // push %r10
    0x48, 0x8b, 0x35, 0xc1, 0x30, 0x2b, 0x00, 0x48, 0x8b, 0x05, 0xc2, 0x30, 0x2b, 0x00, 0x48, 0x39,
    0xc6, 0x75, 0x1c, 0x8b, 0x06, 0x8b, 0x4e, 0x04, 0x48, 0xf7, 0xe7, 0x48, 0x0f, 0xad, 0xd0, 0x48,
    0xd3, 0xea, 0xf6, 0xc1, 0x40, 0x48, 0x0f, 0x45, 0xc2, 0x48, 0x03, 0x46, 0x08, 0xeb, 0x29, 0xff,
    0x46, 0x10, 0x8b, 0x06, 0x8b, 0x4e, 0x04, 0x48, 0xf7, 0xe7, 0x48, 0x0f, 0xad, 0xd0, 0x48, 0xd3,
    0xea, 0xf6, 0xc1, 0x40, 0x48, 0x0f, 0x45, 0xc2, 0x48, 0x03, 0x46, 0x08, 0xff, 0x4e, 0x10, 0x75,
    0x07, 0x48, 0x89, 0x35, 0x78, 0x30, 0x2b, 0x00, 0x41, 0x5a, // pop %r10
    0x5d, // pop %rbp
    0x49, 0x8d, 0x62, 0xf8, // lea -0x8(%r10),%rsp
    0xc3, // ret
];

#[test]
fn x86_64_a_return_through_a_realigned_frame_stays_complete() {
    assert_complete(
        Arch::X64,
        X86_64_DRAP_CYCLES_2_NS.to_vec(),
        "lea 0x8(%rsp),%r10 ; and $-16,%rsp ; ... ; lea -0x8(%r10),%rsp ; ret",
    );
}

/// lea 0x8(%rsp),%r10 ; and $-16,%rsp ; push -0x8(%r10) ; push %rbp ;
/// mov %rsp,%rbp ; push %r10 ; `clobber` ; pop %r10 ; pop %rbp ;
/// lea -0x8(%r10),%rsp ; ret
fn x86_64_drap_frame_with(clobber: &[u8]) -> Vec<u8> {
    let mut bytes = X86_64_DRAP_CYCLES_2_NS[..19].to_vec();
    bytes.extend_from_slice(clobber);
    bytes.extend_from_slice(&X86_64_DRAP_CYCLES_2_NS[107..]);
    bytes
}

#[test]
fn x86_64_a_realigned_frame_whose_saved_stack_pointer_is_overwritten_is_reported() {
    // mov %rsi,-0x8(%rbp)
    let bytes = x86_64_drap_frame_with(&[0x48, 0x89, 0x75, 0xf8]);
    let ret = bytes.len() as u64 - 1;
    assert_reported(Arch::X64, bytes, ret, "the saved %r10 slot overwritten");
}

// A DRAP frame reloading its realignment pointer after a call, which is what
// keeps the reload from folding back to the `lea`: lea 0x8(%rsp),%r10 ;
// and $-16,%rsp ; push -0x8(%r10) ; push %rbp ; mov %rsp,%rbp ; push %r10 ;
// sub $8,%rsp ; call ; mov -0x8(%rbp),%r10 ; [write] ; mov -0x8(%rbp),%r10 ;
// leave ; lea -0x8(%r10),%rsp ; ret
fn x86_64_drap_reload_after_a_call(write: &[u8]) -> Vec<u8> {
    let mut bytes = vec![
        0x4c, 0x8d, 0x54, 0x24, 0x08, 0x48, 0x83, 0xe4, 0xf0, 0x41, 0xff, 0x72, 0xf8, 0x55, 0x48,
        0x89, 0xe5, 0x41, 0x52, 0x48, 0x83, 0xec, 0x08, 0xe8, 0xfb, 0x0f, 0x00, 0x00, 0x4c, 0x8b,
        0x55, 0xf8,
    ];
    bytes.extend_from_slice(write);
    bytes.extend_from_slice(&[0x4c, 0x8b, 0x55, 0xf8, 0xc9, 0x49, 0x8d, 0x62, 0xf8, 0xc3]);
    bytes
}

#[test]
fn x86_64_a_write_through_the_reloaded_realignment_pointer_rests_on_a_claim() {
    // mov %rdi,-0x8(%r10): the reloaded pointer is the entry SP + 8, so the
    // write lands on the return slot. Only a guess reaches that address, which
    // is what the cleared claim brings in.
    let bytes = x86_64_drap_reload_after_a_call(&[0x49, 0x89, 0x7a, 0xf8]);
    let ret = bytes.len() as u64 - 1;
    assert_reported_only_without_assumptions(
        Arch::X64,
        bytes,
        ret,
        "a write through the reloaded %r10 pointer",
    );
}

#[test]
fn x86_64_a_realignment_pointer_reloaded_after_a_call_stays_complete() {
    assert_complete(
        Arch::X64,
        x86_64_drap_reload_after_a_call(&[]),
        "the same frame with nothing written through the reloaded %r10",
    );
}

#[test]
fn x86_64_a_write_through_a_pointer_loaded_from_a_global_rests_on_a_claim() {
    // push %rbx ; mov 0x20000,%rax ; mov %rdi,0x8(%rax) ; pop %rbx ; ret
    assert_reported_only_without_assumptions(
        Arch::X64,
        vec![
            0x53, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x02, 0x00, 0x48, 0x89, 0x78, 0x08, 0x5b,
            0xc3,
        ],
        14,
        "a store through a pointer held in a global",
    );
}

#[test]
fn x86_64_an_indexed_stack_write_rests_on_a_claim() {
    // sub $0x18,%rsp ; mov %rsi,(%rsp,%rdi,8) ; add $0x18,%rsp ; ret: %rdi = 3
    // puts the write on the return slot.
    assert_reported_only_without_assumptions(
        Arch::X64,
        vec![
            0x48, 0x83, 0xec, 0x18, 0x48, 0x89, 0x34, 0xfc, 0x48, 0x83, 0xc4, 0x18, 0xc3,
        ],
        12,
        "a stack write at an index no offset names",
    );
}

#[test]
fn x86_64_a_return_slot_address_passed_to_a_callee_rests_on_a_claim() {
    // push %rbx ; lea 0x8(%rsp),%rdi ; call ; pop %rbx ; ret: the callee is
    // handed the return slot's address. Without the claim every callee is taken
    // to reach the slot, so the floor reports this whether it escapes or not.
    assert_reported_only_without_assumptions(
        Arch::X64,
        vec![
            0x53, 0x48, 0x8d, 0x7c, 0x24, 0x08, 0xe8, 0xfb, 0x0f, 0x00, 0x00, 0x5b, 0xc3,
        ],
        12,
        "the return slot's address handed to a callee",
    );
}

#[test]
fn x86_64_a_realigned_frame_whose_saved_stack_pointer_may_be_overwritten_is_reported() {
    // mov %rsi,-0x28(%r10): the entry-SP slot 32 below the entry SP is the
    // saved %r10 slot when the entry SP was 8 above a 16-byte boundary.
    let bytes = x86_64_drap_frame_with(&[0x49, 0x89, 0x72, 0xd8]);
    let ret = bytes.len() as u64 - 1;
    assert_reported(
        Arch::X64,
        bytes,
        ret,
        "a store the alignment may place on the saved %r10",
    );
}
