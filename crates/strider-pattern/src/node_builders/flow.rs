//! # Slot conventions, per the IR `expected_signature`
//!
//! * `Call` inputs `[ctrl(0), mem(1), target(2), sp(3), arg0(4), arg1(5),
//!   ...]`, outputs `[Control(0), Memory(1), clobbers...]`.
//!   [`CallPat::arg`] shifts by +4, past the stack-pointer anchor at raw
//!   slot 3.
//! * `CallOther` inputs `[ctrl(0), mem(1), arg0(2), ...]`, outputs
//!   `[Control(0), Memory(1), ...]`. [`CallOtherPat::arg`] writes the raw
//!   input slot, unshifted.
//! * `Return` inputs `[ctrl(0), mem(1), retval0(2), retval1(3), ...]`, no
//!   outputs. [`RetPat::ret_val`] shifts by +2.
//! * `If` inputs `[ctrl(0), cond(1)]`, outputs `[Control(0) true,
//!   Control(1) false]`.

use super::delegate_node_pat;
use crate::node_builders::delegate_with_output;
use itertools::Itertools;
use strider_ir::IRViewer;
use strider_ir::node::{NodeId, NodeKind, ValueType};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, MatcherBuilder, PatNodeRef, PatValueRef, Pattern, WalkCaptures};
use crate::typed::int_const;

use super::MemPat;
use super::node_pat::{NodePat, variant_kind};

/// Walks from a matched If to a control output's single consumer and matches
/// a sub-pattern there.
type BranchWalk = Box<
    dyn Fn(
            &crate::Matcher,
            NodeId,
            &mut crate::Bindings,
            &mut dyn FnMut(&mut crate::Bindings) -> bool,
        ) -> bool
        + Send,
>;

/// The branch `Pattern` a [`BranchWalk`] holds. Nesting these is unbounded
/// (`with_branch` takes an already built pattern, so no builder cap sees it)
/// and the derived glue frees one frame per level, so the free goes through
/// the drop pit.
struct BranchPattern(Option<Pattern>);

impl BranchPattern {
    fn get(&self) -> &Pattern {
        self.0.as_ref().expect("taken only by Drop")
    }
}

impl Drop for BranchPattern {
    fn drop(&mut self) {
        if let Some(pat) = self.0.take() {
            crate::node_builders::defer_drop(pat);
        }
    }
}

/// A `Call` clobbers caller-saved registers and the memory token.
pub struct CallPat(NodePat);

impl CallPat {
    /// `inputs[2]`.
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(2, p))
    }

    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(u128::from(addr)))
    }

    /// 0-based past `ctrl` / `mem` / `target` / `sp`, so raw input slot
    /// `idx + 4`.
    pub fn arg<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(NodeKind::Call.input_head_len() + idx, p))
    }

    /// `inputs[0]`. The sub-pattern's root produces a control edge, not a
    /// value.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }

    /// `inputs[1]`, taking a `store` / `mem_phi` / prior `call`.
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(1, p))
    }

    /// When nested as a value operand, pins the operand to the declared
    /// result output at raw slot 2, so a caller-saved clobber output cannot
    /// match. No effect on a root or memory producer.
    pub fn res(self) -> Self {
        Self(self.0.pin_anchor_slot())
    }
}

/// Commits a sibling-output constraint onto a multi-output family builder.
pub trait WithOutput: Sized {
    fn capture_output(self, slot: Option<usize>, c: Capture) -> Self;
    fn output_width(self, slot: Option<usize>, bits: u32) -> Self;
    fn output_ty(self, slot: Option<usize>, ty: ValueType) -> Self;

    /// A sibling output at raw slot `slot`. A leaf, naming the output value
    /// itself rather than recursing into what it feeds; which slot is which is
    /// per node kind, laid out by the IR's `expected_signature`.
    fn output(self, slot: usize) -> OutputPat<Self> {
        OutputPat::at(self, Some(slot))
    }

    /// Some output rather than a fixed slot; otherwise [`output`](Self::output).
    ///
    /// Enumerating: `find_all` dedups at value granularity, so every
    /// qualifying output is its own match. The collapse to one match per node
    /// is `strider-py`'s, in `dedup_matches`.
    fn any_output(self) -> OutputPat<Self> {
        OutputPat::at(self, None)
    }
}

