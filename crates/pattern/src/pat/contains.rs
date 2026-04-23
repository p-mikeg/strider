//! Forward walk along the control chain searching for an inner pattern.
//!
//! The inner is stored as a [`Pat`] (not a `DynCtrlPat`) so the fluent
//! builder API (`impl Into<Pat>`) can wrap either a data- or control-level
//! inner uniformly.  The actual forward walk lives in
//! [`crate::matcher::traversal::match_contains`]; sites that need one
//! (`If.true_branch` / `If.false_branch` / `Return.preceded_by`) peel the
//! `ContainsPat` shell via [`ControlPattern::contains_inner`] and feed the
//! inner directly to the walker, avoiding a double forward-walk.

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
        // there's no outer ctrl chain to walk — a top-level `contains(...)`
        // fed to `find_all` matches nothing on its own.  The real forward
        // walk is done by `ControlNodePat` when it encounters a `Contains`
        // shell on its `true_branch` / `false_branch` / `preceded_by`
        // fields.
        false
    }

    fn contains_inner(&self) -> Option<&Pat> {
        Some(&self.inner)
    }
}
