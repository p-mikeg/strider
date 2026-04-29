//! Capture variable newtypes used throughout the pattern engine.
//!
//! [`Capture`] is the unified data/control capture handle: every
//! pattern position that wants to bind a matched node uses the same
//! type.  After a successful match, [`crate::Match::node`] returns the
//! `NodeId` and [`crate::Match::output`] returns the value
//! `NodeOutputId` (or `None` for control-flow nodes that have no
//! single value output).
//!
//! Every capture variable is a globally-unique `u32` id.  The 12 typed
//! payload-capture types below carry no data besides that id — they
//! exist purely to keep the type of a binding visible at the call site
//! (an `IntVar` binds an integer constant value, an `IntBinaryOpVar`
//! binds an `IntBinaryOp` discriminant, and so on).
//!
//! All share the same `::new()` / `Default::default()` / deriveable
//! traits; the `decl_var!` macro emits each instance in a single line.

use std::sync::atomic::{AtomicU32, Ordering};

static NEXT: AtomicU32 = AtomicU32::new(0);

fn next_id() -> u32 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Unified capture variable.  Binds to a single matched node — every
/// successful match records both the node's `NodeId` and (when the
/// pattern is value-producing) the value `NodeOutputId`.
///
/// Each `Capture::new()` call produces a globally unique id via a
/// process-wide atomic counter; uniqueness lets the matcher's
/// [`Bindings`](crate::Bindings) storage (an append-only `Vec`)
/// identify entries unambiguously without per-pattern bookkeeping.
///
/// The same `Capture` can appear in multiple positions of a pattern;
/// the matcher requires all occurrences to bind to the **same** node
/// (and the same value output, if applicable).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Capture(u32);

impl Capture {
    #[must_use]
    pub fn new() -> Self {
        Self(next_id())
    }
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! decl_var {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        ///
        /// Shares a global id counter with every other capture-variable
        /// type so ids are unique across both kinds and all types.
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
        pub struct $name(u32);

        impl $name {
            pub fn new() -> Self { Self(next_id()) }
        }

        impl Default for $name {
            fn default() -> Self { Self::new() }
        }
    };
}

decl_var!(IntVar,
    "A capture variable that binds the **integer constant value** (`u128`) carried by an `IntConst` node.");
decl_var!(BoolVar,
    "A capture variable that binds the **boolean constant value** (`bool`) carried by a `BoolConst` node.");
decl_var!(FloatVar,
    "A capture variable that binds the **IEEE 754 bit pattern** (`u64`) carried by a `FloatConst` node.");
decl_var!(IntBinaryOpVar,
    "A capture variable that binds the **operator variant** of an `IntBinaryOp` node.\n\nUse in [`crate::pat::int_binary_any`] to match any integer binary operator and recover the concrete variant after matching.");
decl_var!(IntUnaryOpVar,
    "A capture variable that binds the **operator variant** of an `IntUnaryOp` node.\n\nUse in [`crate::pat::int_unary_any`] to match any integer unary operator and recover the concrete variant after matching.");
decl_var!(IntCmpOpVar,
    "A capture variable that binds the **operator variant** of an `IntCmpOp` node.\n\nUse in [`crate::pat::int_cmp_any`] to match any integer comparison operator and recover the concrete variant after matching.");
decl_var!(BoolBinaryOpVar,
    "A capture variable that binds the **operator variant** of a `BoolBinaryOp` node.\n\nUse in [`crate::pat::bool_binary_any`] to match any boolean binary operator and recover the concrete variant after matching.");
decl_var!(BoolUnaryOpVar,
    "A capture variable that binds the **operator variant** of a `BoolUnaryOp` node.\n\nUse in [`crate::pat::bool_unary_any`] to match any boolean unary operator and recover the concrete variant after matching.");
decl_var!(FloatBinaryOpVar,
    "A capture variable that binds the **operator variant** of a `FloatBinaryOp` node.\n\nUse in [`crate::pat::float_binary_any`] to match any float binary operator and recover the concrete variant after matching.");
decl_var!(FloatUnaryOpVar,
    "A capture variable that binds the **operator variant** of a `FloatUnaryOp` node.\n\nUse in [`crate::pat::float_unary_any`] to match any float unary operator and recover the concrete variant after matching.");
decl_var!(FloatCmpOpVar,
    "A capture variable that binds the **operator variant** of a `FloatCmpOp` node.\n\nUse in [`crate::pat::float_cmp_any`] to match any float comparison operator and recover the concrete variant after matching.");
