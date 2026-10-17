//! End-to-end lift against a toolchain-produced ET_REL object file.
//!
//! ET_REL has no PT_LOAD program headers, so the loader walks sections
//! instead.  A `.o` commonly hosts several sections at VMA 0 pre-link
//! (`.text` holding `tzcount`, `.text.startup` holding `main`), which
//! `strider_reader::elf::ElfSectionLayout` rebases apart; without the section
//! dispatch the memory map comes out empty.

use object::Object;
use strider_cfg::{CfgOptions, DecodeEngine};
use strider_target::SleighArch;

#[test]
fn et_rel_x64_object_file_lifts_tzcount_into_a_cfg() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out/x64/tzcount.o");
    // The only ET_REL lift coverage in strider-cfg.
    assert!(
        path.exists(),
        "{} is not built; run `make -C fixtures`",
        path.display()
    );

    let elf = strider_reader::load_elf(&path).expect("load tzcount.o");
    let obj = elf.checked_file().expect("the mapped file is unchanged");
    assert_eq!(
        obj.kind(),
        object::ObjectKind::Relocatable,
        "fixture must be ET_REL"
    );

    let mem =
        strider_reader::ElfFileMemReader::new(&elf, strider_reader::elf::RelocationMode::Unapplied)
            .expect("ElfFileMemReader::new on .o");
    let arch = SleighArch::x86_64();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem)
        .expect("create Sleigh for ET_REL fixture");

    // Through the layout, not raw `st_value`: an ET_REL symbol's `st_value` is
    // an offset into its section, and the reader seats those sections apart at
    // a synthetic base.
    let layout = strider_reader::elf::ElfSectionLayout::new(&obj);
    let tz_addr = layout.symbol_address(
        &obj.symbol_by_name("tzcount")
            .expect("tzcount symbol present in .o"),
    );

    let cfg = DecodeEngine::new(arch, sleigh)
        .expect("decode engine")
        .build_cfg(tz_addr.into(), &CfgOptions::default())
        .expect("build_cfg on tzcount lifted from .o");

    // Exact region counts move with GCC optimisation and inlining, so only
    // the control-flow shape is pinned; without the section-walker dispatch
    // the CFG comes out empty or a single-region trap.
    assert!(
        cfg.region_graph().node_count() >= 1,
        "expected at least one region after lifting tzcount; got {} \
         (an empty CFG implies the loader produced no readable bytes \
         for the .o)",
        cfg.region_graph().node_count()
    );
    assert!(
        cfg.regions()
            .any(|r| matches!(r.terminator, strider_cfg::RegionTerminator::Return)),
        "tzcount must lift to a CFG containing at least one return-\
         terminated region; got terminators {:?}",
        cfg.regions()
            .map(|r| r.terminator.clone())
            .collect::<Vec<_>>()
    );
}
