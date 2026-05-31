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

/// Matches any value output whose type is exactly `width` bits wide.
///
/// The single mechanism for querying by output width — e.g. `width == 1`
/// selects booleans (the 1-bit integer `I1`), since booleans are no longer
/// a distinct type.  Matches integer *and* float types of the width (e.g.
/// width 32 matches both `I32` and `F32`), mirroring the `bit_width` filter
/// on `Load` / `Store` patterns.
pub struct ValueWidthPat {
    pub(crate) width: u32,
}

impl Pattern for ValueWidthPat {
    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, _: &mut Bindings) -> bool {
        ctx.require_value_output(target)
            .is_some_and(|ty| ty.bit_width() == self.width as usize)
    }
}

/// Matches `inner`, then additionally requires every **value input** of the
/// matched node to be exactly `width` bits wide.
///
/// This is the input-side companion to [`ValueWidthPat`]: where output-width
/// asks "produces an N-bit value" (a comparison and a boolean-AND both
/// produce `I1`), input-width asks "operates on N-bit values".  `width == 1`
/// therefore isolates boolean-logic operations (`And`/`Or`/`Xor`/`BitNot` on
/// booleans, whose operands are `I1`) and **excludes** comparisons (whose
/// operands are wider).  Non-value inputs (control / memory tokens) are
/// ignored.
pub struct InputWidthPat {
    pub(crate) width: u32,
    pub(crate) inner: crate::pattern::pat::Pat,
}

impl Pattern for InputWidthPat {
    fn kind_spec(&self) -> crate::pattern::pat::node_pat::KindSpec {
        // Inherit the inner pattern's prefilter so `find_all` stays fast.
        self.inner.as_dyn().kind_spec()
    }

    fn try_match(&self, ctx: &MatchCtx, target: NodeOutputId, b: &mut Bindings) -> bool {
        let mark = b.mark();
        if !ctx.matcher.match_output(target, &self.inner, b) {
            b.restore(mark);
            return false;
        }
        let node = ctx.function.node_for_output(target);
        let want = self.width as usize;
        // "Operates on N-bit values": there must be at least one value input,
        // and every value-typed input must be `want` bits.  Non-value inputs
        // (control / memory / phi-token) are ignored; a leaf with no value
        // inputs (e.g. a constant) does not match.
        let mut value_inputs = 0usize;
        let ok = ctx.function.node_inputs(node).into_iter().all(|inp| {
            match ctx.function.output_kind(inp).as_value() {
                Some(ty) => {
                    value_inputs += 1;
                    ty.bit_width() == want
                }
                None => true,
            }
        }) && value_inputs > 0;
        if ok {
            true
        } else {
            b.restore(mark);
            false
        }
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
        b.bind_capture(self.capture, Binding::Output(target))
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
        // A value-producing match binds the output directly; a non-value
        // (control / memory) target falls back to the owning node id.
        let binding = if ctx.require_value_output(target).is_some() {
            Binding::Output(target)
        } else {
            Binding::Node(ctx.function.node_for_output(target))
        };
        if b.bind_capture(self.capture, binding) {
            true
        } else {
            b.restore(mark);
            false
        }
    }

    fn try_match_node(&self, ctx: &MatchCtx, node: NodeId, b: &mut Bindings) -> bool {
        let outputs = ctx.function.node_outputs(node);
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
        if b.bind_capture(self.capture, Binding::Node(node)) {
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
