#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Consumes a [`strider_cfg::Cfg`] and produces a `strider_ir::Function`,
//! lifting p-code to IR values region by region.

pub mod lift;
pub mod lift_options;

pub use lift_options::LiftOptions;
