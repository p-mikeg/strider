//! Control-flow builders: `CallPat`, `CallOtherPat`, `RetPat`, `IfPat`.
//!
//! All four accumulate sparse positional sub-pattern constraints on the
//! IR's input slots and wire them at the right slot. They return a
//! finished [`Pattern`] directly (via `.build()`) — control patterns are
//! builder-only per the design boundary.
//!
//! # Slot conventions (matching the IR `expected_signature`)
//!
//! * `Call` inputs: `[ctrl(0), mem(1), target(2), arg0(3), arg1(4), …]`;
//!   outputs `[Control(0), Memory(1), …clobbers]`. [`CallPat::arg`]
//!   shifts by +3 so callers address positional arguments directly.
//!   `Call` clobbers memory — its memory token (output slot 1) is
//!   exposed via [`MemPat`] so a downstream `load` / `store` can chain
//!   off it.
//! * `CallOther` inputs: `[ctrl(0), mem(1), arg0(2), …]`; outputs
//!   `[Control(0), Memory(1), …]`. [`CallOtherPat::arg`] writes the raw
//!   input slot (no shift); `.ctrl` / `.mem` write slots 0 / 1.
//!   `.name(s)` filters on `Function::call_other_name` via a node-only
//!   limit.
//! * `Return` inputs: `[ctrl(0), mem(1), retval0(2), retval1(3), …]`;
//!   no outputs. [`RetPat::ret_val`] shifts by +2;
//!   [`RetPat::preceded_by`] writes slot 0.
//! * `If` inputs: `[ctrl(0), cond(1)]`; outputs `[Control(0) (true),
//!   Control(1) (false)]` — both modelled as genuine control-output
//!   vertices. [`IfPat::cond`] writes slot 1; [`IfPat::with_true`] /
//!   [`IfPat::with_false`] forward-walk from the matched If's control
//!   output to its single consumer and match there.

use strider_ir::node::{NodeId, NodeKind};

use crate::builder::{MatcherBuilder, PatNodeRef, PatOutRef};
use crate::capture::Capture;
use crate::match_pat::MatchPat;
use crate::pattern::{KindSpec, Pattern};
use crate::typed::{int_const, int_const_any_of};

use super::{IndexedInputs, MemPat, SubCompiler};

/// A forward-branch-walk predicate for [`IfPat`]: given the matched If
/// node, walk to a control output's single consumer and match a
/// sub-pattern there.
type BranchWalk = Box<dyn Fn(&crate::Matcher, NodeId) -> bool>;

/// Wire every indexed sub-pattern into `node`'s matching input slot.
fn wire_inputs(b: &mut MatcherBuilder, node: PatNodeRef, inputs: IndexedInputs) {
    for (slot, compile) in inputs {
        let o = compile(b);
        b.input(node, slot, o);
    }
}

/// A [`SubCompiler`] that compiles a value-pattern then relaxes its root
/// output to match a control edge. Used for control-predecessor slots
/// (`ctrl` / `preceded_by`) where the producer's output is `Control`,
/// not a value.
fn control_compiler<P: MatchPat + 'static>(p: P) -> SubCompiler {
    Box::new(move |b| {
        let o = p.compile(b);
        b.set_output_control(o);
        o
    })
}

// ── CallPat ───────────────────────────────────────────────────────────────────

/// Builder for `Call` node patterns. Created by [`call`].
///
/// `Call` is the lifter's representation of a function call; it clobbers
/// caller-saved registers and the memory token.
#[derive(Default)]
pub struct CallPat {
    target: Option<SubCompiler>,
    ctrl: Option<SubCompiler>,
    mem: Option<SubCompiler>,
    args: IndexedInputs,
    capture: Option<Capture>,
}

