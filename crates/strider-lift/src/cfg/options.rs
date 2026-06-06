use rustc_hash::FxHashMap;

use crate::cfg::builder::ResolvedTargets;
use crate::cfg::types::PcodeInsnAddr;

/// Configuration that governs how [`crate::cfg::Builder`] builds the CFG.
///
/// Construct via [`OptionsBuilder`].
///
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct Options {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call.
    pub(super) fn_max_size: Option<u64>,
    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    pub(super) allow_code_before_start_addr: bool,
    /// Pre-classified `BranchIndirect` results to thread back into the
    /// CFG build.  When the cfg builder encounters a `BranchIndirect`
    /// at one of these pcode addresses, it seats the cached
    /// classification's terminator directly; every other site is
    /// deferred via `UnresolvedIndirectBranch`.  This is the feedback
    /// loop the orchestrator's rebuild-driven fixed-point uses to wire
    /// IR-level indirect-branch resolution into a CFG rebuild.
    ///
    /// Default is empty (no known targets).  Populated by the
    /// orchestrator via [`super::Builder::with_known_targets`].
    pub(super) known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>,
}

/// Builder for `Options`.
///
/// # Example
/// ```rust
/// use strider_lift::cfg::OptionsBuilder;
///
/// let opts = OptionsBuilder::new()
///     .set_function_max_size(0x1000)
///     .allow_code_before_start_addr()
///     .build();
/// # let _ = opts;
/// ```
#[derive(Clone, Debug, Default)]
pub struct OptionsBuilder {
    options: Options,
}

impl OptionsBuilder {
    /// Creates an `OptionsBuilder` with all options at their defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the maximum size (in bytes) of the function being analysed.
    ///
    /// Any unconditional branch whose target address is ≥ `start_addr + max_size`
    /// will be treated as a tail call.
    ///
    /// `max_size == 0` is **silently coerced to unbounded** (no
    /// effect) rather than panicking — Python and other downstream
    /// callers should reject zero at their own API boundary (e.g.
    /// `strider.run(function_max_size=0)` raises a typed Python
    /// `ValueError`), but a zero reaching this far is a defensive
    /// no-op so the lifter doesn't decode past `start_addr`.
    pub fn set_function_max_size(mut self, max_size: u64) -> Self {
        if max_size == 0 {
            // Silent fallback to unbounded; documented above.
            self.options.fn_max_size = None;
            return self;
        }
        self.options.fn_max_size = Some(max_size);
        self
    }

    /// Allows the CFG builder to follow unconditional branches whose target
    /// address is below the function start address.
    ///
    /// By default such branches are classified as tail calls (they are
    /// assumed to leave the current function).  Enable this option when the
    /// binary layout places shared or out-of-order code before the entry point.
    ///
    /// **Note:** when paired with [`Self::set_function_max_size`], the
    /// max-size bound wins — the lower-bound relaxation is silently
    /// ignored.
    pub fn allow_code_before_start_addr(mut self) -> Self {
        self.options.allow_code_before_start_addr = true;
        self
    }

    /// Consumes the builder and returns the final `Options`.
    pub fn build(self) -> Options {
        self.options
    }
}
