//! Core traits and type aliases for the trait-based pattern engine.
//!
//! Data-level patterns implement [`DataPattern`] (matching a `NodeOutputId`)
//! and control-level patterns implement [`ControlPattern`] (matching a
//! `NodeId`).  Every [`crate::pat::Pat`] wraps exactly one of these two.

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputId};

use crate::matcher::{Bindings, Matcher};

/// Context passed through every `try_match` call. Carries the graph (for
/// reading node kinds / inputs / outputs / side-tables) and a back-reference
/// to the [`Matcher`] (needed by combinators like `CapturePat`/`WhenPat` that
/// wrap an inner `Pat` — dispatch goes through [`Matcher::match_output`] or
/// [`Matcher::match_node_id`] so the same inner can be either a data or
/// control pattern).
#[derive(Clone, Copy)]
pub struct MatchCtx<'g, 'm> {
    pub graph: &'g BuiltFunctionGraph,
    pub(crate) matcher: &'m Matcher<'g>,
}

/// A pattern that matches against a single `NodeOutputId` (data edge).
pub trait DataPattern: Send + Sync {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool;
}

/// A pattern that matches against a control-level `NodeId`.
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
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Call,
    CallOther,
    Return,
    If,
    /// Reserved for a future `FunctionArg` fast path — `FunctionArg` is a
    /// data pattern and the `DataPattern` trait does not yet expose
    /// `candidate_kind`, so no `ControlPattern` impl currently returns this
    /// variant.  The `find_all` arm falls through to the full-graph scan.
    #[allow(dead_code)]
    FunctionArg,
}

pub type DynDataPat = Arc<dyn DataPattern>;
pub type DynCtrlPat = Arc<dyn ControlPattern>;
