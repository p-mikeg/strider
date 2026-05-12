//! Tests for [`cfg::Builder::with_known_targets`] — the feedback path
//! the strider fixed-point orchestrator uses to thread IR-level
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
/// constant write — cfg-time resolver's mini-graph cannot classify it, and the
/// CFG builder produces `RegionTerminator::UnresolvedIndirectBranch`.
fn build_unresolved_jmp_rax_cfg() -> cfg::Cfg<BufMemReader<Vec<u8>>> {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();
    Builder::for_arch(&arch, sleigh, base, opts)
        .build()
        .expect("build")
}

fn locate_unresolved_addr(cfg: &cfg::Cfg<BufMemReader<Vec<u8>>>) -> PcodeInsnAddr {
    for region_id in cfg.region_ids() {
        let region = cfg.graph().node_weight(region_id).expect("region");
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
    assert!(addr.machine_addr_u64() >= 0x1000);
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
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();

    let mut known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    known.insert(unresolved_addr, ResolvedTargets::LinkRegister);

    let cfg_v2 = Builder::for_arch(&arch, sleigh, base, opts)
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
fn with_known_targets_empty_map_falls_through_to_cfg_time() {
    // Pinning that an empty `known_targets` map is equivalent to not
    // calling `with_known_targets` at all.  Defends against accidental
    // "always classify" behaviour from a future refactor.
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8];
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = OptionsBuilder::new().build();

    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .with_known_targets(HashMap::new())
        .build()
        .expect("build with empty known_targets");
    // cfg-time still can't classify `jmp rax`; the terminator is
    // UnresolvedIndirectBranch.
    let mut had_unresolved = false;
    for region in cfg.regions() {
        if matches!(region.terminator, RegionTerminator::UnresolvedIndirectBranch { .. }) {
            had_unresolved = true;
        }
    }
    assert!(had_unresolved);
}

/// Regression: when the orchestrator feeds back a `Multiple` resolution
/// where one of the targets lies outside the function range, the cfg
/// builder must defer the whole site to `UnresolvedIndirectBranch`
/// rather than hard-failing — `Switch` has no per-target tail-call
/// escape, so encoding mixed in-range / tail-call targets in a single
/// Switch terminator would misroute the OOB cases.  Pre-fix the
/// builder bailed with "could not be statically resolved" even though
/// IR-level indirect-branch resolver had already resolved the targets.
#[test]
fn known_multiple_with_out_of_range_target_defers_to_unresolved() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0xccu8, 16));
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    // Cap the function range so 0x9000 lies outside.
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();

    // Locate the BranchIndirect address by first building without
    // overrides.
    let cfg_v1 = {
        let reader2 = BufMemReader::new(vec![0xff, 0xe0u8], base);
        let sleigh2 = Sleigh::new(arch.sla_spec(), arch.pspec(), reader2).expect("sleigh");
        Builder::for_arch(&arch, sleigh2, base, OptionsBuilder::new().build())
            .build()
            .expect("v1 build")
    };
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    // Feed a Multiple with one in-range (0x1004) and one out-of-range
    // (0x9000) target.  The cfg builder must surface
    // UnresolvedIndirectBranch, NOT a Switch with the OOB target.
    let mut known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    known.insert(
        unresolved_addr,
        ResolvedTargets::Multiple(vec![0x1004, 0x9000]),
    );

    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .with_known_targets(known)
        .build()
        .expect("build must succeed; mixed Multiple defers via UnresolvedIndirectBranch");

    let mut had_unresolved = false;
    let mut had_switch = false;
    for region in cfg.regions() {
        match &region.terminator {
            RegionTerminator::UnresolvedIndirectBranch { .. } => had_unresolved = true,
            RegionTerminator::Switch { .. } => had_switch = true,
            _ => {}
        }
    }
    assert!(
        had_unresolved && !had_switch,
        "Multiple with an OOB target must defer via UnresolvedIndirectBranch, not emit a Switch"
    );
}

/// Companion: a `Multiple` whose targets are *all* in-range produces a
/// Switch terminator without bailing.  Pin that the OOB-detection
/// logic is gated on actual range, not always-defer.
#[test]
fn known_multiple_in_range_targets_produces_switch() {
    let base = 0x1000u64;
    let mut bytes = vec![0xff, 0xe0u8]; // jmp rax
    bytes.extend(std::iter::repeat_n(0x90u8, 32)); // pad with NOPs
    bytes.push(0xc3); // ret at the end so each target decodes
    let reader = BufMemReader::new(bytes, base);
    let arch = target::SleighArch::x86_64();
    let sleigh = Sleigh::new(arch.sla_spec(), arch.pspec(), reader).expect("sleigh");
    let opts = OptionsBuilder::new().set_function_max_size(0x100).build();

    let cfg_v1 = build_unresolved_jmp_rax_cfg();
    let unresolved_addr = locate_unresolved_addr(&cfg_v1);

    // Both targets land at NOPs that fall through to ret — both
    // in-range relative to the 0x100 limit.
    let mut known: HashMap<PcodeInsnAddr, ResolvedTargets> = HashMap::new();
    known.insert(
        unresolved_addr,
        ResolvedTargets::Multiple(vec![0x1004, 0x1008]),
    );

    let cfg = Builder::for_arch(&arch, sleigh, base, opts)
        .with_known_targets(known)
        .build()
        .expect("build with in-range Multiple must succeed");

    let mut had_switch = false;
    for region in cfg.regions() {
        if matches!(region.terminator, RegionTerminator::Switch { .. }) {
            had_switch = true;
        }
    }
    assert!(had_switch, "in-range Multiple must produce a Switch terminator");
}
