//! Lifter facade tests.
//!
//! These verify the per-region Lifter API wraps the underlying
//! `cfg::Builder` + `DecodeCache` without changing behavior.  The
//! Lifter is a thin per-region facade callers can query when they want
//! lifting driven region-by-region; the on-demand-reachable-cached
//! decoding behavior comes from the underlying CFG builder.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

use strider_lift::cfg::Region;
use strider_lift::lifter::Lifter;
use strider_target::{CallingConvention, SleighArch};

/// Resolve a fixture binary's path.  Mirrors
/// `crates/strider/tests/common/mod.rs::binary_path`.
fn fixture_path(arch: &str, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch)
        .join(format!("{case}.elf"))
}

/// Build a `Lifter` for a named symbol in an x86 ELF fixture.  Panics
/// with an actionable message if the fixture is missing — same
/// `make -C fixtures` instruction as the per-arch test harness.
fn lifter_for(
    case: &str,
    fn_name: &str,
) -> (
    Lifter<strider_reader::ElfFileMemReader>,
    u64,
) {
    let path = fixture_path("x86", case);
    if !path.exists() {
        panic!(
            "missing test binary {path:?}; run `make -C fixtures` (or \
             `make -C fixtures ARCH=x86 CASE={case}` for just this case)"
        );
    }
    let obj =
        strider_reader::load_elf(&path).unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let arch = SleighArch::x86();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let cc = CallingConvention::x86_cdecl()
        .build(&regs)
        .expect("build cc");
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();
    let lifter = Lifter::new(mem, arch, cc, addr).expect("Lifter::new");
    (lifter, addr)
}

#[test]
fn lifter_caches_regions_arc_eq() {
    let (mut lifter, entry) = lifter_for("arithmetic", "add");
    let r1: std::sync::Arc<Region> = lifter.region(entry).expect("first region lookup");
    let r2: std::sync::Arc<Region> = lifter.region(entry).expect("second region lookup");
    assert!(
        std::sync::Arc::ptr_eq(&r1, &r2),
        "repeated Lifter::region({entry:#x}) should return the same Arc allocation"
    );

    // After one CFG build, each reachable machine address was lifted
    // exactly once — the cache count equals the total lift-call count.
    let stats = lifter.decode_stats();
    assert!(
        stats.unique_addresses > 0,
        "expected at least one decoded address, got {stats:?}"
    );
    assert_eq!(
        stats.unique_addresses, stats.total_lift_calls,
        "no address should have been decoded more than once (stats = {stats:?})"
    );
}

#[test]
fn lifter_returns_distinct_regions_for_distinct_addresses() {
    // `control.elf::sum_to_n` has a loop (header + body + exit) which
    // guarantees ≥2 regions starting at *distinct machine addresses*
    // (not just distinct pcode-index sub-regions within one machine
    // insn).  Two distinct region start addresses should produce
    // distinct Arc allocations.
    let (mut lifter, entry) = lifter_for("control", "sum_to_n");
    let entry_region = lifter.region(entry).expect("entry region");

    // Walk the entry region's pcode insns to find a successor BB's
    // start address — any address that is *outside* the entry region's
    // own instruction range qualifies.  We pick the first such address
    // from a CondBranch / Branch by introspecting the terminator's
    // successors via the CFG; failing that, fall back to scanning the
    // CFG for any other region.
    let other_start = find_other_region_start(&mut lifter, entry, entry_region.start_addr)
        .unwrap_or_else(|| {
            let all: Vec<_> = lifter.region_starts().collect();
            panic!(
                "control.elf::abs_val should have at least one non-entry region; \
                 entry_region.start_addr={:?}, all region starts={all:?}",
                entry_region.start_addr
            );
        });

    let other_region = lifter
        .region(other_start)
        .expect("non-entry region lookup");
    assert!(
        !std::sync::Arc::ptr_eq(&entry_region, &other_region),
        "distinct region addresses should produce distinct Arc allocations"
    );
    assert_ne!(entry_region.start_addr, other_region.start_addr);
}

/// Find a region start address whose **machine address** differs from
/// the entry's by walking the Lifter's `region_starts()` view.  Helper
/// for the second test: we don't care WHICH non-entry region we pick,
/// only that one with a distinct machine address exists.  Filtering
/// on machine address (not the full `PcodeInsnAddr`) avoids a false
/// match against another sub-region within the same machine
/// instruction — `Lifter::region(addr: u64)` is keyed on machine
/// address, so two sub-regions sharing one machine addr can't be
/// disambiguated through that API anyway.
fn find_other_region_start(
    lifter: &mut Lifter<strider_reader::ElfFileMemReader>,
    entry_addr: u64,
    entry_start: strider_lift::cfg::PcodeInsnAddr,
) -> Option<u64> {
    // Ensure the CFG is built (region(entry_addr) does this lazily).
    let _ = lifter.region(entry_addr).ok()?;
    let entry_machine = entry_start.machine_addr_u64();
    for start in lifter.region_starts() {
        if start.machine_addr_u64() != entry_machine {
            return Some(start.machine_addr_u64());
        }
    }
    None
}
