use std::fmt::Write as _;

use cranelift_entity::SecondaryMap;
use entity_utils::set::DenseEntitySet;

use crate::IRViewer;
use crate::function::Function;
use crate::node::const_value::ConstValue;
use crate::node::{
    ExtendOp, FloatBinaryOp, FloatCmpOp, FloatUnaryOp, IntBinaryOp, IntCmpOp, IntUnaryOp, NodeId,
    NodeKind, ValueId, ValueKind,
};

impl Function {
    /// The canonical text form: a header (endianness, tracked varnodes, default
    /// convention, argument carriers), then one line per node reachable from
    /// [`Self::entry`] as `outs = op[payload] ins`, values named `%vN` in print
    /// order. With `fingerprints`, each line ends in its sorted asm address set
    /// `  @{..}`; those sets can dwarf the rest on large functions.
    ///
    /// Control nodes print in reverse postorder of the control graph, taking
    /// successors in output-index order. Each is preceded by its unprinted
    /// data inputs in post-order over input slots. A `Phi` / `MemPhi` prints
    /// right after its `Region`, siblings in the order a consumer first
    /// demands them, and the cone of an input it takes over a predecessor edge
    /// prints after that predecessor. Nothing reads an arena id, a hash order
    /// or a use-list, save a control output's single use, so equal graphs
    /// print equal text however they were built.
    ///
    /// Unreachable nodes are not printed, which is why [`Self::compact`]
    /// leaves the text unchanged.
    pub fn to_text(&self, fingerprints: bool) -> String {
        let order = Orderer::new(self).run();
        render(self, &attach_phis(self, order), fingerprints)
    }
}

fn is_phi(kind: &NodeKind) -> bool {
    matches!(kind, NodeKind::Phi | NodeKind::MemPhi)
}

/// The `Region` a phi's token input comes from.
fn phi_region(f: &Function, phi: NodeId) -> Option<NodeId> {
    let token = f.nth_input(phi, 0)?;
    let region = f.producer(token);
    matches!(f.node_kind(region), NodeKind::Region).then_some(region)
}

enum Frame {
    Enter(NodeId),
    Resume(NodeId, usize),
}

/// Where a pulled node prints: the block of a control node, or the tail.
type Sink = Option<NodeId>;

struct Orderer<'a> {
    f: &'a Function,
    in_cfg: DenseEntitySet<NodeId>,
    visited: DenseEntitySet<NodeId>,
    /// Per control node: its unprinted data inputs, itself, then the phi
    /// inputs its outgoing edge carries.
    blocks: SecondaryMap<NodeId, Vec<NodeId>>,
    /// Phi inputs waiting for the keyed control node to print.
    waiting: SecondaryMap<NodeId, Vec<NodeId>>,
    /// Nodes reached only past the control walk, after every block.
    tail: Vec<NodeId>,
    /// Blocks are flattened; everything further goes to the tail.
    sealed: bool,
}

impl<'a> Orderer<'a> {
    fn new(f: &'a Function) -> Self {
        Self {
            f,
            in_cfg: DenseEntitySet::new(),
            visited: DenseEntitySet::new(),
            blocks: SecondaryMap::new(),
            waiting: SecondaryMap::new(),
            tail: Vec::new(),
            sealed: false,
        }
    }

    fn run(mut self) -> Vec<NodeId> {
        let rpo = self.cfg_reverse_postorder();
        for &node in &rpo {
            self.in_cfg.insert(node);
        }
        for &node in &rpo {
            self.pull(node, Some(node));
            loop {
                let due = std::mem::take(&mut self.waiting[node]);
                if due.is_empty() {
                    break;
                }
                for n in due {
                    self.pull(n, Some(node));
                }
            }
        }
        self.sealed = true;
        let mut order = Vec::new();
        for &node in &rpo {
            order.append(&mut self.blocks[node]);
        }
        // Control successors of dead predecessors a live node reached as data.
        let mut i = 0;
        while let Some(&node) = order.get(i) {
            let succs: Vec<NodeId> = crate::walk::cfg_succs(self.f.graph(), node).collect();
            for succ in succs {
                self.pull(succ, None);
            }
            order.append(&mut self.tail);
            i += 1;
        }
        order
    }

