#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Strider lift: Sleigh integration, CFG construction, and pcode → IR
//! value lifting.  Sits between `strider-target` (architecture
//! descriptors) and `strider-orchestrator` (orchestrator + opt + pattern).

pub mod pcode_lift;
pub mod cfg;
pub mod lift;
pub mod lift_options;

pub use lift_options::LiftOptions;
