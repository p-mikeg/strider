//! Template trait + `PatGraph<R>` impls.  A Template is a graph that
//! can be *instantiated* into the IR as a fresh sub-graph, resolving
//! captures through a `Bindings` overlay.
//!
//! `Template` is implemented for both `PatGraph<Concrete>` and
//! `PatGraph<Wildcard>` (plus `Pat<*>` by delegation).  The `Concrete`
//! impl is a compile-time guarantee that every node has either a
//! `TemplateSpec` or a `Capture`; the `Wildcard` impl performs the same
//! check at runtime so the strider-py wrapper — which only ever
//! produces `Pat<Wildcard>` — can drive a rewrite without a separate
//! Concrete-typed Python builder surface.
//!
//! Rust callers that want compile-time enforcement keep using
//! [`rewrite_rule`](crate::rewrite::rewrite_rule) with a `Pat<Concrete>`
//! RHS.  Python (and other dynamic) callers reach
//! [`rewrite_rule_dynamic`](crate::rewrite::rewrite_rule_dynamic)
//! which accepts a `Pat<Wildcard>` RHS and validates buildability up
//! front via [`PatGraph::assert_concrete_at_runtime`].

use std::collections::{BTreeMap, HashMap};

use anyhow::anyhow;
use petgraph::stable_graph::NodeIndex;
use petgraph::visit::EdgeRef;
use strider_ir::Function;
use strider_ir::node::{NodeId, NodeOutputId, NodeOutputKind, NodeOutputType};

use crate::bindings::Bindings;
use crate::matcher::TemplateCtx;
use crate::pat_graph::{TemplateKind, TemplateTy, Concrete, PatGraph, Role, Wildcard};

/// A graph shape that can be materialised as fresh IR.
///
/// Implemented for `PatGraph<Concrete>`, `Pat<Concrete>`,
/// `PatGraph<Wildcard>`, and `Pat<Wildcard>`.  The `Concrete` impls
/// statically guarantee every node has a build path; the `Wildcard`
/// impls perform a runtime check via
/// [`PatGraph::assert_concrete_at_runtime`] and fail with an error if
/// any node lacks both a `TemplateSpec` and a `Capture`.
pub trait Template {
    /// Materialise `self` as an IR sub-graph rooted at the returned
    /// output.  Captures are resolved from `bindings`; `root_ty` is the
    /// output type to use for any node whose `TemplateTy` is
    /// `InheritRoot`.  `lhs_root` is the matched LHS root `NodeId` that
    /// gets exposed to [`TemplateKind::Fn`] closures via
    /// [`TemplateCtx::root`] — pure-`Exact` templates ignore it, so
    /// standalone callers may pass any valid `NodeId` from the
    /// `function`.
    ///
    /// # Errors
    ///
    /// Returns an error if the template is rootless, references an
    /// unbound capture, has a [`TemplateKind::Fn`] closure that itself
    /// errors, has a `Wildcard` node without a build path (only
    /// possible for `Wildcard`-roled templates; the `Concrete` impls
    /// rule this out at compile time), or if the underlying IR
    /// `create_node` call fails to produce exactly one value output.
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        lhs_root: NodeId,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId>;
}

