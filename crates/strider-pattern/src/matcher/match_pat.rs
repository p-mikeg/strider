use crate::matcher::{MatcherBuilder, PatValueRef, Pattern};

/// `Send` so a compiled `Pattern` can move between threads: the boxed
/// closures a pattern lowers to carry the same bound, and a pattern is built
/// from plain data, so nothing real is excluded.
pub trait MatchPat: Sized + Send {
    /// Lower into `b`, returning the value-output handle of the root node.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef;

    /// Lower into a memory slot, returning the memory-token handle. A node
    /// producing both a token and values (`Call`, `CallOther`) anchors on the
    /// token here and on a value in [`compile`](Self::compile); for everything
    /// else the two coincide.
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.compile(b)
    }

    fn into_pattern(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.compile(&mut b);
        b.finish()
    }
}

/// A pre-compiled output handle re-presented as a [`MatchPat`], so a shared
/// operand compiled once can feed several consumers.
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

    // The trait default would route back through `compile`, lowering the inner
    // pattern as a value operand in a memory slot.
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile_mem(b);
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

            fn compile_mem($me, $b: &mut MatcherBuilder) -> PatValueRef {
                let $o = $me.inner.compile_mem($b);
                $body
                $o
            }
        }
    };
}

decorator! {
    /// Node predicate on the inner pattern's root.
    Limited<P, F>
    [ where F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static + Send ]
    { f: F }
    |b, o, self| { b.set_node_predicate(o, Box::new(self.f)); }
}

decorator! {
    /// Post-match guard, with bindings visibility, on the inner root. A root
    /// with no value output fails it: the guard is typed, and there is no type
    /// to hand it.
    Guarded<P, F>
    [ where F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool + 'static + Send ]
    { f: F }
    |b, o, self| {
        b.set_post_match(
            o,
            Box::new(move |m, _node, ty, bnd| ty.is_some_and(|ty| (self.f)(m, ty, bnd))),
        );
    }
}

decorator! {
    /// Disables commutative operand reordering on the inner root.
    Ordered<P>
    {}
    |b, o, self| { b.set_force_ordered(o); }
}

decorator! {
    /// Pins the output-vertex width.
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
    /// `f` is typed, so a root with no value output (a control, memory or
    /// `PhiToken` edge, or a zero-output node) fails it. Guard those through
    /// [`Pattern::with_root_post_match`](crate::Pattern::with_root_post_match),
    /// which sees the missing type.
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
