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

use super::{delegate_node_pat, node_match_pat};
use crate::node_builders::delegate_with_output;
use strider_ir::node::{NodeKind, SwitchTableId, ValueType};

use crate::capture::Capture;
use crate::matcher::match_pat::MatchPat;
use crate::matcher::{KindSpec, Pattern};
use crate::typed::int_const;

use super::MemPat;
use crate::expr::Node;

/// A `Call` clobbers caller-saved registers and the memory token.
pub struct CallPat(Node);

impl CallPat {
    /// The target, input [`NodeKind::CALL_TARGET_SLOT`].
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::CALL_TARGET_SLOT, p))
    }

    /// [`int_const`]`(addr)` on the target, so `addr` is truncated to the
    /// target's width like any `int_const` query.
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(u128::from(addr)))
    }

    /// 0-based past `ctrl` / `mem` / `target` / `sp`, so raw input slot
    /// `idx + 4`.
    pub fn arg<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(
            self.0
                .input(NodeKind::Call { cc: None }.input_head_len() + idx, p),
        )
    }

    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge, not a value.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(NodeKind::CTRL_SLOT, p))
    }

    /// Input [`NodeKind::CTRL_MEM_SLOT`], taking a `store` / `mem_phi` / prior
    /// `call`.
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(NodeKind::CTRL_MEM_SLOT, p))
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

impl MemPat for CallPat {}

/// Nests as a value operand through the first value output. Loose: any
/// value output matches. `.res()` tightens it.
pub fn call() -> CallPat {
    CallPat(
        Node::new(KindSpec::variant_of(&NodeKind::Call { cc: None }))
            .with_mem_value(NodeKind::CTRL_MEM_OUT_SLOT)
            .with_value_anchor(NodeKind::Call { cc: None }.output_head_len()),
    )
}

/// A `CallOther` is a Sleigh `CALLOTHER` user-op: an opaque
/// architecture-specific instruction modelled outside the pcode core.
pub struct CallOtherPat {
    inner: Node,
    name_filter: Option<String>,
}

impl CallOtherPat {
    pub fn user_op_id(mut self, v: u64) -> Self {
        self.inner = self
            .inner
            .with_kind(KindSpec::Exact(NodeKind::CallOther { user_op_id: v }));
        self
    }

    /// Filters on `Function::call_other_name`.
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name_filter = Some(name.into());
        self
    }

    /// The raw input slot, unshifted: control, memory and args are addressed
    /// uniformly.
    pub fn arg<P: MatchPat + 'static>(mut self, idx: usize, p: P) -> Self {
        self.inner = self.inner.raw_input(idx, p);
        self
    }

    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge, not a value.
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_control(NodeKind::CTRL_SLOT, p);
        self
    }

    /// Input [`NodeKind::CTRL_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(mut self, p: M) -> Self {
        self.inner = self.inner.input_mem(NodeKind::CTRL_MEM_SLOT, p);
        self
    }

    /// See [`CallPat::res`]. Here it excludes implicit-write clobber outputs.
    pub fn res(mut self) -> Self {
        self.inner = self.inner.pin_anchor_slot();
        self
    }

    /// Lowers any `.name` filter to a node predicate.
    fn configured(self) -> Node {
        let CallOtherPat { inner, name_filter } = self;
        match name_filter {
            // Node-only, so it short-circuits before child recursion.
            Some(want) => inner.with_node_predicate(Box::new(move |matcher, n| {
                matcher.function().call_other_name(n) == Some(want.as_str())
            })),
            None => inner,
        }
    }

    pub fn build(self) -> Pattern {
        self.configured().build()
    }
}

delegate_with_output!(CallOtherPat, inner);
delegate_node_pat!(CallOtherPat, inner, [capture, input, any_input]);

impl MemPat for CallOtherPat {}

pub fn call_other() -> CallOtherPat {
    let kind = KindSpec::variant_of(&NodeKind::CallOther { user_op_id: 0 });
    CallOtherPat {
        inner: Node::new(kind)
            .with_mem_value(NodeKind::CTRL_MEM_OUT_SLOT)
            .with_value_anchor(NodeKind::CallOther { user_op_id: 0 }.output_head_len()),
        name_filter: None,
    }
}

/// A `Return` has no outputs, so the pattern is rooted on the node itself and
/// a capture binds the node.
pub struct RetPat(Node);

impl RetPat {
    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(NodeKind::CTRL_SLOT, p))
    }

    /// 0-based past ctrl and mem, so raw input slot `idx + 2`.
    pub fn ret_val<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(NodeKind::Return.input_head_len() + idx, p))
    }
}

delegate_node_pat!(RetPat, 0, [capture, build, input, any_input]);

pub fn ret() -> RetPat {
    RetPat(Node::new(KindSpec::Exact(NodeKind::Return)))
}

