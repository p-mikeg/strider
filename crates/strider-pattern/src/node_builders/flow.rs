//! Control-flow builders: `CallPat`, `CallOtherPat`, `RetPat`, `IfPat`.
//!
//! All four accumulate sparse positional sub-pattern constraints on the
//! IR's input slots and wire them at the right slot. They return a
//! finished [`Pattern`] directly (via `.build()`) — control patterns are
//! builder-only per the design boundary.
//!
//! `CallPat` / `CallOtherPat` / `RetPat` are thin slot-convention
//! wrappers over the shared `NodePat` core; only `IfPat` (whose
//! branch-walk shape doesn't fit the slot map) stays hand-written.
//!
//! # Slot conventions (matching the IR `expected_signature`)
//!
//! * `Call` inputs: `[ctrl(0), mem(1), target(2), sp(3), arg0(4), arg1(5), …]`;
//!   outputs `[Control(0), Memory(1), …clobbers]`. [`CallPat::arg`]
//!   shifts by +4 so callers address positional arguments directly
//!   (the stack-pointer anchor sits at raw slot 3, ahead of the args).
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

use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatValueRef, Pattern};
use crate::typed::{int_const, int_const_any_of};

use super::MemPat;
use super::node_pat::{NodePat, variant_kind};

/// A forward-branch-walk predicate for [`IfPat`]: given the matched If
/// node, walk to a control output's single consumer and match a
/// sub-pattern there.
type BranchWalk = Box<dyn Fn(&crate::Matcher, NodeId) -> bool>;

// ── CallPat ───────────────────────────────────────────────────────────────────

/// Builder for `Call` node patterns. Created by [`call`].
///
/// `Call` is the lifter's representation of a function call; it clobbers
/// caller-saved registers and the memory token.
pub struct CallPat(NodePat);

impl CallPat {
    /// Constrain the call target (`inputs[2]`).
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(2, p))
    }

    /// Constrain the call target to the literal address `addr`.
    /// Equivalent to `.target(int_const(addr))`.
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(u128::from(addr)))
    }

    /// Constrain the call target to any address in `addrs`. An empty
    /// iterator vacuously fails. Equivalent to
    /// `.target(int_const_any_of(addrs))`.
    pub fn at_any<I>(self, addrs: I) -> Self
    where
        I: IntoIterator<Item = u64>,
    {
        self.target(int_const_any_of(addrs))
    }

    /// Constrain positional argument `idx` (0-based, after `ctrl` /
    /// `mem` / `target` / `sp`). Mapped to raw input slot `idx + 4`.
    pub fn arg<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(4 + idx, p))
    }

    /// Constrain the call's control predecessor (`inputs[0]`). The
    /// sub-pattern's root produces a control edge, not a value.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }

    /// Constrain the call's memory predecessor (`inputs[1]`) to a
    /// memory-producing sub-pattern (a `store` / `mem_phi` / prior
    /// `call`).
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(1, p))
    }

    /// Bind the resulting `Call` node to `c`.
    pub fn capture(self, c: Capture) -> Self {
        Self(self.0.capture(c))
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the `Call`
    /// node.
    pub fn build(self) -> Pattern {
        self.0.build()
    }
}

impl MemPat for CallPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_mem(b)
    }
}

/// Construct a fresh [`CallPat`].
pub fn call() -> CallPat {
    // Call clobbers memory: its memory token is output slot 1.
    CallPat(NodePat::node(KindSpec::Exact(NodeKind::Call)).with_mem_value(1))
}

// ── CallOtherPat ─────────────────────────────────────────────────────────────

/// Builder for `CallOther` node patterns. Created by [`call_other`].
///
/// `CallOther` represents a user-op (Sleigh `CALLOTHER`) — an opaque
/// architecture-specific instruction modelled outside the pcode core.
pub struct CallOtherPat {
    inner: NodePat,
    name_filter: Option<String>,
}

