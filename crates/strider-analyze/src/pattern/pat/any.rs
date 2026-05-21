//! Small wildcard / capture combinators.

use strider_ir::node::{NodeId, NodeOutputId};

use crate::pattern::error::Result;
use crate::pattern::matcher::Bindings;
use crate::pattern::matcher::bindings::Binding;
use crate::pattern::pat::traits::{BuildCtx, BuildOutcome, MatchCtx, Pattern};
use crate::pattern::var::Capture;

/// Matches any output unconditionally.
pub struct AnyPat;

impl Pattern for AnyPat {
    fn try_match(&self, _: &MatchCtx, _: NodeOutputId, _: &mut Bindings) -> bool {
        true
    }
}

/// Matches any output and binds it to `capture`.  Dedicated type so the very
/// common `var(c)` path avoids the double dispatch + snapshot of the
/// general-purpose [`CapturePat`] wrapping [`AnyPat`].  Produced only by
/// [`crate::pattern::pat::var`]; `any().capture(c)` still yields a `CapturePat`.
pub struct VarPat {
    pub(crate) capture: Capture,
}

impl Pattern for VarPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        if ctx.require_value_output(target).is_none() {
            return false;
        }
        let node = ctx.graph.get_node_from_output(target);
        b.bind_capture(
            self.capture,
            Binding {
                node,
                output: Some(target),
            },
        )
    }

    fn try_build(&self, ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        let out = ctx
            .bindings
            .get_output(self.capture)
            .ok_or_else(|| crate::pattern::error::missing_binding("Capture"))?;
        Ok(BuildOutcome::Out(out))
    }
}

/// Matches `inner`, then additionally binds the matched node (and its
/// value output, if value-producing) to `capture`.  Produced by
/// [`crate::pattern::pat::IntoPat::capture`].
///
/// The wrapper handles both dispatch directions: when matched at a
/// value output (`try_match`) the binding records both node and
/// output; when matched at a node directly (`try_match_node`, used for
/// zero-output control patterns like `Return`) the binding records
/// only the node id.
pub struct CapturePat {
    pub(crate) inner: crate::pattern::pat::Pat,
    pub(crate) capture: Capture,
}

impl Pattern for CapturePat {
    fn kind_spec(&self) -> crate::pattern::pat::node_pat::KindSpec {
        // A capture inherits the inner pattern's kind spec — wrapping
        // `add(...)` in `.capture(c)` must not forfeit the find_all
        // prefilter speedup.
        self.inner.as_dyn().kind_spec()
    }

    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let mark = b.mark();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            b.restore(mark);
            return false;
        }
        let node = ctx.graph.get_node_from_output(target);
        let output = ctx.require_value_output(target).map(|_| target);
        if b.bind_capture(self.capture, Binding { node, output }) {
            true
        } else {
            b.restore(mark);
            false
        }
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let outputs = ctx.graph.node_outputs(node);
        if !outputs.is_empty() {
            // Default behavior: iterate value outputs; bind both node + output.
            for &out in outputs {
                let mark = b.mark();
                if self.try_match(ctx, out, b) {
                    return true;
                }
                b.restore(mark);
            }
            return false;
        }
        // Zero-output node (e.g. `Return`): match the inner at the
        // node, then bind only the node id.
        let mark = b.mark();
        if !ctx.matcher.match_node_id(node, &self.inner, b) {
            b.restore(mark);
            return false;
        }
        if b.bind_capture(self.capture, Binding { node, output: None }) {
            true
        } else {
            b.restore(mark);
            false
        }
    }

    fn try_build(&self, ctx: &mut BuildCtx<'_>) -> Result<BuildOutcome> {
        // In build position, materialize by looking up the bound
        // value output.  Control-flow bindings (output: None) cannot
        // be used as build operands.
        let out = ctx
            .bindings
            .get_output(self.capture)
            .ok_or_else(|| crate::pattern::error::missing_binding("Capture"))?;
        Ok(BuildOutcome::Out(out))
    }
}
