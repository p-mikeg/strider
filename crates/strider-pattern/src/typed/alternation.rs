//! `one_of` is a union: every matching arm is enumerated with its own bindings,
//! so order carries no meaning and two overlapping arms each give a match,
//! as long as they BIND differently. Results dedup on the bindings alone, so
//! two arms that bind identically while covering different nodes collapse to
//! one, and the first listed arm's `Match::matched_nodes` is the one kept.
//! Give the arms distinct captures when that distinction matters.
//! [`OneOf::first`] is the ordered variant, cutting to the first arm that
//! yields a match. Captures under an arm that did not fire stay unbound, so
//! `Match::value` / `node` returning `None` tells which arm won.

use crate::matcher::match_pat::MatchPat;
use crate::matcher::{MatcherBuilder, PatValueRef};
use crate::node_builders::MemPat;

/// Which flavour of slot the alternation is being lowered into, decided by the
/// consumer rather than by the arm.
#[derive(Clone, Copy)]
pub enum AltSlot {
    Value,
    Memory,
}

/// One type-erased entry of a [`OneOf`], normally built by the `one_of!` macro.
/// It keeps both lowerings open until the slot is known.
pub type BoxedAlt = Box<dyn FnOnce(&mut MatcherBuilder, AltSlot) -> PatValueRef + Send>;

/// Match-only: a rewrite RHS must build one concrete shape.
pub struct OneOf {
    alts: Vec<BoxedAlt>,
    first_match: bool,
}

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
        Self {
            alts,
            first_match: false,
        }
    }

    /// An ordered choice: cut to the first alternative that yields a match, and
    /// report every binding THAT arm produces. An arm a guard above rejects
    /// falls through to the next one; a winning arm with two commutative
    /// orderings still reports both.
    pub fn first(alts: Vec<BoxedAlt>) -> Self {
        Self {
            alts,
            first_match: true,
        }
    }
}

#[doc(hidden)]
pub fn boxed_alt<P: MatchPat + 'static>(p: P) -> BoxedAlt {
    Box::new(move |b, slot| match slot {
        AltSlot::Value => p.compile(b),
        AltSlot::Memory => p.compile_mem(b),
    })
}

impl OneOf {
    fn lower(self, b: &mut MatcherBuilder, slot: AltSlot) -> PatValueRef {
        let first_match = self.first_match;
        let refs: Vec<PatValueRef> = self
            .alts
            .into_iter()
            .map(|compile| compile(b, slot))
            .collect();
        if first_match {
            b.first_of(&refs)
        } else {
            b.one_of(&refs)
        }
    }
}

/// The alternation's own output is `Any`, so it nests in a value, memory or
/// control slot alike; the slot it lands in is passed down to the arms, which
/// anchor accordingly. A control slot retypes the arm vertices instead, in
/// [`MatcherBuilder::set_output_control`](crate::matcher::MatcherBuilder::set_output_control).
impl MatchPat for OneOf {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b, AltSlot::Value)
    }

    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b, AltSlot::Memory)
    }
}

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
