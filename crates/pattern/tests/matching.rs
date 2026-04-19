#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

#[path = "matching/common.rs"]
mod common;
#[path = "matching/arithmetic.rs"]
mod arithmetic;
#[path = "matching/captures_predicates.rs"]
mod captures_predicates;
#[path = "matching/control.rs"]
mod control;
#[path = "matching/helpers_tests.rs"]
mod helpers_tests;
#[path = "matching/float_stack.rs"]
mod float_stack;
#[path = "matching/variant_agnostic.rs"]
mod variant_agnostic;
