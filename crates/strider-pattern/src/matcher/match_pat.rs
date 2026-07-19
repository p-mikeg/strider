//! The compile-time-typed match-side builder trait, implemented by every typed
//! builder struct in [`crate::typed`].
//!
//! The combinator wrappers ([`Captured`] / [`Limited`] / [`Guarded`] /
//! [`Ordered`] / [`OfWidth`] / [`ValueTy`]) decorate an inner [`MatchPat`] with
//! the builder's annotator surface, and are produced by the [`CaptureExt`]
//! blanket trait.

use crate::matcher::{MatcherBuilder, PatValueRef, Pattern};

pub trait MatchPat: Sized {
    /// Lower into `b`, returning the value-output handle of the root node.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef;

    fn into_pattern(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.compile(&mut b);
        b.finish()
    }
}

/// A pre-compiled output handle re-presented as a [`MatchPat`], so a lowered
/// builder like `float_le` can compile a shared operand once and feed it to
/// several consumers. One output vertex fanning out to several `Consumes`
/// edges is still a DAG.
pub(crate) struct Pre(pub(crate) PatValueRef);
impl MatchPat for Pre {
    fn compile(self, _b: &mut MatcherBuilder) -> PatValueRef {
        self.0
    }
}

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

/// Emit a `MatchPat` decorator: a struct wrapping `inner: P` plus the listed
/// payload fields, whose `compile` lowers the inner pattern, runs `$body` (with
/// `b` the builder, `o` the inner root output, `self` bound) to annotate the
/// root, and returns `o`.
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
    /// Node predicate on the inner pattern's root.
    Limited<P, F>
    [ where F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static ]
    { f: F }
    |b, o, self| { b.set_node_predicate(o, Box::new(self.f)); }
}

decorator! {
    /// Post-match guard, with bindings visibility, on the inner root.
    Guarded<P, F>
    [ where F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool + 'static ]
    { f: F }
    |b, o, self| {
        b.set_post_match(o, Box::new(move |m, _node, ty, bnd| (self.f)(m, ty, bnd)));
    }
}

decorator! {
    /// Disables commutative operand reordering on the inner root.
    Ordered<P>
    {}
    |b, o, self| { b.set_force_ordered(o); }
}

decorator! {
    /// Pins the output-vertex width. Checked both at the root (where it
    /// applies to whichever output is rooted, regardless of slot) and when the
    /// node is consumed nested.
    OfWidth<P>
    { bits: u32 }
    |b, o, self| { b.set_value_width(o, self.bits); }
}

decorator! {
    /// [`OfWidth`] pinning the exact type rather than the width.
    ValueTy<P>
    { ty: strider_ir::node::ValueType }
    |b, o, self| { b.set_value_ty(o, self.ty); }
}

/// Fluent combinator surface available on every [`MatchPat`].
pub trait CaptureExt: MatchPat {
    fn capture(self, c: crate::capture::Capture) -> Captured<Self> {
        Captured {
            inner: self,
            cap: c,
        }
    }
    /// Run `f` after the whole sub-pattern matches; `false` fails the match.
    fn when_match<F>(self, f: F) -> Guarded<Self, F>
    where
        F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool + 'static,
    {
        Guarded { inner: self, f }
    }
    /// Gate on a predicate running before inputs are descended into.
    fn filter<F>(self, f: F) -> Limited<Self, F>
    where
        F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static,
    {
        Limited { inner: self, f }
    }
    fn ordered(self) -> Ordered<Self> {
        Ordered { inner: self }
    }
    fn of_width(self, n: u32) -> OfWidth<Self> {
        OfWidth {
            inner: self,
            bits: n,
        }
    }
    fn value_ty(self, ty: strider_ir::node::ValueType) -> ValueTy<Self> {
        ValueTy { inner: self, ty }
    }
    /// [`of_width(1)`](Self::of_width).
    fn bool_valued(self) -> OfWidth<Self> {
        self.of_width(1)
    }
}
impl<P: MatchPat> CaptureExt for P {}
