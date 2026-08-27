/// How hard the SP-aware walkers work to prove an intervening Store misses
/// the query range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AliasMode {
    /// Forward only when disjointness is structurally provable from the IR
    /// alone (`mem_analysis::decompose` agrees on both addresses, ranges
    /// disjoint).
    /// Cross-class pairs are treated as possibly-aliasing.
    ///
    /// Sound under any input program with [`AssumptionOptions`] left at its
    /// default.  Every assumption knob adds Disjoint verdicts this mode does not
    /// gate: a non-empty `noalias_allocators` adds heap-vs-stack, heap-vs-global
    /// and heap-vs-heap between distinct allocation bases,
    /// `distinct_sp_bases_disjoint` adds different-SP-base to incoming-argument
    /// detection, and `escape_analysis` / `callee_preserves_stack_args` relax
    /// the call boundary.
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
    /// Assume a `Call` on an incoming stack-argument slot's memory chain leaves
    /// the slot as it found it, so the argument is still detectable after the
    /// call.  Reaches which loads count as incoming arguments, never the memory
    /// edges: a narrowed edge outlives the pass, so narrowing uses what a
    /// call-blocking walk proves.  It holds for a conforming callee that was
    /// not handed a pointer into the area, since those slots sit above the
    /// entry SP.  A memory-clobbering `CallOther` is outside it entirely.
    /// Default on.
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
    ///
    /// Scoped to incoming-argument detection: it is the only walk that reads
    /// it, so it never widens what `LoadForward` forwards.
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
    /// It sits here because the proof has two gaps the IR cannot close: an sret
    /// call handed a hidden pointer to a caller-frame return slot the lifter
    /// does not model as an argument, and an alignment hole in the outgoing
    /// argument window read as the window's end (see `mem_analysis`'s KNOWN
    /// LIMIT).
    pub escape_analysis: bool,
}
