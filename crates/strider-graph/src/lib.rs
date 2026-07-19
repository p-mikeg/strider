//! A generic, payload-agnostic bipartite sea-of-nodes graph.
//!
//! [`Graph<N, V, C>`] is the structural layout only: nodes carry payload `N`,
//! their outputs (values) carry payload `V`, and the dedup-or-create policy is
//! `C: NodeCacheable<N, V>`. The struct imposes NO `Hash`/`Eq` bound on `N`/`V`;
//! deduplication, if any, lives entirely in the policy. [`NeverCacheable`]
//! always allocates and adds no bound at all.
//!
//! Bipartite: nodes connect to values (their outputs) and values connect to
//! nodes (their consumers), tracked by an intrusive doubly-linked use-list. The
//! [`Vertex`] enum unifies both for the petgraph view, so
//! `petgraph::algo::toposort` and `petgraph::visit::DfsPostOrder` run directly
//! on a `&Graph`.

/// Aliased to `anyhow::Result` so a downstream crate with its own such alias
/// unifies with it across the crate boundary.
pub type Result<T> = anyhow::Result<T>;

mod cache;
mod graph;
mod ids;
mod iter;
mod petgraph_view;
mod storage;

pub use cache::{NeverCacheable, NodeCacheable};
pub use graph::{Graph, NodeIdRemap};
pub use ids::{NodeId, UseId, ValueId, Vertex};
pub use iter::{InputCursor, InputIter, Inputs};
pub use storage::RawStore;
