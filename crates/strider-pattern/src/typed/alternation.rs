//! `one_of` is a union: every matching arm yields its own match, deduplicated on bindings.
//! [`OneOf::first`] is the ordered variant. Captures under an arm that did not fire stay unbound.

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
    /// A union over the alternatives. An empty list matches nothing. `k` alternations of `m`
    /// matching arms run up to `m^k` configurations per root candidate.
    pub fn new(alts: Vec<BoxedAlt>) -> Self {
        Self(Expr::alt(alts.into_iter().map(|a| a.0).collect(), false))
    }

    /// An ordered choice: at each read of a value, keep only the matches through the first-listed
    /// arm that yields one. Each read settles on its own (`float_le` reads its operands twice).
    /// An arm a guard above rejects falls through to the next. Runs every configuration before
    /// answering unless the first arms everywhere give the one match wanted.
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
