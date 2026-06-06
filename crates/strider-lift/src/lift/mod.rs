//! Binary CFG → IR lifting.  Owns the region-by-region translation of a
//! `crate::cfg::Cfg` into a `strider_ir::Function`, given a resolved set
//! of indirect-branch targets.  No optimization — that is the
//! orchestrator's concern.