/// Commits one sibling-output constraint, then returns the family builder so
/// the chain continues.
///
/// One `.output(slot)` call carries exactly one aspect: capture, width or
/// type. Call it again on the same slot for a second vertex.
///
/// `slot` is `None` for the existential `.any_output()`.
pub struct OutputPat<B: WithOutput> {
    parent: B,
    slot: Option<usize>,
}

impl<B: WithOutput> OutputPat<B> {
    pub(crate) fn at(parent: B, slot: Option<usize>) -> Self {
        Self { parent, slot }
    }

    pub fn capture(self, c: Capture) -> B {
        self.parent.capture_output(self.slot, c)
    }

    pub fn of_width(self, bits: u32) -> B {
        self.parent.output_width(self.slot, bits)
    }

    pub fn of_type(self, ty: ValueType) -> B {
        self.parent.output_ty(self.slot, ty)
    }
}

delegate_with_output!(CallPat, 0);
delegate_node_pat!(CallPat, 0, [capture, build, input, any_input]);

impl MatchPat for CallPat {
    /// Nests as a value operand, anchored on the first value output. Loose:
    /// any value output matches. `.res()` tightens it.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0
            .with_value_anchor(FIRST_VALUE_OUT_SLOT)
            .compile_anchored(b)
    }

    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_anchored(b)
    }
}

impl MemPat for CallPat {}

/// `Call` / `CallOther` outputs are `[Control(0), Memory(1), value...(2)]`,
/// so return and clobber values start at slot 2.
const FIRST_VALUE_OUT_SLOT: usize = 2;

pub fn call() -> CallPat {
    CallPat(NodePat::node(KindSpec::Exact(NodeKind::Call)).with_mem_value(1))
}

/// A `CallOther` is a Sleigh `CALLOTHER` user-op: an opaque
/// architecture-specific instruction modelled outside the pcode core.
pub struct CallOtherPat {
    inner: NodePat,
    name_filter: Option<String>,
}

impl CallOtherPat {
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

    /// Filters on `SideTables::call_other_name`.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name_filter = Some(name.into());
        self
    }

    /// The raw input slot, unshifted: control, memory and args are addressed
    /// uniformly.
    pub fn arg<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inner = self.inner.input(idx, p);
        self
    }

    /// `inputs[0]`. The sub-pattern's root produces a control edge, not a
    /// value.
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_control(0, p);
        self
    }

    /// `inputs[1]`.
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(1, p);
        self
    }

    /// See [`CallPat::res`]. Here it excludes implicit-write clobber outputs.
    pub fn res(mut self) -> Self {
        self.inner = self.inner.pin_anchor_slot();
        self
    }

    /// Lowers any `.name` filter to a node predicate.
    fn configured(self) -> NodePat {
        let CallOtherPat { inner, name_filter } = self;
        match name_filter {
            // Node-only, so it short-circuits before child recursion.
            Some(want) => inner.with_node_predicate(move || {
                Box::new(move |matcher, n| {
                    matcher.function().side_tables().call_other_name(n) == Some(want.as_str())
                })
            }),
            None => inner,
        }
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

delegate_with_output!(CallOtherPat, inner);
delegate_node_pat!(CallOtherPat, inner, [capture, input, any_input]);

impl MatchPat for CallOtherPat {
    /// See [`CallPat`]'s impl.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured()
            .with_value_anchor(FIRST_VALUE_OUT_SLOT)
            .compile_anchored(b)
    }

    fn compile_mem(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.configured().compile_anchored(b)
    }
}

impl MemPat for CallOtherPat {}

pub fn call_other() -> CallOtherPat {
    let exemplar = NodeKind::CallOther { user_op_id: 0 };
    let kind = variant_kind(std::mem::discriminant(&exemplar), None);
    CallOtherPat {
        inner: NodePat::node(kind).with_mem_value(1),
        name_filter: None,
    }
}

/// A `Return` has no outputs, so the pattern is rooted on the node itself and
/// a capture binds the node.
pub struct RetPat(NodePat);

