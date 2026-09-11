//! End-to-end lift against a toolchain-produced ET_REL object file.
//!
//! ET_REL has no PT_LOAD program headers, so the loader walks sections
//! instead.  A `.o` commonly hosts several sections at VMA 0 pre-link
//! (`.text` holding `tzcount`, `.text.startup` holding `main`), which
//! `strider_reader::elf::ElfSectionLayout` rebases apart; without the section
//! dispatch the memory map comes out empty.

use object::{Object, ObjectSymbol};
use strider_cfg::{Builder, CfgOptions};
use strider_target::SleighArch;

#[test]
fn et_rel_x64_object_file_lifts_tzcount_into_a_cfg() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/out/x64/tzcount.o");
    if !path.exists() {
        // A missing fixture must be VISIBLE: a silent return reports as a pass
        // and this file is the only coverage the ET_REL loader has.
        eprintln!(
            "SKIP {}: {} is not built; run `make -C fixtures`",
            module_path!(),
            path.display()
        );
        return;
    }

    let obj = strider_reader::load_elf(&path).expect("load tzcount.o");
    let obj = obj.file();
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
