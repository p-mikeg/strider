//! `one_of` — alternation over match-side sub-patterns.
//!
//! `one_of![a, b]` (Rust) / `one_of([a, b])` (Python) matches a value if *any*
//! alternative matches it (first-match wins, with the matcher's usual
//! backtracking). Nest it anywhere a value operand is accepted, so a single
//! pattern covers a value that may or may not be wrapped:
//!
//! ```ignore
//! // load whose address is `add(base, off)`, optionally masked by `and(_, k)`
//! let inner = || add(var(base), var(off));
//! load().addr(one_of![inner(), int_and(inner(), any_int_const(k))])
//! ```
//!
//! # Order the alternatives most-specific first
//!
//! First-match wins, so a **permissive** alternative placed before a narrower
//! one shadows it: the narrower arm is never tried, and — because the shadowing
//! arm still *matches* — the query silently returns the wrong binding rather
//! than failing. The wildcards (`any()` / `var(c)`) match ANY node, including
//! the very operator a later arm was meant to recognise:
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
//! An alternative that does not bind is left unbound rather than defaulted, so
//! `Match::value`/`node` returns `None` for its captures — that is how a caller
//! tells which arm fired (and supplies its own default for the absent one).

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{MatcherBuilder, PatValueRef};

/// A boxed, type-erased alternative — one entry of a [`OneOf`]. Produced by
/// [`boxed_alt`]; you normally build these through the [`one_of!`] macro rather
/// than by hand.
pub type BoxedAlt = Box<dyn FnOnce(&mut MatcherBuilder) -> PatValueRef>;

/// An alternation over several match-side sub-patterns. Build it with the
/// [`one_of!`] macro (`one_of![a, b, c]`); it lowers to a single alternation
/// node the matcher tries each alternative against. Match-only — there is no
/// template counterpart (a rewrite RHS must build one concrete shape, not
/// choose among several).
pub struct OneOf {
    alts: Vec<BoxedAlt>,
}

impl OneOf {
    /// Build an alternation from already-boxed alternatives. Prefer the
    /// [`one_of!`] macro, which boxes each pattern for you. An empty list
    /// matches nothing.
    pub fn new(alts: Vec<BoxedAlt>) -> Self {
        Self { alts }
    }
}

/// Type-erase one match-side pattern into a [`BoxedAlt`]. A [`one_of!`] macro
/// helper; not usually called directly.
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

/// `one_of![a, b, c]` — match a value if any of the listed sub-patterns matches
/// it. Sugar for [`OneOf::new`] that boxes each alternative.
#[macro_export]
macro_rules! one_of {
    ($($alt:expr),+ $(,)?) => {
        $crate::OneOf::new(::std::vec![ $( $crate::boxed_alt($alt) ),+ ])
    };
}