impl RetPat {
    /// `inputs[0]`. The sub-pattern's root produces a control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }

    /// 0-based past ctrl and mem, so raw input slot `idx + 2`.
    pub fn ret_val<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(NodeKind::Return.input_head_len() + idx, p))
    }
}

delegate_node_pat!(RetPat, 0, [capture, build, input, any_input]);

impl MatchPat for RetPat {
    /// Node-rooted: the synthesized `Any` output is what an alternation or an
    /// existential slot wires; no value slot can bind it.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_alt_arm(b)
    }
}

pub fn ret() -> RetPat {
    RetPat(NodePat::node(KindSpec::Exact(NodeKind::Return)))
}

/// Inputs `[ctrl(0), mem(1), target(2)]` plus the optional interworking ISA
/// mode at `3`, no outputs, so the pattern is rooted on the node itself.
/// [`any_input`](Self::any_input) reaches the mode input too.
pub struct IndirectBranchPat(NodePat);

impl IndirectBranchPat {
    /// `inputs[2]`.
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(2, p))
    }

    /// `inputs[3]`, the interworking ISA-mode bit. Present only on a
    /// mode-switching branch, so pinning it rejects every other one.
    pub fn isa_mode<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(3, p))
    }

    /// `inputs[0]`. The sub-pattern's root produces a control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }

    /// `inputs[1]`.
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(1, p))
    }
}

delegate_node_pat!(IndirectBranchPat, 0, [capture, build, input, any_input]);

impl MatchPat for IndirectBranchPat {
    /// Node-rooted: the synthesized `Any` output is what an alternation or an
    /// existential slot wires; no value slot can bind it.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_alt_arm(b)
    }
}

pub fn indirect_branch() -> IndirectBranchPat {
    IndirectBranchPat(NodePat::node(KindSpec::Exact(NodeKind::IndirectBranch)))
}

/// Inputs `[ctrl(0)]` plus the optional memory slot 1 an exit-free-cycle
/// sink carries; no outputs.
pub struct UnreachablePat(NodePat);

impl UnreachablePat {
    /// `inputs[0]`. The sub-pattern's root produces a control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }
}

delegate_node_pat!(UnreachablePat, 0, [capture, build, input, any_input]);

impl MatchPat for UnreachablePat {
    /// Node-rooted: the synthesized `Any` output is what an alternation or an
    /// existential slot wires; no value slot can bind it.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_alt_arm(b)
    }
}

pub fn unreachable() -> UnreachablePat {
    UnreachablePat(NodePat::node(KindSpec::Exact(NodeKind::Unreachable)))
}

/// The function's unique entry node: no inputs, one control output at slot 0.
/// Producing a control output means an `EntryPat` also nests as a control
/// operand, as in `region().input(0, entry())` or `.ctrl(entry())`.
pub struct EntryPat(NodePat);

impl EntryPat {}

impl MatchPat for EntryPat {
    /// The control output is the anchor, so nesting wires that edge into
    /// whatever control-consuming slot it is passed to.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_anchored(b)
    }
}

pub fn entry() -> EntryPat {
    EntryPat(NodePat::node(KindSpec::Exact(NodeKind::Entry)).with_control_value(0))
}

delegate_with_output!(EntryPat, 0);
delegate_node_pat!(EntryPat, 0, [capture, build]);

/// Joins control edges at a CFG merge: one variadic Control input per
/// predecessor at raw slots `0..N`, no fixed prefix. Anchored on the control
/// output at slot 0; the `PhiToken` output at slot 1 is reachable through
/// [`output`](Self::output).
pub struct RegionPat(NodePat);

impl RegionPat {
    /// Raw input slot `idx`. The sub-pattern must be control-rooted
    /// (`entry()`, `region()`) or an untyped wildcard (`var`, `anything`): a
    /// typed value sub can never bind a Control edge.
    pub fn input<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(idx, p))
    }
}

impl MatchPat for RegionPat {
    /// The control output is the anchor, so nesting wires that edge into
    /// whatever control-consuming slot it is passed to.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_anchored(b)
    }
}

