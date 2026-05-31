//! Merge a child `PatGraph` into a parent `PatGraph`.  Used by every
//! chained binary / unary / call builder.  Child is consumed by value;
//! `NodeData` is move-only so cloning is never needed.

use std::collections::HashMap;

use petgraph::stable_graph::NodeIndex;

use super::PatGraph;
use super::role::Role;

/// Merges `child` into `parent`.  Returns the parent-side `NodeIndex`
/// corresponding to `child.root()`.  All child nodes / edges are
/// *moved* (not cloned) into the parent with remapped indices.
///
/// # Panics
///
/// Panics if `child` has no root set.  The caller (every builder)
/// always finalises a child before merging, so this is a builder-bug
/// guard rather than a runtime error path.
// `dead_code` allow: wired in upcoming chained-builder tasks.
#[allow(clippy::expect_used, dead_code)]
pub(crate) fn merge_subgraph<RChild, RParent>(
    parent: &mut PatGraph<RParent>,
    child: PatGraph<RChild>,
) -> NodeIndex
where
    RChild: Role,
    RParent: Role,
{
    let PatGraph {
        inner: child_inner,
        root: child_root,
        _role: _,
    } = child;
    let child_root = child_root.expect("merge_subgraph called on rootless child");

    // petgraph 0.8: `into_nodes_edges_iters` on StableDiGraph yields
    // owned `StableGraphNode` / `StableGraphEdge` wrappers carrying
    // their own `index` (we remap to parent-side indices below).
    let (node_iter, edge_iter) = child_inner.into_nodes_edges_iters();
    let mut remap: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    for stable_node in node_iter {
        let new_idx = parent.inner.add_node(stable_node.weight);
        remap.insert(stable_node.index, new_idx);
    }
    for stable_edge in edge_iter {
        let new_src = remap[&stable_edge.source];
        let new_dst = remap[&stable_edge.target];
        parent.inner.add_edge(new_src, new_dst, stable_edge.weight);
    }
    remap[&child_root]
}
