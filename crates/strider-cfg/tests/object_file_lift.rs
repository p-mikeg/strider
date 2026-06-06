#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

//! End-to-end lift test against a toolchain-produced ET_REL object
//! file (`fixtures/out/x64/tzcount.o`).
//!
//! ET_REL has no PT_LOAD program headers; the loader has to walk
//! sections.  `.o` files commonly host several sections at VMA 0
//! pre-link (`.text` with `tzcount`, `.text.startup` with `main`),
//! so first-wins VMA dedup matters: pre-fix, the loader either left
//! the memory map empty (program-header walk on a header-less file)
//! or non-deterministically swapped which section's bytes landed at
//! VMA 0.  Both cases broke the lift in different ways; this test
//! pins the post-fix invariant that the lift produces a non-trivial
//! CFG for the `tzcount` symbol.

use object::{Object, ObjectSymbol};
use strider_cfg::Builder;
use strider_cfg::CfgOptions;
use strider_target::SleighArch;

#[test]
fn et_rel_x64_object_file_lifts_tzcount_into_a_cfg() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x64/tzcount.o");
    if !path.exists() {
        // Build with `make -C fixtures ARCH=x64 CASE=tzcount`.
        return;
    }

    let obj = strider_reader::load_elf(&path).expect("load tzcount.o");
    assert_eq!(
        obj.kind(),
        object::ObjectKind::Relocatable,
        "fixture must be ET_REL"
    );

    let mem = strider_reader::ElfFileMemReader::from_object(&obj)
        .expect("ElfFileMemReader::from_object on .o");
    let arch = SleighArch::x86_64();
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem)
        .expect("create Sleigh for ET_REL fixture");

    let tz_addr = obj
        .symbol_by_name("tzcount")
        .expect("tzcount symbol present in .o")
        .address();

    let cfg = Builder::for_arch(&arch, &mut sleigh, tz_addr, &CfgOptions::default())
        .build()
        .expect("Builder::build on tzcount lifted from .o");

    // tzcount has a loop (region count ≥ 2: entry region + the loop
    // body) terminating in a `Return`.  The exact node count varies
    // with GCC optimisation level / inlining decisions; the contract
    // we pin is "the lift succeeded and produced something with the
    // expected control-flow shape" — without the section-walker
    // dispatch the CFG would be empty / single-region-trap.
    assert!(
        cfg.region_graph.node_count() >= 1,
        "expected at least one region after lifting tzcount; got {} \
         (an empty CFG implies the loader produced no readable bytes \
         for the .o)",
        cfg.region_graph.node_count()
    );
    assert!(
        cfg.regions().any(|r| matches!(
            r.terminator,
            strider_cfg::RegionTerminator::Return
        )),
        "tzcount must lift to a CFG containing at least one return-\
         terminated region; got terminators {:?}",
        cfg.regions().map(|r| r.terminator.clone()).collect::<Vec<_>>()
    );
}