pub fn region() -> RegionPat {
    RegionPat(NodePat::node(KindSpec::Exact(NodeKind::Region)).with_control_value(0))
}

delegate_with_output!(RegionPat, 0);
delegate_node_pat!(RegionPat, 0, [capture, build, any_input]);

/// Inputs `[ctrl(0), selector(1)]`, one control output per arm. No anchor
/// output, so the pattern is rooted on the node itself; `output` /
/// `any_output` reach the arm edges.
pub struct SwitchPat(NodePat);

impl SwitchPat {
    /// `inputs[1]`, the value the switch dispatches on. The arms' addresses
    /// are the control outputs, not this slot.
    pub fn selector<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(1, p))
    }

    /// `inputs[0]`. The sub-pattern's root produces a control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(0, p))
    }
}

impl MatchPat for SwitchPat {
    /// Node-rooted: the synthesized `Any` output is what an alternation or an
    /// existential slot wires; no value slot can bind it.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        self.0.compile_alt_arm(b)
    }
}

pub fn switch() -> SwitchPat {
    SwitchPat(NodePat::node(KindSpec::Exact(NodeKind::Switch)))
}

delegate_with_output!(SwitchPat, 0);
delegate_node_pat!(SwitchPat, 0, [capture, build, input, any_input]);

/// An `If` carries two control-output vertices, true at slot 0 and false at
/// slot 1.
///
/// `.with_true(q)` / `.with_false(r)` forward-walk from the matched If's
/// control output to its single consumer and match there. Both fail the match
/// when that output has zero or several consumers.
#[derive(Default)]
pub struct IfPat {
    cond: Option<crate::node_builders::SubCompiler>,
    ctrl: Option<crate::node_builders::SubCompiler>,
    /// Sparse raw input slots, `ANY_INPUT_SLOT` for the existentials.
    inputs: Vec<(usize, crate::node_builders::SubCompiler)>,
    outputs: Vec<(Option<usize>, IfOutput)>,
    true_branch: Option<BranchWalk>,
    false_branch: Option<BranchWalk>,
    captures: Vec<Capture>,
    capture_true: Option<Capture>,
    capture_false: Option<Capture>,
    /// Per branch slot, so replacing a branch drops the captures the
    /// discarded walk declared.
    branch_captures: [WalkCaptures; 2],
    /// An unmatchable branch pattern, replayed onto the builder at `lower`.
    branch_refusal: Option<String>,
}

/// The one aspect a single `.output(slot)` / `.any_output()` call commits.
enum IfOutput {
    Capture(Capture),
    Width(u32),
    Ty(ValueType),
}

