#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for `OptionsBuilder` — verify the fluent setters produce distinct
//! `Options` values. Stronger behavioral tests (does `fn_max_size` actually
//! change `Builder::build` output?) live in `build_end_to_end.rs`.

use cfg::OptionsBuilder;

#[test]
fn options_builder_defaults_reflexive() {
    let a = OptionsBuilder::new().build();
    let b = OptionsBuilder::new().build();
    assert_eq!(a, b);
}

#[test]
fn options_builder_set_fn_max_size_produces_distinct_options() {
    let default = OptionsBuilder::new().build();
    let sized = OptionsBuilder::new().set_function_max_size(0x1000).build();
    assert_ne!(default, sized);
}

#[test]
fn options_builder_allow_code_before_start_addr_produces_distinct_options() {
    let default = OptionsBuilder::new().build();
    let allow = OptionsBuilder::new().allow_code_before_start_addr().build();
    assert_ne!(default, allow);
}

#[test]
fn options_builder_both_set_produces_distinct_options() {
    let default = OptionsBuilder::new().build();
    let fn_max = OptionsBuilder::new().set_function_max_size(0x1000).build();
    let allow = OptionsBuilder::new().allow_code_before_start_addr().build();
    let both = OptionsBuilder::new()
        .set_function_max_size(0x1000)
        .allow_code_before_start_addr()
        .build();
    // `both` must differ from default and from each of the single-flag forms.
    assert_ne!(default, both);
    assert_ne!(fn_max, both);
    assert_ne!(allow, both);
}
