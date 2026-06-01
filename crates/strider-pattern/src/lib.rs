#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Sea-of-nodes pattern + template crate. Replaces `strider-analyze::pattern`.
//!
//! Internal representation: `PatGraph` backed by `petgraph::StableDiGraph`.
//! Public surface: chained builder free-functions (`add`, `int_const`, `var`,
//! …) plus the `Pattern` and `Template` traits implemented by `PatGraph`.

pub mod builders;
pub mod capture;
pub mod matcher;
pub mod pat_graph;
pub mod template;

pub use builders::*;
pub use capture::{Bindings, BindingsMark, Capture, CaptureRef, Match};
pub use matcher::{BuildCtx, CastMask, MatchCtx, Matcher, MatcherOptions, Pattern, PatternExt};
pub use pat_graph::{Combine, Concrete, EdgeData, KindSpec, NodeData, PatGraph, Role, Wildcard};
pub use template::Template;
