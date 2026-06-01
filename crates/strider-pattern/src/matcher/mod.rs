//! Pattern matcher over a lifted [`strider_ir::Function`].
//!
//! Phase 1 carries only the minimal handle that the bipartite
//! `Pattern` representation references in its closure type aliases; the
//! full matching engine is built in a later phase.

pub use strider_ir::walk::CastMask;

use strider_ir::Function;

/// Matches [`crate::pattern::Pattern`]s against a lifted function's IR.
///
/// The function must have an entry node recorded
/// ([`Function::entry`]); [`Self::try_new`] validates this.
pub struct Matcher<'f> {
    pub(crate) function: &'f Function,
}

impl<'f> Matcher<'f> {
    /// Builds a matcher over `function`.
    ///
    /// # Errors
    ///
    /// Returns an error if `function` has no entry node recorded.
    pub fn try_new(function: &'f Function) -> anyhow::Result<Self> {
        if function.entry().is_none() {
            return Err(anyhow::anyhow!(
                "Matcher::try_new: function has no entry node"
            ));
        }
        Ok(Self { function })
    }

    /// The function this matcher queries.
    #[must_use]
    pub fn function(&self) -> &Function {
        self.function
    }
}