impl CallOtherPat {
    /// Constrain the matched node's user-op id (the `CallOther` payload).
    pub fn user_op_id(mut self, v: u64) -> Self {
        let exemplar = NodeKind::CallOther { user_op_id: 0 };
        let kind = variant_kind(
            std::mem::discriminant(&exemplar),
            Some(Box::new(
                move |k| matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == v),
            )),
        );
        self.inner = self.inner.with_kind(kind);
        self
    }

    /// Restrict the match to `CallOther` nodes whose
    /// `Function::call_other_name` equals `name`.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name_filter = Some(name.into());
        self
    }

    /// Constrain `inputs[idx]` of the matched `CallOther` (raw input
    /// slot — callers address control / memory / args uniformly).
    pub fn arg<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inner = self.inner.input(idx, p);
        self
    }

    /// Convenience: match the control input (`inputs[0]`). The
    /// sub-pattern's root produces a control edge, not a value.
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_control(0, p);
        self
    }

    /// Convenience: match the memory input (`inputs[1]`).
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(1, p);
        self
    }

    /// Bind the resulting `CallOther` node to `c`.
    pub fn capture(mut self, c: Capture) -> Self {
        self.inner = self.inner.capture(c);
        self
    }

    /// Apply the `.name` filter (when set) as a node predicate, then
    /// hand back the configured [`NodePat`].
    fn configured(self) -> NodePat {
        let CallOtherPat { inner, name_filter } = self;
        match name_filter {
            // `call_other_name` is a node-only predicate — short-circuits
            // before child recursion.
            Some(want) => inner.with_node_predicate(move || {
                Box::new(move |matcher, n| {
                    matcher.function().call_other_name(n) == Some(want.as_str())
                })
            }),
            None => inner,
        }
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the
    /// `CallOther` node.
    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

impl MemPat for CallOtherPat {
    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_mem(b)
    }
}

/// Construct a fresh [`CallOtherPat`].
pub fn call_other() -> CallOtherPat {
    let exemplar = NodeKind::CallOther { user_op_id: 0 };
    let kind = variant_kind(std::mem::discriminant(&exemplar), None);
    // CallOther also produces a memory token at output slot 1.
    CallOtherPat {
        inner: NodePat::node(kind).with_mem_value(1),
        name_filter: None,
    }
}

// ── RetPat ────────────────────────────────────────────────────────────────────

/// Builder for `Return` node patterns. Created by [`ret`]. A `Return`
/// has no outputs, so the pattern is rooted on the node itself (no value
/// output to anchor on; the capture binds the node).
pub struct RetPat(NodePat);

impl RetPat {
    /// Match `p` against the Return's direct ctrl predecessor
    /// (`inputs[0]`). The sub-pattern's root produces a control edge.
    pub fn preceded_by<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }

    /// Constrain return value at position `idx` (0-based after ctrl and
    /// mem). Mapped to raw input slot `idx + 2`.
    pub fn ret_val<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(2 + idx, p))
    }

    /// Bind the resulting `Return` node to `c`.
    pub fn capture(self, c: Capture) -> Self {
        Self(self.0.capture(c))
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the
    /// `Return` node.
    pub fn build(self) -> Pattern {
        self.0.build()
    }
}

/// Construct a fresh [`RetPat`].
pub fn ret() -> RetPat {
    RetPat(NodePat::node(KindSpec::Exact(NodeKind::Return)))
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
    cond: Option<crate::node_builders::SubCompiler>,
    true_branch: Option<BranchWalk>,
    false_branch: Option<BranchWalk>,
    capture: Option<Capture>,
}

