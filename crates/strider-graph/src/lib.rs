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
