use expect_test::{Expect, expect};

use crate::IRViewer;
use crate::function::Function;
use crate::node::{
    IntBinaryOp, IntCmpOp, NodeId, NodeKind, SwitchTableId, ValueId, ValueKind, ValueType,
};

const RAM: rsleigh::VnSpace = rsleigh::VnSpace::RAM;
const CTRL: ValueKind = ValueKind::Control;
const MEM: ValueKind = ValueKind::Memory;
const TOKEN: ValueKind = ValueKind::PhiToken;
const I1: ValueKind = ValueKind::Typed(ValueType::I1);
const I32: ValueKind = ValueKind::Typed(ValueType::I32);
const I64: ValueKind = ValueKind::Typed(ValueType::I64);

fn reg(offset: u64) -> rsleigh::Vn {
    rsleigh::Vn {
        addr_space: rsleigh::VnSpace::REGISTER,
        addr_off: offset,
        size: 8,
    }
}

/// `(spec index, output index)`.
type Out = (usize, usize);

type KindFn = Box<dyn Fn(&mut Function) -> NodeKind>;

struct Spec {
    kind: KindFn,
    inputs: Vec<Out>,
    outputs: Vec<ValueKind>,
    asm: Vec<u64>,
    /// Created input-less, inputs added once every node exists.
    deferred: bool,
}

/// A function described independently of creation order: [`Net::build`]
/// materialises it front-to-back or back-to-front.
struct Net {
    tracked: Vec<rsleigh::Vn>,
    specs: Vec<Spec>,
    value_vns: Vec<(Out, rsleigh::Vn)>,
    args: Vec<(u32, Out)>,
    names: Vec<(u64, &'static str)>,
    call_ccs: Vec<(usize, strider_target::BuiltCallingConvention)>,
}

impl Net {
    /// Spec 0 is `Entry`, spec 1 `InitialMemory`.
    fn new(tracked: Vec<rsleigh::Vn>) -> Self {
        let mut net = Self {
            tracked,
            specs: Vec::new(),
            value_vns: Vec::new(),
            args: Vec::new(),
            names: Vec::new(),
            call_ccs: Vec::new(),
        };
        net.node(|_| NodeKind::Entry, &[], &[CTRL], &[]);
        net.node(|_| NodeKind::InitialMemory, &[], &[MEM], &[]);
        net
    }

    fn node(
        &mut self,
        kind: impl Fn(&mut Function) -> NodeKind + 'static,
        inputs: &[Out],
        outputs: &[ValueKind],
        asm: &[u64],
    ) -> usize {
        self.specs.push(Spec {
            kind: Box::new(kind),
            inputs: inputs.to_vec(),
            outputs: outputs.to_vec(),
            asm: asm.to_vec(),
            deferred: false,
        });
        self.specs.len() - 1
    }

    fn plain(
        &mut self,
        kind: NodeKind,
        inputs: &[Out],
        outputs: &[ValueKind],
        asm: &[u64],
    ) -> usize {
        let n = self.node(move |_| kind, inputs, outputs, asm);
        self.specs[n].deferred =
            matches!(kind, NodeKind::Region | NodeKind::Phi | NodeKind::MemPhi);
        n
    }

    fn int(&mut self, value: u128, ty: ValueType, asm: u64) -> usize {
        self.node(
            move |f| NodeKind::IntConst(f.intern_int_const(value, ty)),
            &[],
            &[ValueKind::Typed(ty)],
            &[asm],
        )
    }

    fn initial_var(&mut self, vn: rsleigh::Vn) -> usize {
        self.node(
            move |f| NodeKind::InitialVar(f.vn_id_of(&vn).expect("tracked")),
            &[],
            &[I64],
            &[],
        )
    }

