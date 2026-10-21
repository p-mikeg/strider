use cranelift_entity::PrimaryMap;
use cranelift_entity::packed_option::ReservedValue;

use crate::error::Result;
use crate::function::Function;
use crate::node::{IntBinaryOp, NodeId, NodeKind, ValueId, ValueKind};
use crate::region::Region;

mod build_trait;
pub use build_trait::IRBuilder;
mod builder_ext;
pub use builder_ext::IRBuilderExt;
mod call;
pub(crate) mod checks;
mod nodes;
#[cfg(any(test, feature = "test-util"))]
mod test_support;
#[cfg(test)]
mod tests;
mod vars;

/// `VnSpace` renders as its shortcut character alone, and ram's is `r`, which
/// reads as "register" in a message rejecting one.
fn space_name(space: rsleigh::VnSpace) -> &'static str {
    match space {
        rsleigh::VnSpace::REGISTER => "register",
        rsleigh::VnSpace::UNIQUE => "unique",
        rsleigh::VnSpace::RAM => "ram",
        rsleigh::VnSpace::CONST => "const",
        _ => "an unnamed",
    }
}

/// Errors unless `vn` is in REGISTER or UNIQUE space.
pub(super) fn require_reg_or_unique(vn: &rsleigh::Vn) -> crate::error::Result<()> {
    match vn.addr_space {
        rsleigh::VnSpace::REGISTER | rsleigh::VnSpace::UNIQUE => Ok(()),
        space => Err(anyhow::anyhow!(
            "a call-class read or write names a {} varnode at {:#x} size {}; \
             only register and unique varnodes can carry one",
            space_name(space),
            vn.addr_off,
            vn.size,
        )),
    }
}

/// Tracks SSA-style per-region variable state: each variable has exactly one
/// current `ValueId` inside the active region, and every read and write goes
/// through that mapping.
pub struct FunctionBuilder {
    pub(crate) function: Function,
    /// The single `Memory` output of the `InitialMemory` node.
    pub(crate) entry_memory: ValueId,
    pub(crate) regions: PrimaryMap<crate::region::RegionId, Region>,
    pub(crate) cur_region: Option<crate::region::RegionId>,
    /// Stamped onto every node `create_node` produces while it is `Some`.
    lift_addr: Option<u64>,
    /// What each `Call`'s clobbered registers held when it was built, at the
    /// stack pointer's width.
    call_register_values: Vec<ValueId>,
    /// Registers no callee reads, left out of `call_register_values`.
    internal_registers: Vec<rsleigh::Vn>,
}

impl FunctionBuilder {
    pub fn function(&self) -> &Function {
        &self.function
    }

    pub fn function_mut(&mut self) -> &mut Function {
        &mut self.function
    }

    pub fn entry(&self) -> NodeId {
        self.function.entry()
    }

    /// The sole constructor. `all_used_variables` is every varnode appearing
    /// in the function; a bare `Endianness` is the order of every space.
    pub fn new(
        mut all_used_variables: Vec<rsleigh::Vn>,
        cc: strider_target::BuiltCallingConvention,
        byte_order: impl Into<strider_target::ByteOrder>,
    ) -> Result<Self> {
        // Callers pass the stack vn in `all_used_variables` themselves.
        for v in cc
            .ret_val_regs
            .iter()
            .chain(cc.ret_val_regs_float.iter())
            .chain(cc.arg_passing_regs.iter())
            .chain(cc.arg_passing_regs_float.iter())
        {
            if !all_used_variables.contains(v) {
                all_used_variables.push(*v);
            }
        }
        let mut fb = FunctionBuilder {
            function: Function::new(cc, byte_order, all_used_variables),
            entry_memory: ValueId::reserved_value(),
            regions: PrimaryMap::new(),
            cur_region: None,
            lift_addr: None,
            call_register_values: Vec::new(),
            internal_registers: Vec::new(),
        };
        fb.build_entry()?;
        Ok(fb)
    }

