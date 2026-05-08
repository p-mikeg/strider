use std::collections::HashMap;
use std::sync::Arc;

use opt::ReadOnlyMemory;

use crate::cfg::builder::ResolvedTargets;
use crate::cfg::types::PcodeInsnAddr;

/// Configuration that governs how [`crate::cfg::Builder`] builds the CFG.
///
/// Construct via [`OptionsBuilder`].
///
/// `Options` is intentionally **not** `Copy` / `Eq` / `Hash` because
/// [`Self::read_only_memory`] holds an `Arc<dyn ReadOnlyMemory>` whose
/// trait object cannot meaningfully be compared by value.  Pre-existing
/// scalar knobs (`fn_max_size`, `allow_code_before_start_addr`,
/// `link_register_vn`) keep their cheap-clone semantics.
#[derive(Clone, Default)]
pub struct Options {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call.
    pub(super) fn_max_size: Option<u64>,
    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    pub(super) allow_code_before_start_addr: bool,
    /// Calling-convention link-register varnode.  Threaded into the
    /// indirect-branch resolver — when the resolver finds the
    /// `BranchIndirect` target is the function-entry value of this
    /// varnode (`InitialVar(lr)` after fold), it classifies the branch
    /// as a `Return`.
    ///
    /// `None` on stack-push ISAs (x86, x86_64) where there is no
    /// architectural link register.  Default is `None` — callers
    /// (typically `strider`) compute the value from
    /// [`target::BuiltCallingConvention::link_register_vn`] and plumb
    /// it through with [`OptionsBuilder::set_link_register`].
    pub(super) link_register_vn: Option<rsleigh::Vn>,
    /// Read-only memory image (typically `.rodata` / `.text` from the
    /// binary being analysed).  Used by the indirect-branch resolver to
    /// fold constant-address loads into constants so that targets
    /// stored in jump tables / read-only globals resolve.  `None`
    /// disables that step.
    pub(super) read_only_memory: Option<Arc<dyn ReadOnlyMemory>>,
    /// Pre-classified `BranchIndirect` results to thread back into the
    /// CFG build.  When the cfg builder encounters a `BranchIndirect`
    /// at one of these pcode addresses, it skips the cfg-time mini-graph
    /// resolver and uses the cached classification directly — this is
    /// the feedback loop the strider fixed-point orchestrator uses to
    /// wire IR-level indirect-branch resolver results into a CFG rebuild.
    ///
    /// Default is empty (no known targets).  Populated by the
    /// orchestrator via [`super::Builder::with_known_targets`].
    pub(super) known_targets: HashMap<PcodeInsnAddr, ResolvedTargets>,
}

// Manual `Debug` impl: `dyn ReadOnlyMemory` doesn't implement `Debug`,
// so the auto-derive can't handle the `Option<Arc<dyn ReadOnlyMemory>>`
// field.  We render it as a presence/absence marker instead.
impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("fn_max_size", &self.fn_max_size)
            .field("allow_code_before_start_addr", &self.allow_code_before_start_addr)
            .field("link_register_vn", &self.link_register_vn)
            .field("read_only_memory", &self.read_only_memory.as_ref().map(|_| "<rom>"))
            .field("known_targets", &self.known_targets)
            .finish()
    }
}

// `Options` cannot derive `PartialEq` / `Eq` because
// `Arc<dyn ReadOnlyMemory>` doesn't implement them (trait objects have
// no value equality).  We provide a `PartialEq` that compares the
// scalar knobs by value and the ROM by `Arc::ptr_eq`, which is the
// strongest equality available without forcing all implementors to
// derive `PartialEq`.
impl PartialEq for Options {
    fn eq(&self, other: &Self) -> bool {
        self.fn_max_size == other.fn_max_size
            && self.allow_code_before_start_addr == other.allow_code_before_start_addr
            && self.link_register_vn == other.link_register_vn
            && self.known_targets == other.known_targets
            && match (&self.read_only_memory, &other.read_only_memory) {
                (None, None) => true,
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
    }
}

/// Builder for [`Options`].
///
/// # Example
/// ```rust
/// use cfg::OptionsBuilder;
///
/// let opts = OptionsBuilder::new()
///     .set_function_max_size(0x1000)
///     .allow_code_before_start_addr()
///     .build();
/// # let _ = opts;
/// ```
#[derive(Clone, Debug, Default)]
pub struct OptionsBuilder {
    lifter_options: Options,
}

impl OptionsBuilder {
    /// Creates an `OptionsBuilder` with all options at their defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum size (in bytes) of the function being analysed.
    ///
    /// Any unconditional branch whose target address is ≥ `start_addr + max_size`
    /// will be treated as a tail call.
    #[must_use]
    pub fn set_function_max_size(mut self, max_size: u64) -> Self {
        self.lifter_options.fn_max_size = Some(max_size);
        self
    }

    /// Allows the CFG builder to follow unconditional branches whose target
    /// address is below the function start address.
    ///
    /// By default such branches are classified as tail calls (they are
    /// assumed to leave the current function).  Enable this option when the
    /// binary layout places shared or out-of-order code before the entry point.
    #[must_use]
    pub fn allow_code_before_start_addr(mut self) -> Self {
        self.lifter_options.allow_code_before_start_addr = true;
        self
    }

    /// Sets the calling-convention link-register varnode used by the
    /// indirect-branch resolver to classify `BranchIndirect target = lr`
    /// (the architectural return idiom on link-register ISAs) as a
    /// `Return` terminator.
    ///
    /// Callers typically pass the value from
    /// [`target::BuiltCallingConvention::link_register_vn`].  Has no
    /// effect on stack-push ISAs (x86, x86_64) — leave unset (the
    /// default) on those.
    #[must_use]
    pub fn set_link_register(mut self, vn: rsleigh::Vn) -> Self {
        self.lifter_options.link_register_vn = Some(vn);
        self
    }

    /// Sets the read-only memory image consulted by the indirect-branch
    /// resolver when folding constant-address loads.  Use the same
    /// `ReadOnlyMemory` that the optimizer's `LoadReadOnly` pass would
    /// see (typically the binary's mapped `.rodata` / `.text`).
    #[must_use]
    pub fn set_read_only_memory(mut self, rom: Arc<dyn ReadOnlyMemory>) -> Self {
        self.lifter_options.read_only_memory = Some(rom);
        self
    }

    /// Consumes the builder and returns the final [`Options`].
    #[must_use]
    pub fn build(self) -> Options {
        self.lifter_options
    }
}