impl CallPat {
    /// Constrain the call target (`inputs[2]`).
    #[must_use]
    pub fn target<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.target = Some(Box::new(move |b| p.compile(b)));
        self
    }

    /// Constrain the call target to the literal address `addr`.
    /// Equivalent to `.target(int_const(addr))`.
    #[must_use]
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(u128::from(addr)))
    }

    /// Constrain the call target to any address in `addrs`. An empty
    /// iterator vacuously fails. Equivalent to
    /// `.target(int_const_any_of(addrs))`.
    #[must_use]
    pub fn at_any<I>(self, addrs: I) -> Self
    where
        I: IntoIterator<Item = u64>,
    {
        self.target(int_const_any_of(addrs))
    }

    /// Constrain positional argument `idx` (0-based, after `ctrl` /
    /// `mem` / `target`). Mapped to raw input slot `idx + 3`.
    #[must_use]
    pub fn arg<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.args.push((3 + idx, Box::new(move |b| p.compile(b))));
        self
    }

    /// Constrain the call's control predecessor (`inputs[0]`). The
    /// sub-pattern's root produces a control edge, not a value.
    #[must_use]
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.ctrl = Some(control_compiler(p));
        self
    }

    /// Constrain the call's memory predecessor (`inputs[1]`) to a
    /// memory-producing sub-pattern (a `store` / `mem_phi` / prior
    /// `call`).
    #[must_use]
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.mem = Some(Box::new(move |b| p.compile_mem(b)));
        self
    }

    /// Bind the resulting `Call` node to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Lower the call onto `b`, returning its node handle and
    /// memory-token output (output slot 1).
    fn lower(self, b: &mut MatcherBuilder) -> (PatNodeRef, PatOutRef) {
        let CallPat {
            target,
            ctrl,
            mem,
            args,
            capture,
        } = self;
        let node = b.node(KindSpec::Exact(NodeKind::Call));
        // Call clobbers memory: its memory token is output slot 1.
        let mem_out = b.memory_output(node, 1);

        let mut indexed: IndexedInputs = Vec::new();
        if let Some(p) = ctrl {
            indexed.push((0, p));
        }
        if let Some(p) = mem {
            indexed.push((1, p));
        }
        if let Some(p) = target {
            indexed.push((2, p));
        }
        indexed.extend(args);
        wire_inputs(b, node, indexed);
        if let Some(c) = capture {
            b.capture_node(mem_out, c);
        }
        (node, mem_out)
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the `Call`
    /// node.
    #[must_use]
    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let (node, _mem_out) = self.lower(&mut b);
        b.finish_node(node)
    }
}

impl MemPat for CallPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatOutRef {
        let (_node, mem_out) = self.lower(b);
        mem_out
    }
}

/// Construct a fresh [`CallPat`].
#[must_use]
pub fn call() -> CallPat {
    CallPat::default()
}

// ── CallOtherPat ─────────────────────────────────────────────────────────────

/// Builder for `CallOther` node patterns. Created by [`call_other`].
///
/// `CallOther` represents a user-op (Sleigh `CALLOTHER`) — an opaque
/// architecture-specific instruction modelled outside the pcode core.
#[derive(Default)]
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    inputs: IndexedInputs,
    name_filter: Option<String>,
    capture: Option<Capture>,
}

impl CallOtherPat {
    /// Constrain the matched node's user-op id (the `CallOther` payload).
    #[must_use]
    pub fn user_op_id(mut self, v: u64) -> Self {
        self.user_op_id = Some(v);
        self
    }