    /// Registers the sla declares for its own semantics, which a call's value
    /// in them cannot reach the callee through.
    pub fn set_internal_registers(&mut self, registers: Vec<rsleigh::Vn>) {
        self.internal_registers = registers;
    }

    /// Registers each arg-passing register's largest tracked container's
    /// `InitialVar` as the carrier for that argument's positional index.
    pub fn record_register_arg_carriers(&mut self) {
        let arg_regs: Vec<rsleigh::Vn> = self.function.default_cc().arg_passing_regs.clone();
        for (i, reg) in arg_regs.iter().enumerate() {
            let container = vn_container::largest_container_in(self.function.all_vns(), reg);
            if let Some(value) = self.function.initial_var_value(&container) {
                self.function
                    .side_tables_mut()
                    .register_arg_value(i as u32, value);
            }
        }
    }

    /// Attributes every subsequent `create_node` to `addr` until replaced.
    #[inline]
    pub fn set_lift_addr(&mut self, addr: Option<u64>) {
        self.lift_addr = addr;
    }

    /// The machine address currently being lifted, if any.
    #[inline]
    pub fn lift_addr(&self) -> Option<u64> {
        self.lift_addr
    }

    /// Stamps the current lift address into the node's asm-fingerprint. On a
    /// dedup-cache hit the address is unioned into the existing entry.
    pub(crate) fn create_node(
        &mut self,
        kind: NodeKind,
        inputs: impl IntoIterator<Item = ValueId>,
        output_kinds: impl IntoIterator<Item = ValueKind>,
    ) -> NodeId {
        let addr = self.lift_addr;
        let node_id = self
            .function_mut()
            .create_node_attributed(kind, inputs, output_kinds, &[]);
        if let Some(addr) = addr {
            self.function_mut()
                .side_tables_mut()
                .extend_asm_fingerprint(node_id, &[addr]);
        }
        node_id
    }

    /// Validates before handing the function over. A failure wraps a
    /// [`crate::validate::ValidationReport`], recoverable with
    /// `err.downcast_ref::<crate::validate::ValidationReport>()`.
    pub fn build(mut self) -> crate::Result<crate::Function> {
        crate::validate::validate(&self.function).map_err(|e| e.report(&self.function))?;
        if let Some(reach) = frame_reach(&self.function, &self.call_register_values) {
            self.function
                .side_tables_mut()
                .set_frame_address_in_call_register(reach);
        }
        Ok(self.function)
    }
}

/// What a value is, as an address: the entry stack pointer plus a constant,
/// a constant, derived from the stack pointer some other way, or anything
/// else.  `Top` is not yet known, the optimistic start a loop `Phi` needs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    Top,
    Const(i128),
    Sp(i128),
    SpUnplaced,
    Other,
}

impl Reach {
    fn meet(self, other: Self) -> Self {
        use Reach::*;
        match (self, other) {
            (Top, x) | (x, Top) => x,
            (a, b) if a == b => a,
            (SpUnplaced | Sp(_), _) | (_, SpUnplaced | Sp(_)) => SpUnplaced,
            _ => Other,
        }
    }

    fn is_stack(self) -> bool {
        matches!(self, Reach::Sp(_) | Reach::SpUnplaced)
    }
}