    /// Node ids by spec index.
    fn build(&self, reverse: bool) -> (Function, Vec<NodeId>) {
        let mut f = Function::new(
            strider_target::BuiltCallingConvention::default(),
            strider_target::Endianness::Little,
            self.tracked.clone(),
        );
        if reverse {
            // Zombies ahead of the live nodes shift every arena id.
            let id = f.intern_int_const(0xdead, ValueType::I64);
            let zombie = f.graph_mut().create_node(NodeKind::IntConst(id), [], [I64]);
            let [z] = f.node_outputs_exact::<1>(zombie).unwrap();
            f.graph_mut()
                .create_node(NodeKind::IntBinaryOp(IntBinaryOp::Mul), [z, z], [I64]);
        }
        let n = self.specs.len();
        let mut ids: Vec<Option<NodeId>> = vec![None; n];
        let value = |ids: &[Option<NodeId>], f: &Function, (s, o): Out| {
            ids[s].map(|node| f.node_outputs(node)[o])
        };
        let indices: Vec<usize> = if reverse {
            (0..n).rev().collect()
        } else {
            (0..n).collect()
        };
        while ids.iter().any(Option::is_none) {
            let mut progressed = false;
            for &i in &indices {
                if ids[i].is_some() {
                    continue;
                }
                let spec = &self.specs[i];
                let inputs: Option<Vec<ValueId>> = if spec.deferred {
                    Some(Vec::new())
                } else {
                    spec.inputs.iter().map(|&o| value(&ids, &f, o)).collect()
                };
                let Some(inputs) = inputs else { continue };
                let kind = (spec.kind)(&mut f);
                let node = match kind {
                    NodeKind::Entry => f.entry(),
                    _ => f
                        .graph_mut()
                        .create_node(kind, inputs, spec.outputs.iter().copied()),
                };
                f.side_tables_mut().extend_asm_fingerprint(node, &spec.asm);
                ids[i] = Some(node);
                progressed = true;
            }
            assert!(progressed, "a data cycle outside a phi");
        }
        for &i in &indices {
            let spec = &self.specs[i];
            if spec.deferred {
                let node = ids[i].unwrap();
                for &o in &spec.inputs {
                    let v = value(&ids, &f, o).unwrap();
                    f.graph_mut().add_node_input(node, v);
                }
            }
        }
        for &(o, vn) in &self.value_vns {
            let v = value(&ids, &f, o).unwrap();
            f.set_vn_for_value(v, vn);
        }
        for &(index, o) in &self.args {
            let v = value(&ids, &f, o).unwrap();
            f.side_tables_mut().register_arg_value(index, v);
        }
        for (s, cc) in &self.call_ccs {
            f.set_call_cc(ids[*s].unwrap(), cc.clone());
        }
        for &(op, name) in &self.names {
            f.set_call_other_name(op, name);
        }
        crate::validate::validate(&f).expect("the net builds a valid function");
        (f, ids.into_iter().map(Option::unwrap).collect())
    }
}

/// Both creation orders print `expected`; so do a clone and the compacted
/// function.
fn check(net: &Net, expected: Expect) {
    let (forward, fwd_ids) = net.build(false);
    let (mut backward, bwd_ids) = net.build(true);
    assert_ne!(fwd_ids, bwd_ids, "the two builds share arena ids");
    let text = forward.to_text();
    expected.assert_eq(&text);
    assert_eq!(backward.to_text(), text, "creation order changed the text");
    assert_eq!(
        backward.clone().to_text(),
        text,
        "a clone prints differently"
    );
    backward.compact().unwrap();
    assert_eq!(backward.to_text(), text, "compact changed the text");
}

#[test]
fn diamond_with_phi() {
    let mut net = Net::new(vec![reg(0), reg(0x38)]);
    let x = net.initial_var(reg(0x38));
    let zero = net.int(0, ValueType::I64, 0x1000);
    let cond = net.plain(
        NodeKind::IntCmpOp(IntCmpOp::Equal),
        &[(x, 0), (zero, 0)],
        &[I1],
        &[0x1000],
    );
    let branch = net.plain(NodeKind::If, &[(0, 0), (cond, 0)], &[CTRL, CTRL], &[0x1004]);
    let then_ = net.plain(NodeKind::Region, &[(branch, 0)], &[CTRL, TOKEN], &[]);
    let else_ = net.plain(NodeKind::Region, &[(branch, 1)], &[CTRL, TOKEN], &[]);
    let one = net.int(1, ValueType::I64, 0x1008);
    let two = net.int(2, ValueType::I64, 0x100c);
    let merge = net.plain(
        NodeKind::Region,
        &[(then_, 0), (else_, 0)],
        &[CTRL, TOKEN],
        &[],
    );
    let phi = net.plain(
        NodeKind::Phi,
        &[(merge, 1), (one, 0), (two, 0)],
        &[I64],
        &[],
    );
    net.plain(
        NodeKind::Return,
        &[(merge, 0), (1, 0), (phi, 0)],
        &[],
        &[0x1010],
    );
    net.value_vns.push(((phi, 0), reg(0)));
    net.args.push((0, (x, 0)));
    check(
        &net,
        expect![[r#"
            endian little
            tracked [%:0x0:8, %:0x38:8]
            default_cc arg_passing_regs=[] arg_passing_regs_float=[] callee_saved_regs=[] ret_val_regs=[] ret_val_regs_float=[] stack_vn=%:0xffffffffffff0000:8 stack_args=none ret_stack_pop=0 link_register_vn=none preserves_memory=false preserves_all_registers=false no_return=false
            arg 0 [%v1]
            %v0:ctrl = entry
            %v1:i64 = initial_var[%:0x38:8]
            %v2:i64 = iconst[0x0]  @{0x1000}
            %v3:i1 = icmp.equal %v1, %v2  @{0x1000}
            %v4:ctrl, %v5:ctrl = if %v0, %v3  @{0x1004}

            %v6:ctrl, %v7:token = region %v4
            %v8:i64 = iconst[0x1]  @{0x1008}

            %v9:ctrl, %v10:token = region %v5
            %v11:i64 = iconst[0x2]  @{0x100c}

            %v12:ctrl, %v13:token = region %v6, %v9
            %v14:i64<%:0x0:8> = phi %v13, %v8, %v11
            %v15:mem = initial_memory
            return %v12, %v15, %v14  @{0x1010}
        "#]],
    );
}

#[test]
fn loop_with_phi_and_mem_phi() {
    let mut net = Net::new(Vec::new());
    let head = net.plain(NodeKind::Region, &[], &[CTRL, TOKEN], &[]);
    let zero = net.int(0, ValueType::I64, 0x2000);
    let i = net.plain(NodeKind::Phi, &[], &[I64], &[]);
    let m = net.plain(NodeKind::MemPhi, &[], &[MEM], &[]);
    let ten = net.int(10, ValueType::I64, 0x2004);
    let lt = net.plain(
        NodeKind::IntCmpOp(IntCmpOp::Less),
        &[(i, 0), (ten, 0)],
        &[I1],
        &[0x2004],
    );
    let branch = net.plain(
        NodeKind::If,
        &[(head, 0), (lt, 0)],
        &[CTRL, CTRL],
        &[0x2008],
    );
    let body = net.plain(NodeKind::Region, &[(branch, 0)], &[CTRL, TOKEN], &[]);
    let addr = net.int(0x9000, ValueType::I64, 0x200c);
    let store = net.plain(
        NodeKind::Store(RAM),
        &[(m, 0), (addr, 0), (i, 0)],
        &[MEM],
        &[0x200c],
    );
    let one = net.int(1, ValueType::I64, 0x2010);
    let next = net.plain(
        NodeKind::IntBinaryOp(IntBinaryOp::Add),
        &[(i, 0), (one, 0)],
        &[I64],
        &[0x2010],
    );
    let exit = net.plain(NodeKind::Region, &[(branch, 1)], &[CTRL, TOKEN], &[]);
    let load = net.plain(NodeKind::Load(RAM), &[(m, 0), (addr, 0)], &[I64], &[0x2014]);
    net.plain(
        NodeKind::Return,
        &[(exit, 0), (m, 0), (load, 0)],
        &[],
        &[0x2018],
    );
    net.specs[head].inputs = vec![(0, 0), (body, 0)];
    net.specs[i].inputs = vec![(head, 1), (zero, 0), (next, 0)];
    net.specs[m].inputs = vec![(head, 1), (1, 0), (store, 0)];
    check(
        &net,
        expect![[r#"
            endian little
            tracked []
            default_cc arg_passing_regs=[] arg_passing_regs_float=[] callee_saved_regs=[] ret_val_regs=[] ret_val_regs_float=[] stack_vn=%:0xffffffffffff0000:8 stack_args=none ret_stack_pop=0 link_register_vn=none preserves_memory=false preserves_all_registers=false no_return=false
            %v0:ctrl = entry
            %v1:i64 = iconst[0x0]  @{0x2000}
            %v2:mem = initial_memory

            %v3:ctrl, %v4:token = region %v0, %v11
            %v5:i64 = phi %v4, %v1, %v14
            %v6:mem = memphi %v4, %v2, %v15
            %v7:i64 = iconst[0xa]  @{0x2004}
            %v8:i1 = icmp.less %v5, %v7  @{0x2004}
            %v9:ctrl, %v10:ctrl = if %v3, %v8  @{0x2008}

            %v11:ctrl, %v12:token = region %v9
            %v13:i64 = iconst[0x1]  @{0x2010}
            %v14:i64 = int.add %v5, %v13  @{0x2010}
            %v15:mem = store[r] %v6, %v18, %v5  @{0x200c}

            %v16:ctrl, %v17:token = region %v10
            %v18:i64 = iconst[0x9000]  @{0x200c}
            %v19:i64 = load[r] %v6, %v18  @{0x2014}
            return %v16, %v6, %v19  @{0x2018}
        "#]],
    );
}

#[test]
fn load_store_and_call_with_an_override_convention() {
    let mut net = Net::new(vec![reg(0), reg(0x20)]);
    let sp = net.initial_var(reg(0x20));
    let target = net.int(0x40_1000, ValueType::I64, 0x3000);
    let call = net.plain(
        NodeKind::Call { cc: None },
        &[(0, 0), (1, 0), (target, 0), (sp, 0)],
        &[CTRL, MEM, I64],
        &[0x3000],
    );
    net.call_ccs.push((
        call,
        strider_target::BuiltCallingConvention {
            ret_val_regs: vec![reg(0)],
            preserves_memory: true,
            ..Default::default()
        },
    ));
    let addr = net.int(0x2000, ValueType::I64, 0x3004);
    let load = net.plain(
        NodeKind::Load(RAM),
        &[(call, 1), (addr, 0)],
        &[I32],
        &[0x3004],
    );
    let store = net.plain(
        NodeKind::Store(RAM),
        &[(call, 1), (addr, 0), (load, 0)],
        &[MEM],
        &[0x3008],
    );
    net.plain(
        NodeKind::Return,
        &[(call, 0), (store, 0), (call, 2)],
        &[],
        &[0x300c],
    );
    net.value_vns.push(((call, 2), reg(0)));
    check(
        &net,
        expect![[r#"
            endian little
            tracked [%:0x0:8, %:0x20:8]
            default_cc arg_passing_regs=[] arg_passing_regs_float=[] callee_saved_regs=[] ret_val_regs=[] ret_val_regs_float=[] stack_vn=%:0xffffffffffff0000:8 stack_args=none ret_stack_pop=0 link_register_vn=none preserves_memory=false preserves_all_registers=false no_return=false
            %v0:ctrl = entry
            %v1:mem = initial_memory
            %v2:i64 = iconst[0x401000]  @{0x3000}
            %v3:i64 = initial_var[%:0x20:8]
            %v4:ctrl, %v5:mem, %v6:i64<%:0x0:8> = call[ret_val_regs=[%:0x0:8] preserves_memory=true] %v0, %v1, %v2, %v3  @{0x3000}
            %v7:i64 = iconst[0x2000]  @{0x3004}
            %v8:i32 = load[r] %v5, %v7  @{0x3004}
            %v9:mem = store[r] %v5, %v7, %v8  @{0x3008}
            return %v4, %v9, %v6  @{0x300c}
        "#]],
    );
}

#[test]
fn switch_after_call_other() {
    let mut net = Net::new(vec![reg(0x38)]);
    let x = net.initial_var(reg(0x38));
    let op = net.plain(
        NodeKind::CallOther { user_op_id: 7 },
        &[(0, 0), (1, 0), (x, 0)],
        &[CTRL, MEM, I64],
        &[0x4000],
    );
    let switch = net.node(
        |f| NodeKind::Switch(f.add_switch_table(vec![0x40_1000, 0x40_1010])),
        &[(op, 0), (op, 2)],
        &[CTRL, CTRL],
        &[0x4004],
    );
    let a = net.plain(NodeKind::Region, &[(switch, 0)], &[CTRL, TOKEN], &[]);
    let b = net.plain(NodeKind::Region, &[(switch, 1)], &[CTRL, TOKEN], &[]);
    net.plain(NodeKind::Return, &[(a, 0), (op, 1)], &[], &[0x40_1000]);
    net.plain(NodeKind::Unreachable, &[(b, 0)], &[], &[0x40_1010]);
    net.names.push((7, "cpuid"));
    check(
        &net,
        expect![[r#"
            endian little
            tracked [%:0x38:8]
            default_cc arg_passing_regs=[] arg_passing_regs_float=[] callee_saved_regs=[] ret_val_regs=[] ret_val_regs_float=[] stack_vn=%:0xffffffffffff0000:8 stack_args=none ret_stack_pop=0 link_register_vn=none preserves_memory=false preserves_all_registers=false no_return=false
            %v0:ctrl = entry
            %v1:mem = initial_memory
            %v2:i64 = initial_var[%:0x38:8]
            %v3:ctrl, %v4:mem, %v5:i64 = callother[7 "cpuid"] %v0, %v1, %v2  @{0x4000}
            %v6:ctrl, %v7:ctrl = switch[0x401000, 0x401010] %v3, %v5  @{0x4004}

            %v8:ctrl, %v9:token = region %v6
            return %v8, %v4  @{0x401000}

            %v10:ctrl, %v11:token = region %v7
            unreachable %v10  @{0x401010}
        "#]],
    );
}

#[test]
fn wide_and_float_constants() {
    let mut net = Net::new(Vec::new());
    let wide = net.node(
        |f| {
            NodeKind::IntConst(f.intern_int_const_limbs(
                &[0x1122_3344_5566_7788, 0, 0, 0x8000_0000_0000_0000],
                ValueType::I256,
            ))
        },
        &[],
        &[ValueKind::Typed(ValueType::I256)],
        &[0x5000],
    );
    let float = net.plain(
        NodeKind::FloatConst(0x3ff0_0000_0000_0000),
        &[],
        &[ValueKind::Typed(ValueType::F64)],
        &[0x5004],
    );
    net.plain(
        NodeKind::Return,
        &[(0, 0), (1, 0), (wide, 0), (float, 0)],
        &[],
        &[0x5008],
    );
    check(
        &net,
        expect![[r#"
            endian little
            tracked []
            default_cc arg_passing_regs=[] arg_passing_regs_float=[] callee_saved_regs=[] ret_val_regs=[] ret_val_regs_float=[] stack_vn=%:0xffffffffffff0000:8 stack_args=none ret_stack_pop=0 link_register_vn=none preserves_memory=false preserves_all_registers=false no_return=false
            %v0:ctrl = entry
            %v1:mem = initial_memory
            %v2:i256 = iconst[0x8000000000000000000000000000000000000000000000001122334455667788]  @{0x5000}
            %v3:f64 = fconst[0x3ff0000000000000]  @{0x5004}
            return %v0, %v1, %v2, %v3  @{0x5008}
        "#]],
    );
}

/// Payload ids this function never minted print as `?` rather than panic.
#[test]
fn every_node_kind_prints() {
    use cranelift_entity::EntityRef;
    let mut f = Function::new(
        strider_target::BuiltCallingConvention::default(),
        strider_target::Endianness::Big,
        Vec::new(),
    );
    let mut kinds = crate::node_signature::every_node_kind();
    kinds.push(NodeKind::Call {
        cc: Some(crate::node::CcId::new(3)),
    });
    kinds.push(NodeKind::Switch(SwitchTableId::new(3)));
    let values: Vec<ValueId> = kinds
        .iter()
        .map(|&kind| {
            let node = f.graph_mut().create_node(kind, [], [I64, CTRL]);
            f.node_outputs(node)[0]
        })
        .collect();
    let [ctrl] = f.node_outputs_exact::<1>(f.entry()).unwrap();
    let inputs: Vec<ValueId> = [ctrl].into_iter().chain(values).collect();
    f.graph_mut().create_node(NodeKind::Return, inputs, []);
    expect![[r#"
        endian big
        tracked []
        default_cc arg_passing_regs=[] arg_passing_regs_float=[] callee_saved_regs=[] ret_val_regs=[] ret_val_regs_float=[] stack_vn=%:0xffffffffffff0000:8 stack_args=none ret_stack_pop=0 link_register_vn=none preserves_memory=false preserves_all_registers=false no_return=false
        %v0:ctrl = entry
        %v1:i64, %v2:ctrl = entry
        %v3:i64, %v4:ctrl = initial_memory
        %v5:i64, %v6:ctrl = initial_var[?]

        %v7:i64, %v8:ctrl = region
        %v9:i64, %v10:ctrl = memphi
        %v11:i64, %v12:ctrl = phi
        %v13:i64, %v14:ctrl = if
        %v15:i64, %v16:ctrl = switch[?]
        %v17:i64, %v18:ctrl = call
        %v19:i64, %v20:ctrl = return
        %v21:i64, %v22:ctrl = indirect_branch
        %v23:i64, %v24:ctrl = unreachable
        %v25:i64, %v26:ctrl = load[r]
        %v27:i64, %v28:ctrl = store[r]
        %v29:i64, %v30:ctrl = iconst[?]
        %v31:i64, %v32:ctrl = int.neg
        %v33:i64, %v34:ctrl = int.add
        %v35:i64, %v36:ctrl = icmp.equal
        %v37:i64, %v38:ctrl = truncate
        %v39:i64, %v40:ctrl = popcount
        %v41:i64, %v42:ctrl = lzcount
        %v43:i64, %v44:ctrl = extend.zero
        %v45:i64, %v46:ctrl = fconst[0x0]
        %v47:i64, %v48:ctrl = float.add
        %v49:i64, %v50:ctrl = float.neg
        %v51:i64, %v52:ctrl = fcmp.equal
        %v53:i64, %v54:ctrl = int_to_float
        %v55:i64, %v56:ctrl = float_to_int
        %v57:i64, %v58:ctrl = float_to_float
        %v59:i64, %v60:ctrl = int_bits_to_float
        %v61:i64, %v62:ctrl = float_bits_to_int
        %v63:i64, %v64:ctrl = callother[0]
        %v65:i64, %v66:ctrl = cpool_ref
        %v67:i64, %v68:ctrl = new
        %v69:i64, %v70:ctrl = call[?]
        %v71:i64, %v72:ctrl = switch[?]
        return %v0, %v1, %v3, %v5, %v7, %v9, %v11, %v13, %v15, %v17, %v19, %v21, %v23, %v25, %v27, %v29, %v31, %v33, %v35, %v37, %v39, %v41, %v43, %v45, %v47, %v49, %v51, %v53, %v55, %v57, %v59, %v61, %v63, %v65, %v67, %v69, %v71
    "#]].assert_eq(&f.to_text());
}