    /// Successors pushed in reverse so output 0's subtree comes first.
    fn cfg_reverse_postorder(&self) -> Vec<NodeId> {
        let g = self.f.graph();
        let mut seen = DenseEntitySet::new();
        let mut post = Vec::new();
        let entry = self.f.entry();
        seen.insert(entry);
        let succs = |n: NodeId| -> Vec<NodeId> {
            let mut s: Vec<NodeId> = crate::walk::cfg_succs(g, n).collect();
            s.reverse();
            s
        };
        let mut stack: Vec<(NodeId, Vec<NodeId>, usize)> = vec![(entry, succs(entry), 0)];
        while let Some((node, next, idx)) = stack.last_mut() {
            if let Some(&succ) = next.get(*idx) {
                *idx += 1;
                if seen.insert(succ) {
                    stack.push((succ, succs(succ), 0));
                }
            } else {
                post.push(*node);
                stack.pop();
            }
        }
        post.reverse();
        post
    }

    /// Prints `root` into `sink` after its unprinted inputs, never pulling a
    /// control node out of its own block.
    fn pull(&mut self, root: NodeId, sink: Sink) {
        let f = self.f;
        let mut jobs = vec![(root, sink)];
        let mut next_job = 0;
        while let Some(&(root, sink)) = jobs.get(next_job) {
            next_job += 1;
            let mut stack = vec![Frame::Enter(root)];
            while let Some(frame) = stack.pop() {
                match frame {
                    Frame::Enter(node) => {
                        if self.visited.insert(node) {
                            stack.push(Frame::Resume(node, 0));
                        }
                    }
                    Frame::Resume(node, slot) => {
                        let phi = is_phi(f.node_kind(node));
                        if !phi && let Some(value) = f.nth_input(node, slot) {
                            stack.push(Frame::Resume(node, slot + 1));
                            let producer = f.producer(value);
                            if !self.in_cfg.contains(producer) {
                                stack.push(Frame::Enter(producer));
                            }
                            continue;
                        }
                        match sink {
                            Some(block) => self.blocks[block].push(node),
                            None => self.tail.push(node),
                        }
                        if phi {
                            self.defer_phi_inputs(node, &mut stack, &mut jobs);
                        }
                    }
                }
            }
        }
    }

    /// An input arriving over an edge from a control node goes to that node's
    /// block, now if it has printed and else once it does; any other input
    /// prints here.
    fn defer_phi_inputs(
        &mut self,
        phi: NodeId,
        stack: &mut Vec<Frame>,
        jobs: &mut Vec<(NodeId, Sink)>,
    ) {
        let f = self.f;
        let region = phi_region(f, phi);
        let mut here = Vec::new();
        for (slot, value) in f.node_inputs(phi).into_iter().enumerate() {
            let producer = f.producer(value);
            if self.visited.contains(producer) || self.in_cfg.contains(producer) {
                continue;
            }
            let pred = slot
                .checked_sub(1)
                .zip(region)
                .and_then(|(edge, region)| f.nth_input(region, edge))
                .map(|ctrl| f.producer(ctrl));
            match pred {
                Some(pred) if self.in_cfg.contains(pred) && !self.sealed => {
                    if self.visited.contains(pred) {
                        jobs.push((producer, Some(pred)));
                    } else {
                        self.waiting[pred].push(producer);
                    }
                }
                _ => here.push(producer),
            }
        }
        stack.extend(here.into_iter().rev().map(Frame::Enter));
    }
}

/// Moves each phi to just after its `Region`, keeping sibling order.
fn attach_phis(f: &Function, seq: Vec<NodeId>) -> Vec<NodeId> {
    let mut printed = DenseEntitySet::new();
    for &node in &seq {
        printed.insert(node);
    }
    let mut attached: SecondaryMap<NodeId, Vec<NodeId>> = SecondaryMap::new();
    let mut moved = DenseEntitySet::new();
    for &node in &seq {
        if is_phi(f.node_kind(node))
            && let Some(region) = phi_region(f, node)
            && printed.contains(region)
        {
            attached[region].push(node);
            moved.insert(node);
        }
    }
    let mut out = Vec::with_capacity(seq.len());
    for node in seq {
        if moved.contains(node) {
            continue;
        }
        out.push(node);
        out.append(&mut attached[node]);
    }
    out
}

type Names = SecondaryMap<ValueId, Option<u32>>;

fn push_name(out: &mut String, names: &Names, value: ValueId) {
    let _ = match names.get(value).copied().flatten() {
        Some(n) => write!(out, "%v{n}"),
        None => write!(out, "%?"),
    };
}

