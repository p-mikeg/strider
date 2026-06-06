use rustc_hash::FxHashMap;

use crate::cfg::builder::ResolvedTargets;
use crate::cfg::types::PcodeInsnAddr;

/// The subset of [`crate::LiftOptions`] the CFG builder actually
/// consults, snapshotted onto [`super::Builder`] at construction.
///
/// `crate::LiftOptions` is the single public options type for the whole
/// binary → IR lift; this private struct is just the CFG-shaping slice
/// the builder reads (`fn_max_size`, `allow_code_before_start_addr`,
/// `known_targets`).  The IR-lift knobs (`all_vns`, `per_address_ccs`)
/// are not the cfg builder's concern and are ignored here.
#[derive(Clone, Default, Debug)]
pub(super) struct CfgOptions {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call.
    pub(super) fn_max_size: Option<u64>,
    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    pub(super) allow_code_before_start_addr: bool,
    /// Pre-classified `BranchIndirect` results threaded back into the
    /// CFG build (see [`crate::LiftOptions::known_targets`]).
    pub(super) known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
}

impl CfgOptions {
    /// Snapshots the CFG-shaping knobs out of the full
    /// [`crate::LiftOptions`].  `max_size == 0` is coerced to unbounded
    /// (no effect) rather than panicking — downstream callers reject
    /// zero at their own API boundary, but a zero reaching this far is a
    /// defensive no-op so the lifter doesn't decode past `start_addr`.
    pub(super) fn from_lift_options(options: &crate::LiftOptions) -> Self {
        Self {
            fn_max_size: match options.fn_max_size {
                Some(0) => None,
                other => other,
            },
            allow_code_before_start_addr: options.allow_code_before_start_addr,
            known_targets: options.known_targets.clone(),
        }
    }
}
