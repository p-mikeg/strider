//! Phi-family builders: `PhiPat`, `MemPhiPat`, `ValuePhiPat`.
//!
//! `Phi` and `MemPhi` are distinguished by `NodeKind` discriminant.
//! Input layout: predecessor 0's value lives at raw input slot 1 —
//! input 0 is the phi-token edge from the owning `Region`. `.input(i, p)`
//! shifts by +1 so callers address predecessor slots directly.
//!
//! The tagged-vs-anonymous `Phi` distinction reads `Function::phi_var_tag`
//! at match time via a node-only limit (short-circuits before child
//! recursion):
//!
//! * [`PhiPat::for_vn`] — restrict to `Phi` whose tag is `Some(vn)`.
//! * [`ValuePhiPat`] — restrict to anonymous phis (`tag == None`).
//!
//! `MemPhi` produces a memory token (output slot 0); it implements
//! [`MemPat`] so a `load` / `store` can chain off it. `Phi` / `ValuePhi`
//! produce a value output (slot 0).

use strider_ir::node::NodeKind;

use crate::builder::{MatcherBuilder, PatNodeRef, PatOutRef};
use crate::match_pat::MatchPat;
use crate::pattern::{KindSpec, Pattern};

use super::{MemPat, SubCompiler};

/// Filter applied at match time over `Function::phi_var_tag`.
#[derive(Clone, Copy)]
enum PhiVarFilter {
    /// Match only phis whose tag equals `Some(vn)`.
    Exact(rsleigh::Vn),
    /// Match only anonymous phis (`tag == None`).
    Anonymous,
}

/// Collected sparse predecessor sub-patterns (raw input slot → compiler).
type IndexedInputs = Vec<(usize, SubCompiler)>;

/// Lower a phi-family node (`kind_exemplar`) onto `b` with the given
/// indexed predecessor sub-patterns and optional `phi_var_tag` filter,
/// returning the node handle plus its slot-0 output (a value output for
/// `Phi`, a memory-token output for `MemPhi`). Shared by `Phi`,
/// `MemPhi`, and anonymous `Phi`.
fn lower_phi(
    b: &mut MatcherBuilder,
    kind_exemplar: NodeKind,
    inputs: IndexedInputs,
    var_filter: Option<PhiVarFilter>,
) -> (PatNodeRef, PatOutRef) {
    let node = b.node(KindSpec::Variant(std::mem::discriminant(&kind_exemplar)));
    // The slot-0 output models the phi's produced token: a value output
    // for `Phi`, the memory token for `MemPhi`. This is the chaining
    // handle (a `MemPhi`'s memory output feeds a downstream load/store)
    // and the anchor for the var-tag node limit.
    let anchor = match kind_exemplar {
        NodeKind::MemPhi => b.memory_output(node, 0),
        _ => b.value_output(node, 0),
    };
    for (slot, compile) in inputs {
        let o = compile(b);
        b.input(node, slot, o);
    }
    if let Some(f) = var_filter {
        b.set_node_limit(
            anchor,
            Box::new(move |m, n, _ty| {
                let tag = m.function().phi_var_tag(n);
                match f {
                    PhiVarFilter::Exact(want) => tag == Some(want),
                    PhiVarFilter::Anonymous => tag.is_none(),
                }
            }),
        );
    }
    (node, anchor)
}

// ── PhiPat (tagged or any) ────────────────────────────────────────────────────

/// Builder for `Phi` node patterns. Created by [`phi`].
///
/// Without [`for_vn`](Self::for_vn) matches any `Phi` discriminant;
/// `for_vn(vn)` narrows to the lifter-emitted SSA φ tagged `Some(vn)` in
/// `Function::phi_var_tag`.
#[derive(Default)]
pub struct PhiPat {
    inputs: IndexedInputs,
    var_filter: Option<PhiVarFilter>,
}

impl PhiPat {
    /// Constrain the value arriving from predecessor slot `idx` (shifted
    /// to raw input slot `idx + 1` to skip the phi-token input).
    #[must_use]
    pub fn input<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inputs
            .push((idx + 1, Box::new(move |b| p.compile(b))));
        self
    }

    /// Restrict the match to lifter-emitted SSA φ nodes whose
    /// `Function::phi_var_tag` is `Some(vn)`.
    #[must_use]
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.var_filter = Some(PhiVarFilter::Exact(vn));
        self
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let (node, _out) = lower_phi(&mut b, NodeKind::Phi, self.inputs, self.var_filter);
        b.finish_node(node)
    }
}

/// Construct a fresh [`PhiPat`].
#[must_use]
pub fn phi() -> PhiPat {
    PhiPat::default()
}

/// Start building a tagged-`Phi` pattern (see [`phi`]) pinned to varnode
/// `vn` in `Function::phi_var_tag`.
#[must_use]
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    PhiPat::default().for_vn(vn)
}

// ── MemPhiPat ─────────────────────────────────────────────────────────────────

/// Builder for `MemPhi` node patterns. Created by [`mem_phi`].
///
/// `MemPhi` is the memory-token phi at join points. Produces a memory
/// token (output slot 0) — implements [`MemPat`] so a `load` / `store`
/// can chain off it. Same input shift (+1) as [`PhiPat`].
#[derive(Default)]
pub struct MemPhiPat {
    inputs: IndexedInputs,
}

impl MemPhiPat {
    /// Constrain the memory token arriving from predecessor slot `idx`
    /// (shifted to raw input slot `idx + 1`). The sub-pattern must be a
    /// memory producer.
    #[must_use]
    pub fn input<M: MemPat + 'static>(mut self, idx: usize, p: M) -> Self {
        self.inputs
            .push((idx + 1, Box::new(move |b| p.compile_mem(b))));
        self
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let (node, _out) = lower_phi(&mut b, NodeKind::MemPhi, self.inputs, None);
        b.finish_node(node)
    }
}

impl MemPat for MemPhiPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatOutRef {
        let (_node, mem_out) = lower_phi(b, NodeKind::MemPhi, self.inputs, None);
        mem_out
    }
}

/// Construct a fresh [`MemPhiPat`].
#[must_use]
pub fn mem_phi() -> MemPhiPat {
    MemPhiPat::default()
}

// ── ValuePhiPat (anonymous) ──────────────────────────────────────────────────

/// Builder for anonymous `Phi` (value-phi) node patterns. Created by
/// [`value_phi`]. Same kind discriminant as [`PhiPat`], narrowed to
/// anonymous phis (`phi_var_tag == None`) — the shape `LoadForward`
/// synthesises when forwarding a `Load[sp+K]` across a `MemPhi`.
#[derive(Default)]
pub struct ValuePhiPat {
    inputs: IndexedInputs,
}

impl ValuePhiPat {
    /// Constrain the value arriving from predecessor slot `idx` (shifted
    /// to raw input slot `idx + 1`).
    #[must_use]
    pub fn input<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inputs
            .push((idx + 1, Box::new(move |b| p.compile(b))));
        self
    }

    /// Seal the builder into a finished [`Pattern`].
    #[must_use]
    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let (node, _out) = lower_phi(
            &mut b,
            NodeKind::Phi,
            self.inputs,
            Some(PhiVarFilter::Anonymous),
        );
        b.finish_node(node)
    }
}

/// Construct a fresh [`ValuePhiPat`].
#[must_use]
pub fn value_phi() -> ValuePhiPat {
    ValuePhiPat::default()
}
