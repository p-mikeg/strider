//! A generic, payload-agnostic bipartite sea-of-nodes graph.
//!
//! [`Graph<N, V, C>`] is the structural graph LAYOUT — nodes carry the payload
//! `N`, their outputs (values) carry the payload `V`, and the dedup-or-create
//! policy is supplied by `C: NodeCacheable<N, V>`. The struct imposes NO
//! `Hash`/`Eq` bound on `N`/`V`; deduplication, if any, lives entirely in the
//! cacher. [`NeverCacheable`] is the always-allocate policy that adds no bound
//! at all.
//!
//! The graph is bipartite: nodes connect to values (their outputs) and values
//! connect to nodes (their consumers), tracked by an intrusive doubly-linked
//! use-list. The [`Vertex`] enum unifies both for the petgraph view, so
//! `petgraph::algo::toposort` and `petgraph::visit::DfsPostOrder` run directly
//! on a `&Graph`.

/// Convenience alias for the workspace-universal [`anyhow::Result`].
///
/// The generic graph's partial structural accessors (`node_inputs_exact`,
/// `node_outputs_exact`, `node_input_id_at`) return this so a downstream
/// crate aliasing its own `Result` to `anyhow::Result` unifies with it across
/// the crate boundary.
pub type Result<T> = anyhow::Result<T>;

mod cache;
mod graph;
mod ids;
mod iter;
mod petgraph_view;
mod storage;
mod walk;

pub use cache::{NeverCacheable, NodeCacheable};
pub use graph::{Graph, NodeIdRemap};
pub use ids::{NodeId, UseId, ValueId, Vertex};
pub use iter::{InputCursor, InputIter, Inputs};
pub use storage::RawStore;
