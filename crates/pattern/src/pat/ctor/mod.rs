//! Free-function constructors for [`crate::pat::Pat`].
//!
//! Grouped by family; everything is re-exported from the parent [`crate::pat`]
//! module.

// `bool_` (trailing underscore) because `bool` is a Rust primitive type
// and reusing it as a module name requires `mod r#bool;` / `r#bool::…` at
// every call site, which is uglier than the suffix.
mod bool_;
mod casts;
pub(crate) mod consts;
mod control;
mod float;
mod int;
mod variant_agnostic;
mod wildcards;

pub use bool_::*;
pub use casts::*;
pub use control::*;
pub use float::*;
pub use int::*;
pub use variant_agnostic::*;
pub use wildcards::*;
