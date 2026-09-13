//! Bytes that hold no instruction, reached by a direct branch or past a call,
//! are a result, not an error: the rest of the function lifts and the address
//! is reported.

mod common;

const BASE: u64 = 0x1000;

fn be_words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_be_bytes()).collect()
}

fn analyze(words: &[u32]) -> strider_orchestrator::AnalyzeResult {
    let (mut strider, cc) =
        common::strider_over_bytes(common::Arch::Ppc32be, be_words(words), BASE, None);
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(u64::try_from(words.len() * 4).expect("size")),
            ..Default::default()
        },
        ..Default::default()
    };
    strider
        .analyze(BASE, &cc, &lift_opts, &Default::default(), None)
        .expect("bytes that hold no instruction must not fail the whole function")
}

fn reported(result: &strider_orchestrator::AnalyzeResult) -> Vec<u64> {
    result
        .undecodable_branch_targets
        .iter()
        .map(|a| a.machine_addr.addr)
        .collect()
}

#[test]
fn a_branch_to_glibcs_ppc_abort_word_is_reported_not_raised() {
    // 0x1000 beq 0x1008 ; 0x1004 blr ; 0x1008 .long 0
    let result = analyze(&[0x4182_0008, 0x4e80_0020, 0]);
    assert_eq!(reported(&result), vec![0x1008]);
    assert!(!result.is_complete());
}

#[test]
fn a_traceback_table_after_a_call_ends_the_call() {
    // 0x1000 bl 0x1100 ; 0x1004 li r3,0 (the TOC-restore slot) ; 0x1008 .long 0
    let result = analyze(&[0x4800_0101, 0x3860_0000, 0]);
    assert_eq!(reported(&result), vec![0x1008]);
    assert!(!result.is_complete());
}

#[test]
fn data_after_a_nop_after_a_call_lifts() {
    // 0x1000 call 0x2005 ; 0x1005 nop ; 0x1006 data (decodes as `ret`)
    let bytes = vec![0xe8, 0x00, 0x10, 0x00, 0x00, 0x90, 0xc3];
    let (mut strider, cc) = common::strider_over_bytes(common::Arch::X64, bytes, BASE, None);
    let lift_opts = strider_orchestrator::LiftOptions {
        cfg: strider_cfg::CfgOptions {
            fn_max_size: Some(7),
            data_ranges: strider_cfg::DataRanges::new(std::iter::once(0x1006..0x1007)),
            ..Default::default()
        },
        ..Default::default()
    };
    let result = strider
        .analyze(BASE, &cc, &lift_opts, &Default::default(), None)
        .expect("a stub reached only through an empty region still lifts");
    assert_eq!(reported(&result), vec![0x1006]);
}