impl IfPat {
    /// Constrain the branch condition (`inputs[1]`). `inputs[0]` is the
    /// ctrl predecessor.
    pub fn cond<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.cond = Some(Box::new(move |b| p.compile(b)));
        self
    }

    /// Match `pat` against the single consumer of the If's true-branch
    /// (control output slot 0). Refuses to match when the output has
    /// zero or multiple consumers.
    ///
    /// A branch consumer is matched **node-wise** (the consumer node is a
    /// `Region` / `Return` / `Call` …, not a value operand), so this slot
    /// takes a finished [`Pattern`] — pass a control builder's
    /// `.build()` (e.g. `call().arg(0, x).build()`) or any value builder
    /// sealed via [`MatchPat::into_pattern`].
    ///
    /// # Captures
    ///
    /// A capture bound inside `pat` is matched against an **isolated**
    /// `Bindings` during the node-wise branch attempt: it is observable to
    /// `pat`'s own `when_match` predicates (a supported idiom) but is **not**
    /// propagated into the outer [`Match`]. Reading such a capture from the
    /// outer match returns `None` — match the branch separately if you need
    /// its bindings.
    ///
    /// # Panics
    ///
    /// Panics if `pat` is malformed (not a single-rooted, acyclic graph the
    /// matcher can handle). The malformed case used to be swallowed into a
    /// silent "branch did not match" via `.ok()`; it is now surfaced eagerly
    /// at build time (matching the eager `check_capture_coverage` policy).
    pub fn with_true(self, pat: Pattern) -> Self {
        self.with_branch(0, pat)
    }

    /// Match `pat` against the single consumer of the If's false-branch
    /// (control output slot 1). Takes a finished [`Pattern`] — see
    /// [`with_true`](Self::with_true).
    ///
    /// # Panics
    ///
    /// Panics on a malformed `pat` — see [`with_true`](Self::with_true)
    /// (which also documents branch-capture isolation).
    pub fn with_false(self, pat: Pattern) -> Self {
        self.with_branch(1, pat)
    }

    /// Shared body of [`with_true`](Self::with_true) /
    /// [`with_false`](Self::with_false): validate `pat`, then store a branch
    /// walk against the If's control-output `slot` (0 = true, 1 = false).
    fn with_branch(mut self, slot: usize, pat: Pattern) -> Self {
        validate_branch_pattern(&pat);
        let walk = Box::new(move |m: &crate::Matcher, if_node| {
            match_branch_consumer(m, if_node, slot, &pat)
        });
        if slot == 0 {
            self.true_branch = Some(walk);
        } else {
            self.false_branch = Some(walk);
        }
        self
    }

    /// Bind the resulting `If` node to `c`.
    pub fn capture(mut self, c: Capture) -> Self {
        self.capture = Some(c);
        self
    }

    /// Seal the builder into a finished [`Pattern`] rooted on the `If`
    /// node.
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
            b.set_node_predicate(
                true_out,
                Box::new(move |m, if_node| {
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
            b.capture_node(node, c);
        }
        b.finish()
    }
}

/// Validate a branch sub-pattern at [`IfPat::with_true`] /
/// [`IfPat::with_false`] build time.
///
/// A branch pattern is matched node-wise against the If's single branch
/// consumer; it must therefore be a single-rooted, matchable [`Pattern`].
/// A malformed one (multi-sink / rootless / cyclic) would, at match time,
/// make `match_at` return `Err`, which the old `.ok()` swallow turned into
/// a silent "branch did not match" — a user typo that vanished. We reject
/// it eagerly here instead.
///
/// Branch *captures* are intentionally NOT rejected: a capture bound inside
/// the branch sub-pattern is observable to that sub-pattern's own
/// `when_match` predicates (which run during the isolated branch match, a
/// real and supported idiom — e.g. an arg-carrier guard). What such a
/// capture is NOT is observable in the *outer* match — that isolation is
/// documented on [`IfPat::with_true`] rather than enforced, so legitimate
/// in-branch predicate scratch captures keep working.
///
/// # Panics
///
/// Panics if `pat` has no derivable match root.
#[allow(clippy::panic)]
fn validate_branch_pattern(pat: &Pattern) {
    if let Err(e) = pat.root() {
        panic!("If branch pattern is not matchable ({e})");
    }
}

/// Walk forward to the single consumer of the If's control output at
/// `output_index` and match `pat` against it. Returns `false` when the
/// output has zero or multiple consumers, or when `pat` doesn't match.
///
/// The consumer may be value-producing (e.g. a `Region`) or a
/// zero-output kind (e.g. `Return`); the matcher's `match_at` dispatches
/// through both shapes. `pat` is validated at build time
/// ([`validate_branch_pattern`]) to be single-rooted and capture-free, so
/// `match_at` here cannot error on a malformed root and no branch capture
/// is silently dropped.
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
    let mut uses = f.graph().value_uses(out);
    let Some((first, _)) = uses.next() else {
        return false;
    };
    if uses.next().is_some() {
        return false;
    }
    // `pat` is validated single-rooted at build time, so `match_at` cannot
    // report a structural error here; an `Err` would be a real bug, hence
    // it is surfaced rather than swallowed.
    match matcher.match_at(first, pat) {
        Ok(opt) => opt.is_some(),
        Err(e) => unreachable!("validated branch pattern failed to match: {e}"),
    }
}

/// Construct a fresh [`IfPat`].
pub fn if_node() -> IfPat {
    IfPat::default()
}

#[cfg(test)]
mod tests {
    use super::if_node;

    /// White-box: the built `If` pattern carries exactly two control-output
    /// vertices (representation invariant — true at slot 0, false at slot 1).
    #[test]
    fn if_pattern_has_two_control_output_vertices() {
        let pat = if_node().build();
        assert_eq!(
            pat.control_output_count(),
            2,
            "If pattern must declare two control-output vertices"
        );
    }
}
