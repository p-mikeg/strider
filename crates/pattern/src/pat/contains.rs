//! Forward walk along the control chain searching for an inner control
//! pattern. Replaces `PatKind::Contains`.
//!
//! Phase 3.1: the inner is stored as a transitional [`Pat`] (not a
//! `DynCtrlPat`) so it can wrap either legacy, data-level, or control-level
//! inner patterns uniformly.  The actual forward walk lives in
//! [`crate::matcher::traversal::match_contains`]; sites that need one
//! (`If.true_branch` / `If.false_branch` / `Return.preceded_by`) peel the
//! `ContainsPat` shell via [`ControlPattern::contains_inner`] and feed the
//! inner directly to the walker, preserving the legacy no-double-walk
//! semantics.

use ir::node::NodeId;

use crate::matcher::Bindings;
use crate::pat::Pat;
use crate::pat::traits::{ControlPattern, MatchCtx};

pub struct ContainsPat {
    pub(crate) inner: Pat,
}

impl ControlPattern for ContainsPat {
    fn try_match(&self, _ctx: &MatchCtx, _target: NodeId, _b: &mut Bindings) -> bool {
        // Reaching `ContainsPat` as the target of a match attempt means
        // there's no outer ctrl chain to walk — preserves the legacy
        // `PatKind::Contains` behaviour where a top-level `contains(...)`
        // fed to `find_all` fell through both dispatch paths and
        // returned `false`.  The real forward walk is done by
        // `ControlNodePat` when it encounters a `Contains` shell on its
        // `true_branch` / `false_branch` / `preceded_by` fields.
        false
    }

    fn contains_inner(&self) -> Option<&Pat> {
        Some(&self.inner)
    }
}
