//! Integration tests for `strider_analyze::pattern`.
//!
//! Files are organised by semantic concern (what the user can do) rather
//! than by IR node kind.  Each module has a short header describing its
//! scope; every positive test has a matching negative test nearby.
//!
//! Ported from `feature/ai:crates/pattern/tests/matching/*`.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
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

#[path = "pattern_matching/cast_mask_walk.rs"]
mod cast_mask_walk;

#[path = "pattern_matching/rewrite.rs"]
mod rewrite;

#[path = "pattern_matching/variant_agnostic.rs"]
mod variant_agnostic;

#[path = "pattern_matching/if_pat_symmetric.rs"]
mod if_pat_symmetric;

#[path = "pattern_matching/int_const_width_aware.rs"]
mod int_const_width_aware;
