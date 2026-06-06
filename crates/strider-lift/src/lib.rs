#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider lift: pcode → IR value lifting and CFG → IR translation.
//! Consumes a [`strider_cfg::Cfg`] and produces a `strider_ir::Function`.
//! Sits between `strider-cfg` (CFG construction) / `strider-target`
//! (architecture descriptors) and `strider-orchestrator` (orchestrator +
//! opt + pattern).

pub mod pcode_lift;
pub mod lift;
pub mod lift_options;

pub use lift_options::LiftOptions;
