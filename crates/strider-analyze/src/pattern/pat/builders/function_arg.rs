//! `FunctionArgPat` — matches function argument carrier nodes registered
//! in the `Function::arg_index_to_nodes` side-table.
//!
//! After `FunctionArgDetect`, arguments are represented as `InitialVar`
//! (register args) or `Load` (stack args) nodes recorded in the
//! side-table.  This pattern resolves the side-table at match time
//! rather than searching for the now-defunct `FunctionArg` node kind.

use std::sync::Arc;

use strider_ir::node::{FunctionArgSource, NodeId, NodeKind, NodeOutputId};

use crate::pattern::matcher::Bindings;
use crate::pattern::pat::Pat;
use crate::pattern::pat::node_pat::KindSpec;
use crate::pattern::pat::traits::{MatchCtx, Pattern};

/// Builder for function argument carrier patterns.  Created by
/// [`crate::pattern::pat::function_arg`], [`crate::pattern::pat::function_arg_any`],
/// [`crate::pattern::pat::function_arg_reg`], [`crate::pattern::pat::function_arg_stack`].
///
/// Capture the matched output with `.capture(v)` from
/// [`crate::pattern::pat::IntoPat`].
pub struct FunctionArgPat {
    pub(super) source: Option<FunctionArgSource>,
    pub(super) index: Option<u32>,
}

impl FunctionArgPat {
    pub(crate) fn new() -> Self {
        Self { source: None, index: None }
    }
    /// Restrict the match to a specific ABI source (register or stack slot).
    #[must_use]
    pub fn source(mut self, s: FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }
    /// Restrict the match to a specific argument index.
    #[must_use]
    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }
}

/// Runtime `Pattern` impl for `FunctionArgPat`.
///
/// Matches any carrier node that appears in `Function::arg_index_to_nodes`
/// for the requested `index` (or any index when `index` is `None`), and
/// whose node kind satisfies the optional `source` constraint.
struct FunctionArgPattern {
    source: Option<FunctionArgSource>,
    index: Option<u32>,
}

impl FunctionArgPattern {
    /// Returns `true` if `node_id` satisfies the source constraint (or there
    /// is no source constraint).
    fn source_matches(&self, ctx: &MatchCtx<'_, '_>, node_id: NodeId) -> bool {
        let Some(ref expected) = self.source else {
            return true;
        };
        match (expected, ctx.function.node_kind(node_id)) {
            (FunctionArgSource::Register(expected_vn), NodeKind::InitialVar(actual_vn)) => {
                expected_vn == actual_vn
            }
            (FunctionArgSource::Stack { .. }, NodeKind::Load(_)) => true,
            _ => false,
        }
    }

    /// Core check: is `node_id` a registered carrier that passes index +
    /// source constraints?
    fn is_match(&self, ctx: &MatchCtx<'_, '_>, node_id: NodeId) -> bool {
        match self.index {
            Some(idx) => {
                // Check if node_id is in the side-table for this index.
                if !ctx.function.arg_index_to_nodes(idx).contains(&node_id) {
                    return false;
                }
                self.source_matches(ctx, node_id)
            }
            None => {
                // No index constraint — accept if the node is in ANY index.
                ctx.function
                    .arg_indices()
                    .any(|idx| ctx.function.arg_index_to_nodes(idx).contains(&node_id))
                    && self.source_matches(ctx, node_id)
            }
        }
    }
}

impl Pattern for FunctionArgPattern {
    fn kind_spec(&self) -> KindSpec {
        // We can't narrow to a single discriminant: register args are
        // `InitialVar`, stack args are `Load`, and an unconstrained
        // `FunctionArgPat` accepts both.  Return `Any` so the matcher
        // visits every node; the is_match check above is the actual gate.
        KindSpec::Any
    }

    fn try_match(&self, ctx: &MatchCtx<'_, '_>, target: NodeOutputId, b: &mut Bindings) -> bool {
        let node_id = ctx.function.node_for_output(target);
        if !self.is_match(ctx, node_id) {
            return false;
        }
        // Must be a value output (not Control / Memory / PhiToken).
        ctx.require_value_output(target).is_some()
        // No extra bindings to install; callers use `.capture(v)` on top.
        && { let _ = b; true }
    }
}

impl From<FunctionArgPat> for Pat {
    fn from(b: FunctionArgPat) -> Pat {
        let FunctionArgPat { source, index } = b;
        Pat::from_dyn(Arc::new(FunctionArgPattern { source, index }))
    }
}
