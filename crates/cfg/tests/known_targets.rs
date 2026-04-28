//! Tests for [`cfg::Builder::with_known_targets`] — the feedback path
//! the strider fixed-point orchestrator uses to thread tier-2
//! resolution results into a CFG rebuild.
//!
//! Each test:
//!   1. Builds a CFG with no `known_targets` and observes that the
//!      offending `BranchIndirect` defers to
//!      `RegionTerminator::UnresolvedIndirectBranch`.
//!   2. Rebuilds with `known_targets` populated for that pcode
//!      address, asserts the terminator changes accordingly.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use cfg::{Builder, OptionsBuilder, PcodeInsnAddr, RegionTerminator, ResolvedTargets};
use rsleigh::Sleigh;
use rsleigh::mem_readers::BufMemReader;
use std::collections::HashMap;

/// x86_64: `jmp rax`.  RAX is a function-entry value with no
/// constant write — tier 1's mini-graph cannot classify it, and the
/// CFG builder produces `RegionTerminator::UnresolvedIndirectBranch`.
fn build_unresolved_jmp_rax_cfg() -> cfg::Cfg<BufMemReader<Vec<u8>>> {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();
    Builder::with_endianness(sleigh, base, opts, arch.endianness)
        .build()
        .expect("build")
}

fn locate_unresolved_addr(cfg: &cfg::Cfg<BufMemReader<Vec<u8>>>) -> PcodeInsnAddr {
    for region_id in cfg.region_ids() {
        let region = cfg.graph.node_weight(region_id).expect("region");
        if let RegionTerminator::UnresolvedIndirectBranch { addr, .. } = &region.terminator {
            return *addr;
        }
    }
    panic!("CFG has no UnresolvedIndirectBranch region");
}

#[test]
fn with_known_targets_default_is_unresolved() {
    let cfg = build_unresolved_jmp_rax_cfg();
    let addr = locate_unresolved_addr(&cfg);
    assert!(addr.machine_addr.addr >= 0x1000);
}

#[test]
fn with_known_targets_link_register_overrides_to_return() {
    // Pre-classify the `jmp rax` as `LinkRegister`; the builder must
    // emit `RegionTerminator::Return` instead of UnresolvedIndirectBranch.
    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    // Build the same CFG with the addr pre-resolved to LinkRegister.
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();

    let mut known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    known.insert(unresolved_addr, ResolvedTargets::LinkRegister);

    let cfg_v2 = Builder::with_endianness(sleigh, base, opts, arch.endianness)
        .with_known_targets(known)
        .build()
        .expect("build with known_targets");

    // No UnresolvedIndirectBranch left; the matching region is now
    // a Return.
    let mut had_return = false;
    for region in cfg_v2.regions() {
        if matches!(region.terminator, RegionTerminator::Return) {
            had_return = true;
        }
        assert!(
            !matches!(region.terminator, RegionTerminator::UnresolvedIndirectBranch { .. }),
            "with_known_targets must override UnresolvedIndirectBranch",
        );
    }
    assert!(had_return, "expected at least one Return region");
}

#[test]
fn with_known_targets_empty_map_falls_through_to_tier_1() {
    // Pinning that an empty `known_targets` map is equivalent to not
    // calling `with_known_targets` at all.  Defends against accidental
    // "always classify" behaviour from a future refactor.
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec, arch.pspec, reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();

    let cfg = Builder::with_endianness(sleigh, base, opts, arch.endianness)
        .with_known_targets(HashMap::new())
        .build()
        .expect("build with empty known_targets");
    // Tier-1 still can't classify `jmp rax`; the terminator is
    // UnresolvedIndirectBranch.
    let mut had_unresolved = false;
    for region in cfg.regions() {
        if matches!(region.terminator, RegionTerminator::UnresolvedIndirectBranch { .. }) {
            had_unresolved = true;
        }
    }
    assert!(had_unresolved);
}
