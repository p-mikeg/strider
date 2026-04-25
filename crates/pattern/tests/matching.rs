//! Integration tests for the `pattern` crate.
//!
//! Files are organised by semantic concern (what the user can do) rather than
//! by IR node kind.  Each module has a short header describing its scope;
//! every positive test has a matching negative test nearby.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable
)]

#[path = "matching/support/mod.rs"]
mod support;

#[path = "matching/wildcards_and_consts.rs"]
mod wildcards_and_consts;

#[path = "matching/arithmetic.rs"]
mod arithmetic;

#[path = "matching/commutativity.rs"]
mod commutativity;

#[path = "matching/variant_agnostic.rs"]
mod variant_agnostic;

#[path = "matching/casts_and_conversions.rs"]
mod casts_and_conversions;

#[path = "matching/memory.rs"]
mod memory;

#[path = "matching/stack.rs"]
mod stack;

#[path = "matching/control_flow.rs"]
mod control_flow;

#[path = "matching/ssa.rs"]
mod ssa;

#[path = "matching/captures_and_predicates.rs"]
mod captures_and_predicates;

#[path = "matching/matcher_api.rs"]
mod matcher_api;

#[path = "matching/bindings.rs"]
mod bindings;

#[path = "matching/rewrite.rs"]
mod rewrite;

#[path = "matching/int_const_width_aware.rs"]
mod int_const_width_aware;
