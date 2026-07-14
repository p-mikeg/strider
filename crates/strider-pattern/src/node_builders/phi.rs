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
//! `value_vn` entry (keyed by output slot 0, queried via
//! `Function::get_vn_for_value`) is `Some(vn)`, read at match time via a
//! node-only limit (short-circuits before child recursion).
//!
//! `MemPhi` produces a memory token (output slot 0); it implements
//! [`MemPat`] so a `load` / `store` can chain off it. `Phi` produces a
//! value output (slot 0).

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, NodePredicate, PatValueRef, Pattern};

use super::MemPat;
use super::node_pat::NodePat;

/// A node-limit pinning the matched `Phi`'s `value_vn` entry to `Some(vn)`.
fn phi_var_limit(want: rsleigh::Vn) -> NodePredicate {
    Box::new(move |m, n| {
        let v = m.function().node_outputs(n)[0];
        // The tag stores the largest container, so a pinned sub-register
        // matches its container (see [`vn_container::vn_contains`]).
        m.function()
            .get_vn_for_value(v)
            .is_some_and(|got| vn_container::vn_contains(&got, &want))
    })
}

// ── PhiPat (tagged or any) ────────────────────────────────────────────────────

/// Builder for `Phi` node patterns. Created by [`phi`].
///
/// Without [`for_vn`](Self::for_vn) matches any `Phi` discriminant;
/// `for_vn(vn)` narrows to the lifter-emitted SSA φ tagged `Some(vn)` in
/// `Function::get_vn_for_value` (keyed by the Phi's output value).
pub struct PhiPat {
    inner: NodePat,
    var_filter: Option<rsleigh::Vn>,
}

impl PhiPat {
    /// Constrain the value arriving from predecessor slot `idx` (shifted
    /// to raw input slot `idx + 1` to skip the phi-token input).
    pub fn input<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inner = self.inner.input(idx + 1, p);
        self
    }

    /// Require that *some* data input of the `Phi` matches `p`, without
    /// pinning which predecessor slot. A `Phi`'s incoming values are one per
    /// predecessor and usually order-irrelevant, so this is the common way to
    /// constrain a phi operand. Captures inside `p` bind out normally.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_any(p);
        self
    }

    /// Restrict the match to lifter-emitted SSA φ nodes whose
    /// `value_vn` entry (via `Function::get_vn_for_value`) is `Some(vn)`.
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.var_filter = Some(vn);
        self
    }

    /// Bind the matched `Phi`'s value output to `c`.
    pub fn capture(mut self, c: Capture) -> Self {
        self.inner = self.inner.capture(c);
        self
    }

    /// Apply the `for_vn` filter (if any) to the inner [`NodePat`].
    fn configured(self) -> NodePat {
        let PhiPat { inner, var_filter } = self;
        match var_filter {
            Some(vn) => inner.with_node_predicate(move || phi_var_limit(vn)),
            None => inner,
        }
    }

    /// Seal the builder into a finished [`Pattern`].
    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MatchPat for PhiPat {
    /// A `Phi` produces a value output (slot 0), so it nests as a value
    /// operand — `store(data=phi())`, `add(x, phi())` — anchored at that
    /// output.  (`MemPhi`, a memory token, implements [`MemPat`] instead.)
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

/// Construct a fresh [`PhiPat`].
pub fn phi() -> PhiPat {
    PhiPat {
        // `Phi` has a value output at slot 0 (captured/limited via it).
        inner: NodePat::value(KindSpec::variant_of(&NodeKind::Phi), 0),
        var_filter: None,
    }
}

/// Start building a tagged-`Phi` pattern (see [`phi`]) pinned to varnode
/// `vn` in `Function::get_vn_for_value` (keyed by the Phi's output value).
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
    pub fn input<M: MemPat + 'static>(self, idx: usize, p: M) -> Self {
        Self(self.0.input_mem(idx + 1, p))
    }

    /// Bind the matched `MemPhi`'s memory-token output to `c`.
    pub fn capture(self, c: Capture) -> Self {
        Self(self.0.capture(c))
    }

    /// Seal the builder into a finished [`Pattern`].
    pub fn build(self) -> Pattern {
        self.0.build()
    }
}

impl MemPat for MemPhiPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_anchored(b)
    }
}

/// Construct a fresh [`MemPhiPat`].
pub fn mem_phi() -> MemPhiPat {
    // `MemPhi` is node-rooted with a memory-token output at slot 0.
    MemPhiPat(NodePat::node(KindSpec::variant_of(&NodeKind::MemPhi)).with_mem_value(0))
}
