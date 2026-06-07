//! [`LiftOptions`] — the options type for the whole binary → IR lift.
//!
//! Composes the CFG-shaping knobs (a [`strider_cfg::CfgOptions`], handed
//! to [`strider_cfg::Builder::for_arch`]) with the IR-lift knob
//! `per_address_ccs` that the [`crate::lift`] driver reads.  The tracked
//! varnode set is always scanned fresh from the CFG at lift time, so it
//! is not an option.

use rustc_hash::FxHashMap;

/// Owned, lifetime-free options governing the whole binary → IR lift.
///
/// The embedded [`strider_cfg::CfgOptions`] (`cfg`) drives CFG
/// construction; the IR lifter reads `per_address_ccs`.
/// `#[derive(Default)]` yields the convenience behaviour both layers'
/// bare entry points use (unbounded function, no pre-start code, no
/// known targets, no CC overrides).
#[derive(Default)]
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
}
