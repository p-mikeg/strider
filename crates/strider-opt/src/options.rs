//! Optimizer option types — the per-run tuning knobs every pass agrees on.
//!
//! [`OptOptions`] (the bag threaded through every pass via
//! [`crate::OptCtx::options`]) collects all of them; [`AliasMode`] is the
//! alias-analysis precision knob it carries.
//!
//! The SP-aware passes (`LoadForward`, `FunctionArgDetect`,
//! `CallStackArgCollect`) all face the same soundness/coverage trade-off
//! when walking back across an intervening Store whose address is NOT
//! SP-rooted: under the strict floor we cannot prove the store does
//! not coincidentally alias an SP-rooted slot, so we bail.  In practice
//! that floor blocks legitimate forwarding in every function with a
//! global write interleaved between stack ops — the most common
//! pattern in compiler output.
//!
//! [`AliasMode`] lets the user fall back to the conservative `Strict`
//! floor only when needed; the default
//! ([`AliasMode::StackGlobalDisjoint`]) takes the targeted
//! assumption that global/constant-address memory and SP-relative memory
//! live in disjoint VM regions, recovering coverage on well-behaved
//! (non-malicious) code without admitting the broader escape-analysis
//! questions.

/// How aggressively the SP-aware walkers prove that an intervening
/// Store does not alias the query range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AliasMode {
    /// Forward only when disjointness is structurally provable from the
    /// IR alone (`decompose_sp` agrees on both addresses, ranges
    /// disjoint).  Cross-class store/load pairs are conservatively
    /// treated as possibly-aliasing.  Sound under any input program.
    Strict,

    /// Assume the stack region and the global/constant-address (`.data`,
    /// `.rodata`, `.bss`, MMIO) region never overlap at runtime — true
    /// for every standard process memory layout.  Lets the walker
    /// step through a constant-address Store when looking back from an
    /// SP-rooted Load (and vice-versa).  Unsound only if a constant
    /// address in the IR coincidentally equals `sp + K` at runtime,
    /// which requires either adversarial code or a pathological
    /// memory layout.  Other cross-class pairs (anything Anchor) still
    /// bail — closing those gaps requires escape analysis.
    ///
    /// This is the default: SP-rooted and global/constant addresses
    /// genuinely don't overlap in any standard process layout, so the
    /// more aggressive disjointness is the right floor for real binaries.
    #[default]
    StackGlobalDisjoint,
}

/// Configuration knobs for a single optimizer pipeline run.
///
/// `OptOptions` is the single source of truth for all per-run tuning
/// parameters read by the SP-aware passes.  It lives on [`crate::OptCtx`] as
/// `options` so callers have one named struct to set rather than scattered
/// loose fields:
///
/// * `alias_mode` — global alias-analysis precision for
///   [`crate::LoadForward`], [`crate::FunctionArgDetect`], and
///   [`crate::CallStackArgCollect`].
/// * `calls_clobber_stack_arguments` — whether a `Call` / `CallOther` on a
///   stack-arg `Load`'s memory chain shadows the slot (read by
///   [`crate::FunctionArgDetect`]).
///
/// (Post-run arena compaction is not an optimiser knob — it lives on
/// `strider_lift::LiftOptions::compact`, consumed by the analyze/run
/// driver after the pipeline completes.)
#[derive(Debug, Clone, Default)]
pub struct OptOptions {
    /// Global alias-analysis precision for every SP-aware pass.  Default
    /// is [`AliasMode::StackGlobalDisjoint`] (`AliasMode`'s own `Default`).
    pub alias_mode: AliasMode,
    /// Whether a `Call` / `CallOther` on a stack-arg `Load`'s memory
    /// chain shadows the slot, read by [`crate::FunctionArgDetect`].
    /// Default `false` (aggressive arg detection).
    pub calls_clobber_stack_arguments: bool,
    /// During stack-arg detection only, whether a `Store` rooted at a
    /// *different* SP base than the entry SP (e.g. an alignment-masked
    /// `sp & -16` frame local) is assumed disjoint from the incoming-arg
    /// slots rather than conservatively may-aliasing them.  Read by
    /// [`crate::FunctionArgDetect`]; other passes (e.g. [`crate::LoadForward`])
    /// stay conservative regardless.  Default `false` (may-alias — sound but
    /// can block arg detection when a distinct-base store happens to overlap
    /// a slot offset).
    pub args_assume_distinct_sp_bases_disjoint: bool,
}
