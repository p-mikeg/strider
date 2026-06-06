//! The orchestrator's lift + optimize driver.
//!
//! The CFG→IR lift itself lives in [`strider_lift::lift`]; this module
//! holds [`LiftDriver`], which wraps a [`strider_lift::lift::Lifter`] and
//! adds the optimization concern (alias mode + pipeline builder).  The
//! lift outcome / options types are re-exported from `strider-lift`.

mod pipeline;

pub use pipeline::{LiftDriver, LiftOptions, LiftOutcome};
