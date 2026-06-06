use rustc_hash::FxHashMap;

use crate::builder::ResolvedTargets;
use crate::types::PcodeInsnAddr;

/// CFG-shaping knobs consumed by [`crate::Builder`].
///
/// This is the SSoT for the options the CFG build reads. `strider-lift`'s
/// `LiftOptions` embeds a `CfgOptions` (alongside the IR-lift knobs
/// `all_vns` / `per_address_ccs`) and hands `&lift_opts.cfg` to
/// [`crate::Builder::for_arch`].
#[derive(Clone, Default, Debug)]
pub struct CfgOptions {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call. `Some(0)` is
    /// coerced to unbounded (no effect) by [`crate::Builder::for_arch`].
    pub fn_max_size: Option<u64>,
    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    pub allow_code_before_start_addr: bool,
    /// Pre-classified `BranchIndirect` results threaded back into the
    /// CFG build. When the cfg builder encounters a `BranchIndirect` at
    /// one of these pcode addresses, it seats the cached
    /// classification's terminator directly; every other site is
    /// deferred via `UnresolvedIndirectBranch`. This is the feedback
    /// loop the orchestrator's rebuild-driven fixed-point uses to wire
    /// IR-level indirect-branch resolution into a CFG rebuild.
    pub known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
}