/// Inputs `[ctrl, mem, target]` plus the optional interworking ISA mode after
/// them, reached through [`input`](Self::input) and present only on a
/// mode-switching branch. No outputs, so the pattern is rooted on the node
/// itself; [`any_input`](Self::any_input) reaches the mode input too.
pub struct IndirectBranchPat(Node);

impl IndirectBranchPat {
    /// Input [`NodeKind::INDIRECT_BRANCH_TARGET_SLOT`].
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::INDIRECT_BRANCH_TARGET_SLOT, p))
    }

    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(NodeKind::CTRL_SLOT, p))
    }

    /// Input [`NodeKind::CTRL_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(NodeKind::CTRL_MEM_SLOT, p))
    }
}

delegate_node_pat!(IndirectBranchPat, 0, [capture, build, input, any_input]);

pub fn indirect_branch() -> IndirectBranchPat {
    IndirectBranchPat(Node::new(KindSpec::Exact(NodeKind::IndirectBranch)))
}

/// Inputs `[ctrl]` plus the optional memory token an exit-free-cycle sink
/// carries; no outputs.
pub struct UnreachablePat(Node);

impl UnreachablePat {
    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(NodeKind::CTRL_SLOT, p))
    }
}

delegate_node_pat!(UnreachablePat, 0, [capture, build, input, any_input]);

pub fn unreachable() -> UnreachablePat {
    UnreachablePat(Node::new(KindSpec::Exact(NodeKind::Unreachable)))
}

/// The function's unique entry node: no inputs, one control output at slot 0.
/// Producing a control output means an `EntryPat` also nests as a control
/// operand, as in `region().input(0, entry())` or `.ctrl(entry())`.
pub struct EntryPat(Node);

pub fn entry() -> EntryPat {
    EntryPat(
        Node::new(KindSpec::Exact(NodeKind::Entry)).with_control_value(NodeKind::CTRL_OUT_SLOT),
    )
}

delegate_with_output!(EntryPat, 0);
delegate_node_pat!(EntryPat, 0, [capture, build]);

/// Joins control edges at a CFG merge: one variadic Control input per
/// predecessor at raw slots `0..N`, no fixed prefix. Anchored on the control
/// output at slot 0; the `PhiToken` output at slot 1 is reachable through
/// [`output`](Self::output).
pub struct RegionPat(Node);

impl RegionPat {
    /// Raw input slot `idx`. The sub-pattern must be control-rooted
    /// (`entry()`, `region()`) or an untyped wildcard (`var`, `anything`): a
    /// typed value sub can never bind a Control edge.
    pub fn input<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.raw_input(idx, p))
    }
}

pub fn region() -> RegionPat {
    RegionPat(
        Node::new(KindSpec::Exact(NodeKind::Region)).with_control_value(NodeKind::CTRL_OUT_SLOT),
    )
}

delegate_with_output!(RegionPat, 0);
delegate_node_pat!(RegionPat, 0, [capture, build, any_input]);

/// Inputs `[ctrl, selector]`, one control output per arm. No anchor
/// output, so the pattern is rooted on the node itself; `output` /
/// `any_output` reach the arm edges.
pub struct SwitchPat(Node);

impl SwitchPat {
    /// Input [`NodeKind::SWITCH_SELECTOR_SLOT`], the value the switch
    /// dispatches on. The arms' addresses are the control outputs, not this
    /// slot.
    pub fn selector<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::SWITCH_SELECTOR_SLOT, p))
    }

    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge.
    pub fn ctrl<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input_control(NodeKind::CTRL_SLOT, p))
    }
}

pub fn switch() -> SwitchPat {
    SwitchPat(Node::new(KindSpec::variant_of(&NodeKind::Switch(
        SwitchTableId::from_u32(0),
    ))))
}

delegate_with_output!(SwitchPat, 0);
delegate_node_pat!(SwitchPat, 0, [capture, build, input, any_input]);

/// An `If` carries two control-output vertices, true at slot 0 and false at
/// slot 1.
pub struct IfPat {
    inner: Node,
    capture_true: Option<Capture>,
    capture_false: Option<Capture>,
}

impl Default for IfPat {
    fn default() -> Self {
        Self {
            inner: Node::new(KindSpec::Exact(NodeKind::If)),
            capture_true: None,
            capture_false: None,
        }
    }
}

impl IfPat {
    /// Input [`NodeKind::IF_COND_SLOT`]; [`NodeKind::CTRL_SLOT`] is the ctrl
    /// predecessor.
    pub fn cond<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input(NodeKind::IF_COND_SLOT, p);
        self
    }

    /// Input [`NodeKind::CTRL_SLOT`]. The sub-pattern's root produces a
    /// control edge.
    pub fn ctrl<P: MatchPat + 'static>(mut self, p: P) -> Self {
        self.inner = self.inner.input_control(NodeKind::CTRL_SLOT, p);
        self
    }

    /// Binds control output slot 0, propagated into the outer `Match`. Unlike
    /// the successor `Region`, this value survives single-input-region
    /// collapse, so it is the stable handle for the edge join constraints,
    /// and for joining the arm's consumer: the same capture on that
    /// consumer's control input.
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
        self.lower().build()
    }

    fn lower(self) -> Node {
        let IfPat {
            inner,
            capture_true,
            capture_false,
        } = self;
        inner
            .with_control_output(NodeKind::IF_TRUE_OUT_SLOT, capture_true)
            .with_control_output(NodeKind::IF_FALSE_OUT_SLOT, capture_false)
    }
}

