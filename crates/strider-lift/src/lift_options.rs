use rustc_hash::FxHashMap;

/// What `Default` lends: it hands out a `&'a`, which it cannot borrow from a
/// temporary.
static NO_PER_ADDRESS_CCS: FxHashMap<u64, strider_target::BuiltCallingConvention> =
    FxHashMap::with_hasher(rustc_hash::FxBuildHasher);

/// `Default` is unbounded function, no pre-start code, no known targets, no CC
/// overrides, compaction ON.
pub struct LiftOptions<'a> {
    pub cfg: strider_cfg::CfgOptions<'a>,

    /// Keyed by direct-call target address.  The CCs must already be resolved
    /// against the same Sleigh register table as the function-default CC.
    ///
    /// Borrowed, as [`strider_cfg::CfgOptions::known_targets`] is.
    pub per_address_ccs: &'a FxHashMap<u64, strider_target::BuiltCallingConvention>,

    /// Read at the post-pipeline finalize step by the orchestrator, not during
    /// lifting; the lift methods ignore it.
    pub compact: bool,
}

impl Default for LiftOptions<'_> {
    fn default() -> Self {
        Self {
            cfg: strider_cfg::CfgOptions::default(),
            per_address_ccs: &NO_PER_ADDRESS_CCS,
            compact: true,
        }
    }
}
