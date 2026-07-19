//! The tracked varnode set is always scanned fresh from the CFG at lift time,
//! so it is deliberately not a knob here.

use rustc_hash::FxHashMap;

/// `Default` is unbounded function, no pre-start code, no known targets, no CC
/// overrides, compaction ON.
pub struct LiftOptions {
    pub cfg: strider_cfg::CfgOptions,

    /// Keyed by direct-call target address.  The CCs must already be resolved
    /// against the same Sleigh register table as the function-default CC.
    pub per_address_ccs: FxHashMap<u64, strider_target::BuiltCallingConvention>,

    /// Read at the post-pipeline finalize step by the orchestrator, not during
    /// lifting; the lift methods ignore it.
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
