use std::marker::PhantomData;

use crate::expr::{Deco, Expr, IntoExpr, Slot};
use crate::matcher::{MatcherBuilder, PatValueRef, Pattern};
use crate::typed::value_ops::MatchOnly;

/// A pattern the match side lowers.
pub trait MatchPat: IntoExpr + Send {
    /// Lower into `b`, returning the value-output handle of the root node.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.into_expr().lower(b, Slot::Value, false)
    }

    /// Lower as a root, whose value anchor also accepts the memory token of a node with no value
    /// output.
    fn into_pattern(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.into_expr().lower(&mut b, Slot::Root, false);
        b.finish()
    }
}

/// `P` with its root output bound to a capture.
pub struct Captured<P>(Expr, PhantomData<fn() -> P>);

impl<P> IntoExpr for Captured<P> {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl<P: MatchPat> MatchPat for Captured<P> {}

/// Only a `var()` leaf's capture is a template.
impl crate::TemplatePat for Captured<crate::typed::wildcards::Var> {}
impl<P> crate::template::template_pat::sealed::Sealed for Captured<P> {}

/// Combinators on every [`MatchPat`].
pub trait CaptureExt: MatchPat + 'static {
    fn capture(self, c: crate::capture::Capture) -> Captured<Self> {
        Captured(self.into_expr().capture(c), PhantomData)
    }
    /// Run `f` after the whole sub-pattern matches; `false` fails the match. Below the root, `f`
    /// sees only the captures of its own cone and of operands written before it. Refused on a
    /// root with no value output, which takes
    /// [`Pattern::with_root_post_match`](crate::Pattern::with_root_post_match).
    fn when_match<F>(self, f: F) -> MatchOnly
    where
        F: Fn(&crate::Matcher, strider_ir::node::ValueType, &crate::Bindings) -> bool
            + 'static
            + Send,
    {
        let guard: crate::PostMatchFn =
            Box::new(move |m, _node, ty, bnd| ty.is_some_and(|ty| f(m, ty, bnd)));
        MatchOnly(self.into_expr().deco(Deco::Guard(guard)))
    }
    /// Gate on a predicate run before inputs are descended into; see
    /// [`NodePredicate`](crate::matcher::NodePredicate).
    fn filter<F>(self, f: F) -> MatchOnly
    where
        F: Fn(&crate::Matcher, strider_ir::node::NodeId) -> bool + 'static + Send,
    {
        MatchOnly(self.into_expr().deco(Deco::Filter(Box::new(f))))
    }
    /// Pin the operand order of the node holding the pair this pattern was
    /// written with: the root, except where a lowering added the root over it
    /// (`int_ne` pins its `int_eq`).
    fn ordered(self) -> MatchOnly {
        MatchOnly(self.into_expr().deco(Deco::Ordered))
    }
    /// Pins the output-vertex width.
    fn of_width(self, n: u32) -> MatchOnly {
        MatchOnly(self.into_expr().deco(Deco::Width(n)))
    }
    /// Pins the output-vertex type.
    fn value_ty(self, ty: strider_ir::node::ValueType) -> MatchOnly {
        MatchOnly(self.into_expr().deco(Deco::Ty(ty)))
    }
    /// [`of_width(1)`](Self::of_width).
    fn bool_valued(self) -> MatchOnly {
        self.of_width(1)
    }
}
impl<P: MatchPat + 'static> CaptureExt for P {}