/// Shared instantiation body for `PatGraph<R>`.  Role-generic so the
/// `Concrete` (compile-time-checked) and `Wildcard` (runtime-checked)
/// impls share one code path.
///
/// `precondition_checked = true` skips the per-node "has either
/// TemplateSpec or Capture" guard — the `Concrete` role already enforces
/// this at construction time.  `false` performs the check during the
/// walk and surfaces a clear error if a node would be unbuildable.
fn instantiate_pat_graph<R: Role>(
    pg: &PatGraph<R>,
    function: &mut Function,
    bindings: &Bindings,
    lhs_root: NodeId,
    root_ty: NodeOutputType,
    precondition_checked: bool,
) -> anyhow::Result<NodeOutputId> {
    let Some(root) = pg.root else {
        return Err(anyhow!("rootless PatGraph"));
    };
    let order = crate::pat_graph::topo_order_from_root(&pg.inner, root)?;
    // Map from pat NodeIndex → materialised IR NodeOutputId.
    let mut materialised: HashMap<NodeIndex, NodeOutputId> = HashMap::new();

    for pn in order {
        let nd = pg
            .inner
            .node_weight(pn)
            .ok_or_else(|| anyhow!("topo returned dangling NodeIndex"))?;

        // 1. Node with a Capture: resolve through Bindings.  A
        //    capture-only node has no TemplateSpec (the var(c) builder
        //    takes this path); a node with both a capture and a
        //    TemplateSpec is unusual but the capture takes precedence —
        //    the binding *is* the materialisation.
        if let Some(cap) = nd.capture {
            let bound_out = bindings.get_output(cap).ok_or_else(|| {
                anyhow!("capture {cap:?} referenced in template but unbound by LHS")
            })?;
            materialised.insert(pn, bound_out);
            continue;
        }

        // 2. Node with a TemplateSpec: synthesise the IR node.
        let bs = nd.template_spec.as_ref().ok_or_else(|| {
            if precondition_checked {
                anyhow!(
                    "Template node has no TemplateSpec and no Capture — \
                     should be impossible on PatGraph<Concrete>"
                )
            } else {
                anyhow!(
                    "Template node has no TemplateSpec and no Capture — \
                     cannot instantiate a Wildcard-roled RHS that contains an \
                     un-buildable node (kind-`Any`, post-match predicate, or \
                     other match-only shape).  Rewrite RHS must consist of \
                     concrete builders (e.g. `int_const(0)`, `add(...)`) and \
                     captures bound by the LHS."
                )
            }
        })?;

        let ty = match bs.ty {
            TemplateTy::InheritRoot => root_ty,
            TemplateTy::Fixed(t) => t,
        };
        let kind = match &bs.kind {
            TemplateKind::Exact(k) => *k,
            TemplateKind::Fn(f) => {
                // Construct a per-call `TemplateCtx` exposing the
                // function, captured bindings, the matched LHS
                // root, and the resolved output type.  The
                // closure decides the `NodeKind` to materialise
                // (e.g. an `IntConst(value)` whose `value` is
                // computed from one or more captured operands).
                let ctx = TemplateCtx {
                    function,
                    bindings,
                    root: lhs_root,
                    root_ty: ty,
                };
                f(&ctx)?
            }
        };

        // Collect input outputs in slot order (BTreeMap → sorted
        // keys → values).
        let mut inputs_by_slot: BTreeMap<usize, NodeOutputId> = BTreeMap::new();
        for edge in pg.inner.edges_directed(pn, petgraph::Incoming) {
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

impl Template for PatGraph<Concrete> {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        lhs_root: NodeId,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId> {
        instantiate_pat_graph(self, function, bindings, lhs_root, root_ty, true)
    }
}

impl Template for PatGraph<Wildcard> {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        lhs_root: NodeId,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId> {
        // Defensive: a Wildcard-typed RHS may carry kind-`Any` nodes
        // without build paths; surface a clear error rather than
        // letting the inner topology walk hit them mid-build.
        // `rewrite_rule_dynamic` runs the same check up front at rule
        // construction time, but the trait impl keeps the safety net
        // for direct `instantiate` callers.
        self.assert_concrete_at_runtime()?;
        instantiate_pat_graph(self, function, bindings, lhs_root, root_ty, false)
    }
}

// Delegate Template through Pat<R> for both roles.
impl Template for crate::builders::Pat<Concrete> {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        lhs_root: NodeId,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId> {
        self.0.instantiate(function, bindings, lhs_root, root_ty)
    }
}

impl Template for crate::builders::Pat<Wildcard> {
    fn instantiate(
        &self,
        function: &mut Function,
        bindings: &Bindings,
        lhs_root: NodeId,
        root_ty: NodeOutputType,
    ) -> anyhow::Result<NodeOutputId> {
        self.0.instantiate(function, bindings, lhs_root, root_ty)
    }
}
