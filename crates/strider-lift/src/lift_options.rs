//! [`LiftOptions`] — the single options type for binary → IR lifting.
//!
//! Lives at the crate root so BOTH the [`crate::cfg`] builder (which
//! reads the CFG-shaping knobs `fn_max_size`,
//! `allow_code_before_start_addr`, and `known_targets`) and the
//! [`crate::lift`] driver (which reads the IR-lift knobs `all_vns` and
//! `per_address_ccs`) can consume it without a `cfg → lift` dependency
//! edge.  Re-exported from both [`crate::lift`] (so existing
//! `strider_lift::lift::LiftOptions` paths keep working) and the crate
//! root (`strider_lift::LiftOptions`).

use rustc_hash::FxHashMap;

use crate::cfg::{PcodeInsnAddr, ResolvedTargets};

/// Owned, lifetime-free options governing the whole binary → IR lift.
///
/// The CFG builder reads `fn_max_size`, `allow_code_before_start_addr`,
/// and `known_targets`; the IR lifter reads `all_vns` and
/// `per_address_ccs`.  `#[derive(Default)]` yields the convenience
/// behaviour both modules' bare entry points use (unbounded function,
/// no pre-start code, no known targets, scan-for-vns, no CC overrides).
#[derive(Default)]
pub struct LiftOptions {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call.  `n == 0` is
    /// treated as unbounded (no effect) — downstream callers should
    /// reject zero at their own API boundary.
    pub fn_max_size: Option<u64>,

    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    pub allow_code_before_start_addr: bool,

    /// Pre-classified `BranchIndirect` results to thread back into the
    /// CFG build.  When the cfg builder encounters a `BranchIndirect`
    /// at one of these pcode addresses, it seats the cached
    /// classification's terminator directly; every other site is
    /// deferred via `UnresolvedIndirectBranch`.  This is the feedback
    /// loop the orchestrator's rebuild-driven fixed-point uses to wire
    /// IR-level indirect-branch resolution into a CFG rebuild.
    ///
    /// Default is empty (no known targets).
    pub known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,

    /// Pre-computed varnode set.  When `None`, the lifter computes it
    /// internally.  When `Some`, must be sorted by
    /// `crate::pcode_lift::vn_sort_key` and must include every varnode
    /// any instruction in the CFG references.  Under-tracking drops
    /// pcode reads; over-tracking is safe but allocates one extra
    /// `InitialVar` per superfluous vn.  The orchestrator passes
    /// `Some(cached_vns)` so it shares one vn table across rebuild
    /// iterations.
    pub all_vns: Option<Vec<rsleigh::Vn>>,

    /// Per-target-address CC override map.  Keys are direct-call target
    /// addresses; values are CCs already resolved against the same
    /// Sleigh register table the function-default CC was built against.
    /// Empty by default — every direct `Call` uses the function-default
    /// CC.
    pub per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention>,
}
