#![cfg_attr(
    test,
    allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::unreachable
    )
)]

//! Sea-of-nodes pattern + template crate.
//!
//! Internal representation: a bipartite [`pattern::Pattern`] backed by
//! `petgraph::StableDiGraph`, mirroring the IR's `Node → NodeOutput →
//! Node` structure with real `PatNode` and `PatOutput` vertices.

pub mod bindings;
pub mod builder;
pub mod capture;
pub mod error;
pub mod matcher;
pub mod pattern;
pub mod template;

pub use bindings::Bindings;
pub use capture::Capture;
pub use error::{MissingBinding, Result, RewriteSkip, is_skip, missing_binding, skip};
