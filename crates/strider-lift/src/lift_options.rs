//! [`LiftOptions`] — the options type for the whole binary → IR lift.
//!
//! Composes the CFG-shaping knobs (a [`strider_cfg::CfgOptions`], handed
//! to [`strider_cfg::Builder::for_arch`]) with the IR-lift knob
//! `per_address_ccs` that the [`crate::lift`] driver reads, plus the
//! post-analysis `compact` knob the analyze/run driver consumes once the
//! pipeline completes.  The tracked varnode set is always scanned fresh
//! from the CFG at lift time, so it is not an option.

use rustc_hash::FxHashMap;

/// Owned, lifetime-free options governing the whole binary → IR lift.
///
/// The embedded [`strider_cfg::CfgOptions`] (`cfg`) drives CFG
/// construction; the IR lifter reads `per_address_ccs`; the analyze/run
/// driver reads `compact`.  `Default` yields the convenience behaviour
/// both layers' bare entry points use (unbounded function, no pre-start
/// code, no known targets, no CC overrides, **compaction on**).
pub struct LiftOptions {
    /// CFG-shaping knobs (`fn_max_size`, `allow_code_before_start_addr`,
    /// `known_targets`), passed by value to the CFG builder as
    /// `&lift_opts.cfg`.
    pub cfg: strider_cfg::CfgOptions,

    /// Per-target-address CC override map.  Keys are direct-call target
    /// addresses; values are CCs already resolved against the same
    /// Sleigh register table the function-default CC was built against.
    /// Empty by default — every direct `Call` uses the function-default
    /// CC.
    pub per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention>,

    /// Whether the analyze/run driver compacts the function's IR arena
    /// after the optimiser pipeline completes.  Default `true`.  Not
    /// consumed during lifting itself — the lift methods ignore it; the
    /// orchestrator (and the `strider.run` / `Strider.analyze` Python
    /// paths) read it at the post-pipeline finalize step.
    pub compact: bool,
}

impl Default for LiftOptions {
    fn default() -> Self {
        Self {
            cfg: strider_cfg::CfgOptions::default(),
            per_address_ccs: FxHashMap::default(),
            compact: true,
        }
    }
}
