//! Matches the function-argument *carrier* node the `FunctionArgDetect`
//! post-pass records in `Function::arg_index_to_values`: an `InitialVar(vn)`
//! for a register-passed arg, a `Load` for a stack-passed one.
//!
//! The carrier produces a value output, so the pattern is value-rooted. Its
//! kind spec has to be [`KindSpec::Any`]: carriers are `InitialVar` or `Load`
//! depending on the ABI, so no discriminant separates a carrier from a
//! non-carrier. The index and source constraints instead ride one node-only
//! predicate that consults the side-table at match time, short-circuiting
//! before the matcher walks any child inputs.

use strider_ir::IRViewer;
use strider_ir::node::{FunctionArgSource, NodeKind};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef, Pattern};

#[derive(Default)]
pub struct FunctionArgPat {
    source: Option<FunctionArgSource>,
    index: Option<u32>,
    capture: Option<Capture>,
}

impl FunctionArgPat {
    /// Register- versus stack-passed.
    pub fn source(mut self, s: FunctionArgSource) -> Self {
        self.source = Some(s);
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

    /// Returns the value output at slot 0. Shared by
    /// [`build`](Self::build), which seals on it, and [`MatchPat::compile`],
    /// which nests the carrier as a value operand.
    fn lower(self, b: &mut MatcherBuilder) -> PatValueRef {
        let FunctionArgPat {
            source,
            index,
            capture,
        } = self;
        let node = b.node(KindSpec::Any);
        let value_out = b.value_output(node, 0);

        // Index and source carry no cross-binding state, so one node
        // predicate covers both and short-circuits before child recursion.
        b.set_node_predicate(
            value_out,
            Box::new(move |matcher, node| {
                let f = matcher.function();
                match index {
                    Some(idx) => {
                        if !f
                            .side_tables()
                            .arg_index_to_values(idx)
                            .iter()
                            .any(|&v| f.producer(v) == node)
                        {
                            return false;
                        }
                    }
                    None => {
                        let any = f.side_tables().iter_arg_indices().any(|i| {
                            f.side_tables()
                                .arg_index_to_values(i)
                                .iter()
                                .any(|&v| f.producer(v) == node)
                        });
                        if !any {
                            return false;
                        }
                    }
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

impl MatchPat for FunctionArgPat {
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.lower(b)
    }
}

/// Unfiltered by source, so both register (`InitialVar`) and stack (`Load`)
/// carriers match.
pub fn function_arg(idx: u32) -> FunctionArgPat {
    FunctionArgPat::default().index(idx)
}

/// Any carrier, whatever its index or source: for passes enumerating every
/// function-arg carrier.
pub fn function_arg_any() -> FunctionArgPat {
    FunctionArgPat::default()
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
