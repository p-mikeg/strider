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

/// Regression for round-12 EC-1: `set_function_max_size(0)` is silently
/// treated as the unbounded default.  A zero-byte function bound is
/// semantically meaningless and would otherwise cause the lifter to
/// decode past the entry address whenever the first machine instruction
/// produced zero pcode operations (e.g. AArch64 NOPs).
///
/// In release builds (where the `debug_assert!` at the setter is
/// compiled out) the silent fallback prevents the corruption; in debug
/// builds the assertion fires before any lifting occurs.
#[test]
#[cfg_attr(debug_assertions, ignore = "debug_assert!-checked in debug builds")]
fn options_builder_set_function_max_size_zero_falls_back_to_unbounded_in_release() {
    let zero = OptionsBuilder::new().set_function_max_size(0).build();
    let default = OptionsBuilder::new().build();
    assert_eq!(zero, default, "zero must be treated as unbounded");
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
