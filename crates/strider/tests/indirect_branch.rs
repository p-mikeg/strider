//! Computed-goto fixture tests for the tier-2 indirect-branch resolver.
//!
//! `fixtures/cases/indirect_branch.c::indirect_branch_resolved` lowers
//! the indirect goto to a load from a local stack array of label
//! addresses on every supported toolchain at every optimisation level
//! we can target — gcc/clang collapse direct constant computed-gotos
//! into straight-line `mov; ret`, so the surviving lowering is
//! always: write label addresses to the local stack array, load the
//! per-iteration target from the array, branch through the loaded
//! value.
//!
//! Resolving this lowering requires **cross-region stack-load
//! forwarding** (`StackStoreDetect` + `StackLoadForward` joined
//! across the function's region graph) — round 1 of the
//! indirect-branch fixed-point design does not yet implement that
//! layer.  Tier 1's mini-graph runs `ConstantFold` + `KnownBits` on
//! a single region only and cannot prove the loaded target is one of
//! the pushed label addresses.  Tier 2 in round 1 has the
//! `LinkRegister` / `IntConst` / `Multiple-of-IntConsts` arms but
//! no stack-array-of-labels arm.
//!
//! Consequence: every arch's lifted CFG carries an
//! `UnresolvedIndirectBranch` terminator at the goto site.  This is
//! tracked under **BUG-30** in the analyzer-known-issues tracker —
//! "computed-goto via local stack-array of label addresses".  The
//! per-arch tests below are gated on BUG-30 so they remain in the
//! suite as a regression target: when round-2 wires cross-region
//! stack-load forwarding, the ignores can be lifted and the assertion
//! ("no `UnresolvedIndirectBranch` terminator survives") will start
//! holding without any test rewrite.
//!
//! See:
//!   - docs/superpowers/specs/2026-04-27-indirect-branch-fixedpoint-design.md
//!     (§"Future work" / cross-region stack analysis).
//!   - docs/superpowers/plans/2026-04-25-analyzer-known-issues.md
//!     (BUG-30 entry).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

use object::{Object, ObjectSymbol};

/// Build the CFG for `indirect_branch_resolved` with the same setup
/// `common::analyze` uses (read-only-memory + link-register threaded
/// through the cfg builder), and panic if any region still carries
/// `RegionTerminator::UnresolvedIndirectBranch` at fixed point.
///
/// Mirrors `common::analyze`'s prologue verbatim down to the
/// function-symbol lookup; only diverges by stopping at the CFG
/// instead of running `analyze_cfg`.
fn assert_no_unresolved_indirect_branch(arch: Arch) {
    let path = binary_path(arch, "indirect_branch");
    if !path.exists() {
        panic!(
            "missing test binary {path:?}; run `make -C fixtures` (or \
             `make -C fixtures ARCH={} CASE=indirect_branch` for just this case)",
            arch.name()
        );
    }
    let obj = reader::load_elf(&path)
        .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let sleigh_arch = arch.sleigh();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let ana = strider::Strider::new(sleigh_arch, regs, arch.cc())
        .expect("Strider::new");
    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, mem)
        .expect("real sleigh new");
    let raw_addr = obj
        .symbol_by_name("indirect_branch_resolved")
        .unwrap_or_else(|| panic!("symbol indirect_branch_resolved not found in {path:?}"))
        .address();
    // ARM-Thumb interworking: mask the LSB Thumb-mode marker (see
    // common::analyze for the same masking).
    let addr = match arch {
        Arch::Arm | Arch::ArmThumb => raw_addr & !1u64,
        _ => raw_addr,
    };
    let rom_for_cfg: std::sync::Arc<dyn opt::ReadOnlyMemory> = std::sync::Arc::new(
        reader::ElfFileMemReader::from_object(&obj).expect("rom reader (cfg)"),
    );
    let mut cfg_opts_b = cfg::OptionsBuilder::new()
        .allow_code_before_start_addr()
        .set_read_only_memory(rom_for_cfg);
    if let Some(lr) = ana.calling_convention().link_register_vn {
        cfg_opts_b = cfg_opts_b.set_link_register(lr);
    }
    let cfg_opts = cfg_opts_b.build();
    let cfg = cfg::Builder::with_endianness(sleigh, addr, cfg_opts, sleigh_arch.endianness)
        .build()
        .unwrap_or_else(|e| panic!("Cfg build for indirect_branch_resolved: {e:?}"));

    // Walk every region's terminator.  An `UnresolvedIndirectBranch`
    // here means tier 1's mini-graph couldn't classify a
    // `BranchIndirect` and tier 2 (in its round-1 form, lacking
    // cross-region stack-load forwarding) couldn't either.
    for region_id in cfg.region_ids() {
        let region = cfg
            .graph
            .node_weight(region_id)
            .unwrap_or_else(|| panic!("cfg has no region for id {region_id:?}"));
        if let cfg::RegionTerminator::UnresolvedIndirectBranch { addr, .. } = &region.terminator {
            panic!(
                "indirect_branch_resolved on {} has UnresolvedIndirectBranch \
                 terminator at addr {:?} — neither tier-1 nor tier-2 \
                 classified the branch (see BUG-30)",
                arch.name(),
                addr,
            );
        }
    }

    // Sanity: also lift through the strider IR pipeline so we exercise
    // the full per-arch path.  This catches any post-CFG lifting
    // regression on the indirect-branch placeholder code-path.
    let _ = analyze(arch, "indirect_branch", "indirect_branch_resolved");
}

// One #[test] per architecture.  All arches are #[ignore = "BUG-30"]
// because the load-from-stack-array lowering needs cross-region
// stack-load forwarding (round-2 work).  When that lands the per-test
// `#[ignore]` attribute can be removed individually as the resolver
// learns each arch's exact shape; the assertion body is identical
// across arches and does not need a rewrite.

#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_x86() {
    assert_no_unresolved_indirect_branch(Arch::X86);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_x64() {
    assert_no_unresolved_indirect_branch(Arch::X64);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_aarch64() {
    assert_no_unresolved_indirect_branch(Arch::Aarch64);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_aarch64be() {
    assert_no_unresolved_indirect_branch(Arch::Aarch64Be);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_arm() {
    assert_no_unresolved_indirect_branch(Arch::Arm);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_arm_be() {
    assert_no_unresolved_indirect_branch(Arch::ArmBe);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_arm_thumb() {
    assert_no_unresolved_indirect_branch(Arch::ArmThumb);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_mips32le() {
    assert_no_unresolved_indirect_branch(Arch::Mips32le);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_mips32be() {
    assert_no_unresolved_indirect_branch(Arch::Mips32be);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_mips64le() {
    assert_no_unresolved_indirect_branch(Arch::Mips64le);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_mips64be() {
    assert_no_unresolved_indirect_branch(Arch::Mips64be);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_ppc32be() {
    assert_no_unresolved_indirect_branch(Arch::Ppc32be);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_ppc32le() {
    assert_no_unresolved_indirect_branch(Arch::Ppc32le);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_ppc64be() {
    assert_no_unresolved_indirect_branch(Arch::Ppc64be);
}
#[test] #[ignore = "BUG-30: computed-goto via stack-array of label addresses needs cross-region StackLoadForward (round-2 work)"]
fn indirect_branch_resolved_ppc64le() {
    assert_no_unresolved_indirect_branch(Arch::Ppc64le);
}