    /// Restrict the match to `CallOther` nodes whose
    /// `Function::call_other_name` equals `name`.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name_filter = Some(name.into());
        self
    }

    /// Constrain `inputs[idx]` of the matched `CallOther` (raw input
    /// slot — callers address control / memory / args uniformly).
    #[must_use]
    pub fn arg<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inputs.push((idx, Box::new(move |b| p.compile(b))));
        self
    }

    /// Convenience: match the control input (`inputs[0]`). The
    /// sub-pattern's root produces a control edge, not a value.
    #[must_use]
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inputs.push((0, control_compiler(p)));
        self
    }

    /// Convenience: match the memory input (`inputs[1]`).
    #[must_use]
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inputs.push((1, Box::new(move |b| p.compile_mem(b))));
        self
    }

    /// Bind the resulting `CallOther` node to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Lower the call-other onto `b`, returning its node handle and
    /// memory-token output (output slot 1).
    fn lower(self, b: &mut MatcherBuilder) -> (PatNodeRef, PatOutRef) {
        let CallOtherPat {
            user_op_id,
            inputs,
            name_filter,
            capture,
        } = self;
        let exemplar = NodeKind::CallOther { user_op_id: 0 };
        let kind = match user_op_id {
            None => KindSpec::Variant(std::mem::discriminant(&exemplar)),
            Some(expected) => KindSpec::VariantWith {
                discriminant: std::mem::discriminant(&exemplar),
                check: Box::new(move |k| {
                    matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == expected)
                }),
            },
        };
        let node = b.node(kind);
        // CallOther also produces a memory token at output slot 1.
        let mem_out = b.memory_output(node, 1);
        wire_inputs(b, node, inputs);
        if let Some(want) = name_filter {
            // `call_other_name` is a node-only predicate — short-circuits
            // before child recursion.
            b.set_node_limit(
                mem_out,
                Box::new(move |matcher, n, _ty| {
                    matcher.function().call_other_name(n) == Some(want.as_str())
                }),
            );
        }
        if let Some(c) = capture {
            b.capture_node(mem_out, c);
        }
        (node, mem_out)
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the
    /// `CallOther` node.
    #[must_use]
    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        let (node, _mem_out) = self.lower(&mut b);
        b.finish_node(node)
    }
}

impl MemPat for CallOtherPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatOutRef {
        let (_node, mem_out) = self.lower(b);
        mem_out
    }
}

/// Construct a fresh [`CallOtherPat`].
#[must_use]
pub fn call_other() -> CallOtherPat {
    CallOtherPat::default()
}

// ── RetPat ────────────────────────────────────────────────────────────────────

/// Builder for `Return` node patterns. Created by [`ret`]. A `Return`
/// has no outputs, so the pattern is rooted on the node itself
/// (`finish_node`).
#[derive(Default)]
pub struct RetPat {
    preceded_by: Option<SubCompiler>,
    ret_vals: IndexedInputs,
    capture: Option<Capture>,
}

impl RetPat {
    /// Match `p` against the Return's direct ctrl predecessor
    /// (`inputs[0]`). The sub-pattern's root produces a control edge.
    #[must_use]
    pub fn preceded_by<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.preceded_by = Some(control_compiler(p));
        self
    }

    /// Constrain return value at position `idx` (0-based after ctrl and
    /// mem). Mapped to raw input slot `idx + 2`.
    #[must_use]
    pub fn ret_val<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.ret_vals
            .push((2 + idx, Box::new(move |b| p.compile(b))));
        self
    }

    /// Bind the resulting `Return` node to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the
    /// `Return` node.
    #[must_use]
    pub fn build(self) -> Pattern {
        let RetPat {
            preceded_by,
            ret_vals,
            capture,
        } = self;
        let mut b = MatcherBuilder::new();
        let node = b.node(KindSpec::Exact(NodeKind::Return));
        let mut indexed: IndexedInputs = Vec::new();
        if let Some(p) = preceded_by {
            indexed.push((0, p));
        }
        indexed.extend(ret_vals);
        wire_inputs(&mut b, node, indexed);
        if let Some(c) = capture {
            b.capture_node_for(node, c);
        }
        b.finish_node(node)
    }
}

/// Construct a fresh [`RetPat`].
#[must_use]
pub fn ret() -> RetPat {
    RetPat::default()
}

// ── IfPat ─────────────────────────────────────────────────────────────────────

