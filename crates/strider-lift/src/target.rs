//! Re-export of the external `target` crate.
//!
//! Phase 2 Task 2.2 originally moved the target source into this module,
//! but Phase 2 Task 2.3's need to add a temporary `opt` dependency (for
//! the cfg mini-IR resolver) created a dependency cycle through opt -> target.
//! Resolution: keep `target` as a standalone crate at `crates/target/`,
//! and re-export it here so callers using `strider_lift::target::*`
//! continue to work transparently.  Phase 3 will fold target back in
//! once opt moves into strider-analyze and the back-edge is inverted.
pub use ::target::*;
