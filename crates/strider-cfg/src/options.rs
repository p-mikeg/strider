use rustc_hash::FxHashMap;

use crate::indirect_resolver::ResolvedTargets;
use crate::types::PcodeInsnAddr;

/// The SSoT for CFG-shaping knobs.  `strider-lift`'s `LiftOptions` embeds
/// one of these and hands it to [`crate::Builder::for_arch`].
#[derive(Clone, Default, Debug)]
pub struct CfgOptions {
    /// `Some(n)`: an unconditional branch to `>= start + n` is a tail call.
    /// `Some(0)` is coerced to unbounded by [`crate::Builder::for_arch`].
    pub fn_max_size: Option<u64>,
    /// When `false`, an unconditional branch *below* the function start is
    /// a tail call rather than an edge to follow.
    pub allow_code_before_start_addr: bool,
    /// The orchestrator's feedback loop: a `BranchIndirect` listed here
    /// seats its cached terminator directly, every other site defers via
    /// `UnresolvedIndirectBranch`.  That is how IR-level resolution reaches
    /// a CFG rebuild without the cfg crate depending on the IR.
    pub known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
}
