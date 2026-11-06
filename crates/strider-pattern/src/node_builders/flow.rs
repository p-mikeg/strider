//! Control-flow node patterns. Slots, per `expected_signature`:
//!
//! * `Call` in `[ctrl, mem, target, sp, args...]`, out `[Control, Memory, ...]`.
//! * `CallOther` in `[ctrl, mem, args...]`, out `[Control, Memory, ...]`.
//! * `Return` in `[ctrl, mem, retvals...]`, no outputs.
//! * `If` in `[ctrl, cond]`, out `[Control true, Control false]`.

use super::{delegate_node_pat, node_match_pat};
use strider_ir::node::{NodeKind, SwitchTableId, ValueType};

use crate::capture::Capture;
use crate::matcher::KindSpec;
use crate::matcher::match_pat::MatchPat;
use crate::typed::int_const;

use super::MemPat;
use crate::expr::{Node, OutputAspect};

/// A `Call` clobbers caller-saved registers and the memory token.
pub struct CallPat(Node);

impl CallPat {
    /// The target, input [`NodeKind::CALL_TARGET_SLOT`].
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::CALL_TARGET_SLOT, p))
    }

    /// [`int_const`]`(addr)` on the target, truncated to the target's width.
    pub fn at(self, addr: u64) -> Self {
        self.target(int_const(u128::from(addr)))
    }

    /// Argument `idx`: raw input slot `idx + 4`.
    pub fn arg<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(
            self.0
                .input(NodeKind::Call { cc: None }.input_head_len() + idx, p),
        )
    }

    /// Input [`NodeKind::CTRL_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(NodeKind::CTRL_MEM_SLOT, p))
    }

    /// As a value or raw-slot operand, matches only the result output (raw slot 2), not a
    /// clobber. No effect on a root, a control predecessor or a memory producer.
    pub fn res(self) -> Self {
        Self(self.0.pin_anchor_slot())
    }
}

/// A multi-output node builder that takes output constraints.
pub trait WithOutput: Sized {
    fn with_output(self, slot: Option<usize>, aspect: OutputAspect) -> Self;

    /// The output at raw slot `slot`.
    fn output(self, slot: usize) -> OutputPat<Self> {
        OutputPat::at(self, Some(slot))
    }

    /// Any output; each qualifying output is its own `find_all` match.
    fn any_output(self) -> OutputPat<Self> {
        OutputPat::at(self, None)
    }
}

/// One output constraint (capture, width or type), returning the node builder.
/// `slot` is `None` for `.any_output()`.
pub struct OutputPat<B: WithOutput> {
    parent: B,
    slot: Option<usize>,
}

impl<B: WithOutput> OutputPat<B> {
    pub(crate) fn at(parent: B, slot: Option<usize>) -> Self {
        Self { parent, slot }
    }

    pub fn capture(self, c: Capture) -> B {
        self.parent.with_output(self.slot, OutputAspect::Capture(c))
    }

    pub fn of_width(self, bits: u32) -> B {
        self.parent
            .with_output(self.slot, OutputAspect::Width(bits))
    }

    pub fn of_type(self, ty: ValueType) -> B {
        self.parent.with_output(self.slot, OutputAspect::Ty(ty))
    }
}

delegate_node_pat! { CallPat, 0, [capture, build, input, any_input, ctrl], with_output }

impl MemPat for CallPat {}

/// A `Call`. As a value operand any value output matches (any output in a raw
/// slot); [`CallPat::res`] narrows it to the result.
pub fn call() -> CallPat {
    CallPat(
        Node::new(KindSpec::variant_of(&NodeKind::Call { cc: None }))
            .with_mem_value(NodeKind::CTRL_MEM_OUT_SLOT)
            .with_value_anchor(NodeKind::Call { cc: None }.output_head_len()),
    )
}

/// A Sleigh `CALLOTHER` user-op.
pub struct CallOtherPat(Node);

impl CallOtherPat {
    pub fn user_op_id(self, v: u64) -> Self {
        Self(
            self.0
                .with_kind(KindSpec::Exact(NodeKind::CallOther { user_op_id: v })),
        )
    }

    /// Filters on `Function::call_other_name`; repeated, every name must hold.
    pub fn name(self, name: impl Into<String>) -> Self {
        let want = name.into();
        Self(self.0.with_node_predicate(Box::new(move |matcher, n| {
            matcher.function().call_other_name(n) == Some(want.as_str())
        })))
    }

