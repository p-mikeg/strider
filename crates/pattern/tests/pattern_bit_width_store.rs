//! StorePat::bit_width(n) filters Stores by data width.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use ir::node::NodeOutputType;
use pattern::{Matcher, int_const, store};

#[test]
fn bit_width_filters_store_by_data_width() {
    // Two Stores with different data widths.  Both are reachable
    // because both sit on the memory chain ending at Return.
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::U64)
        .expect("addr1");
    let v32 = b
        .build_int_const(1u64, NodeOutputType::U32)
        .expect("v32");
    b.build_store(addr1, v32, rsleigh::VnSpace::RAM)
        .expect("u32 store");
    let addr2 = b
        .build_int_const(0x108u64, NodeOutputType::U64)
        .expect("addr2");
    let v64 = b
        .build_int_const(2u64, NodeOutputType::U64)
        .expect("v64");
    b.build_store(addr2, v64, rsleigh::VnSpace::RAM)
        .expect("u64 store");
    b.build_return(None, &[]).expect("ret");
    let fg = b.build().expect("build");

    let m = Matcher::new(&fg);
    let h32 = m.find_all(&store().addr(int_const(0x100u64)).bit_width(32).into());
    let h64 = m.find_all(&store().addr(int_const(0x108u64)).bit_width(64).into());
    assert_eq!(h32.len(), 1);
    assert_eq!(h64.len(), 1);
    // Cross-check: the wrong width filter doesn't match.
    let h32_wrong = m.find_all(&store().addr(int_const(0x100u64)).bit_width(64).into());
    let h64_wrong = m.find_all(&store().addr(int_const(0x108u64)).bit_width(32).into());
    assert_eq!(h32_wrong.len(), 0);
    assert_eq!(h64_wrong.len(), 0);
}
