//! The compile-time-typed match-side builder trait.
//!
//! [`MatchPat`] is implemented by every typed builder struct in
//! [`crate::typed`]. A struct's [`compile`](MatchPat::compile) lowers it
//! onto the imperative [`MatcherBuilder`](crate::builder::MatcherBuilder)
//! primitives, returning the [`PatOutRef`] of the sub-pattern's value
//! root; [`into_pattern`](MatchPat::into_pattern) seals a fresh builder
//! into a finished [`Pattern`].
//!
//! The combinator wrappers ([`Captured`] / [`Limited`] / [`Guarded`] /
//! [`Ordered`]) decorate an inner [`MatchPat`] with the annotator surface
//! of the builder (capture, node-limit, post-match, force-ordered). They
//! are produced by the [`CaptureExt`] blanket extension trait so any
//! `MatchPat` gains the `.capture(c)` / `.filter(f)` / `.when_match(f)` /
//! `.ordered()` fluent methods.

use crate::builder::{MatcherBuilder, PatOutRef};
use crate::pattern::Pattern;

/// A compile-time-typed match-side pattern that lowers onto the
/// imperative [`MatcherBuilder`].
pub trait MatchPat: Sized {
    /// Lower this pattern into `b`, returning the value-output handle of
    /// its root node.
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef;

    /// Seal this pattern into a finished [`Pattern`].
    #[must_use]
    fn into_pattern(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let root = self.compile(&mut b);
        b.finish(root)
    }
}

/// A pre-compiled output handle re-presented as a [`MatchPat`].
///
/// Lets a lowered builder (e.g. `float_le`) compile a shared operand
/// once and feed the resulting [`PatOutRef`] into multiple consumer
/// nodes — the bipartite store allows one output vertex to fan out to
/// several `Consumes` edges (still a DAG), so the operand sub-pattern is
/// shared rather than duplicated.
#[allow(dead_code)] // consumed by the lowered builders in `crate::typed`.
pub(crate) struct Pre(pub(crate) PatOutRef);
impl MatchPat for Pre {
    fn compile(self, _b: &mut MatcherBuilder) -> PatOutRef {
        self.0
    }
}

/// Captures the node producing the inner pattern's root output.
pub struct Captured<P> {
    pub(crate) inner: P,
    pub(crate) cap: crate::capture::Capture,
}
impl<P: MatchPat> MatchPat for Captured<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let o = self.inner.compile(b);
        b.capture_node(o, self.cap);
        o
    }
}

/// Attaches a node-local limit to the inner pattern's root node.
pub struct Limited<P, F> {
    pub(crate) inner: P,
    pub(crate) f: F,
}
impl<P: MatchPat, F> MatchPat for Limited<P, F>
where
    F: Fn(&crate::Matcher, strider_ir::node::NodeId, strider_ir::node::NodeOutputType) -> bool
        + 'static,
{
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let o = self.inner.compile(b);
        b.set_node_limit(o, Box::new(self.f));
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
    F: Fn(&crate::Matcher, strider_ir::node::NodeOutputType, &crate::Bindings) -> bool + 'static,
{
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
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
    fn compile(self, b: &mut MatcherBuilder) -> PatOutRef {
        let o = self.inner.compile(b);
        b.set_force_ordered(o);
        o
    }
}

/// Fluent combinator surface available on every [`MatchPat`].
pub trait CaptureExt: MatchPat {
    /// Bind the matched root node to `c`.
    fn capture(self, c: crate::capture::Capture) -> Captured<Self> {
        Captured { inner: self, cap: c }
    }
    /// Run `f` after the whole sub-pattern matches; fail the match if it
    /// returns `false`.
    fn when_match<F>(self, f: F) -> Guarded<Self, F>
    where
        F: Fn(&crate::Matcher, strider_ir::node::NodeOutputType, &crate::Bindings) -> bool + 'static,
    {
        Guarded { inner: self, f }
    }
    /// Gate the match on a node-local predicate that runs before
    /// descending into inputs.
    fn filter<F>(self, f: F) -> Limited<Self, F>
    where
        F: Fn(&crate::Matcher, strider_ir::node::NodeId, strider_ir::node::NodeOutputType) -> bool
            + 'static,
    {
        Limited { inner: self, f }
    }
    /// Forbid commutative operand reordering for this node.
    fn ordered(self) -> Ordered<Self> {
        Ordered { inner: self }
    }
}
impl<P: MatchPat> CaptureExt for P {}
