//! Function-argument-carrier builder: [`FunctionArgPat`].
//!
//! After the `FunctionArgDetect` post-pass, function arguments are
//! represented by a *carrier* node recorded in the
//! `Function::arg_index_to_nodes` side-table: an `InitialVar(vn)` node
//! for a register-passed arg, or a `Load` node for a stack-passed arg.
//! [`FunctionArgPat`] matches that carrier.
//!
//! The carrier produces a value output, so the pattern is value-rooted
//! (sealed via [`finish`](crate::builder::MatcherBuilder::finish)). The
//! kind spec is [`KindSpec::Any`] — register carriers are `InitialVar`
//! and stack carriers are `Load`, so the discriminant alone can't
//! distinguish a carrier from a non-carrier. The index / source
//! constraints are a single node-only predicate (a
//! [`LocalLimit`](crate::pattern::LocalLimit)) that consults
//! `Function::arg_index_to_nodes` at match time, short-circuiting before
//! the matcher walks into any child inputs.
//!
//! Enum-dispatch source distinction (register vs stack) is preserved via
//! [`FunctionArgSource`].

use strider_ir::node::{FunctionArgSource, NodeKind};

use crate::builder::MatcherBuilder;
use crate::capture::Capture;
use crate::pattern::{KindSpec, Pattern};

/// Builder for a function-argument-carrier pattern. Created by
/// [`function_arg`] / [`function_arg_any`] / [`function_arg_reg`] /
/// [`function_arg_stack`].
///
/// Matches the carrier node registered in
/// `Function::arg_index_to_nodes`. All constraints (index + source) ride
/// a single node-only predicate; the kind spec is [`KindSpec::Any`].
#[derive(Default)]
pub struct FunctionArgPat {
    source: Option<FunctionArgSource>,
    index: Option<u32>,
    capture: Option<Capture>,
}

impl FunctionArgPat {
    /// Restrict the match to a specific ABI source (register- vs
    /// stack-passed).
    #[must_use]
    pub fn source(mut self, s: FunctionArgSource) -> Self {
        self.source = Some(s);
        self
    }

    /// Restrict the match to a specific argument index.
    #[must_use]
    pub fn index(mut self, i: u32) -> Self {
        self.index = Some(i);
        self
    }

    /// Bind the matched carrier's value output to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the carrier
    /// node's value output.
    #[must_use]
    pub fn build(self) -> Pattern {
        let FunctionArgPat {
            source,
            index,
            capture,
        } = self;
        let mut b = MatcherBuilder::new();
        let node = b.node(KindSpec::Any);
        // The carrier (`InitialVar` / `Load`) produces a value at slot 0.
        let value_out = b.value_output(node, 0);

        // Index + source predicates are node-only — no cross-binding
        // state — so they live on the node limit and short-circuit before
        // child recursion.
        b.set_node_limit(
            value_out,
            Box::new(move |matcher, node, _ty| {
                let f = matcher.function();
                // Index constraint.
                match index {
                    Some(idx) => {
                        if !f.arg_index_to_nodes(idx).contains(&node) {
                            return false;
                        }
                    }
                    None => {
                        let any = f
                            .iter_arg_indices()
                            .any(|i| f.arg_index_to_nodes(i).contains(&node));
                        if !any {
                            return false;
                        }
                    }
                }
                // Source constraint.
                let Some(expected) = source else {
                    return true;
                };
                match (expected, f.node_kind(node)) {
                    (FunctionArgSource::Register(want), NodeKind::InitialVar(actual)) => {
                        want == *actual
                    }
                    (
                        FunctionArgSource::Stack {
                            space: want_space,
                            offset: want_offset,
                        },
                        NodeKind::Load(actual_space),
                    ) => {
                        // Enforce the carrier's address space (the `Load`
                        // payload) and its SP-relative offset (the
                        // `StackOffsetDetect`-populated `stack_offset`
                        // side-table). A carrier with no recorded stack
                        // offset, or one at a different (space, offset),
                        // is rejected.
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
            b.capture_node(value_out, c);
        }
        b.finish(value_out)
    }
}

/// Match the carrier registered at side-table index `idx`. No source
/// filter — accepts both register-passed (`InitialVar`) and stack-passed
/// (`Load`) carriers.
#[must_use]
pub fn function_arg(idx: u32) -> FunctionArgPat {
    FunctionArgPat::default().index(idx)
}

/// Match any carrier registered in the side-table, regardless of index or
/// source. Used by passes that want to enumerate every function-arg
/// carrier in a function.
#[must_use]
pub fn function_arg_any() -> FunctionArgPat {
    FunctionArgPat::default()
}

/// Match the carrier at side-table index `idx`, restricted to a
/// register-passed `InitialVar(vn)`.
#[must_use]
pub fn function_arg_reg(vn: rsleigh::Vn, idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .index(idx)
        .source(FunctionArgSource::Register(vn))
}

/// Match the carrier at side-table index `idx`, restricted to a
/// stack-passed `Load` at `(space, offset)`.
#[must_use]
pub fn function_arg_stack(space: rsleigh::VnSpace, offset: i64, idx: u32) -> FunctionArgPat {
    FunctionArgPat::default()
        .index(idx)
        .source(FunctionArgSource::Stack { space, offset })
}
