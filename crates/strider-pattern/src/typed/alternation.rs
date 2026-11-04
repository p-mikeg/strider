//! `one_of` is a union: every matching arm yields its own match. Results dedup on bindings, so
//! arms that bind identically collapse to one, keeping the first listed arm's
//! `Match::matched_nodes`. [`OneOf::first`] is the ordered variant. Captures under an arm that did
//! not fire stay unbound.

use crate::expr::{Expr, IntoExpr};
use crate::matcher::match_pat::MatchPat;
use crate::node_builders::MemPat;

/// One arm of a [`OneOf`], built by `one_of!`. Not a pattern itself, so it fits neither a
/// template nor a memory slot:
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

/// Match-only alternation. Fits a value, memory or control slot; the slot is passed down to the
/// arms.
pub struct OneOf(Expr);

impl OneOf {
    /// A union over the alternatives. An empty list matches nothing.
    ///
    /// # Cost
    ///
    /// Under [`find_all`](crate::Matcher::find_all), `k` alternations of `m` matching arms each
    /// (siblings or nested) run the continuation `m^k` times per root candidate.
    /// [`match_at`](crate::Matcher::match_at) stops at the first match unless the pattern holds a
    /// [`OneOf::first`] or a cast mask.
    pub fn new(alts: Vec<BoxedAlt>) -> Self {
        Self(Expr::alt(alts.into_iter().map(|a| a.0).collect(), false))
    }

    /// An ordered choice: at each read of a value, keep only the matches through the first-listed
    /// arm that yields one, with every binding that arm produces. Each read settles on its own
    /// (`float_le` reads its operands twice), in pattern order, inner before outer. An arm a guard
    /// above rejects falls through to the next.
    ///
    /// # Cost
    ///
    /// Runs every configuration before answering, [`match_at`](crate::Matcher::match_at)
    /// included, unless the query wants one match and finds it through every `first_of`'s first
    /// arm without walking a cast.
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

/// `one_of![a, b, c]`: match if any arm matches. An arm is any pattern (value, memory or
/// control); see [`OneOf::new`] for the `m^k` cost.
#[macro_export]
macro_rules! one_of {
    ($($alt:expr),+ $(,)?) => {
        $crate::OneOf::new(::std::vec![ $( $crate::boxed_alt($alt) ),+ ])
    };
}

/// `first_of![a, b, c]`: ordered choice; see [`OneOf::first`].
#[macro_export]
macro_rules! first_of {
    ($($alt:expr),+ $(,)?) => {
        $crate::OneOf::first(::std::vec![ $( $crate::boxed_alt($alt) ),+ ])
    };
}
