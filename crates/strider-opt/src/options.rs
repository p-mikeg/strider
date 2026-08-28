/// Per-run knobs.
#[derive(Debug, Clone)]
pub struct OptOptions {
    /// Run the indirect-branch classifier.  Off leaves every unseeded site an
    /// `IndirectBranch` placeholder, so a caller can hand its own answers in
    /// through `CfgOptions::known_targets` instead, or see the raw dispatch
    /// shape when a resolution looks wrong.  Caller-supplied targets still
    /// seat: they are read by the CFG builder, not the classifier.
    pub resolve_indirect_branches: bool,
    /// Claims about the program under analysis.
    pub assumptions: AssumptionOptions,
}

impl Default for OptOptions {
    fn default() -> Self {
        Self {
            // The only non-`Default` field: resolution is on unless a caller
            // turns it off.
            resolve_indirect_branches: true,
            assumptions: AssumptionOptions::default(),
        }
    }
}

/// Assertions about the code being analysed, none of which the analysis can
/// check.  Every field's risky value is the positive one, and each one turned
/// on can make the answer wrong on valid input; the miscompile is then the
/// caller's.  [`AssumptionOptions::none`] is the only configuration sound
/// under any input program.
///
/// Two default ON, both of which every compiler whose output this analyses
/// honours and without which the alias oracle answers may-alias almost
/// everywhere: [`stack_global_disjoint`](Self::stack_global_disjoint) and
/// [`assume_incoming_args_survive_calls`](Self::assume_incoming_args_survive_calls).
#[derive(Debug, Clone)]
pub struct AssumptionOptions {
    /// The stack and the global / constant-address regions (`.data`,
    /// `.rodata`, `.bss`, MMIO) never overlap at runtime, so the walker steps
    /// through a constant-address `Store` when looking back from an SP-rooted
    /// `Load` and vice-versa.  Wrong only where a constant address
    /// coincidentally equals `sp + K` at runtime.  No other class pair reads
    /// it.  Default ON.
    pub stack_global_disjoint: bool,
    /// A `Call` on an incoming stack-argument slot's memory chain leaves the
    /// slot as it found it, so the argument is still detectable after the
    /// call.  Reaches which loads count as incoming arguments, never the
    /// memory edges: a narrowed edge outlives the pass, so narrowing uses what
    /// a call-blocking walk proves.  It holds for a conforming callee that was
    /// not handed a pointer into the area, since those slots sit above the
    /// entry SP.  A memory-clobbering `CallOther` is outside it entirely.
    /// Default ON.
    pub assume_incoming_args_survive_calls: bool,
    /// A `Store` rooted at a *different* SP base than the entry SP (e.g. an
    /// alignment-masked `sp & -16` frame local) addresses a region disjoint
    /// from the probed location.  Off is may-alias, the sound answer.
    ///
    /// Scoped to incoming-argument detection: it is the only walk that reads
    /// it, so it never widens what `LoadForward` forwards.
    pub distinct_sp_bases_disjoint: bool,
    /// A callee leaves the outgoing-argument slots the caller wrote as it
    /// found them, so a spill at the stack top survives the call. The psABIs
    /// let a callee write those slots (they are its parameters' storage); this
    /// asserts compiler output does not.
    ///
    /// Not standalone: the only reader is the outgoing-argument-window test,
    /// which is reached only under
    /// [`escape_analysis`](Self::escape_analysis) or a non-empty
    /// [`noalias_allocators`](Self::noalias_allocators), both off by default.
    /// Set alone it changes nothing.
    pub callee_preserves_stack_args: bool,
    /// Callee addresses of pure `noalias` heap allocators (`malloc`/`calloc`-like:
    /// a size in, a fresh non-overlapping pointer out, no pointer arguments).
    /// Distinct allocations are disjoint and a load steps through such a call.
    /// Empty (the default) leaves the feature off.
    ///
    /// Each address must genuinely satisfy that contract.  Listing one that can
    /// return an existing or interior pointer (`realloc`, `free`, an
    /// aligned-alloc taking a pointer) produces unsound Disjoint verdicts.
    pub noalias_allocators: rustc_hash::FxHashSet<u64>,
    /// When the function's frame is provably private (no stack address escapes
    /// to any callee), forward a spill `Load` across a `Call` and step it past
    /// an opaque (`Anchor`) store. Both rest on the same axiom: nothing outside
    /// the frame can name a private slot, barring a fabricated pointer.
    ///
    /// It sits here because the proof has two gaps the IR cannot close: an sret
    /// call handed a hidden pointer to a caller-frame return slot the lifter
    /// does not model as an argument, and an alignment hole in the outgoing
    /// argument window read as the window's end (see `mem_analysis`'s KNOWN
    /// LIMIT).
    pub escape_analysis: bool,
}

impl AssumptionOptions {
    /// Every claim cleared: the only configuration sound under any input
    /// program, forwarding solely what the IR structurally proves.
    #[must_use]
    pub fn none() -> Self {
        // Spelled out, not `..Self::default()`: a field added default-on
        // would otherwise survive `none` and break the claim above.
        Self {
            stack_global_disjoint: false,
            assume_incoming_args_survive_calls: false,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            noalias_allocators: rustc_hash::FxHashSet::default(),
            escape_analysis: false,
        }
    }
}

impl Default for AssumptionOptions {
    fn default() -> Self {
        Self {
            stack_global_disjoint: true,
            assume_incoming_args_survive_calls: true,
            distinct_sp_bases_disjoint: false,
            callee_preserves_stack_args: false,
            noalias_allocators: rustc_hash::FxHashSet::default(),
            escape_analysis: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AssumptionOptions, OptOptions};

    /// The two default-on claims are what a caller reaching for the sound
    /// floor has to clear, so `none` clearing everything is the contract.
    #[test]
    fn none_clears_every_claim() {
        let a = AssumptionOptions::none();
        assert!(!a.stack_global_disjoint);
        assert!(!a.assume_incoming_args_survive_calls);
        assert!(!a.distinct_sp_bases_disjoint);
        assert!(!a.callee_preserves_stack_args);
        assert!(!a.escape_analysis);
        assert!(a.noalias_allocators.is_empty());
    }

    /// The pipeline's own defaults, which are NOT the sound floor.
    #[test]
    fn default_leaves_the_two_pipeline_claims_on() {
        let o = OptOptions::default();
        assert!(o.resolve_indirect_branches);
        assert!(o.assumptions.stack_global_disjoint);
        assert!(o.assumptions.assume_incoming_args_survive_calls);
        assert!(!o.assumptions.escape_analysis);
    }
}
