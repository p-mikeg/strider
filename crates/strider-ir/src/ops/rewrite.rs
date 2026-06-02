//! Graph-mutation helpers defined on [`crate::graph::Graph`].

use crate::Result;
use crate::graph::Graph;
use crate::node::ValueId;

impl Graph {
    /// Redirects every consumer of `old` to `new_val`.
    ///
    /// Returns `true` if at least one use was replaced, `false` if `old` had
    /// no uses.
    ///
    /// # Errors
    ///
    /// Returns an error when the use-list is corrupted such that
    /// `replace_current_with` is invoked on a null cursor (this would indicate
    /// a graph-construction bug, not user error).
    pub fn replace_all_uses(
        &mut self,
        old: ValueId,
        new_val: ValueId,
    ) -> Result<bool> {
        let mut cursor = self.value_use_cursor(old);
        if cursor.current().is_none() {
            return Ok(false);
        }
        while cursor.current().is_some() {
            cursor.replace_current_with(new_val)?;
        }
        Ok(true)
    }
}
