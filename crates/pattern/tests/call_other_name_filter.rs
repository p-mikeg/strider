//! CallOtherPat::name(s) matches by user-op name (from the side
//! table), not by the user_op_id field.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use strider_ir::test_utils::SENTINEL_LIFT_ADDR;
use strider_ir::FunctionBuilder;
use pattern::{Matcher, call_other};

#[test]
fn name_matches_only_target() {
    let mut b = FunctionBuilder::empty().expect("builder");
    let region = b.create_region().expect("region");
    b.set_entry_region(region).expect("entry");
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    let _ = b
        .build_call_other_modeled(1, "cpuid", &[], None, &[], &[], &[])
        .expect("cpuid");
    let _ = b
        .build_call_other_modeled(2, "rdtsc", &[], None, &[], &[], &[])
        .expect("rdtsc");
    b.build_return(None, &[]).expect("return");
    b.set_lift_addr(None);
    let fg = b.build().expect("build");
    let matches = Matcher::new(&fg).find_all(&call_other().name("cpuid").into());
    assert_eq!(matches.len(), 1, "should match exactly the cpuid CallOther");
}