fn render(f: &Function, order: &[NodeId], fingerprints: bool) -> String {
    let mut names: Names = SecondaryMap::new();
    let mut next = 0u32;
    for &node in order {
        for &value in f.node_outputs(node) {
            names[value] = Some(next);
            next += 1;
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "endian {}",
        match f.endianness() {
            strider_target::Endianness::Little => "little",
            strider_target::Endianness::Big => "big",
        }
    );
    let _ = writeln!(out, "tracked {}", vn_list(f.all_vns()));
    let cc_fields = cc_fields(f.default_cc());
    out.push_str("default_cc");
    for (field, value) in &cc_fields {
        let _ = write!(out, " {field}={value}");
    }
    out.push('\n');
    let st = f.side_tables();
    render_args(&mut out, &names, "arg", st.iter_arg_indices(), |i| {
        st.arg_index_to_values(i)
    });
    render_args(
        &mut out,
        &names,
        "float_arg",
        st.iter_float_arg_indices(),
        |i| st.float_arg_index_to_values(i),
    );

    for &node in order {
        let kind = f.node_kind(node);
        if matches!(kind, NodeKind::Region) {
            out.push('\n');
        }
        let outputs = f.node_outputs(node);
        for (i, &value) in outputs.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            push_name(&mut out, &names, value);
            out.push(':');
            out.push_str(value_kind_str(f.value_kind(value)));
            if let Some(&id) = st.value_vn.get(&value) {
                let _ = match f.initial_vn_opt(id) {
                    Some(vn) => write!(out, "<{}>", vn_str(&vn)),
                    None => write!(out, "<?>"),
                };
            }
        }
        if !outputs.is_empty() {
            out.push_str(" = ");
        }
        out.push_str(op_name(kind));
        render_payload(f, node, &cc_fields, &mut out);
        for (i, value) in f.node_inputs(node).into_iter().enumerate() {
            out.push_str(if i == 0 { " " } else { ", " });
            push_name(&mut out, &names, value);
        }
        if fingerprints && !st.asm_fingerprint_is_empty(node) {
            let mut addrs: Vec<u64> = st.asm_fingerprint(node).into_iter().collect();
            addrs.sort_unstable();
            out.push_str("  @{");
            for (i, addr) in addrs.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                let _ = write!(out, "{addr:#x}");
            }
            out.push('}');
        }
        out.push('\n');
    }
    out
}

/// `<label> <index> [carriers]`, indices ascending, carriers in print order.
/// A carrier that is not printed is left out, and so is an index left empty.
fn render_args<'a>(
    out: &mut String,
    names: &Names,
    label: &str,
    indices: impl Iterator<Item = u32>,
    carriers_of: impl Fn(u32) -> &'a [ValueId],
) {
    let mut indices: Vec<u32> = indices.collect();
    indices.sort_unstable();
    for index in indices {
        let mut carriers: Vec<u32> = carriers_of(index)
            .iter()
            .filter_map(|&value| names.get(value).copied().flatten())
            .collect();
        carriers.sort_unstable();
        carriers.dedup();
        if carriers.is_empty() {
            continue;
        }
        let _ = write!(out, "{label} {index} [");
        for (i, n) in carriers.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "%v{n}");
        }
        out.push_str("]\n");
    }
}

