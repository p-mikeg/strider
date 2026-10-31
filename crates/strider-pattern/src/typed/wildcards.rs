use strider_ir::IRViewer;
use strider_ir::node::{InitialVnId, NodeId, NodeKind, ValueType};

use crate::TemplatePat;
use crate::capture::Capture;
use crate::expr::{Deco, Expr, IntoExpr, Leaf};
use crate::matcher::match_pat::{CaptureExt, MatchPat};
use crate::matcher::{KindSpec, NodePredicate};
use crate::typed::value_ops::MatchOnly;

/// Match-only.
pub struct Any;

impl IntoExpr for Any {
    fn into_expr(self) -> Expr {
        // Relax the default value-output constraint so value-less kinds
        // (`Region`, `MemPhi`, ...) match too.
        Leaf::any().into()
    }
}

impl MatchPat for Any {}
impl crate::node_builders::MemPat for Any {}

/// Wildcard; not usable as a rewrite RHS.
pub fn anything() -> Any {
    Any
}

/// Match any node and bind it to `c`; on the build side, the value `c` bound.
pub struct Var {
    cap: Capture,
}

impl IntoExpr for Var {
    fn into_expr(self) -> Expr {
        // Like `anything()`: matches regardless of what the node produces.
        Any.into_expr().capture(self.cap)
    }
}

impl MatchPat for Var {}
impl TemplatePat for Var {}
impl crate::template::template_pat::sealed::Sealed for Var {}
impl crate::node_builders::MemPat for Var {}

/// Match any node and bind its output to `c`.
pub fn var(c: Capture) -> Var {
    Var { cap: c }
}

/// Match any node for which `f` returns `true`. `f` is typed, so nodes and
/// edges carrying no value type never match; see
/// [`crate::CaptureExt::when_match`].
pub fn predicate<F>(f: F) -> impl MatchPat
where
    F: Fn(&crate::Matcher, ValueType) -> bool + 'static + Send,
{
    anything().when_match(move |m, ty, _b| f(m, ty))
}

/// Match any value output exactly `n` bits wide. The width is checked both
/// at the root and when nested inside an op.
pub fn value_of_width(n: u32) -> MatchOnly {
    anything().of_width(n)
}

/// A wildcard leaf whose matched value output's type satisfies `f`.
fn typed_leaf(f: fn(ValueType) -> bool) -> MatchOnly {
    let leaf = Expr::from(Leaf::new(KindSpec::Any));
    MatchOnly(
        leaf.deco(Deco::PostMatch(Box::new(move |_m, _node, ty, _bnd| {
            ty.is_some_and(f)
        }))),
    )
}

/// Any integer-typed node, constant or not. `I1` is an integer type, so
/// booleans match; [`any_bool`] is the `I1`-only form.
pub fn any_int() -> MatchOnly {
    typed_leaf(ValueType::is_integer)
}

/// Any float-typed node, constant or not.
pub fn any_float() -> MatchOnly {
    typed_leaf(ValueType::is_float)
}

/// Any `I1`-typed node, constant or not.
pub fn any_bool() -> MatchOnly {
    // As `anything().bool_valued()`: the width alone narrows the relaxed
    // output kind back to a value.
    let leaf = Leaf {
        width: Some(1),
        ..Leaf::any()
    };
    MatchOnly(leaf.into())
}

/// Match `inner` with every value input `n` bits wide. Also requires at least
/// one value input and at least one value output.
///
/// The width is read off the matched node's own value inputs, so it says
/// nothing about what `inner`'s operand sub-patterns bind: under
/// `ignore_casts` an operand reached through a cast keeps its own width.
///
/// The node predicate alone: pushing the width onto the operand vertices
/// instead lands it on the ARMS of an alternation, which are not operands,
/// and on the far side of a cast walk.
pub fn inputs_of_width<I: MatchPat + 'static>(n: u32, inner: I) -> MatchOnly {
    let want = n as usize;
    let filter = Deco::Filter(Box::new(move |matcher, node| {
        inputs_of_width_check(matcher, node, want)
    }));
    MatchOnly(inner.into_expr().deco(filter).deco(Deco::AsValue))
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

/// Match `inner` whose value inputs are all 1-bit (`I1`).
pub fn bool_inputs<I: MatchPat + 'static>(inner: I) -> MatchOnly {
    inputs_of_width(1, inner)
}

/// `InitialVar` carries a per-function index, not the varnode, so a
/// function-independent pattern can't encode an exact `NodeKind`: it matches
/// the discriminant.
fn initial_var_leaf() -> Leaf {
    Leaf::new(KindSpec::variant_of(&NodeKind::InitialVar(
        InitialVnId::from_index(0),
    )))
}

/// A tracked varnode's entry-time value.
pub fn initial_var() -> MatchOnly {
    MatchOnly(initial_var_leaf().into())
}

/// Match the `InitialVar` whose tracked varnode contains `vn`. The IR keeps
/// only the largest container, so a pinned sub-register (`eax`) matches its
/// container (`rax`).
pub fn initial_var_for(vn: rsleigh::Vn) -> MatchOnly {
    let pred: NodePredicate = Box::new(move |m, node| {
        matches!(
            *m.function().node_kind(node),
            NodeKind::InitialVar(id) if vn_container::vn_contains(&m.function().initial_vn(id), &vn)
        )
    });
    let leaf = Leaf {
        pred: Some(pred),
        ..initial_var_leaf()
    };
    MatchOnly(leaf.into())
}
