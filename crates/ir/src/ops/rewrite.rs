//! Graph-mutation helpers on [`crate::function::BuiltFunctionGraph`].

use crate::Result;
use crate::function::BuiltFunctionGraph;
use crate::node::NodeOutputId;

impl BuiltFunctionGraph {
    /// Redirects every consumer of `old` to `new_val`.
    ///
    /// Returns `true` if at least one use was replaced, `false` if `old` had
    /// no uses.
    pub fn replace_all_uses(
        &mut self,
        old: NodeOutputId,
        new_val: NodeOutputId,
    ) -> Result<bool> {
        let mut cursor = self.graph.output_use_cursor(old);
        if cursor.current().is_none() {
            return Ok(false);
        }
        while cursor.current().is_some() {
            cursor.replace_current_with(new_val)?;
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    // Exhaustively covered by `opt/` integration tests (every pass that
    // rewrites the graph exercises this path). A smoke test here would
    // require a full builder setup that's redundant with those.
}
