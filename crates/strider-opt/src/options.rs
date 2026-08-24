/// How hard the SP-aware walkers work to prove an intervening Store misses
/// the query range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AliasMode {
    /// Forward only when disjointness is structurally provable from the IR
    /// alone (`mem_analysis::decompose` agrees on both addresses, ranges
    /// disjoint).
    /// Cross-class pairs are treated as possibly-aliasing.
    ///
    /// Sound under any input program with
    /// [`AssumptionOptions::noalias_allocators`] empty.  A non-empty set adds
    /// heap-vs-stack and heap-vs-global Disjoint verdicts that this mode does
    /// not gate, so it is then sound only insofar as each listed allocator
    /// returns storage disjoint from the stack and from every statically-
    /// addressed object.
    Strict,

    /// Assume the stack and the global/constant-address (`.data`, `.rodata`,
    /// `.bss`, MMIO) regions never overlap at runtime, letting the walker
    /// step through a constant-address Store when looking back from an
    /// SP-rooted Load and vice-versa. Unsound only if a constant address
    /// coincidentally equals `sp + K` at runtime. Other cross-class pairs
    /// (anything Anchor) still bail.
    #[default]
    StackGlobalDisjoint,
}

/// Per-run knobs.
#[derive(Debug, Clone)]
pub struct OptOptions {
    /// Alias precision for every SP-aware pass.
    pub alias_mode: AliasMode,
    /// Run the indirect-branch classifier.  Off leaves every site an
    /// `IndirectBranch` placeholder, so a caller can hand its own answers in
    /// through `CfgOptions::known_targets` instead, or see the raw dispatch
    /// shape when a resolution looks wrong.  Caller-supplied targets still
    /// seat: they are read by the CFG builder, not the classifier.
    pub resolve_indirect_branches: bool,
    /// Assume a `Call` / `CallOther` on an incoming stack-argument slot's
    /// memory chain leaves the slot as it found it, so the argument is still
    /// detectable after the call.  Reaches incoming-argument detection only,
    /// where it holds for any conforming callee: those slots sit above the
    /// entry SP, which is the caller's memory.  Default on.
    pub assume_incoming_args_survive_calls: bool,
    /// Claims about the program under analysis.
    pub assumptions: AssumptionOptions,
}

impl Default for OptOptions {
    fn default() -> Self {
        Self {
            alias_mode: AliasMode::default(),
            // The only non-`Default` fields: resolution and incoming-argument
            // survival are on unless a caller turns them off.
            resolve_indirect_branches: true,
            assume_incoming_args_survive_calls: true,
            assumptions: AssumptionOptions::default(),
        }
    }
}

/// Assertions about the code being analysed, none of which the analysis can
/// check.  Each one turned on can make the answer wrong on valid input; the
/// miscompile is then the caller's.  All off by default.
#[derive(Debug, Clone, Default)]
pub struct AssumptionOptions {
    /// A `Store` rooted at a *different* SP base than the entry SP (e.g. an
    /// alignment-masked `sp & -16` frame local) addresses a region disjoint
    /// from the probed location.  Off is may-alias, the sound answer.
    pub distinct_sp_bases_disjoint: bool,
    /// A callee leaves the outgoing-argument slots the caller wrote as it
    /// found them, so a spill at the stack top survives the call. The psABIs
    /// let a callee write those slots (they are its parameters' storage); this
    /// asserts compiler output does not.
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
    /// The proof itself is sound; it sits here because it also assumes no callee
    /// returns a struct by value, an sret call being handed a hidden pointer to
    /// a caller-frame return slot that the analysis misses unless the lifter
    /// models that pointer as a call argument.
    pub escape_analysis: bool,
}
