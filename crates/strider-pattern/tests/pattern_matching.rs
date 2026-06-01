//! Integration tests for `strider_pattern`.
//!
//! Files are organised by semantic concern (what the user can do) rather
//! than by IR node kind.  Each module has a short header describing its
//! scope; every positive test has a matching negative test nearby.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::useless_conversion
)]

#[path = "pattern_matching/support/mod.rs"]
mod support;

#[path = "pattern_matching/wildcards_and_consts.rs"]
mod wildcards_and_consts;

#[path = "pattern_matching/arithmetic.rs"]
mod arithmetic;

#[path = "pattern_matching/commutativity.rs"]
mod commutativity;

#[path = "pattern_matching/captures_and_predicates.rs"]
mod captures_and_predicates;

#[path = "pattern_matching/matcher_api.rs"]
mod matcher_api;

#[path = "pattern_matching/rewrite.rs"]
mod rewrite;

#[path = "pattern_matching/variant_agnostic.rs"]
mod variant_agnostic;

#[path = "pattern_matching/int_const_width_aware.rs"]
mod int_const_width_aware;

#[path = "pattern_matching/asm_fingerprint.rs"]
mod asm_fingerprint;

#[path = "pattern_matching/memory.rs"]
mod memory;

#[path = "pattern_matching/casts_and_conversions.rs"]
mod casts_and_conversions;

#[path = "pattern_matching/control_flow.rs"]
mod control_flow;

#[path = "pattern_matching/call_other_arg_ret_slots.rs"]
mod call_other_arg_ret_slots;

#[path = "pattern_matching/call_other_name_filter.rs"]
mod call_other_name_filter;

#[path = "pattern_matching/memory_chain.rs"]
mod memory_chain;

#[path = "pattern_matching/bit_width.rs"]
mod bit_width;

#[path = "pattern_matching/get_vn_call_other_clobber.rs"]
mod get_vn_call_other_clobber;

#[path = "pattern_matching/load_store_stack_only.rs"]
mod load_store_stack_only;
