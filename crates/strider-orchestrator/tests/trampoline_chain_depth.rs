//! One resolve round seats one LEVEL of discovery, so a chain of trampolines
//! needs one round per link and the iteration cap is the depth limit.
//!
//! Pins both halves of that: a chain within the cap converges on every link,
//! and one past it comes back as a report rather than an error.

use rsleigh::mem_readers::BufMemReader;
use strider_orchestrator::opt::OptOptions;
use strider_orchestrator::{LiftOptions, Strider};

const BASE: u64 = 0x40_0000;
/// `mov eax, <next>` (5) + `jmp rax` (2).
const LINK: u64 = 7;

/// `links` trampolines, each jumping to the next, ending on a `ret`.
fn chain(links: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((links * LINK + 1) as usize);
    for i in 0..links {
        let next = u32::try_from(BASE + (i + 1) * LINK).expect("32-bit immediate");
        bytes.push(0xb8);
        bytes.extend_from_slice(&next.to_le_bytes());
        bytes.extend_from_slice(&[0xff, 0xe0]);
    }
    bytes.push(0xc3);
    bytes
}

/// The five report channels, so a failure says which one fired.
fn caveats(result: &strider_orchestrator::AnalyzeResult) -> String {
    format!(
        "unresolved {:?}, unverified {:?}, isa {:?}, interior {:?}, unmapped {:?}",
        result.unresolved_indirect_branches,
        result.unverified_seeded_sites,
        result.isa_mode_conflicts,
        result.interior_branch_targets,
        result.unmapped_branch_targets,
    )
}

fn analyze(links: u64) -> strider_orchestrator::AnalyzeResult {
    let arch = strider_target::SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(
        arch.sla_spec(),
        arch.pspec(),
        BufMemReader::new(chain(links), BASE),
    )
    .expect("sleigh");
    let cc = strider_target::CallingConvention::x86_64_systemv()
        .build(&sleigh.regs().expect("regs"))
        .expect("cc");
    let mut strider = Strider::new(arch, sleigh, None).expect("Strider::new");
    strider
        .analyze(
            BASE,
            &cc,
            &LiftOptions::default(),
            &OptOptions::default(),
            None,
        )
        .expect("a chain deeper than the cap is a result, not an error")
}

#[test]
fn every_link_of_a_chain_within_the_cap_is_seated() {
    let links = 8;
    let result = analyze(links);
    assert!(result.is_complete(), "{}", caveats(&result));
    for i in 0..=links {
        assert!(
            result
                .cfg
                .regions()
                .any(|r| r.start_addr.machine_addr.addr == BASE + i * LINK),
            "link {i} never got a region",
        );
    }
}

/// The cap is 256 seated levels: the last chain that fits converges, and the
/// first that does not reports its frontier instead of failing the function.
#[test]
fn the_iteration_cap_is_the_chain_depth_it_seats() {
    let fits = analyze(256);
    assert!(fits.is_complete(), "{}", caveats(&fits));

    let over = analyze(257);
    assert!(!over.is_complete());
    assert_eq!(
        over.unresolved_indirect_branches
            .iter()
            .map(|a| a.machine_addr.addr)
            .collect::<Vec<_>>(),
        vec![BASE + 256 * LINK + 5],
        "the frontier `jmp rax` is the one still growing when the cap ran out",
    );
}