/// Builder for `If` node patterns. Created by [`if_node`].
///
/// The `If` node carries **two** control-output vertices (true at slot
/// 0, false at slot 1) — a representation invariant modelled explicitly.
/// `.cond(p)` constrains the branch condition (`inputs[1]`).
/// `.with_true(q)` / `.with_false(r)` forward-walk from the matched If's
/// control output to its single consumer and match the sub-pattern
/// there; both fail the match when the output has zero or multiple
/// consumers (we refuse to pick arbitrarily when a control output
/// forks).
#[derive(Default)]
pub struct IfPat {
    cond: Option<SubCompiler>,
    true_branch: Option<BranchWalk>,
    false_branch: Option<BranchWalk>,
    capture: Option<Capture>,
}

impl IfPat {
    /// Constrain the branch condition (`inputs[1]`). `inputs[0]` is the
    /// ctrl predecessor.
    #[must_use]
    pub fn cond<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.cond = Some(Box::new(move |b| p.compile(b)));
        self
    }

    /// Match `p` against the single consumer of the If's true-branch
    /// (control output slot 0). Refuses to match when the output has
    /// zero or multiple consumers.
    #[must_use]
    pub fn with_true(mut self, p: impl MatchPat + 'static) -> Self {
        let pat = p.into_pattern();
        self.true_branch = Some(Box::new(move |m, if_node| {
            match_branch_consumer(m, if_node, 0, &pat)
        }));
        self
    }

    /// Match `p` against the single consumer of the If's false-branch
    /// (control output slot 1).
    #[must_use]
    pub fn with_false(mut self, p: impl MatchPat + 'static) -> Self {
        let pat = p.into_pattern();
        self.false_branch = Some(Box::new(move |m, if_node| {
            match_branch_consumer(m, if_node, 1, &pat)
        }));
        self
    }

    /// Bind the resulting `If` node to `c`.
    #[must_use]
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the `If`
    /// node.
    #[must_use]
    pub fn build(self) -> Pattern {
        let IfPat {
            cond,
            true_branch,
            false_branch,
            capture,
        } = self;
        let mut b = MatcherBuilder::new();
        let node = b.node(KindSpec::Exact(NodeKind::If));
        // Representation invariant: the If carries two genuine
        // control-output vertices — true at slot 0, false at slot 1.
        let true_out = b.control_output(node, 0);
        let _false_out = b.control_output(node, 1);

        if let Some(cond) = cond {
            let c = cond(&mut b);
            b.input(node, 1, c);
        }
        // The branch forward-walks are node-only predicates (they inspect
        // the If's control outputs + their use lists, not the outer match
        // bindings), so they ride a single node limit anchored on the
        // true control output (which resolves to the If node).
        if true_branch.is_some() || false_branch.is_some() {
            b.set_node_limit(
                true_out,
                Box::new(move |m, if_node, _ty| {
                    if let Some(tb) = &true_branch
                        && !tb(m, if_node)
                    {
                        return false;
                    }
                    if let Some(fb) = &false_branch
                        && !fb(m, if_node)
                    {
                        return false;
                    }
                    true
                }),
            );
        }
        if let Some(c) = capture {
            b.capture_node_for(node, c);
        }
        b.finish_node(node)
    }
}

/// Walk forward to the single consumer of the If's control output at
/// `output_index` and match `pat` against it. Returns `false` when the
/// output has zero or multiple consumers, or when `pat` doesn't match.
///
/// The consumer may be value-producing (e.g. a `Region`) or a
/// zero-output kind (e.g. `Return`); the matcher's `match_at` dispatches
/// through both shapes. Captures inside the branch sub-pattern are not
/// propagated into the outer match — they bind against an isolated
/// attempt.
fn match_branch_consumer(
    matcher: &crate::Matcher,
    if_node: NodeId,
    output_index: usize,
    pat: &Pattern,
) -> bool {
    let f = matcher.function();
    let outputs = f.node_outputs(if_node);
    let Some(&out) = outputs.get(output_index) else {
        return false;
    };
    let mut uses = f.output_uses(out);
    let Some((first, _)) = uses.next() else {
        return false;
    };
    if uses.next().is_some() {
        return false;
    }
    matcher.match_at(first, pat).is_some()
}

/// Construct a fresh [`IfPat`].
#[must_use]
pub fn if_node() -> IfPat {
    IfPat::default()
}