    /// Raw input slot `idx`, unshifted.
    pub fn arg<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.raw_input(idx, p))
    }

    /// Input [`NodeKind::CTRL_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(NodeKind::CTRL_MEM_SLOT, p))
    }

    /// See [`CallPat::res`].
    pub fn res(self) -> Self {
        Self(self.0.pin_anchor_slot())
    }
}

delegate_node_pat! { CallOtherPat, 0, [capture, build, input, any_input, ctrl], with_output }

impl MemPat for CallOtherPat {}

pub fn call_other() -> CallOtherPat {
    let kind = KindSpec::variant_of(&NodeKind::CallOther { user_op_id: 0 });
    CallOtherPat(
        Node::new(kind)
            .with_mem_value(NodeKind::CTRL_MEM_OUT_SLOT)
            .with_value_anchor(NodeKind::CallOther { user_op_id: 0 }.output_head_len()),
    )
}

/// A `Return`. It has no outputs, so a capture binds the node.
pub struct RetPat(Node);

impl RetPat {
    /// Return value `idx`: raw input slot `idx + 2`.
    pub fn ret_val<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.input(NodeKind::Return.input_head_len() + idx, p))
    }
}

delegate_node_pat! { RetPat, 0, [capture, build, input, any_input, ctrl] }

pub fn ret() -> RetPat {
    RetPat(Node::new(KindSpec::Exact(NodeKind::Return)))
}

/// An `IndirectBranch`: inputs `[ctrl, mem, target]`, plus an ISA mode on a
/// mode-switching branch. No outputs.
pub struct IndirectBranchPat(Node);

impl IndirectBranchPat {
    /// Input [`NodeKind::INDIRECT_BRANCH_TARGET_SLOT`].
    pub fn target<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::INDIRECT_BRANCH_TARGET_SLOT, p))
    }

    /// Input [`NodeKind::CTRL_MEM_SLOT`].
    pub fn mem<M: MemPat + 'static>(self, p: M) -> Self {
        Self(self.0.input_mem(NodeKind::CTRL_MEM_SLOT, p))
    }
}

delegate_node_pat! { IndirectBranchPat, 0, [capture, build, input, any_input, ctrl] }

pub fn indirect_branch() -> IndirectBranchPat {
    IndirectBranchPat(Node::new(KindSpec::Exact(NodeKind::IndirectBranch)))
}

/// An `Unreachable`: inputs `[ctrl]` plus an optional memory token; no outputs.
pub struct UnreachablePat(Node);

delegate_node_pat! { UnreachablePat, 0, [capture, build, input, any_input, ctrl] }

pub fn unreachable() -> UnreachablePat {
    UnreachablePat(Node::new(KindSpec::Exact(NodeKind::Unreachable)))
}

/// The function's entry node: no inputs, one control output, so it nests as a
/// control operand (`.ctrl(entry())`).
pub struct EntryPat(Node);

pub fn entry() -> EntryPat {
    EntryPat(
        Node::new(KindSpec::Exact(NodeKind::Entry)).with_control_value(NodeKind::CTRL_OUT_SLOT),
    )
}

delegate_node_pat! { EntryPat, 0, [capture, build], with_output }

/// A `Region`: one control input per predecessor at slots `0..N`. Anchored on
/// the control output at slot 0; the `PhiToken` at slot 1 is
/// [`output`](Self::output)`(1)`.
pub struct RegionPat(Node);

impl RegionPat {
    /// Raw input slot `idx`. A typed value sub-pattern never matches there.
    pub fn input<P: MatchPat + 'static>(self, idx: usize, p: P) -> Self {
        Self(self.0.raw_input(idx, p))
    }
}

pub fn region() -> RegionPat {
    RegionPat(
        Node::new(KindSpec::Exact(NodeKind::Region)).with_control_value(NodeKind::CTRL_OUT_SLOT),
    )
}

delegate_node_pat! { RegionPat, 0, [capture, build, any_input], with_output }

/// A `Switch`: inputs `[ctrl, selector]`, one control output per arm.
pub struct SwitchPat(Node);

impl SwitchPat {
    /// Input [`NodeKind::SWITCH_SELECTOR_SLOT`].
    pub fn selector<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::SWITCH_SELECTOR_SLOT, p))
    }
}

pub fn switch() -> SwitchPat {
    SwitchPat(Node::new(KindSpec::variant_of(&NodeKind::Switch(
        SwitchTableId::from_u32(0),
    ))))
}

delegate_node_pat! { SwitchPat, 0, [capture, build, input, any_input, ctrl], with_output }

/// An `If`: control outputs true at slot 0, false at slot 1.
pub struct IfPat(Node);

