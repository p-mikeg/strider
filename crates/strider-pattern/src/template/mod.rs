//! Template trait + `PatGraph<Concrete>` impl.  A Template is a graph
//! that can be *instantiated* into the IR as a fresh sub-graph,
//! resolving captures through a `Bindings` overlay.
//!
//! `Template` is implemented only for `PatGraph<Concrete>` (and
//! `Pat<Concrete>` by delegation) — by construction, a Concrete graph
//! has either a `BuildSpec` or a `Capture` on every node, so every
//! node has a path to materialisation.  `PatGraph<Wildcard>` carries
//! at least one Any-kind node without a capture, which has no
//! materialisation rule, so `Template` is not implemented for it —
//! the type system rejects `Rewrite::new(_, wildcard_pat)` at compile
//! time.

use std::collections::{BTreeMap, HashMap};

use anyhow::anyhow;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::Function;
use strider_ir::node::{NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::capture::Bindings;
use crate::pat_graph::{BuildKind, BuildTy, Concrete, PatGraph};

/// A graph shape that can be materialised as fresh IR.
///
/// Implemented only for `PatGraph<Concrete>` and `Pat<Concrete>`; the
/// `Wildcard` role has no impl, so a wildcard pattern cannot be used
/// as a rewrite RHS — the type system rejects it at compile time.
pub trait Template {
    /// Materialise `self` as an IR sub-graph rooted at the returned
    /// output.  Captures are resolved from `bindings`; `root_ty` is the
    /// output type to use for any node whose `BuildTy` is
    /// `InheritRoot`.
    ///
    /// # Errors
    ///
    /// Returns an error if the template is rootless, references an
    /// unbound capture, has a `BuildKind::Fn` variant (not yet wired),
    /// or if the underlying IR `create_node` call fails to produce
    /// exactly one value output.
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId>;
}

impl Template for PatGraph<Concrete> {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId> {
        let Some(root) = self.root else {
            return Err(anyhow!("rootless PatGraph"));
        };
        let order = crate::pat_graph::topo_order_from_root(&self.inner, root)?;
        // Map from pat NodeIndex → materialised IR NodeOutputId.
        let mut materialised: HashMap<NodeIndex, NodeOutputId> = HashMap::new();

        for pn in order {
            let nd = self
                .inner
                .node_weight(pn)
                .ok_or_else(|| anyhow!("topo returned dangling NodeIndex"))?;

            // 1. Node with a Capture: resolve through Bindings.  On a
            //    Concrete graph a capture-only node has no BuildSpec
            //    (the var(c) builder takes this path); a node with
            //    both a capture and a BuildSpec is unusual but the
            //    capture takes precedence — the binding *is* the
            //    materialisation.
            if let Some(cap_ref) = &nd.capture {
                let cap = cap_ref.capture();
                let bound_out = bindings.get_output(cap).ok_or_else(|| {
                    anyhow!("capture {cap:?} referenced in template but unbound by LHS")
                })?;
                materialised.insert(pn, bound_out);
                continue;
            }

            // 2. Node with a BuildSpec: synthesise the IR node.
            let bs = nd.build_spec.as_ref().ok_or_else(|| {
                anyhow!(
                    "Template node has no BuildSpec and no Capture — \
                     should be impossible on PatGraph<Concrete>"
                )
            })?;

            let kind = match &bs.kind {
                BuildKind::Exact(k) => *k,
                BuildKind::Fn(_) => {
                    // The Fn(BuildCtx -> NodeKind) variant requires
                    // wiring BuildCtx — the rewriter (next commit)
                    // will thread that through.  For pure-Exact
                    // templates this branch is unreachable.
                    return Err(anyhow!(
                        "BuildKind::Fn not yet wired in PatGraph::instantiate; \
                         expected only Exact in this commit"
                    ));
                }
            };
            let ty = match bs.ty {
                BuildTy::InheritRoot => root_ty,
                BuildTy::Fixed(t) => t,
            };

            // Collect input outputs in slot order (BTreeMap → sorted
            // keys → values).
            let mut inputs_by_slot: BTreeMap<usize, NodeOutputId> = BTreeMap::new();
            for edge in self.inner.edges_directed(pn, petgraph::Incoming) {
                let producer_pat = edge.source();
                let producer_out = *materialised.get(&producer_pat).ok_or_else(|| {
                    anyhow!("producer pat node not materialised — topo order bug")
                })?;
                inputs_by_slot.insert(edge.weight().consumer_slot, producer_out);
            }
            let inputs: Vec<NodeOutputId> = inputs_by_slot.into_values().collect();

            let node = function.create_node(kind, inputs, [NodeOutputKind::OutputType(ty)]);
            let [out_id] = function.node_outputs_exact::<1>(node)?;
            materialised.insert(pn, out_id);
        }

        materialised
            .remove(&root)
            .ok_or_else(|| anyhow!("root pat node never materialised"))
    }
}

// Delegate Template through Pat<Concrete>.
impl Template for crate::builders::Pat<Concrete> {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId> {
        self.0.instantiate(function, bindings, root_ty)
    }
}
