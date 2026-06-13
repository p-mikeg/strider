//! The optimization passes — the graph→graph transforms the
//! [`crate::OptimizerPipeline`] runs in its shared fixed-point loop (as opposed
//! to the converged-graph analyses in [`crate::post_opt`]).  Each submodule
//! owns one pass; the pass types are re-exported at the crate root.

pub(crate) mod cfg_detach;
pub(crate) mod constant_fold;
pub(crate) mod dead_branch;
pub(crate) mod dedup_nodes;
pub(crate) mod flag_cmp_canonicalize;
pub(crate) mod if_cond_inversion;
pub(crate) mod known_bits;
pub(crate) mod load_forward;
pub(crate) mod load_readonly;
pub(crate) mod phi_collapse;
pub(crate) mod region_collapse;
