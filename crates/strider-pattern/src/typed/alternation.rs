//! Alternation over match-side sub-patterns: `one_of![a, b]` matches a value if
//! any alternative does. Nests anywhere a value operand is accepted.
//!
//! # Order the alternatives most-specific first
//!
//! First-match wins, so a permissive alternative shadows a narrower one placed
//! after it. The shadowing arm still *matches*, so the query silently returns
//! the wrong binding rather than failing. Wildcards (`any()` / `var(c)`) match
//! any node, including the operator a later arm was meant to recognise:
//!
//! ```ignore
//! // WRONG: `var(base)` also matches the `Add`, so `off` never binds and
//! // every `base + K` load silently reports no offset.
//! load().addr(one_of![var(base), add(var(base), any_int_const(off))])
//!
//! // RIGHT: the specific shape first, the bare fallback last.
//! load().addr(one_of![add(var(base), any_int_const(off)), var(base)])
//! ```
//!
//! Captures under an arm that did not fire stay unbound (not defaulted), so
//! `Match::value`/`node` returning `None` is how a caller tells which arm won.

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{MatcherBuilder, PatValueRef};

/// One type-erased entry of a [`OneOf`], normally built by the `one_of!` macro.
pub type BoxedAlt = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>;

/// Match-only: a rewrite RHS must build one concrete shape, so there is no
/// template counterpart.
pub struct OneOf {
    alts: Vec<BoxedAlt>,
}

impl OneOf {
    /// An empty list matches nothing.
    pub fn new(alts: Vec<BoxedAlt>) -> Self {
        Self { alts }
    }
}

#[doc(hidden)]
pub fn boxed_alt<P: MatchPat + 'static>(p: P) -> BoxedAlt {
    Box::new(move |b| p.compile(b))
}

impl MatchPat for OneOf {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let refs: Vec<PatValueRef> = self.alts.into_iter().map(|compile| compile(b)).collect();
        b.one_of(&refs)
    }
}

/// `one_of![a, b, c]`: match a value if any listed sub-pattern matches it.
#[macro_export]
macro_rules! one_of {
    ($($alt:expr),+ $(,)?) => {
        $crate::OneOf::new(::std::vec![ $( $crate::boxed_alt($alt) ),+ ])
    };
}
