//! Core traits and type aliases for the trait-based pattern engine.
//!
//! The new engine replaces the monolithic `PatKind` enum with trait objects:
//! data-level patterns implement [`DataPattern`] (matching a `NodeOutputId`)
//! and control-level patterns implement [`ControlPattern`] (matching a
//! `NodeId`).
//!
//! Phase 1.1 wires `Pat::Dyn` into the matcher dispatch — `DataPattern` and
//! `MatchCtx` are live; `ControlPattern` and `CandidateKind` remain unused
//! until Phase 3.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputId};

use crate::matcher::Bindings;

/// Context passed through every `try_match` call. Holds a borrow of the
/// built function graph so patterns can read node kinds, inputs, outputs,
/// and graph side-tables (e.g. `stack_phi_offsets`).
#[derive(Clone, Copy)]
pub struct MatchCtx<'g> {
    pub graph: &'g BuiltFunctionGraph,
}

/// A pattern that matches against a single `NodeOutputId` (data edge).
pub trait DataPattern: Send + Sync {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool;
}

/// A pattern that matches against a control-level `NodeId`.
///
/// Unused until Phase 3 migrates Call / CallOther / Return / If to the
/// trait-based engine.  Kept alive as part of the Phase 0 skeleton.
#[allow(dead_code)]
pub trait ControlPattern: Send + Sync {
    fn try_match(&self, ctx: &MatchCtx, target: NodeId, b: &mut Bindings) -> bool;

    /// Used by `Matcher::find_all` to pick a pre-indexed candidate list
    /// instead of scanning every node. Return `None` to fall back to the
    /// full-graph scan.
    fn candidate_kind(&self) -> Option<CandidateKind> {
        None
    }
}

/// Hints `Matcher::find_all` which pre-indexed node list to iterate.
#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Call,
    CallOther,
    Return,
    If,
    FunctionArg,
}

pub type DynDataPat = Arc<dyn DataPattern>;
#[allow(dead_code)]
pub type DynCtrlPat = Arc<dyn ControlPattern>;
