//! Alias-analysis precision knob.
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
//! ([`AliasMode::AssumeStackGlobalDisjoint`]) takes the targeted
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
    AssumeStackGlobalDisjoint,
}
