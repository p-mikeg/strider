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
//! and IR lifting.  Consolidation crate for what previously lived in
//! standalone `strider-target`, `pcode-lift`, and `cfg` crates.

pub mod pcode_lift;
pub mod cfg;
pub mod region_driver;
