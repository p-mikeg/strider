//! `impl<R> Pattern for PatGraph<R>` — single-node case.
//!
//! Recursive input-walking + commutative retry land in a subsequent
//! commit; this commit covers leaf patterns only (zero incoming
//! edges on the root pat node).

use std::mem::Discriminant;
use strider_ir::node::{NodeKind, NodeOutputId};

use crate::capture::{Binding, Bindings};
use crate::matcher::{MatchCtx, Pattern};
use crate::pat_graph::PatGraph;

impl<R> Pattern for PatGraph<R> {
    fn root_kind_discriminant(&self) -> Option<Discriminant<NodeKind>> {
        let root = self.root?;
        self.inner.node_weight(root)?.kind.discriminant()
    }

    fn try_match(
        &self,
        ctx: &MatchCtx,
        root_out: NodeOutputId,
        bindings: &mut Bindings,
    ) -> bool {
        let Some(root) = self.root else {
            return false;
        };
        // `root` is recorded by `set_root` only after `add_node` returns its
        // index; a missing weight would mean the index was invalidated
        // (e.g. by `remove_node` — which `PatGraph` never calls).  Treat
        // an absent weight as a non-match rather than panicking.
        let Some(nd) = self.inner.node_weight(root) else {
            return false;
        };
        let root_node = ctx.function.node_for_output(root_out);
        if !nd.kind.matches(ctx.function.node_kind(root_node)) {
            return false;
        }
        // Capture binding: when the pat node has a `Capture`, record
        // the IR-side `NodeOutputId`.  `bind_capture` returns `true`
        // on new or idempotent bind, `false` on conflict — the
        // caller is responsible for restoring bindings to a
        // pre-attempt mark on a `false` return.
        if let Some(cap_ref) = nd.capture
            && !bindings.bind_capture(cap_ref.capture(), Binding::Output(root_out))
        {
            return false;
        }
        if let Some(pm) = &nd.post_match {
            // post_match closure currently has the placeholder shape
            // `Box<dyn Fn() -> bool>` (a stub).  A subsequent task
            // widens its signature to
            // `Fn(&MatchCtx, NodeOutputType, &Bindings) -> bool`.
            // For now, call it with no arguments and trust the stub.
            if !pm() {
                return false;
            }
        }
        true
    }
}
