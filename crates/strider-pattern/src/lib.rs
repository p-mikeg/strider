//! Sea-of-nodes pattern + template crate. Replaces `strider-analyze::pattern`.
//!
//! Internal representation: `PatGraph` backed by `petgraph::StableDiGraph`.
//! Public surface: chained builder free-functions (`add`, `int_const`, `var`,
//! …) plus the `Pattern` and `Template` traits implemented by `PatGraph`.

// Modules added in subsequent tasks.