impl IfPat {
    /// `inputs[1]`; `inputs[0]` is the ctrl predecessor.
    pub fn cond<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.cond = Some(crate::node_builders::SubCompiler::new(move |b| {
            p.compile(b)
        }));
        self
    }

    /// `inputs[0]`. The sub-pattern's root produces a control edge.
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.ctrl = Some(super::node_pat::control_compiler(p));
        self
    }

    /// Raw input slot `slot`, unshifted. Slot numbering is per node kind, laid
    /// out by the IR's `expected_signature`; the named accessors above are the
    /// intended surface and this is the escape hatch beneath them.
    pub fn input<P: MatchPat + 'static>(mut self, slot: usize, p: P) -> Self {
        self.inputs.push((
            slot,
            crate::node_builders::SubCompiler::new(move |b| p.compile(b)),
        ));
        self
    }

    /// See [`CallPat::any_input`]. An `If` has two inputs, the control
    /// predecessor and the condition.
    pub fn any_input<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inputs.push((
            crate::matcher::ANY_INPUT_SLOT,
            crate::node_builders::SubCompiler::new(move |b| p.compile(b)),
        ));
        self
    }

    /// Matches `pat` against the single consumer of control output slot 0.
    ///
    /// A branch consumer is matched node-wise, so this slot takes a finished
    /// [`Pattern`]: a control builder's `.build()`, or a value builder sealed
    /// via [`MatchPat::into_pattern`].
    ///
    /// # Captures
    ///
    /// `pat` matches against the enclosing match's live `Bindings`: what it
    /// binds reaches the outer `Match`, and a capture it shares with the
    /// condition or the other branch must bind the same thing in both or the
    /// match is rejected.
    ///
    /// # Every binding per branch
    ///
    /// `pat` enumerates: every commutative ordering and every existential slot
    /// it can bind produces its own outer match, exactly as it would standalone,
    /// and the two branches combine, so `N` true bindings against `M` false ones
    /// are `N * M` matches. Enumerating is what lets a binding rejected by the
    /// other branch or by a guard above fall through to the next one instead of
    /// losing the match.
    ///
    /// # Refusal
    ///
    /// A `pat` that is not a single-rooted acyclic graph the matcher can
    /// handle is refused at build time: every query on the resulting pattern
    /// errors, rather than reading as a silent "branch did not match".
    pub fn with_true(self, pat: Pattern) -> Self {
        self.with_branch(0, pat)
    }

    /// Control output slot 1. See [`with_true`](Self::with_true), which also
    /// documents the refusal and the branch-capture agreement rule.
    pub fn with_false(self, pat: Pattern) -> Self {
        self.with_branch(1, pat)
    }

    /// `slot` 0 is true, 1 is false.
    fn with_branch(mut self, slot: usize, pat: Pattern) -> Self {
        if let Err(e) = pat.root() {
            self.branch_refusal
                .get_or_insert(format!("If branch pattern is not matchable ({e})"));
        }
        // Both branches must match, so each one's guarantees carry over whole.
        self.branch_captures[slot] = WalkCaptures {
            bound: pat.bound_captures().collect(),
            guaranteed: pat
                .guaranteed_captures()
                .unwrap_or_default()
                .into_iter()
                .collect(),
        };
        let pat = BranchPattern(Some(pat));
        let walk = Box::new(
            move |m: &crate::Matcher,
                  if_node,
                  b: &mut crate::Bindings,
                  k: &mut dyn FnMut(&mut crate::Bindings) -> bool| {
                match_branch_consumer(m, if_node, slot, pat.get(), b, k)
            },
        );
        if slot == 0 {
            self.true_branch = Some(walk);
        } else {
            self.false_branch = Some(walk);
        }
        self
    }

    /// Repeatable: every capture binds the matched `If` node.
    pub fn capture(mut self, c: Capture) -> Self {
        self.captures.push(c);
        self
    }

    /// Binds control output slot 0, propagated into the outer `Match`. Unlike
    /// the successor `Region`, this value survives single-input-region
    /// collapse, so it is the stable handle for the edge join constraints.
    pub fn capture_true(mut self, c: Capture) -> Self {
        self.capture_true = Some(c);
        self
    }

    /// See [`capture_true`](Self::capture_true).
    pub fn capture_false(mut self, c: Capture) -> Self {
        self.capture_false = Some(c);
        self
    }

    pub fn build(self) -> Pattern {
        let mut b = MatcherBuilder::new();
        self.lower(&mut b);
        b.finish()
    }

    fn lower(self, b: &mut MatcherBuilder) -> PatNodeRef {
        let IfPat {
            cond,
            ctrl,
            inputs,
            outputs,
            true_branch,
            false_branch,
            captures,
            capture_true,
            capture_false,
            branch_captures,
            branch_refusal,
        } = self;
        if let Some(why) = branch_refusal {
            b.reject(why);
        }
        let node = b.node(KindSpec::Exact(NodeKind::If));
        let true_out = b.control_output(node, 0);
        let false_out = b.control_output(node, 1);
        if let Some(c) = capture_true {
            b.capture_output(true_out, c);
        }
        if let Some(c) = capture_false {
            b.capture_output(false_out, c);
        }

        if let Some(cond) = cond {
            let c = cond.call(&mut *b);
            b.input(node, 1, c);
        }
        if let Some(ctrl) = ctrl {
            let c = ctrl.call(&mut *b);
            b.input(node, 0, c);
        }
        for (slot, compile) in inputs {
            let o = compile.call(&mut *b);
            b.input(node, slot, o);
        }
        for (slot, aspect) in outputs {
            let out = match slot {
                Some(slot) => b.value_output(node, slot),
                None => b.any_slot_value_output(node),
            };
            match aspect {
                IfOutput::Capture(c) => {
                    b.set_output_any(out);
                    b.capture_output(out, c);
                }
                IfOutput::Width(bits) => {
                    b.set_output_any(out);
                    b.set_value_width(out, bits);
                }
                IfOutput::Ty(ty) => b.set_value_ty(out, ty),
            }
        }
        // The forward-walks bind into the enclosing match, so they ride the
        // binding walk rather than the node predicate, which runs before the
        // condition's own captures exist.
        if true_branch.is_some() || false_branch.is_some() {
            b.set_binding_walk(
                true_out,
                // Nested, not sequential: the false walk runs inside each true
                // configuration, so a false rejection re-drives the true branch.
                Box::new(
                    move |m, if_node, bnd, k| match (&true_branch, &false_branch) {
                        (Some(tb), Some(fb)) => tb(m, if_node, bnd, &mut |b| fb(m, if_node, b, k)),
                        (Some(w), None) | (None, Some(w)) => w(m, if_node, bnd, k),
                        (None, None) => k(bnd),
                    },
                ),
                merged_branch_captures(branch_captures),
            );
        }
        for c in captures {
            b.capture_node(node, c);
        }
        node
    }
}

