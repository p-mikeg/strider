use strider_ir::FunctionBuilder;
use strider_ir_test_utils::{RegisterSet, Tb};
use strider_pattern::{Matcher, call_other};

#[test]
fn name_matches_only_target() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    Tb::named_call_other(&mut b, 1, "cpuid", &[], &[], false, false).expect("cpuid");
    Tb::named_call_other(&mut b, 2, "rdtsc", &[], &[], false, false).expect("rdtsc");
    b.build_return(None, &[]).expect("return");
    let function = b.build().expect("build");

    let matches = Matcher::new(&function)
        .find_all(&call_other().name("cpuid").build())
        .unwrap();
    assert_eq!(matches.len(), 1, "should match exactly the cpuid CallOther");
}
