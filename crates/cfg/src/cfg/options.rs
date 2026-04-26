/// Configuration that governs how [`crate::cfg::Builder`] builds the CFG.
///
/// Construct via [`OptionsBuilder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Options {
    /// When `Some(n)`, any unconditional branch whose target lies at an
    /// address ≥ `start + n` is treated as a tail call.
    pub(super) fn_max_size: Option<u64>,
    /// When `false` (the default), unconditional branches whose target
    /// address is *below* the function start are treated as tail calls.
    /// When `true`, such branches are followed normally.
    pub(super) allow_code_before_start_addr: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OptionsBuilder {
    lifter_options: Options,
}

impl Default for OptionsBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl OptionsBuilder {
    /// Creates an `OptionsBuilder` with all options at their defaults.
    #[must_use]
    pub fn new() -> Self {
        OptionsBuilder {
            lifter_options: Options::default(),
        }
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

    /// Consumes the builder and returns the final [`Options`].
    #[must_use]
    pub fn build(self) -> Options {
        self.lifter_options
    }
}
