//! `LoadPat::bit_width(n)` / `StorePat::bit_width(n)` filter matches by
//! the value-output / data-input width.  Matches both integer and float
//! types of the same width (e.g. `bit_width(32)` matches I32 and F32).

use strider_analyze::pattern::{Matcher, int_const, load, store};
use strider_ir::FunctionBuilder;
use strider_ir::node::NodeOutputType;
use strider_ir_test_utils::RegisterSet;

#[test]
fn bit_width_filters_load_by_value_width() {
    // Two Loads at the same address, I32 and I64.  Both must be reachable:
    // we return the I32 load directly and route the I64 load through a
    // Store so it sits on the memory chain.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr = b
        .build_int_const(0x100u64, NodeOutputType::I64)
        .expect("addr");
    let l32 = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::I32)
        .expect("u32 load");
    let l64 = b
        .build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::I64)
        .expect("u64 load");
    let other_addr = b
        .build_int_const(0x200u64, NodeOutputType::I64)
        .expect("other_addr");
    b.build_store(other_addr, l64, rsleigh::VnSpace::RAM)
        .expect("store l64");
    b.build_return(Some(l32), &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::try_new(&function).unwrap();
    let h32 = m.find_all(&load().addr(int_const(0x100u64)).bit_width(32).into());
    let h64 = m.find_all(&load().addr(int_const(0x100u64)).bit_width(64).into());
    assert_eq!(h32.len(), 1, "bit_width(32) matches only the I32 load");
    assert_eq!(h64.len(), 1, "bit_width(64) matches only the I64 load");
}

#[test]
fn bit_width_filters_store_by_data_width() {
    // Two Stores with different data widths; both reachable because both
    // sit on the memory chain ending at Return.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let addr1 = b
        .build_int_const(0x100u64, NodeOutputType::I64)
        .expect("addr1");
    let v32 = b
        .build_int_const(1u64, NodeOutputType::I32)
        .expect("v32");
    b.build_store(addr1, v32, rsleigh::VnSpace::RAM)
        .expect("u32 store");
    let addr2 = b
        .build_int_const(0x108u64, NodeOutputType::I64)
        .expect("addr2");
    let v64 = b
        .build_int_const(2u64, NodeOutputType::I64)
        .expect("v64");
    b.build_store(addr2, v64, rsleigh::VnSpace::RAM)
        .expect("u64 store");
    b.build_return(None, &[]).expect("ret");
    let function = b.build().expect("build");

    let m = Matcher::try_new(&function).unwrap();
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
