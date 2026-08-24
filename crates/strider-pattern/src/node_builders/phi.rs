//! Raw input slot 0 is the phi-token edge from the owning `Region`, so
//! predecessor 0's value sits at slot 1. `.phi_input(i, p)` addresses
//! predecessors directly; `.input(i, p)` is the raw slot every other builder's
//! `input` is.
//!
//! `Phi` produces a value output at slot 0; `MemPhi` produces a memory token
//! there and implements [`MemPat`] so a `load` / `store` can chain off it.

use strider_ir::IRViewer;
use strider_ir::node::NodeKind;

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, NodePredicate, PatValueRef, Pattern};

use super::MemPat;
use super::flow::{OutputPat, WithOutput};
use super::node_pat::NodePat;

/// Pins the matched `Phi`'s `value_vn` entry to `Some(vn)`.
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
pub struct PhiPat {
    inner: NodePat,
    var_filter: Option<rsleigh::Vn>,
}

impl PhiPat {
    /// Predecessor `idx`'s incoming value, at raw input slot `idx + 1`.
    pub fn phi_input<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inner = self.inner.input(idx + 1, p);
        self
    }

    /// Raw input slot `slot`, unshifted: slot 0 is the phi token,
    /// predecessor `i`'s value is slot `i + 1`.
    pub fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inner = self.inner.input(slot, p);
        self
    }

    /// Matches some incoming value predecessor without pinning one. A typed
    /// sub matches only value predecessors; `var` / `anything` also binds the
    /// `PhiToken` ownership edge.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_any(p);
        self
    }

    /// The ownership edge: raw slot 0, carrying the owning `Region`'s
    /// `PhiToken` output.
    pub fn phi_token<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(0, p);
        self
    }

    /// Narrows to the lifter-emitted SSA phi whose `value_vn` entry is
    /// `Some(vn)`.
    pub fn for_vn(mut self, vn: rsleigh::Vn) -> Self {
        self.var_filter = Some(vn);
        self
    }

    /// The one output, at slot 0. Returns a terminal taking one of
    /// `.capture(c)`, `.of_width(w)`, `.of_type(ty)`.
    pub fn output(self, slot: usize) -> OutputPat<Self> {
        OutputPat::at(self, Some(slot))
    }

    /// Some output rather than a fixed slot; otherwise
    /// [`output`](Self::output).
    pub fn any_output(self) -> OutputPat<Self> {
        OutputPat::at(self, None)
    }

    /// Binds the value output.
    pub fn capture(mut self, c: Capture) -> Self {
        self.inner = self.inner.capture(c);
        self
    }

    fn configured(self) -> NodePat {
        let PhiPat { inner, var_filter } = self;
        match var_filter {
            Some(vn) => inner.with_node_predicate(move || phi_var_limit(vn)),
            None => inner,
        }
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MatchPat for PhiPat {
    /// Nests as a value operand anchored on the value output, as in
    /// `store(data=phi())` or `int_add(x, phi())`.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

pub fn phi() -> PhiPat {
    PhiPat {
        // Captures and predicates anchor on the value output at slot 0.
        inner: NodePat::value(KindSpec::variant_of(&NodeKind::Phi), 0),
        var_filter: None,
    }
}

impl WithOutput for PhiPat {
    fn capture_output(mut self, slot: Option<usize>, c: Capture) -> Self {
        self.inner = self.inner.capture_output(slot, c);
        self
    }
    fn output_width(mut self, slot: Option<usize>, bits: u32) -> Self {
        self.inner = self.inner.output_width(slot, bits);
        self
    }
    fn output_ty(mut self, slot: Option<usize>, ty: strider_ir::node::ValueType) -> Self {
        self.inner = self.inner.output_ty(slot, ty);
        self
    }
}

/// A [`phi`] pre-narrowed by [`PhiPat::for_vn`].
pub fn phi_for(vn: rsleigh::Vn) -> PhiPat {
    phi().for_vn(vn)
}

/// The memory-token phi at join points, producing its token at output slot 0.
/// Same slot layout as [`PhiPat`], with a memory token per predecessor.
pub struct MemPhiPat(NodePat);

impl MemPhiPat {
    /// Predecessor `idx`'s incoming memory token, at raw input slot `idx + 1`.
    /// The sub-pattern must be a memory producer.
    pub fn phi_input<M: MemPat + 'static>(self, idx: usize, p: M) -> Self {
        Self(self.0.input_mem(idx + 1, p))
    }

    /// Raw input slot `slot`, unshifted: slot 0 is the phi token,
    /// predecessor `i`'s memory token is slot `i + 1`.
    pub fn input<P: MatchPat + 'static>(self, slot: usize, p: P) -> Self {
        Self(self.0.input(slot, p))
    }

    /// Candidates are every input a fixed operand has not pinned: `PhiToken`
    /// at slot 0 and each memory predecessor after it. A typed value sub binds
    /// neither; only `var` / `anything` reaches them. Repeatable.
    pub fn any_input<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_any(p))
    }

    /// See [`PhiPat::phi_token`].
    pub fn phi_token<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(0, p))
    }

    /// The one output, at slot 0. Returns a terminal taking one of
    /// `.capture(c)`, `.of_width(w)`, `.of_type(ty)`.
    pub fn output(self, slot: usize) -> OutputPat<Self> {
        OutputPat::at(self, Some(slot))
    }

    /// Some output rather than a fixed slot; otherwise
    /// [`output`](Self::output).
    pub fn any_output(self) -> OutputPat<Self> {
        OutputPat::at(self, None)
    }

    /// Binds the memory-token output.
    pub fn capture(self, c: Capture) -> Self {
        Self(self.0.capture(c))
    }

    pub fn build(self) -> Pattern {
        self.0.build()
    }
}

impl MatchPat for MemPhiPat {
    /// The memory token is the only output, so a value slot never matches.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_anchored(b)
    }
}

impl MemPat for MemPhiPat {}

pub fn mem_phi() -> MemPhiPat {
    MemPhiPat(NodePat::node(KindSpec::variant_of(&NodeKind::MemPhi)).with_mem_value(0))
}

impl WithOutput for MemPhiPat {
    fn capture_output(self, slot: Option<usize>, c: Capture) -> Self {
        Self(self.0.capture_output(slot, c))
    }
    fn output_width(self, slot: Option<usize>, bits: u32) -> Self {
        Self(self.0.output_width(slot, bits))
    }
    fn output_ty(self, slot: Option<usize>, ty: strider_ir::node::ValueType) -> Self {
        Self(self.0.output_ty(slot, ty))
    }
}
