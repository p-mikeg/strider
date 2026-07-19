use strider_ir::IRViewer;
use strider_ir::node::{NodeId, ValueType};

use crate::capture::Capture;
use crate::matcher::match_pat::{CaptureExt, MatchPat};
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef};

/// Match-only (no template counterpart).
pub struct Any;

impl MatchPat for Any {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = b.leaf(KindSpec::Any);
        // Relax the default value-output constraint so value-less kinds
        // (`Region`, `MemPhi`, ...) match too.
        b.set_output_any(o);
        o
    }
}

/// Wildcard; not usable as a rewrite RHS.
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
        // Like `any()`: matches regardless of what the node produces.
        b.set_output_any(o);
        b.capture_output(o, self.cap);
        o
    }
}

impl crate::template::template_pat::TemplatePat for Var {
    fn compile(self, b: &mut crate::template::TemplateBuilder) -> crate::template::TmplValueRef {
        // Fresh leaf resolving to its LHS binding at instantiation.
        b.capture(self.cap)
    }
}

/// Match any node and bind its output to `c`.
pub fn var(c: Capture) -> Var {
    Var { cap: c }
}

/// Match any node for which `f` returns `true`.
pub fn predicate<F>(f: F) -> impl MatchPat
where
    F: Fn(&crate::Matcher, ValueType) -> bool + 'static,
{
    any().when_match(move |m, ty, _b| f(m, ty))
}

/// Match any value output exactly `n` bits wide. The width is checked both
/// at the root and when nested inside an op.
pub fn value_of_width(n: u32) -> crate::matcher::match_pat::OfWidth<Any> {
    any().of_width(n)
}

/// Match any 1-bit (`I1`) value output.
pub fn bool_value() -> crate::matcher::match_pat::OfWidth<Any> {
    any().bool_valued()
}

/// Match `inner` with every value input `n` bits wide. Also requires at least
/// one value input and at least one value output.
pub struct InputsOfWidth<I> {
    bits: u32,
    inner: I,
}

impl<I: MatchPat> MatchPat for InputsOfWidth<I> {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let o = self.inner.compile(b);
        let want = self.bits;
        b.constrain_input_widths(o, want);
        b.set_node_predicate(
            o,
            Box::new(move |matcher, node| inputs_of_width_check(matcher, node, want as usize)),
        );
        o
    }
}

fn inputs_of_width_check(matcher: &crate::Matcher, node: NodeId, want: usize) -> bool {
    let f = matcher.function();
    // Reject zero-value-output kinds (Return, IndirectBranch, ...).
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

pub fn inputs_of_width<I: MatchPat>(n: u32, inner: I) -> InputsOfWidth<I> {
    InputsOfWidth { bits: n, inner }
}

/// Match `inner` whose value inputs are all 1-bit (`I1`).
pub fn bool_inputs<I: MatchPat>(inner: I) -> InputsOfWidth<I> {
    inputs_of_width(1, inner)
}

/// The register-arg carrier kind.
pub struct InitialVar;

impl MatchPat for InitialVar {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let exemplar =
            strider_ir::node::NodeKind::InitialVar(strider_ir::node::InitialVnId::from_index(0));
        b.leaf(KindSpec::variant_of(&exemplar))
    }
}

pub fn initial_var() -> InitialVar {
    InitialVar
}

pub struct InitialVarFor {
    vn: rsleigh::Vn,
}

impl MatchPat for InitialVarFor {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        // `InitialVar` carries a per-function index, not the varnode, so a
        // function-independent pattern can't encode an exact `NodeKind`. Match
        // the discriminant, then resolve the index at match time.
        let want = self.vn;
        let exemplar =
            strider_ir::node::NodeKind::InitialVar(strider_ir::node::InitialVnId::from_index(0));
        let out = b.leaf(KindSpec::variant_of(&exemplar));
        b.set_node_predicate(
            out,
            Box::new(move |m, node| {
                matches!(
                    *m.function().node_kind(node),
                    strider_ir::node::NodeKind::InitialVar(id)
                        // The IR stores only the largest container, so a pinned
                        // sub-register (`eax`) matches its container (`rax`).
                        if vn_container::vn_contains(&m.function().initial_vn(id), &want)
                )
            }),
        );
        out
    }
}

/// Match `InitialVar(vn)` for the exact varnode `vn`.
pub fn initial_var_for(vn: rsleigh::Vn) -> InitialVarFor {
    InitialVarFor { vn }
}
