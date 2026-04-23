//! Forward walk along the control chain searching for an inner control
//! pattern. Replaces `PatKind::Contains`.
//!
//! Phase 0 only provides the struct + trait-impl signature. The real
//! forward walk is delegated to `matcher/traversal.rs::match_contains` in
//! Phase 3.

#![allow(dead_code)]

use ir::node::NodeId;

use crate::matcher::Bindings;
use crate::pat::traits::{ControlPattern, DynCtrlPat, MatchCtx};

pub struct ContainsPat {
    pub(crate) inner: DynCtrlPat,
}

impl ControlPattern for ContainsPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeId, b: &mut Bindings) -> bool {
        // TODO(phase-3): delegate to traversal::match_contains
        let _ = (ctx, target, b);
        false
    }
}
