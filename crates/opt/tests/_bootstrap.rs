//! Empty placeholder test crate — just makes `tests/common/mod.rs` compile
//! before real integration test files arrive.
mod common;

#[test]
fn common_compiles() {
    let _ = common::sp_vn();
}
