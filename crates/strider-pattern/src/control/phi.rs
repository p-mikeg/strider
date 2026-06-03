//! Phi-family builders: `PhiPat`, `MemPhiPat`.
//!
//! Both are thin slot-convention wrappers over the shared
//! `NodePat` core.
//!
//! `Phi` and `MemPhi` are distinguished by `NodeKind` discriminant.
//! Input layout: predecessor 0's value lives at raw input slot 1 —
//! input 0 is the phi-token edge from the owning `Region`. `.input(i, p)`
//! shifts by +1 so callers address predecessor slots directly.
//!
//! [`PhiPat::for_vn`] restricts the match to a lifter-emitted SSA φ whose
//! `Function::phi_var_tag` is `Some(vn)`, read at match time via a
//! node-only limit (short-circuits before child recursion).
//!
//! `MemPhi` produces a memory token (output slot 0); it implements
//! [`MemPat`] so a `load` / `store` can chain off it. `Phi` produces a
//! value output (slot 0).

use strider_ir::node::NodeKind;

use crate::builder::{MatcherBuilder, PatValueRef};
use crate::capture::Capture;
use crate::match_pat::MatchPat;
use crate::pattern::{KindSpec, NodePredicate, Pattern};

use super::MemPat;
use super::node_pat::NodePat;

/// A node-limit pinning the matched `Phi`'s `phi_var_tag` to `Some(vn)`.
fn phi_var_limit(want: rsleigh::Vn) -> NodePredicate {
    Box::new(move |m, n| m.function().phi_var_tag(n) == Some(want))
}

/// The kind spec pinning a phi-family discriminant.
fn phi_kind(exemplar: NodeKind) -> KindSpec {
    KindSpec::Variant(std::mem::discriminant(&exemplar))
}

// ── PhiPat (tagged or any) ────────────────────────────────────────────────────

/// Builder for `Phi` node patterns. Created by [`phi`].
///
/// Without [`for_vn`](Self::for_vn) matches any `Phi` discriminant;
/// `for_vn(vn)` narrows to the lifter-emitted SSA φ tagged `Some(vn)` in
/// `Function::phi_var_tag`.
pub struct PhiPat {
    inner: NodePat,
    var_filter: Option<rsleigh::Vn>,
}

impl PhiPat {
    /// Constrain the value arriving from predecessor slot `idx` (shifted
    /// to raw input slot `idx + 1` to skip the phi-token input).
    #[must_use]
    pub fn input<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inner = self.inner.input(idx + 1, p);
        self
    }

    /// Restrict the match to lifter-emitted SSA φ nodes whose
    /// `Function::phi_var_tag` is `Some(vn)`.
    #[must_use]
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.var_filter = Some(vn);
        self
    }

    /// Bind the matched `Phi`'s value output to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.inner = self.inner.capture(c);
        self
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        let PhiPat { inner, var_filter } = self;
        match var_filter {
            Some(vn) => inner.with_node_predicate(move || phi_var_limit(vn)),
            None => inner,
        }
        .build()
    }
}

/// Construct a fresh [`PhiPat`].
#[must_use]
pub fn phi() -> PhiPat {
    PhiPat {
        // `Phi` is node-rooted with a value output at slot 0.
        inner: NodePat::node(phi_kind(NodeKind::Phi)).with_anchor_value(0),
        var_filter: None,
    }
}

/// Start building a tagged-`Phi` pattern (see [`phi`]) pinned to varnode
/// `vn` in `Function::phi_var_tag`.
#[must_use]
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    phi().for_vn(vn)
}

// ── MemPhiPat ─────────────────────────────────────────────────────────────────

/// Builder for `MemPhi` node patterns. Created by [`mem_phi`].
///
/// `MemPhi` is the memory-token phi at join points. Produces a memory
/// token (output slot 0) — implements [`MemPat`] so a `load` / `store`
/// can chain off it. Same input shift (+1) as [`PhiPat`].
pub struct MemPhiPat(NodePat);

impl MemPhiPat {
    /// Constrain the memory token arriving from predecessor slot `idx`
    /// (shifted to raw input slot `idx + 1`). The sub-pattern must be a
    /// memory producer.
    #[must_use]
    pub fn input<M: MemPat + 'static>(self, idx: usize, p: M) -> Self {
        Self(self.0.input_mem(idx + 1, p))
    }

    /// Bind the matched `MemPhi`'s memory-token output to `c`.
    #[must_use]
    pub fn capture(self, c: Capture) -> Self {
        Self(self.0.capture(c))
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        self.0.build()
    }
}

impl MemPat for MemPhiPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.lower(b).mem_value()
    }
}

/// Construct a fresh [`MemPhiPat`].
#[must_use]
pub fn mem_phi() -> MemPhiPat {
    // `MemPhi` is node-rooted with a memory-token output at slot 0.
    MemPhiPat(NodePat::node(phi_kind(NodeKind::MemPhi)).with_mem_value(0))
}

