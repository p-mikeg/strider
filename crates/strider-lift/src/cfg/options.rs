use rustc_hash::FxHashMap;

use crate::cfg::builder::ResolvedTargets;
use crate::cfg::types::PcodeInsnAddr;

/// Configuration that governs how [`crate::cfg::Builder`] builds the CFG.
///
/// Construct via [`OptionsBuilder`].
///
/// The read-only memory image consumed by the indirect-branch resolver
/// no longer lives on `Options`: it threads through
/// [`crate::cfg::Builder::with_read_only_memory`] as a borrowed
/// `&dyn ReadOnlyMemory` so the cfg-time mini-IR resolver can see it
/// without forcing `Options` (and every downstream `Builder` ctor) to
/// carry an owned trait object.
#[derive(Clone, Default, Debug, PartialEq, Eq)]
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
    /// [`strider_target::BuiltCallingConvention::link_register_vn`] and plumb
    /// it through with [`OptionsBuilder::set_link_register`].
    pub(super) link_register_vn: Option<rsleigh::Vn>,
    /// Pre-classified `BranchIndirect` results to thread back into the
    /// CFG build.  When the cfg builder encounters a `BranchIndirect`
    /// at one of these pcode addresses, it skips the cfg-time mini-graph
    /// resolver and uses the cached classification directly — this is
    /// the feedback loop the strider fixed-point orchestrator uses to
    /// wire IR-level indirect-branch resolver results into a CFG rebuild.
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
    #[must_use]
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
    #[must_use]
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
    #[must_use]
    pub fn allow_code_before_start_addr(mut self) -> Self {
        self.options.allow_code_before_start_addr = true;
        self
    }

    /// Sets the calling-convention link-register varnode used by the
    /// indirect-branch resolver to classify `BranchIndirect target = lr`
    /// (the architectural return idiom on link-register ISAs) as a
    /// `Return` terminator.
    ///
    /// Callers typically pass the value from
    /// [`strider_target::BuiltCallingConvention::link_register_vn`].  Has no
    /// effect on stack-push ISAs (x86, x86_64) — leave unset (the
    /// default) on those.
    #[must_use]
    pub fn set_link_register(mut self, vn: rsleigh::Vn) -> Self {
        self.options.link_register_vn = Some(vn);
        self
    }

    /// Consumes the builder and returns the final `Options`.
    #[must_use]
    pub fn build(self) -> Options {
        self.options
    }
}
