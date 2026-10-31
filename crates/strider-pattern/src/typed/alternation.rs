//! `one_of` is a union: every matching arm is enumerated with its own bindings,
//! so order carries no meaning and two overlapping arms each give a match,
//! as long as they BIND differently. Results dedup on the bindings alone, so
//! two arms that bind identically while covering different nodes collapse to
//! one, and the first listed arm's `Match::matched_nodes` is the one kept.
//! Give the arms distinct captures when that distinction matters.
//! [`OneOf::first`] is the ordered variant, cutting to the first arm that
//! yields a match. Captures under an arm that did not fire stay unbound, so
//! `Match::value` / `node` returning `None` tells which arm won.

use crate::expr::{Expr, IntoExpr};
use crate::matcher::match_pat::MatchPat;
use crate::node_builders::MemPat;

/// One entry of a [`OneOf`], normally built by the `one_of!` macro. It is
/// no pattern of its own, so an arm reaches neither a template nor a memory
/// slot past the bound its alternation is held to:
///
/// ```compile_fail,E0277
/// use strider_pattern::{Capture, TemplatePat, anything, boxed_alt, template, var};
/// TemplatePat::into_template(template::int_add(boxed_alt(anything()), var(Capture::new())));
/// ```
///
/// ```compile_fail,E0277
/// use strider_pattern::{Capture, boxed_alt, int_add, load, var};
/// let (a, b) = (Capture::new(), Capture::new());
/// load().mem(boxed_alt(int_add(var(a), var(b))));
/// ```
pub struct BoxedAlt(Expr);

/// Match-only: a rewrite RHS must build one concrete shape.
///
/// The alternation's own output is `Any`, so it nests in a value, memory or
/// control slot alike; the slot it lands in is passed down to the arms, which
/// anchor accordingly.
pub struct OneOf(Expr);

impl OneOf {
    /// A union over the alternatives. An empty list matches nothing.
    ///
    /// # Cost
    ///
    /// Under [`find_all`](crate::Matcher::find_all) every arm is explored under
    /// every configuration of the others, so `k` alternations anywhere under
    /// one root, siblings as much as nested, run the continuation `m^k` times
    /// per root candidate for `m` matching arms each, with identically-binding
    /// arms collapsing in the dedup. It scales with the pattern, not the
    /// graph. [`match_at`](crate::Matcher::match_at) stops at the first match;
    /// [`OneOf::first`] cuts instead of enumerating.
    pub fn new(alts: Vec<BoxedAlt>) -> Self {
        Self(Expr::alt(alts.into_iter().map(|a| a.0).collect(), false))
    }

    /// An ordered choice: cut to the first alternative that yields a match, and
    /// report every binding THAT arm produces. An arm a guard above rejects
    /// falls through to the next one; a winning arm with two commutative
    /// orderings still reports both.
    pub fn first(alts: Vec<BoxedAlt>) -> Self {
        Self(Expr::alt(alts.into_iter().map(|a| a.0).collect(), true))
    }
}

#[doc(hidden)]
pub fn boxed_alt<P: MatchPat + 'static>(p: P) -> BoxedAlt {
    BoxedAlt(p.into_expr())
}

impl IntoExpr for OneOf {
    fn into_expr(self) -> Expr {
        self.0
    }
}

impl MatchPat for OneOf {}
impl MemPat for OneOf {}

/// `one_of![a, b, c]`: match if any listed sub-pattern matches, in whatever
/// slot the alternation sits. An arm is any pattern: a value shape, a memory
/// producer (`store`, `mem_phi`, `call`), or a node-rooted control builder
/// (`ret`, `if_else`, ...).
/// [`OneOf::new`] documents the `m^k` enumeration cost of combining these.
#[macro_export]
macro_rules! one_of {
    ($($alt:expr),+ $(,)?) => {
        $crate::OneOf::new(::std::vec![ $( $crate::boxed_alt($alt) ),+ ])
    };
}

/// `first_of![a, b, c]`: ordered choice, cutting to the first sub-pattern that
/// yields a match. Takes the same arms as [`one_of!`].
#[macro_export]
macro_rules! first_of {
    ($($alt:expr),+ $(,)?) => {
        $crate::OneOf::first(::std::vec![ $( $crate::boxed_alt($alt) ),+ ])
    };
}
