//! Matches the function-argument *carrier* node the `FunctionArgDetect`
//! post-pass records in `SideTables::arg_index_to_values`: an `InitialVar(vn)`
//! for a register-passed arg, a `Load` for a stack-passed one.
//!
//! The kind spec has to be [`KindSpec::Any`], since no discriminant separates
//! a carrier from a non-carrier. The index and source constraints ride a
//! node-only predicate consulting the side-table at match time.

use strider_ir::IRViewer;
use strider_ir::node::{FunctionArgSource, NodeKind};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef, Pattern};

/// Which index space [`FunctionArgPat::index`] refers to. Integer and float
/// arguments are numbered separately, so the caller states the class.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum FunctionArgClass {
    #[default]
    Integer,
    Float,
    /// Either class, index counted within whichever one the carrier belongs to.
    Any,
}

#[derive(Default)]
pub struct FunctionArgPat {
    source: Option<FunctionArgSource>,
    class: FunctionArgClass,
    index: Option<u32>,
    capture: Option<Capture>,
}

impl FunctionArgPat {
    /// Register- versus stack-passed.
    pub fn source(mut self, s: FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }

    /// Integer- versus float-register numbering. Defaults to
    /// [`FunctionArgClass::Integer`].
    pub fn class(mut self, c: FunctionArgClass) -> Self {
        self.class = c;
        self
    }

    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }

    /// Binds the carrier's value output.
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Returns the value output at slot 0.
    fn lower(self, b: &mut MatcherBuilder) -> PatValueRef {
        let FunctionArgPat {
            source,
            class,
            index,
            capture,
        } = self;
        let node = b.node(KindSpec::Any);
        let value_out = b.value_output(node, 0);

        // Index, class and source carry no cross-binding state, so one node
        // predicate covers them all and short-circuits before child recursion.
        b.set_node_predicate(
            value_out,
            Box::new(move |matcher, node| {
                let f = matcher.function();
                if !is_carrier(f, node, class, index) {
                    return false;
                }
                let Some(expected) = source else {
                    return true;
                };
                match (expected, f.node_kind(node)) {
                    (FunctionArgSource::Register(want), NodeKind::InitialVar(actual)) => {
                        want == f.initial_vn(*actual)
                    }
                    (
                        FunctionArgSource::Stack {
                            space: want_space,
                            offset: want_offset,
                        },
                        NodeKind::Load(actual_space),
                    ) => {
                        // Both the address space, from the `Load` payload,
                        // and the SP-relative offset, from the
                        // `StackOffsetDetect`-populated side-table, must
                        // agree. A carrier with no recorded offset is
                        // rejected.
                        if want_space != *actual_space {
                            return false;
                        }
                        matches!(f.stack_offset(node), Some((_, off)) if off == want_offset)
                    }
                    _ => false,
                }
            }),
        );
        if let Some(c) = capture {
            b.capture_output(value_out, c);
        }
        value_out
    }

    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.lower(&mut b);
        b.finish()
    }
}

/// Whether `node` produces a carrier of `class` at `index`, `index` `None`
/// accepting any position within the class.
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

impl MatchPat for FunctionArgPat {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b)
    }
}

/// The `idx`-th integer-class argument. Unfiltered by source, so both register
/// (`InitialVar`) and stack (`Load`) carriers match.
pub fn function_arg(idx: u32) -> FunctionArgPat {
    FunctionArgPat::default().index(idx)
}

/// The `idx`-th float-class argument, counting only float parameters: a
/// `double f(int, double)` passes its `double` as float argument 0.
pub fn function_arg_float(idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .class(FunctionArgClass::Float)
        .index(idx)
}

/// Any carrier, whatever its class, index or source.
pub fn any_function_arg() -> FunctionArgPat {
    FunctionArgPat::default().class(FunctionArgClass::Any)
}

/// Restricted to a register-passed `InitialVar(vn)`.
pub fn function_arg_reg(vn: rsleigh::Vn, idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .index(idx)
        .source(FunctionArgSource::Register(vn))
}

/// Restricted to a stack-passed `Load` at `(space, offset)`.
pub fn function_arg_stack(space: rsleigh::VnSpace, offset: i128, idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .index(idx)
        .source(FunctionArgSource::Stack { space, offset })
}
