//! `CallOtherPat::name(s)` matches by user-op name (from the
//! [`strider_ir::Graph::call_other_name`] side-table), independent of
//! the `user_op_id` payload.  Combinable with `user_op_id` / `arg`.

use strider_pattern::{Matcher, call_other};
use strider_ir::FunctionBuilder;
use strider_ir_test_utils::RegisterSet;

#[test]
fn name_matches_only_target() {
    // Two CallOthers with different user-op ids AND different names.
    // `call_other().name("cpuid")` should match the cpuid one only.
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let _ = b
        .build_call_other(1, "cpuid", None, &[], &[], &[], None, false)
        .expect("cpuid");
    let _ = b
        .build_call_other(2, "rdtsc", None, &[], &[], &[], None, false)
        .expect("rdtsc");
    b.build_return(None, &[]).expect("return");
    let function = b.build().expect("build");

    let matches = Matcher::try_new(&function)
        .unwrap()
        .find_all(&call_other().name("cpuid").build());
    assert_eq!(matches.len(), 1, "should match exactly the cpuid CallOther");
}
