use strider_ir::IRViewer;
use strider_ir::node::{FunctionArgSource, NodeKind};

use crate::expr::Node;
use crate::matcher::{KindSpec, Pattern};
use crate::node_builders::{delegate_node_pat, node_match_pat};

/// Which index space [`FunctionArgPat::index`] counts in; integer and float
/// arguments are numbered separately.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum FunctionArgClass {
    #[default]
    Integer,
    Float,
    /// Either class, indexed within the carrier's own.
    Any,
}

/// A function-argument carrier recorded by `FunctionArgDetect`: an `InitialVar` for a
/// register-passed arg, a `Load` for a stack-passed one.
pub struct FunctionArgPat {
    source: Option<FunctionArgSource>,
    class: FunctionArgClass,
    index: Option<u32>,
    inner: Node,
}

impl Default for FunctionArgPat {
    fn default() -> Self {
        Self {
            source: None,
            class: FunctionArgClass::default(),
            index: None,
            inner: Node::value(KindSpec::Any, 0),
        }
    }
}

impl FunctionArgPat {
    /// Register- or stack-passed. A register selects the carrier whose varnode
    /// contains it (`EDI` selects `RDI`).
    pub fn source(mut self, s: FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }

    /// The index space; defaults to [`FunctionArgClass::Integer`].
    pub fn class(mut self, c: FunctionArgClass) -> Self {
        self.class = c;
        self
    }

    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }

    /// Index, class and source as one node predicate.
    fn lower(self) -> Node {
        let FunctionArgPat {
            source,
            class,
            index,
            inner,
        } = self;
        inner.with_node_predicate(Box::new(move |matcher, node| {
            let f = matcher.function();
            if !is_carrier(f, node, class, index) {
                return false;
            }
            let Some(expected) = source else {
                return true;
            };
            match (expected, f.node_kind(node)) {
                (FunctionArgSource::Register(want), NodeKind::InitialVar(actual)) => {
                    vn_container::vn_contains(&f.initial_vn(*actual), &want)
                }
                (
                    FunctionArgSource::Stack {
                        space: want_space,
                        offset: want_offset,
                    },
                    NodeKind::Load(actual_space),
                ) => {
                    // A carrier with no recorded stack offset is rejected.
                    if want_space != *actual_space {
                        return false;
                    }
                    matches!(f.stack_offset(node), Some((_, off)) if off == want_offset)
                }
                _ => false,
            }
        }))
    }

    pub fn build(self) -> Pattern {
        self.lower().build()
    }
}

delegate_node_pat! { FunctionArgPat, inner, [capture] }

/// Whether `node` produces a carrier of `class` at `index` (`None`: any).
fn is_carrier(
    f: &strider_ir::Function,
    node: strider_ir::node::NodeId,
    class: FunctionArgClass,
    index: Option<u32>,
) -> bool {
    let st = f.side_tables();
    let holds =
        |values: &[strider_ir::node::ValueId]| values.iter().any(|&v| f.producer(v) == node);
    match (class, index) {
        (FunctionArgClass::Integer, Some(i)) => holds(st.arg_index_to_values(i)),
        (FunctionArgClass::Float, Some(i)) => holds(st.float_arg_index_to_values(i)),
        (FunctionArgClass::Any, Some(i)) => {
            holds(st.arg_index_to_values(i)) || holds(st.float_arg_index_to_values(i))
        }
        (FunctionArgClass::Integer, None) => st
            .iter_arg_indices()
            .any(|i| holds(st.arg_index_to_values(i))),
        (FunctionArgClass::Float, None) => st
            .iter_float_arg_indices()
            .any(|i| holds(st.float_arg_index_to_values(i))),
        (FunctionArgClass::Any, None) => st.arg_carrier_values().any(|v| f.producer(v) == node),
    }
}

/// The `idx`-th integer-class argument, register- or stack-passed.
pub fn function_arg(idx: u32) -> FunctionArgPat {
    FunctionArgPat::default().index(idx)
}

/// The `idx`-th float-class argument (`double f(int, double)`: the `double` is 0).
pub fn function_arg_float(idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .class(FunctionArgClass::Float)
        .index(idx)
}

/// Any carrier, whatever its class, index or source.
pub fn any_function_arg() -> FunctionArgPat {
    FunctionArgPat::default().class(FunctionArgClass::Any)
}

/// The `idx`-th integer argument, register-passed in a varnode containing `vn`.
pub fn function_arg_reg(vn: rsleigh::Vn, idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .index(idx)
        .source(FunctionArgSource::Register(vn))
}

/// The `idx`-th integer argument, stack-passed at `(space, offset)`.
pub fn function_arg_stack(space: rsleigh::VnSpace, offset: i128, idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .index(idx)
        .source(FunctionArgSource::Stack { space, offset })
}

node_match_pat! {
    FunctionArgPat => |p| p.lower(),
}

#[cfg(test)]
mod tests {
    use super::function_arg;
    use crate::capture::Capture;
    use crate::node_builders::pattern_shape;

    /// Pins the lowering: one `Any` node with one predicate, one value output
    /// carrying every capture.
    #[test]
    fn function_arg_lowering_declares_one_node_and_one_value_output() {
        let pat = function_arg(2)
            .capture(Capture::new())
            .capture(Capture::new())
            .build();
        assert_eq!(
            pattern_shape(&pat),
            "n0 Any caps=0 pred=1 Operands { ordered: false }\n\
             \x20 out v0 AnyValue slot=0 mode=Loose width=None caps=2 ident=0 token=0\n\
             root=Ok(0) total_nodes=1\n",
        );
    }
}
