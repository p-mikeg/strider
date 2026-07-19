//! Per-run tuning knobs, threaded through every pass via
//! [`crate::OptCtx::options`].
//!
//! The SP-aware passes (`LoadForward`, `FunctionArgDetect`,
//! `CallStackArgCollect`) share one soundness/coverage trade-off: walking
//! back across a Store whose address is not SP-rooted, the strict floor
//! cannot prove the store misses an SP-rooted slot, so it bails. That floor
//! blocks forwarding in every function with a global write interleaved
//! between stack ops, which is most compiler output. Hence
//! [`AliasMode::StackGlobalDisjoint`] as the default.

/// How hard the SP-aware walkers work to prove an intervening Store misses
/// the query range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AliasMode {
    /// Forward only when disjointness is structurally provable from the IR
    /// alone (`decompose_sp` agrees on both addresses, ranges disjoint).
    /// Cross-class pairs are treated as possibly-aliasing. Sound under any
    /// input program.
    Strict,

    /// Assume the stack and the global/constant-address (`.data`, `.rodata`,
    /// `.bss`, MMIO) regions never overlap at runtime, letting the walker
    /// step through a constant-address Store when looking back from an
    /// SP-rooted Load and vice-versa. Unsound only if a constant address
    /// coincidentally equals `sp + K` at runtime, which takes adversarial
    /// code or a pathological layout. Other cross-class pairs (anything
    /// Anchor) still bail; closing those needs escape analysis.
    #[default]
    StackGlobalDisjoint,
}

/// Per-run knobs, read from [`crate::OptCtx::options`].
///
/// Post-run arena compaction is not an optimiser knob: it lives on
/// `strider_lift::LiftOptions::compact`.
#[derive(Debug, Clone, Default)]
pub struct OptOptions {
    /// Alias precision for every SP-aware pass.
    pub alias_mode: AliasMode,
    /// Relaxations for incoming function-argument detection. Only
    /// [`crate::FunctionArgDetect`] reads these: incoming args are written
    /// before the body by ABI and are not normally clobbered by later calls
    /// or other-SP-base stores, so the arg-detect walk may assume that. The
    /// type is shared with the call-blocking SP walkers, but this field is
    /// arg-detect-only.
    pub arg_alias: MemAliasOptions,
}

/// Aliasing relaxations for the SP memory-walk. Conservative by default.
#[derive(Debug, Clone, Copy, Default)]
pub struct MemAliasOptions {
    /// Whether a `Call` / `CallOther` on the probed location's memory chain
    /// shadows it. `false` lets incoming args survive a later call.
    pub calls_clobber: bool,
    /// Whether a `Store` rooted at a *different* SP base than the entry SP
    /// (e.g. an alignment-masked `sp & -16` frame local) counts as disjoint
    /// from the probed location. `false` (may-alias) is sound but can block
    /// arg detection when a distinct-base store overlaps a slot offset.
    pub assume_distinct_sp_bases_disjoint: bool,
}
