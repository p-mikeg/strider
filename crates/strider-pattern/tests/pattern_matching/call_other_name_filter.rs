//! `CallOtherPat::name(s)` matches on the `call_other_name` side-table, not
//! the `user_op_id` payload. Combinable with `user_op_id` / `arg`.

use strider_ir::FunctionBuilder;
use strider_ir_test_utils::RegisterSet;
use strider_pattern::{Matcher, call_other};

#[test]
fn name_matches_only_target() {
    let mut b: FunctionBuilder = RegisterSet::new()
        .build_fn_single_region()
        .expect("build_fn_single_region");
    let _ = b
        .build_call_other_abi(
            1,
            "cpuid",
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
                no_return: false,
            },
            None,
            false,
        )
        .expect("cpuid");
    let _ = b
        .build_call_other_abi(
            2,
            "rdtsc",
            &[],
            &strider_target::BuiltCallOtherAbi {
                implicit_reads: Vec::new(),
                implicit_writes: Vec::new(),
                clobbers_memory: false,
                no_return: false,
            },
            None,
            false,
        )
        .expect("rdtsc");
    b.build_return(None, &[]).expect("return");
    let function = b.build().expect("build");

    let matches = Matcher::new(&function)
        .find_all(&call_other().name("cpuid").build())
        .unwrap();
    assert_eq!(matches.len(), 1, "should match exactly the cpuid CallOther");
}
