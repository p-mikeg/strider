//! Graph transforms run inside the pipeline's fixed-point loop. Analyses that
//! need a converged graph live in [`crate::post_opt`] instead.

pub(crate) mod cfg_detach;
pub(crate) mod constant_fold;
pub(crate) mod dead_branch;
pub(crate) mod flag_cmp_canonicalize;
pub(crate) mod if_cond_inversion;
pub(crate) mod known_bits;
pub(crate) mod load_forward;
pub(crate) mod load_readonly;
pub(crate) mod phi_collapse;
pub(crate) mod region_collapse;