fn render_payload(
    f: &Function,
    node: NodeId,
    default_cc: &[(&'static str, String)],
    out: &mut String,
) {
    match *f.node_kind(node) {
        NodeKind::InitialVar(id) => {
            let _ = match f.initial_vn_opt(id) {
                Some(vn) => write!(out, "[{}]", vn_str(&vn)),
                None => write!(out, "[?]"),
            };
        }
        NodeKind::Load(space) | NodeKind::Store(space) => {
            let _ = write!(out, "[{}]", space_str(space));
        }
        NodeKind::IntConst(id) => {
            // The interner probe first: `int_const_u128` panics on a dangling id.
            let _ = match f.const_interner.get(id) {
                None => write!(out, "[?]"),
                Some(stored) => match f
                    .node_outputs(node)
                    .first()
                    .and_then(|&value| f.int_const_u128(value))
                {
                    Some(bits) => write!(out, "[{bits:#x}]"),
                    None => write!(out, "[{}]", const_str(stored)),
                },
            };
        }
        NodeKind::FloatConst(bits) => {
            let _ = write!(out, "[{bits:#x}]");
        }
        NodeKind::Switch(id) => match f.switch_table(id) {
            Some(targets) => {
                out.push('[');
                for (i, target) in targets.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{target:#x}");
                }
                out.push(']');
            }
            None => out.push_str("[?]"),
        },
        NodeKind::Call { cc: Some(id) } => match f.cc(id) {
            Some(cc) => {
                out.push('[');
                let mut first = true;
                for ((field, value), (_, default)) in cc_fields(cc).iter().zip(default_cc) {
                    if value != default {
                        if !first {
                            out.push(' ');
                        }
                        first = false;
                        let _ = write!(out, "{field}={value}");
                    }
                }
                out.push(']');
            }
            None => out.push_str("[?]"),
        },
        NodeKind::CallOther { user_op_id } => {
            let _ = match f.call_other_name(node) {
                Some(op) => write!(out, "[{user_op_id} {op:?}]"),
                None => write!(out, "[{user_op_id}]"),
            };
        }
        NodeKind::Entry
        | NodeKind::InitialMemory
        | NodeKind::Region
        | NodeKind::MemPhi
        | NodeKind::Phi
        | NodeKind::If
        | NodeKind::Call { cc: None }
        | NodeKind::Return
        | NodeKind::IndirectBranch
        | NodeKind::Unreachable
        | NodeKind::IntUnaryOp(_)
        | NodeKind::IntBinaryOp(_)
        | NodeKind::IntCmpOp(_)
        | NodeKind::Truncate
        | NodeKind::Popcount
        | NodeKind::Lzcount
        | NodeKind::Extend(_)
        | NodeKind::FloatBinaryOp(_)
        | NodeKind::FloatUnaryOp(_)
        | NodeKind::FloatCmpOp(_)
        | NodeKind::IntToFloat
        | NodeKind::FloatToInt
        | NodeKind::FloatToFloat
        | NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt
        | NodeKind::CPoolRef
        | NodeKind::New => {}
    }
}

fn op_name(kind: &NodeKind) -> &'static str {
    match *kind {
        NodeKind::Entry => "entry",
        NodeKind::InitialMemory => "initial_memory",
        NodeKind::InitialVar(_) => "initial_var",
        NodeKind::Region => "region",
        NodeKind::MemPhi => "memphi",
        NodeKind::Phi => "phi",
        NodeKind::If => "if",
        NodeKind::Switch(_) => "switch",
        NodeKind::Call { .. } => "call",
        NodeKind::Return => "return",
        NodeKind::IndirectBranch => "indirect_branch",
        NodeKind::Unreachable => "unreachable",
        NodeKind::Load(_) => "load",
        NodeKind::Store(_) => "store",
        NodeKind::IntConst(_) => "iconst",
        NodeKind::IntUnaryOp(IntUnaryOp::Neg) => "int.neg",
        NodeKind::IntBinaryOp(op) => match op {
            IntBinaryOp::Add => "int.add",
            IntBinaryOp::And => "int.and",
            IntBinaryOp::Or => "int.or",
            IntBinaryOp::Xor => "int.xor",
            IntBinaryOp::Div => "int.div",
            IntBinaryOp::Sdiv => "int.sdiv",
            IntBinaryOp::Rem => "int.rem",
            IntBinaryOp::Srem => "int.srem",
            IntBinaryOp::ShiftRight => "int.shift_right",
            IntBinaryOp::SShiftRight => "int.sshift_right",
            IntBinaryOp::ShiftLeft => "int.shift_left",
            IntBinaryOp::Mul => "int.mul",
        },
        NodeKind::IntCmpOp(op) => match op {
            IntCmpOp::Equal => "icmp.equal",
            IntCmpOp::Sless => "icmp.sless",
            IntCmpOp::Less => "icmp.less",
            IntCmpOp::Carry => "icmp.carry",
            IntCmpOp::Scarry => "icmp.scarry",
            IntCmpOp::Sborrow => "icmp.sborrow",
        },
        NodeKind::Truncate => "truncate",
        NodeKind::Popcount => "popcount",
        NodeKind::Lzcount => "lzcount",
        NodeKind::Extend(ExtendOp::ZeroExtend) => "extend.zero",
        NodeKind::Extend(ExtendOp::SignExtend) => "extend.sign",
        NodeKind::FloatConst(_) => "fconst",
        NodeKind::FloatBinaryOp(op) => match op {
            FloatBinaryOp::Add => "float.add",
            FloatBinaryOp::Mul => "float.mul",
            FloatBinaryOp::Div => "float.div",
        },
        NodeKind::FloatUnaryOp(op) => match op {
            FloatUnaryOp::Neg => "float.neg",
            FloatUnaryOp::Abs => "float.abs",
            FloatUnaryOp::Sqrt => "float.sqrt",
            FloatUnaryOp::Ceil => "float.ceil",
            FloatUnaryOp::Floor => "float.floor",
            FloatUnaryOp::Round => "float.round",
        },
        NodeKind::FloatCmpOp(FloatCmpOp::Equal) => "fcmp.equal",
        NodeKind::FloatCmpOp(FloatCmpOp::Less) => "fcmp.less",
        NodeKind::IntToFloat => "int_to_float",
        NodeKind::FloatToInt => "float_to_int",
        NodeKind::FloatToFloat => "float_to_float",
        NodeKind::IntBitsToFloat => "int_bits_to_float",
        NodeKind::FloatBitsToInt => "float_bits_to_int",
        NodeKind::CallOther { .. } => "callother",
        NodeKind::CPoolRef => "cpool_ref",
        NodeKind::New => "new",
    }
}

fn value_kind_str(kind: ValueKind) -> &'static str {
    match kind {
        ValueKind::Typed(ty) => ty.as_str(),
        ValueKind::Control => "ctrl",
        ValueKind::PhiToken => "token",
        ValueKind::Memory => "mem",
    }
}

