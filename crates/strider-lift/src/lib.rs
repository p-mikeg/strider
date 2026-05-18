#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider lift: target descriptions, sleigh integration, CFG construction,
//! and IR lifting. v2 rewrite consolidation crate for `target`, `pcode-lift`,
//! and `cfg`. See `docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md`.

pub mod target;
