//! The compile-time-typed match-side builder trait.
//!
//! [`MatchPat`] is implemented by every typed builder struct in
//! [`crate::typed`]. A struct's [`compile`](MatchPat::compile) lowers it
//! onto the imperative [`MatcherBuilder`]
//! primitives, returning the [`PatValueRef`] of the sub-pattern's value
//! root; [`into_pattern`](MatchPat::into_pattern) seals a fresh builder
//! into a finished [`Pattern`].
//!
//! The combinator wrappers ([`Captured`] / [`Limited`] / [`Guarded`] /
//! [`Ordered`] / [`OfWidth`] / [`ValueTy`]) decorate an inner
//! [`MatchPat`] with the annotator surface of the builder (capture,
//! node-limit, post-match, force-ordered, output width/type). They are
//! produced by the [`CaptureExt`] blanket extension trait so any
//! `MatchPat` gains the `.capture(c)` / `.filter(f)` / `.when_match(f)` /
//! `.ordered()` / `.of_width(n)` / `.value_ty(ty)` / `.bool_valued()`
//! fluent methods.

use crate::matcher::{MatcherBuilder, PatValueRef, Pattern};

/// A compile-time-typed match-side pattern that lowers onto the
/// imperative [`MatcherBuilder`].
pub trait MatchPat: Sized {
    /// Lower this pattern into `b`, returning the value-output handle of
    /// its root node.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef;

    /// Seal this pattern into a finished [`Pattern`].
    fn into_pattern(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.compile(&mut b);
        b.finish()
    }
}

/// A pre-compiled output handle re-presented as a [`MatchPat`].
///
/// Lets a lowered builder (e.g. `float_le`) compile a shared operand
/// once and feed the resulting [`PatValueRef`] into multiple consumer
/// nodes — the bipartite store allows one output vertex to fan out to
/// several `Consumes` edges (still a DAG), so the operand sub-pattern is
/// shared rather than duplicated.
pub(crate) struct Pre(pub(crate) PatValueRef);
impl MatchPat for Pre {
    fn compile(self, _b: &mut MatcherBuilder) -> PatValueRef {
        self.0
    }
}

/// Captures the node producing the inner pattern's root output.
pub struct Captured<P> {
    pub(crate) inner: P,
    pub(crate) cap: crate::capture::Capture,
}
impl<P: MatchPat> MatchPat for Captured<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.capture_output(o, self.cap);
        o
    }
}

/// Attaches a node predicate to the inner pattern's root node.
pub struct Limited<P, F> {
    pub(crate) inner: P,
    pub(crate) f: F,
}
impl<P: MatchPat, F> MatchPat for Limited<P, F>
where
    F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static,
{
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.set_node_predicate(o, Box::new(self.f));
        o
    }
}

/// Attaches a post-match guard (with bindings visibility) to the inner
/// pattern's root node.
pub struct Guarded<P, F> {
    pub(crate) inner: P,
    pub(crate) f: F,
}
impl<P: MatchPat, F> MatchPat for Guarded<P, F>
where
    F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool + 'static,
{
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.set_post_match(o, Box::new(move |m, _node, ty, bnd| (self.f)(m, ty, bnd)));
        o
    }
}

/// Disables commutative operand reordering on the inner pattern's root
/// node.
pub struct Ordered<P> {
    pub(crate) inner: P,
}
impl<P: MatchPat> MatchPat for Ordered<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.set_force_ordered(o);
        o
    }
}

/// Constrains the inner pattern's value output to exactly `bits` wide.
///
/// Pins the declarative output-vertex width, which the matcher checks
/// against the matched output both at the root (the root output vertex's
/// constraint applies to whichever output is rooted, regardless of slot)
/// and when the node is consumed nested.
pub struct OfWidth<P> {
    pub(crate) inner: P,
    pub(crate) bits: u32,
}
impl<P: MatchPat> MatchPat for OfWidth<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.set_value_width(o, self.bits);
        o
    }
}

/// Constrains the inner pattern's value output to exactly `ty`.
///
/// Like [`OfWidth`] but pins the exact
/// [`ValueType`](strider_ir::node::ValueType).
pub struct ValueTy<P> {
    pub(crate) inner: P,
    pub(crate) ty: strider_ir::node::ValueType,
}
impl<P: MatchPat> MatchPat for ValueTy<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.set_value_ty(o, self.ty);
        o
    }
}

/// Fluent combinator surface available on every [`MatchPat`].
pub trait CaptureExt: MatchPat {
    /// Bind the matched root node to `c`.
    fn capture(self, c: crate::capture::Capture) -> Captured<Self> {
        Captured {
            inner: self,
            cap: c,
        }
    }
    /// Run `f` after the whole sub-pattern matches; fail the match if it
    /// returns `false`.
    fn when_match<F>(self, f: F) -> Guarded<Self, F>
    where
        F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool + 'static,
    {
        Guarded { inner: self, f }
    }
    /// Gate the match on a node predicate that runs before descending
    /// into inputs.
    fn filter<F>(self, f: F) -> Limited<Self, F>
    where
        F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static,
    {
        Limited { inner: self, f }
    }
    /// Forbid commutative operand reordering for this node.
    fn ordered(self) -> Ordered<Self> {
        Ordered { inner: self }
    }
    /// Constrain the matched node's value output to exactly `n` bits.
    fn of_width(self, n: u32) -> OfWidth<Self> {
        OfWidth {
            inner: self,
            bits: n,
        }
    }
    /// Constrain the matched node's value output to exactly `ty`.
    fn value_ty(self, ty: strider_ir::node::ValueType) -> ValueTy<Self> {
        ValueTy { inner: self, ty }
    }
    /// Constrain the matched node's value output to a boolean (1-bit
    /// `I1`). Sugar for [`of_width(1)`](Self::of_width).
    fn bool_valued(self) -> OfWidth<Self> {
        self.of_width(1)
    }
}
impl<P: MatchPat> CaptureExt for P {}