fn space_str(space: rsleigh::VnSpace) -> String {
    let byte = space.shortcut_raw();
    if byte.is_ascii_graphic() && byte != b':' {
        char::from(byte).to_string()
    } else {
        format!("{byte:#04x}")
    }
}

/// `space:offset:size`, the space as its Sleigh shortcut character.
fn vn_str(vn: &rsleigh::Vn) -> String {
    format!(
        "{}:{:#x}:{}",
        space_str(vn.addr_space),
        vn.addr_off,
        vn.size
    )
}

fn vn_list(vns: &[rsleigh::Vn]) -> String {
    let items: Vec<String> = vns.iter().map(vn_str).collect();
    format!("[{}]", items.join(", "))
}

/// Hex, high limb first.
fn const_str(value: &ConstValue) -> String {
    match value {
        ConstValue::Bits(bits) => format!("{bits:#x}"),
        ConstValue::Wide(limbs) => {
            let mut high = limbs.iter().rev().skip_while(|&&limb| limb == 0);
            let Some(top) = high.next() else {
                return "0x0".to_owned();
            };
            let mut s = format!("{top:#x}");
            for limb in high {
                let _ = write!(s, "{limb:016x}");
            }
            s
        }
    }
}

/// Every field, in declaration order.
fn cc_fields(cc: &strider_target::BuiltCallingConvention) -> Vec<(&'static str, String)> {
    let strider_target::BuiltCallingConvention {
        arg_passing_regs,
        arg_passing_regs_float,
        callee_saved_regs,
        ret_val_regs,
        ret_val_regs_float,
        stack_vn,
        stack_args,
        ret_stack_pop,
        link_register_vn,
        preserves_memory,
        preserves_all_registers,
        no_return,
    } = cc;
    vec![
        ("arg_passing_regs", vn_list(arg_passing_regs)),
        ("arg_passing_regs_float", vn_list(arg_passing_regs_float)),
        ("callee_saved_regs", vn_list(callee_saved_regs)),
        ("ret_val_regs", vn_list(ret_val_regs)),
        ("ret_val_regs_float", vn_list(ret_val_regs_float)),
        ("stack_vn", vn_str(stack_vn)),
        (
            "stack_args",
            stack_args.map_or_else(
                || "none".to_owned(),
                |s| format!("{}+{}n", s.base_offset, s.increment),
            ),
        ),
        ("ret_stack_pop", ret_stack_pop.to_string()),
        (
            "link_register_vn",
            link_register_vn
                .as_ref()
                .map_or_else(|| "none".to_owned(), vn_str),
        ),
        ("preserves_memory", preserves_memory.to_string()),
        (
            "preserves_all_registers",
            preserves_all_registers.to_string(),
        ),
        ("no_return", no_return.to_string()),
    ]
}

#[cfg(test)]
mod tests;
