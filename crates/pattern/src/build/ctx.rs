//! [`BuildCtx`], [`BuildValue`], and the [`FromCtx`] trait definition.
//!
//! Extracted from `build.rs` for readability.  The `FromCtx` trait impls for
//! each capture-variable type live in [`super::from_ctx_impls`].

use std::sync::Arc;

use ir::BuiltFunctionGraph;
use ir::node::{NodeId, NodeOutputType};

use crate::error::Result;
use crate::matcher::Bindings;

/// Context passed to closure-valued pieces of a [`super::Build`] tree.
///
/// Exposes the captured bindings from the LHS match and the root's output type
/// so user closures can compute fresh constant values based on matched
/// operands.
pub struct BuildCtx<'a> {
    /// The function graph being rewritten (read-only view — closures compute
    /// values, they don't mutate the graph).
    pub graph: &'a BuiltFunctionGraph,
    /// The bindings accumulated during the LHS match.
    pub bindings: &'a Bindings,
    /// The root [`NodeId`] where the LHS matched.
    pub root: NodeId,
    /// The root's declared output type.  All fresh `Build` nodes (except the
    /// bool-producing kinds) are constructed with this type.
    pub root_ty: NodeOutputType,
}

/// Closure type stored inside [`BuildValue::Computed`].  Extracted to a
/// standalone alias so the surrounding type signatures stay legible (and keep
/// clippy's `type_complexity` lint quiet).
pub type BuildValueFn<T> = Arc<dyn Fn(&BuildCtx<'_>) -> Result<T> + Send + Sync>;

/// A value inside a [`super::Build`] node — either a literal or a closure
/// evaluated against a [`BuildCtx`] at rewrite-firing time.
pub enum BuildValue<T> {
    /// A compile-time literal.
    Lit(T),
    /// A closure that computes the value from the match context.
    Computed(BuildValueFn<T>),
}

impl<T: Clone> Clone for BuildValue<T> {
    fn clone(&self) -> Self {
        match self {
            BuildValue::Lit(v) => BuildValue::Lit(v.clone()),
            BuildValue::Computed(f) => BuildValue::Computed(Arc::clone(f)),
        }
    }
}

impl<T> BuildValue<T> {
    pub(super) fn resolve(&self, ctx: &BuildCtx<'_>) -> Result<T>
    where
        T: Clone,
    {
        match self {
            BuildValue::Lit(v) => Ok(v.clone()),
            BuildValue::Computed(f) => f(ctx),
        }
    }
}

/// Extracts a typed value from a [`BuildCtx`] given a capture variable.
///
/// Used by the [`crate::int_const_with!`], [`crate::bool_const_with!`], and
/// [`crate::float_const_with!`] macros to turn a capture identifier into its
/// concrete value without per-closure boilerplate.
///
/// Every capture type added in Phases A1/A2 has an impl: [`crate::var::Var`],
/// [`crate::var::NodeVar`], [`crate::var::IntVar`], [`crate::var::BoolVar`],
/// [`crate::var::FloatVar`], and the eight `*OpVar` types.
///
/// # Errors
///
/// Returns [`crate::error::ErrorKind::MissingBinding`] if the capture was not
/// bound during the LHS match — this indicates a pattern-authoring bug (the
/// capture appears in the RHS but not in the LHS, or the LHS matches a node
/// that doesn't populate that binding).
pub trait FromCtx {
    /// The Rust-native type extracted from the context.
    type Output;
    /// Retrieve the value bound to `self` inside `ctx`.
    ///
    /// Despite the `from_` prefix, this takes `&self` — the capture
    /// variable *is* the key used to look up the binding.  The trait is
    /// named from the *caller's* perspective: "derive a value from the
    /// [`BuildCtx`]".
    #[allow(clippy::wrong_self_convention)]
    fn from_ctx(&self, ctx: &BuildCtx<'_>) -> Result<Self::Output>;
}
