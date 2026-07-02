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
    inner: P,
    pub(crate) cap: crate::capture::Capture,
}
impl<P: MatchPat> MatchPat for Captured<P> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        b.capture_output(o, self.cap);
        o
    }
}

/// Emit a single-purpose `MatchPat` decorator: a struct wrapping
/// `inner: P` plus the listed payload fields, whose `compile` lowers the
/// inner pattern and then runs `$body` (with `b` the builder, `o` the
/// inner root output, and `self` bound) to annotate the root, returning
/// `o`. Each of the five decorators below is exactly this shape — the
/// only variation is the payload fields (none / closure / `u32` /
/// `ValueType`) and the one annotator call — so one macro replaces five
/// near-identical struct+impl pairs. The struct names / fields / generic
/// shape are preserved verbatim (`OfWidth<P>` is named directly by
/// `wildcards::value_of_width`, and all five are re-exported).
macro_rules! decorator {
    (
        $(#[$smeta:meta])*
        $struct:ident < P $(, $g:ident)? >
        $([ where $($pred:tt)+ ])?
        { $($(#[$fmeta:meta])* $field:ident : $fty:ty),* $(,)? }
        |$b:ident, $o:ident, $me:ident| $body:block
    ) => {
        $(#[$smeta])*
        pub struct $struct<P $(, $g)?> {
            inner: P,
            $($(#[$fmeta])* pub(crate) $field: $fty),*
        }
        impl<P: MatchPat $(, $g)?> MatchPat for $struct<P $(, $g)?>
        $(where $($pred)+,)?
        {
            fn compile($me, $b: &mut MatcherBuilder) -> PatValueRef {
                let $o = $me.inner.compile($b);
                $body
                $o
            }
        }
    };
}

decorator! {
    /// Attaches a node predicate to the inner pattern's root node.
    Limited<P, F>
    [ where F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static ]
    { f: F }
    |b, o, self| { b.set_node_predicate(o, Box::new(self.f)); }
}

decorator! {
    /// Attaches a post-match guard (with bindings visibility) to the inner
    /// pattern's root node.
    Guarded<P, F>
    [ where F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool + 'static ]
    { f: F }
    |b, o, self| {
        b.set_post_match(o, Box::new(move |m, _node, ty, bnd| (self.f)(m, ty, bnd)));
    }
}

decorator! {
    /// Disables commutative operand reordering on the inner pattern's root
    /// node.
    Ordered<P>
    {}
    |b, o, self| { b.set_force_ordered(o); }
}

decorator! {
    /// Constrains the inner pattern's value output to exactly `bits` wide.
    ///
    /// Pins the declarative output-vertex width, which the matcher checks
    /// against the matched output both at the root (the root output
    /// vertex's constraint applies to whichever output is rooted,
    /// regardless of slot) and when the node is consumed nested.
    OfWidth<P>
    { bits: u32 }
    |b, o, self| { b.set_value_width(o, self.bits); }
}

decorator! {
    /// Constrains the inner pattern's value output to exactly `ty`.
    ///
    /// Like [`OfWidth`] but pins the exact
    /// [`ValueType`](strider_ir::node::ValueType).
    ValueTy<P>
    { ty: strider_ir::node::ValueType }
    |b, o, self| { b.set_value_ty(o, self.ty); }
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
