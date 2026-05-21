//! Load / Store at the default code space and on the stack.
//!
//! 7 functions × 6 archs = 42 tests.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("memory", "array_sum",          array_sum_has_load_and_loop);
per_arch_test!("memory", "array_fill",         array_fill_has_store_and_loop);
per_arch_test!("memory", "array_copy",         array_copy_has_load_and_store);
per_arch_test!("memory", "pointer_chase",      pointer_chase_has_two_loads);
per_arch_test!("memory", "struct_field_load",  struct_load_has_load);
per_arch_test!("memory", "struct_field_store", struct_store_has_store);
per_arch_test!("memory", "tagged_union_read",  union_read_has_two_loads);

fn array_sum_has_load_and_loop(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 1, "array_sum must have ≥1 Load");
    assert!(count_loops(g) >= 1, "array_sum loop missing VarPhi");
}
fn array_fill_has_store_and_loop(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "array_fill must have ≥1 Store");
    assert!(count_loops(g) >= 1);
}
fn array_copy_has_load_and_store(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 1, "array_copy must Load");
    assert!(count_stores(g) >= 1, "array_copy must Store");
    assert!(count_loops(g) >= 1);
}
fn pointer_chase_has_two_loads(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "pointer_chase has 2 indirections; got {}", count_loads(g));
}
fn struct_load_has_load(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "x and y are two field reads; got {}", count_loads(g));
}
fn struct_store_has_store(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 2, "x and y are two field writes; got {}", count_stores(g));
}
fn union_read_has_two_loads(g: &strider_ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "as_int + bytes[0] = 2 loads; got {}", count_loads(g));
}