/// The highest offset from the entry stack pointer among `values` that are
/// computed from it through the arithmetic an address is built from (`Add`,
/// `And`, `Or`, `Xor`, a width change, a `Phi`): `i128::MAX` when one has no
/// constant offset, `None` when none is.  A value reloaded from memory is
/// left out, since storing the address was already an escape.
///
/// Solved optimistically to a fixed point, so a loop that leaves the stack
/// pointer where it found it keeps the offset.
fn frame_reach(function: &Function, values: &[ValueId]) -> Option<i128> {
    use crate::IRViewer;
    use crate::node::IntUnaryOp;
    let sp = function.stack_vn();
    // The cone the arithmetic reaches, and each value's users within it.
    let mut cone = Vec::new();
    let mut seen = entity_utils::DenseEntitySet::new();
    let mut users: rustc_hash::FxHashMap<ValueId, Vec<ValueId>> = rustc_hash::FxHashMap::default();
    let mut work = values.to_vec();
    while let Some(v) = work.pop() {
        if !seen.insert(v) {
            continue;
        }
        cone.push(v);
        let node = function.producer(v);
        if matches!(
            function.node_kind(node),
            NodeKind::IntBinaryOp(
                IntBinaryOp::Add | IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor
            ) | NodeKind::IntUnaryOp(IntUnaryOp::Neg)
                | NodeKind::Extend(_)
                | NodeKind::Truncate
                | NodeKind::Phi
        ) {
            for input in function.value_inputs(node) {
                users.entry(input).or_default().push(v);
                work.push(input);
            }
        }
    }
    let mut reach: rustc_hash::FxHashMap<ValueId, Reach> = rustc_hash::FxHashMap::default();
    let get =
        |reach: &rustc_hash::FxHashMap<ValueId, Reach>, v| *reach.get(&v).unwrap_or(&Reach::Top);
    let mut work = cone.clone();
    while let Some(v) = work.pop() {
        let node = function.producer(v);
        let inputs: Vec<Reach> = function
            .value_inputs(node)
            .map(|i| get(&reach, i))
            .collect();
        // Offsets wrap at the value's width, as its arithmetic does.
        let bits = function
            .value_type_opt(v)
            .map_or(128, |t| t.bit_width().min(128));
        let wrap = |x: i128| {
            let shift = (128 - bits) as u32;
            x.wrapping_shl(shift).wrapping_shr(shift)
        };
        let next = match *function.node_kind(node) {
            NodeKind::InitialVar(id) if function.initial_vn(id) == sp => Reach::Sp(0),
            NodeKind::IntConst(_) => function
                .int_const_i128(v)
                .map_or(Reach::Other, Reach::Const),
            _ if inputs.contains(&Reach::Top)
                && !matches!(function.node_kind(node), NodeKind::Phi) =>
            {
                Reach::Top
            }
            NodeKind::Phi => inputs.iter().fold(Reach::Top, |acc, &r| acc.meet(r)),
            NodeKind::IntBinaryOp(IntBinaryOp::Add) => match inputs[..] {
                [Reach::Sp(k), Reach::Const(c)] | [Reach::Const(c), Reach::Sp(k)] => {
                    Reach::Sp(wrap(k.wrapping_add(c)))
                }
                [Reach::Const(a), Reach::Const(b)] => Reach::Const(wrap(a.wrapping_add(b))),
                _ if inputs.iter().any(|r| r.is_stack()) => Reach::SpUnplaced,
                _ => Reach::Other,
            },
            NodeKind::IntUnaryOp(IntUnaryOp::Neg) => match inputs[..] {
                [Reach::Const(c)] => Reach::Const(wrap(c.wrapping_neg())),
                _ if inputs.iter().any(|r| r.is_stack()) => Reach::SpUnplaced,
                _ => Reach::Other,
            },
            NodeKind::IntBinaryOp(IntBinaryOp::And | IntBinaryOp::Or | IntBinaryOp::Xor)
            | NodeKind::Extend(_)
            | NodeKind::Truncate
                if inputs.iter().any(|r| r.is_stack()) =>
            {
                Reach::SpUnplaced
            }
            _ => Reach::Other,
        };
        if get(&reach, v) != next {
            reach.insert(v, next);
            work.extend(users.get(&v).into_iter().flatten().copied());
        }
    }
    values
        .iter()
        .filter_map(|&v| match get(&reach, v) {
            Reach::Sp(k) => Some(k),
            Reach::SpUnplaced => Some(i128::MAX),
            Reach::Top | Reach::Const(_) | Reach::Other => None,
        })
        .max()
}
