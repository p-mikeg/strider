//! Wildcard / capture / width-filter typed builders.
//!
//! `any` accepts every node kind; `var` additionally binds a capture;
//! `predicate` gates on a closure; `value_of_width` / `inputs_of_width`
//! and their `bool_*` aliases filter by output / input bit-width;
//! `initial_var` / `initial_var_for` match the register-arg carrier
//! kind.

use strider_ir::node::{NodeId, ValueType};

use crate::matcher::{MatcherBuilder, PatValueRef};
use crate::capture::Capture;
use crate::matcher::match_pat::{CaptureExt, MatchPat};
use crate::matcher::KindSpec;

/// Match any node. Match-only (no template counterpart).
pub struct Any;

impl MatchPat for Any {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = b.leaf(KindSpec::Any);
        // A bare wildcard matches any node, including value-less kinds
        // (`Region`, `MemPhi`, …); relax the default value-output
        // constraint to the unconstrained `Any` kind.
        b.set_output_any(o);
        o
    }
}

/// Match any node. Wildcard — not usable as a rewrite RHS.
pub fn any() -> Any {
    Any
}

/// Match any node and bind it to `c`.
pub struct Var {
    cap: Capture,
}

impl MatchPat for Var {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = b.leaf(KindSpec::Any);
        // Like `any()`, a bare capture matches any node regardless of
        // what it produces (value, control, memory, phi-token).
        b.set_output_any(o);
        b.capture_node(o, self.cap);
        o
    }
}

impl crate::template::template_pat::TemplatePat for Var {
    fn compile(self, b: &mut crate::template::TemplateBuilder) -> crate::template::TmplValueRef {
        // A capture is a fresh leaf that resolves to its LHS binding at
        // instantiation (the `add(x, 0) → x` shape).
        b.capture(self.cap)
    }
}

/// Match any node and bind its output to `c`.
pub fn var(c: Capture) -> Var {
    Var { cap: c }
}

/// Match any node for which `f` returns `true`. Equivalent to
/// `any().when_match(move |m, ty, _| f(m, ty))`, spelled as a single
/// free function for the simple type/context-predicate case.
pub fn predicate<F>(f: F) -> impl MatchPat
where
    F: Fn(&crate::Matcher, ValueType) -> bool + 'static,
{
    any().when_match(move |m, ty, _b| f(m, ty))
}

/// Match any value output that is exactly `n` bits wide.
///
/// Thin sugar over the [`of_width`](crate::CaptureExt::of_width)
/// combinator: `any().of_width(n)` pins the declarative output-vertex
/// width, which the matcher checks both at the root and when nested
/// inside an op.
pub fn value_of_width(n: u32) -> crate::matcher::match_pat::OfWidth<Any> {
    any().of_width(n)
}

/// Match any boolean value — any value output 1 bit wide (`I1`).
pub fn bool_value() -> crate::matcher::match_pat::OfWidth<Any> {
    any().bool_valued()
}

/// Match `inner` and require all of the matched node's value inputs to
/// be `n` bits wide. Preserves the old guard: at least one value input,
/// every value input width == `n`, and the matched node has a value
/// output.
pub struct InputsOfWidth<I> {
    bits: u32,
    inner: I,
}

impl<I: MatchPat> MatchPat for InputsOfWidth<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        let want = self.bits;
        // Declarative per-input width on the wired value-input vertices.
        b.constrain_input_widths(o, want);
        // Plus the "≥1 value input & has value output" guard the old
        // builder enforced via its node filter.
        b.set_node_predicate(
            o,
            Box::new(move |matcher, node| inputs_of_width_check(matcher, node, want as usize)),
        );
        o
    }
}

/// Whether every value input of `node` has width `want`, the node has at
/// least one value input, and the node has at least one value output.
fn inputs_of_width_check(matcher: &crate::Matcher, node: NodeId, want: usize) -> bool {
    let f = matcher.function();
    // Reject zero-value-output kinds (Return, IndirectBranch, …): the old
    // input-width pattern was only dispatched against value-producing
    // nodes.
    let has_value_output = f
        .node_outputs(node)
        .iter()
        .any(|&value| f.value_kind(value).as_value().is_some());
    if !has_value_output {
        return false;
    }
    let mut value_inputs = 0usize;
    for inp in f.node_inputs(node) {
        if let Some(ty) = f.value_kind(inp).as_value() {
            value_inputs += 1;
            if ty.bit_width() != want {
                return false;
            }
        }
    }
    value_inputs > 0
}

/// Match `inner` whose value inputs are all `n` bits wide.
pub fn inputs_of_width<I: MatchPat>(n: u32, inner: I) -> InputsOfWidth<I> {
    InputsOfWidth { bits: n, inner }
}

/// Match `inner` whose value inputs are all booleans (1-bit `I1`).
pub fn bool_inputs<I: MatchPat>(inner: I) -> InputsOfWidth<I> {
    inputs_of_width(1, inner)
}

/// Match any `InitialVar(_)` node (the register-arg carrier kind).
pub struct InitialVar;

impl MatchPat for InitialVar {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let sentinel = rsleigh::Vn {
            size: 0,
            addr_off: 0,
            addr_space: rsleigh::VnSpace::REGISTER,
        };
        let exemplar = strider_ir::node::NodeKind::InitialVar(sentinel);
        b.leaf(KindSpec::Variant(std::mem::discriminant(&exemplar)))
    }
}

/// Match any `InitialVar(_)` node (any varnode).
pub fn initial_var() -> InitialVar {
    InitialVar
}

/// Match `InitialVar(vn)` for the exact varnode `vn`.
pub struct InitialVarFor {
    vn: rsleigh::Vn,
}

impl MatchPat for InitialVarFor {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        b.leaf(KindSpec::Exact(strider_ir::node::NodeKind::InitialVar(self.vn)))
    }
}

/// Match `InitialVar(vn)` for the exact varnode `vn`.
pub fn initial_var_for(vn: rsleigh::Vn) -> InitialVarFor {
    InitialVarFor { vn }
}
