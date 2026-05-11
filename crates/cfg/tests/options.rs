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

/// Regression: `set_function_max_size(0)` and
/// `set_function_boundary(Bounded { max_size: 0 })` BOTH silently
/// coerce to unbounded.  Production callers (especially Python
/// users via strider-py) should reject zero at their own API
/// boundary so users see a typed `ValueError`; a zero reaching this
/// far is a defensive no-op so the lifter doesn't decode past
/// `start_addr`.
#[test]
fn options_builder_set_function_max_size_zero_falls_back_to_unbounded() {
    let zero = OptionsBuilder::new().set_function_max_size(0).build();
    let default = OptionsBuilder::new().build();
    assert_eq!(zero, default, "zero must coerce to unbounded");
}

#[test]
fn options_builder_set_function_boundary_zero_falls_back_to_unbounded() {
    use cfg::FunctionBoundary;
    let zero = OptionsBuilder::new()
        .set_function_boundary(FunctionBoundary::Bounded { max_size: 0 })
        .build();
    let default = OptionsBuilder::new().build();
    assert_eq!(zero, default, "Bounded{{0}} must coerce to unbounded");
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