delegate_with_output!(IfPat, inner);
delegate_node_pat!(IfPat, inner, [capture, input, any_input]);

pub fn if_else() -> IfPat {
    IfPat::default()
}

node_match_pat! {
    CallPat => |p| p.0,
    CallOtherPat => |p| p.configured(),
    RetPat => |p| p.0,
    IndirectBranchPat => |p| p.0,
    UnreachablePat => |p| p.0,
    EntryPat => |p| p.0,
    RegionPat => |p| p.0,
    SwitchPat => |p| p.0,
    IfPat => |p| p.lower(),
}

#[cfg(test)]
mod tests {
    use super::{entry, if_else};
    use crate::capture::Capture;
    use crate::node_builders::{WithOutput, staged_shape};
    use crate::typed::{anything, int_const, var};
    use strider_ir::node::ValueType;

    #[test]
    fn if_pattern_has_two_control_output_vertices() {
        let pat = if_else().build();
        assert_eq!(
            pat.control_output_count(),
            2,
            "If pattern must declare two control-output vertices"
        );
    }

    /// Characterizes `IfPat`'s lowering: which vertices it declares, in which
    /// order, carrying which captures. The two control outputs come before the
    /// condition's own cone, and the node captures land last. Reordering any of that changes which vertex
    /// `root_output_vertex_for` picks, so this string is the contract a
    /// re-homed lowering has to reproduce.
    #[test]
    fn if_lowering_declares_its_vertices_in_a_pinned_order() {
        let (t, f, n, out) = (
            Capture::new(),
            Capture::new(),
            Capture::new(),
            Capture::new(),
        );
        let pat = if_else()
            .cond(int_const(1u128))
            .ctrl(entry())
            .capture_true(t)
            .capture_false(f)
            .capture(n)
            .capture_output(Some(0), out)
            .output_width(None, 32)
            .output_ty(Some(1), ValueType::I1)
            .build();
        same_shape(
            &staged_shape(&pat),
            "\
n0 Exact(Entry) caps=0 pred=0 Operands { ordered: false }
  out v0 Control slot=0 mode=Loose width=None caps=0 ident=0 token=0
n1 Variant caps=0 pred=1 Operands { ordered: false }
  out v1 AnyValue slot=0 mode=Loose width=None caps=0 ident=0 token=0
n2 Exact(If) caps=1 pred=0 Operands { ordered: false }
  out v2 Control slot=0 mode=Loose width=None caps=1 ident=0 token=0
  out v3 Control slot=1 mode=Loose width=None caps=1 ident=0 token=0
  out v4 Any slot=0 mode=Pinned width=None caps=1 ident=0 token=0
  out v5 Any slot=0 mode=Enumerated width=Some(32) caps=0 ident=0 token=0
  out v6 Value slot=1 mode=Pinned width=None caps=0 ident=0 token=0
  in At(1) <- v1
  in At(0) <- v0
root=Ok(2) total_nodes=3
",
        );
    }

    /// Seal numbers every operand cone ahead of the `If` that consumes it, in
    /// the reverse of the order `lower` wired them, so which operand `lower`
    /// compiles first decides every vertex id. Pinned because those ids are
    /// what `anchor_repeats` compares.
    #[test]
    fn if_lowering_stages_every_operand_cone_before_the_if_node() {
        let pat = if_else()
            .cond(var(Capture::new()))
            .input(0, anything())
            .any_input(anything())
            .build();
        same_shape(
            &staged_shape(&pat),
            "\
n0 Any caps=0 pred=0 Operands { ordered: false }
  out v0 Any slot=0 mode=Loose width=None caps=0 ident=0 token=0
n1 Any caps=0 pred=0 Operands { ordered: false }
  out v1 Any slot=0 mode=Loose width=None caps=0 ident=0 token=0
n2 Any caps=0 pred=0 Operands { ordered: false }
  out v2 Any slot=0 mode=Loose width=None caps=1 ident=0 token=0
n3 Exact(If) caps=0 pred=0 Operands { ordered: false }
  out v3 Control slot=0 mode=Loose width=None caps=0 ident=0 token=0
  out v4 Control slot=1 mode=Loose width=None caps=0 ident=0 token=0
  in At(1) <- v2
  in At(0) <- v1
  in Any <- v0
root=Ok(3) total_nodes=4
",
        );
    }

    /// Diffs line by line: `assert_eq!` on two long strings escapes them into
    /// one unreadable line.
    #[track_caller]
    fn same_shape(got: &str, want: &str) {
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            want.lines().collect::<Vec<_>>()
        );
    }
}
