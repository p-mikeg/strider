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
pub mod pcode_lift;
pub mod cfg;
pub mod lifter;
pub mod region_driver;

/// Test-only re-exports for the absorbed `cfg` module, mirroring the
/// original `cfg` crate's `test_api` flat surface so integration tests
/// under `crates/cfg/tests/` continue to compile after Phase 2 Task 2.3.
#[doc(hidden)]
pub mod cfg_test_api;