/// One declaration covering both branch walks, which ride a single
/// `binding_walk`.
fn merged_branch_captures([t, f]: [WalkCaptures; 2]) -> WalkCaptures {
    WalkCaptures {
        bound: [t.bound, f.bound].concat(),
        guaranteed: [t.guaranteed, f.guaranteed].concat(),
    }
}

impl WithOutput for IfPat {
    fn capture_output(mut self, slot: Option<usize>, c: Capture) -> Self {
        self.outputs.push((slot, IfOutput::Capture(c)));
        self
    }
    fn output_width(mut self, slot: Option<usize>, bits: u32) -> Self {
        self.outputs.push((slot, IfOutput::Width(bits)));
        self
    }
    fn output_ty(mut self, slot: Option<usize>, ty: ValueType) -> Self {
        self.outputs.push((slot, IfOutput::Ty(ty)));
        self
    }
}

impl MatchPat for IfPat {
    /// Node-rooted: the two control outputs stay sibling vertices and an `Any`
    /// output is synthesized for an alternation or existential slot to wire.
    fn compile(self, b: &mut MatcherBuilder) -> PatValueRef {
        let node = self.lower(b);
        b.any_value_output(node)
    }
}

/// `false` when the output has zero or several consumers, or when no
/// configuration of `pat` against `bindings` is accepted.
///
/// The consumer may be value-producing, such as a `Region`, or a zero-output
/// kind such as `Return`; the match dispatches through both shapes.
///
/// [`crate::Matcher`]'s node-rooted entry enumerates, so `k` runs once per way
/// `pat` binds; see [`IfPat::with_true`].
fn match_branch_consumer(
    matcher: &crate::Matcher,
    if_node: NodeId,
    output_index: usize,
    pat: &Pattern,
    bindings: &mut crate::Bindings,
    k: &mut dyn FnMut(&mut crate::Bindings) -> bool,
) -> bool {
    let f = matcher.function();
    let outputs = f.node_outputs(if_node);
    let Some(&out) = outputs.get(output_index) else {
        return false;
    };
    let Ok((first, _)) = f.value_uses(out).exactly_one() else {
        return false;
    };
    // `with_branch` refused an unmatchable `pat`, so this walk is unreachable
    // for one; an `Err` here is a real bug and is surfaced rather than
    // swallowed.
    match matcher.match_at_into(first, pat, bindings, k) {
        Ok(hit) => hit,
        Err(e) => unreachable!("refused branch pattern reached the matcher: {e}"),
    }
}

pub fn if_else() -> IfPat {
    IfPat::default()
}

#[cfg(test)]
mod tests {
    use super::if_else;

    #[test]
    fn if_pattern_has_two_control_output_vertices() {
        let pat = if_else().build();
        assert_eq!(
            pat.control_output_count(),
            2,
            "If pattern must declare two control-output vertices"
        );
    }
}
