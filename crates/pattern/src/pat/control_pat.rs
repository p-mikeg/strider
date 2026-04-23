//! Generic control-level pattern. Covers `Call`, `CallOther`, `Return`, and
//! `If` — the four legacy `PatKind` variants whose target is a control-level
//! `NodeId` rather than a data `NodeOutputId`.
//!
//! Phase 0 only pins down the struct / enum / trait-impl signatures. The
//! body of `try_match` is a placeholder; the real per-kind dispatch is
//! wired up in Phase 3.

#![allow(dead_code)]

use ir::node::NodeId;

use crate::matcher::Bindings;
use crate::pat::traits::{
    CandidateKind, ControlPattern, DynCtrlPat, DynDataPat, MatchCtx,
};
use crate::var::NodeVar;

pub struct ControlNodePat {
    pub(crate) kind: CtrlKind,
    pub(crate) node_var: Option<NodeVar>,
}

pub enum CtrlKind {
    Call {
        target: Option<DynDataPat>,
        args: Vec<(usize, DynDataPat)>,
        ret_outputs: Vec<(usize, DynDataPat)>,
    },
    CallOther {
        user_op_id: Option<u64>,
        args: Vec<(usize, DynDataPat)>,
    },
    Return {
        preceded_by: Option<DynCtrlPat>,
        ret_vals: Vec<(usize, DynDataPat)>,
    },
    If {
        cond: Option<DynDataPat>,
        true_branch: Option<DynCtrlPat>,
        false_branch: Option<DynCtrlPat>,
    },
}

impl ControlPattern for ControlNodePat {
    // TODO(phase-3): implement
    fn try_match(&self, ctx: &MatchCtx, target: NodeId, b: &mut Bindings) -> bool {
        let _ = (ctx, target, b);
        false
    }

    fn candidate_kind(&self) -> Option<CandidateKind> {
        Some(match self.kind {
            CtrlKind::Call { .. } => CandidateKind::Call,
            CtrlKind::CallOther { .. } => CandidateKind::CallOther,
            CtrlKind::Return { .. } => CandidateKind::Return,
            CtrlKind::If { .. } => CandidateKind::If,
        })
    }
}
