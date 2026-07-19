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
    /// coincidentally equals `sp + K` at runtime. Other cross-class pairs
    /// (anything Anchor) still bail.
    #[default]
    StackGlobalDisjoint,
}

/// Per-run knobs.
#[derive(Debug, Clone, Default)]
pub struct OptOptions {
    /// Alias precision for every SP-aware pass.
    pub alias_mode: AliasMode,
    /// Relaxations for incoming function-argument detection.
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
    /// from the probed location. `false` is may-alias, the sound answer.
    pub assume_distinct_sp_bases_disjoint: bool,
}