impl IfPat {
    /// Input [`NodeKind::IF_COND_SLOT`].
    pub fn cond<P: MatchPat + 'static>(self, p: P) -> Self {
        Self(self.0.input(NodeKind::IF_COND_SLOT, p))
    }

    /// Binds the true control output. Unlike the successor `Region`, it
    /// survives single-input-region collapse, so join constraints use it.
    pub fn capture_true(self, c: Capture) -> Self {
        Self(self.0.capture_control_output(NodeKind::IF_TRUE_OUT_SLOT, c))
    }

    /// Binds the false control output; see [`capture_true`](Self::capture_true).
    pub fn capture_false(self, c: Capture) -> Self {
        Self(
            self.0
                .capture_control_output(NodeKind::IF_FALSE_OUT_SLOT, c),
        )
    }
}

delegate_node_pat! { IfPat, 0, [capture, build, input, any_input, ctrl], with_output }

pub fn if_else() -> IfPat {
    IfPat(
        Node::new(KindSpec::Exact(NodeKind::If))
            .with_control_output(NodeKind::IF_TRUE_OUT_SLOT)
            .with_control_output(NodeKind::IF_FALSE_OUT_SLOT),
    )
}

node_match_pat! {
    CallPat => |p| p.0,
    CallOtherPat => |p| p.0,
    RetPat => |p| p.0,
    IndirectBranchPat => |p| p.0,
    UnreachablePat => |p| p.0,
    EntryPat => |p| p.0,
    RegionPat => |p| p.0,
    SwitchPat => |p| p.0,
    IfPat => |p| p.0,
}

#[cfg(test)]
mod tests {
    use super::{entry, if_else};
    use crate::capture::Capture;
    use crate::node_builders::{WithOutput, pattern_shape};
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

    /// Pins `IfPat`'s lowering: operand cones first, then the two control
    /// outputs, then the output constraints.
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
            .output(0)
            .capture(out)
            .any_output()
            .of_width(32)
            .output(1)
            .of_type(ValueType::I1)
            .build();
        same_shape(
            &pattern_shape(&pat),
            "\
n0 Variant caps=0 pred=1 Operands { ordered: false }
  out v0 AnyValue slot=0 mode=Loose width=None caps=0 ident=0 token=0
n1 Exact(Entry) caps=0 pred=0 Operands { ordered: false }
  out v1 Control slot=0 mode=Loose width=None caps=0 ident=0 token=0
n2 Exact(If) caps=1 pred=0 Operands { ordered: false }
  out v2 Control slot=0 mode=Loose width=None caps=1 ident=0 token=0
  out v3 Control slot=1 mode=Loose width=None caps=1 ident=0 token=0
  out v4 Any slot=0 mode=Pinned width=None caps=1 ident=0 token=0
  out v5 Any slot=0 mode=Enumerated width=Some(32) caps=0 ident=0 token=0
  out v6 Value slot=1 mode=Pinned width=None caps=0 ident=0 token=0
  in At(1) <- v0
  in At(0) <- v1
root=Ok(2) total_nodes=7
",
        );
    }

    /// Every operand cone is built ahead of the `If` that consumes it, in the
    /// order `lower` compiles them, so that order decides every vertex id.
    #[test]
    fn if_lowering_builds_every_operand_cone_before_the_if_node() {
        let pat = if_else()
            .cond(var(Capture::new()))
            .input(0, anything())
            .any_input(anything())
            .build();
        same_shape(
            &pattern_shape(&pat),
            "\
n0 Any caps=0 pred=0 Operands { ordered: false }
  out v0 Any slot=0 mode=Loose width=None caps=1 ident=0 token=0
n1 Any caps=0 pred=0 Operands { ordered: false }
  out v1 Any slot=0 mode=Loose width=None caps=0 ident=0 token=0
n2 Any caps=0 pred=0 Operands { ordered: false }
  out v2 Any slot=0 mode=Loose width=None caps=0 ident=0 token=0
n3 Exact(If) caps=0 pred=0 Operands { ordered: false }
  out v3 Control slot=0 mode=Loose width=None caps=0 ident=0 token=0
  out v4 Control slot=1 mode=Loose width=None caps=0 ident=0 token=0
  in At(1) <- v0
  in At(0) <- v1
  in Any <- v2
root=Ok(3) total_nodes=5
",
        );
    }

    /// Line-by-line diff, readable unlike `assert_eq!` on two long strings.
    #[track_caller]
    fn same_shape(got: &str, want: &str) {
        assert_eq!(
            got.lines().collect::<Vec<_>>(),
            want.lines().collect::<Vec<_>>()
        );
    }
}
