//! Raw input slot 0 is the phi-token edge from the owning `Region`, so
//! predecessor 0's value sits at slot 1. `.phi_input(i, p)` addresses
//! predecessors directly; `.input(i, p)` is the raw slot every other builder's
//! `input` is.
//!
//! `Phi` produces a value output at slot 0; `MemPhi` produces a memory token
//! there and implements [`MemPat`] so a `load` / `store` can chain off it.

use super::{delegate_node_pat, node_match_pat};
use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, NodePredicate};

use super::MemPat;
use crate::expr::Node;

/// Requires the matched `Phi`'s `value_vn` tag to contain `vn`.
fn phi_var_limit(want: rsleigh::Vn) -> NodePredicate {
    Box::new(move |m, n| {
        let v = m.function().node_outputs(n)[0];
        // The tag stores the largest container, so a pinned sub-register
        // matches its container.
        m.function()
            .get_vn_for_value(v)
            .is_some_and(|got| vn_container::vn_contains(&got, &want))
    })
}

/// Matches any `Phi` unless narrowed by [`for_vn`](Self::for_vn).
pub struct PhiPat(Node);

impl PhiPat {
    /// Predecessor `idx`'s incoming value, at raw input slot `idx + 1`.
    pub fn phi_input<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(NodeKind::Phi.input_head_len() + idx, p))
    }

    /// The ownership edge, carrying the owning `Region`'s `PhiToken` output.
    pub fn phi_token<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.raw_input(NodeKind::PHI_TOKEN_SLOT, p))
    }

    /// Narrows to the lifter-emitted SSA phi whose `value_vn` tag contains
    /// `vn`, so a sub-register matches its container's phi. Repeated, every
    /// `vn` must hold.
    pub fn for_vn(self, vn: rsleigh::Vn) -> Self {
        Self(self.0.with_node_predicate(phi_var_limit(vn)))
    }
}

pub fn phi() -> PhiPat {
    // Captures and predicates anchor on the value output at slot 0.
    PhiPat(Node::value(KindSpec::variant_of(&NodeKind::Phi), 0))
}

delegate_node_pat! { PhiPat, 0, [capture, build, input, any_input], with_output }

/// A [`phi`] pre-narrowed by [`PhiPat::for_vn`].
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    phi().for_vn(vn)
}

/// The memory-token phi at join points, producing its token at output slot 0.
/// Same slot layout as [`PhiPat`], with a memory token per predecessor.
pub struct MemPhiPat(Node);

impl MemPhiPat {
    /// Predecessor `idx`'s incoming memory token.
    /// The sub-pattern must be a memory producer.
    pub fn phi_input<M: MemPat + 'static>(self, idx: usize, p: M) -> Self {
        Self(self.0.input_mem(NodeKind::MemPhi.input_head_len() + idx, p))
    }

    /// See [`PhiPat::phi_token`].
    pub fn phi_token<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.raw_input(NodeKind::PHI_TOKEN_SLOT, p))
    }
}

impl MemPat for MemPhiPat {}

pub fn mem_phi() -> MemPhiPat {
    MemPhiPat(
        Node::new(KindSpec::variant_of(&NodeKind::MemPhi)).with_mem_value(NodeKind::MEM_OUT_SLOT),
    )
}

delegate_node_pat! { MemPhiPat, 0, [capture, build, input, any_input], with_output }

node_match_pat! {
    PhiPat => |p| p.0,
    MemPhiPat => |p| p.0,
}
