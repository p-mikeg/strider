//! End-to-end lift of `fixtures/out/arm_be8/*.elf` under
//! [`SleighArch::arm_be_kernel`], the BE8 preset: `EF_ARM_BE8` is set,
//! instructions are stored little-endian and data big-endian, so the preset
//! pairs the little-endian `SLA_SPEC_ARM8_LE` with `Endianness::Big`.
//!
//! The two byte orders are separable only in a real image, which is what these
//! fixtures add: `ld --be8` reverses the code against the `$a` / `$d` mapping
//! symbols and leaves `.text` literal-pool words alone, so a pool word and the
//! instruction beside it are stored in opposite orders.
//!
//! What the split puts at risk, one test each: the register space follows the
//! sla (`register_endianness`) while loads and stores follow the data order
//! (`endianness`), and a sub-register landing in the wrong half of its
//! container is a silent miscompile rather than a decode failure.

use object::{Object, ObjectSymbol};
use std::path::PathBuf;
use strider_ir::node::{IntBinaryOp, NodeId, NodeKind, ValueId};
use strider_ir::{Function, IRViewer, IRWalker};
use strider_target::{CallingConvention, SleighArch};

/// Optimiser depth: the whole default pipeline, or `LoadReadOnly` alone so a
/// folded read is still visible as its own constant.
#[derive(Clone, Copy)]
enum Opt {
    Full,
    LoadReadOnlyOnly,
}

fn analyze(case: &str, fn_name: &str, opt: Opt) -> Function {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/arm_be8")
        .join(format!("{case}.elf"));
    assert!(
        path.exists(),
        "missing {path:?}; run `make -C fixtures ARCH=arm_be8`"
    );
    let owned = strider_reader::load_elf(&path).expect("load_elf");
    assert!(
        owned.is_arm_be8().expect("e_flags"),
        "{path:?} must carry EF_ARM_BE8, else it is a BE32 image and this \
         whole file tests nothing"
    );
    let obj = owned.checked_file().expect("the mapped file is unchanged");
    let arch = SleighArch::arm_be_kernel();
    let mem = strider_reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem).expect("sleigh");
    let mut lifter = strider_orchestrator::Lifter::new(arch, sleigh).expect("lifter");
    let cc = CallingConvention::arm_aapcs()
        .build(lifter.sleigh_regs())
        .expect("cc");
    let addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not in {path:?}"))
        .address();
    let cfg_opts = strider_cfg::CfgOptions {
        allow_code_before_start_addr: true,
        ..Default::default()
    };
    let cfg = lifter
        .build_cfg(
            strider_cfg::MachineInsnAddr::from(addr),
            &cfg_opts,
            &Default::default(),
        )
        .unwrap_or_else(|e| panic!("build_cfg {fn_name}: {e:?}"));
    let mut function = lifter
        .build_ir(&cfg, cc)
        .unwrap_or_else(|e| panic!("build_ir {fn_name}: {e:?}"))
        .function;
    let rom = strider_reader::ElfFileMemReader::from_object(&obj).expect("rom reader");
    let mut ctx = strider_orchestrator::opt::OptCtx::new(Some(&rom));
    match opt {
        Opt::Full => strider_orchestrator::opt::default_pipeline()
            .run(&mut function, &mut ctx)
            .expect("pipeline"),
        Opt::LoadReadOnlyOnly => {
            strider_orchestrator::opt::run_one(
                &strider_orchestrator::opt::LoadReadOnly,
                &mut function,
                &mut ctx,
            )
            .expect("LoadReadOnly");
        }
    }
    function
}

fn initial_value(function: &Function, reg: &str) -> ValueId {
    let vn = SleighArch::arm_be_kernel()
        .probe_regs()
        .expect("probe regs")
        .name_to_vn(reg)
        .unwrap_or_else(|| panic!("no register named {reg:?}"));
    function
        .initial_var_value(&vn)
        .unwrap_or_else(|| panic!("{reg} is not a tracked initial varnode"))
}

/// The constant operand of a two-input node, whichever slot holds it.
fn const_operand(function: &Function, node: NodeId) -> Option<u128> {
    (0..2).find_map(|i| function.int_const_u128(function.nth_input(node, i)?))
}

/// The `InitialVar` and bit offset within it that `value`'s bit `bit` reads,
/// unwrapping the bit-cast, width-cast and container read / write chain the
/// lifter wraps around every sub-register access. `None` once the chain leaves
/// that shape.
fn initial_var_bit(function: &Function, value: ValueId, bit: u64) -> Option<(NodeId, u64)> {
    let node = function.producer(value);
    let input = |i| function.nth_input(node, i);
    match function.node_kind(node) {
        NodeKind::InitialVar(_) => Some((node, bit)),
        NodeKind::IntBitsToFloat
        | NodeKind::FloatBitsToInt
        | NodeKind::Truncate
        | NodeKind::Extend(_) => initial_var_bit(function, input(0)?, bit),
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftRight) => {
            let k = u64::try_from(const_operand(function, node)?).ok()?;
            initial_var_bit(function, input(0)?, bit.checked_add(k)?)
        }
        NodeKind::IntBinaryOp(IntBinaryOp::ShiftLeft) => {
            let k = u64::try_from(const_operand(function, node)?).ok()?;
            initial_var_bit(function, input(0)?, bit.checked_sub(k)?)
        }
        // The preserve half of a container write: the bit has to survive the mask.
        NodeKind::IntBinaryOp(IntBinaryOp::And) => {
            let mask = const_operand(function, node)?;
            if bit >= 128 || (mask >> bit) & 1 == 0 {
                return None;
            }
            let src = (0..2)
                .filter_map(input)
                .find(|&v| function.int_const_u128(v).is_none())?;
            initial_var_bit(function, src, bit)
        }
        // A container write: exactly one arm can own any one bit.
        NodeKind::IntBinaryOp(IntBinaryOp::Or) => {
            let mut found = (0..2).filter_map(|i| initial_var_bit(function, input(i)?, bit));
            let first = found.next()?;
            found.next().is_none().then_some(first)
        }
        _ => None,
    }
}

/// The float operand of `node` that is not the `IntBitsToFloat` of `bits`.
fn other_float_operand(function: &Function, node: NodeId, bits: u128) -> Option<ValueId> {
    let is_lit = |v: ValueId| {
        matches!(function.kind_of_value(v), NodeKind::IntBitsToFloat)
            && function
                .nth_input(function.producer(v), 0)
                .and_then(|b| function.int_const_u128(b))
                == Some(bits)
    };
    let a = function.nth_input(node, 0)?;
    let b = function.nth_input(node, 1)?;
    match (is_lit(a), is_lit(b)) {
        (true, false) => Some(b),
        (false, true) => Some(a),
        _ => None,
    }
}

fn load_nodes(function: &Function) -> Vec<NodeId> {
    function
        .walk()
        .filter(|&n| matches!(function.node_kind(n), NodeKind::Load(_)))
        .collect()
}

/// ```text
/// 100b8 <f32_arith>:                          # ((a + b) * a - b) / (a + 1.0f)
///   100c4: ed0b0a02  vstr    s0, [fp, #-8]    # a
///   100c8: ed4b0a03  vstr    s1, [fp, #-12]   # b
///   ...
///   100fc: eeb00a67  vmov.f32 s0, s15         # the return value
/// ```
///
/// `s0` is `d0`'s LOW half and `s1` its high half under the little-endian
/// branch of `ARM.sinc`, which is the branch `SLA_SPEC_ARM8_LE` compiled. The
/// divisor `a + 1.0f` names the first float argument unambiguously, so
/// resolving it back through the container chain says which half the lifter
/// read `s0` out of; keying the shift off the data order instead would answer
/// bit 32.
#[test]
fn a_float_argument_reads_the_half_of_its_container_that_the_sla_names() {
    const ONE_F32: u128 = 0x3f80_0000;
    let function = analyze("floats", "f32_arith", Opt::Full);
    let d0 = function.producer(initial_value(&function, "d0"));

    let divisor = function
        .walk()
        .filter(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::FloatBinaryOp(strider_ir::FloatBinaryOp::Add)
            )
        })
        .find_map(|n| other_float_operand(&function, n, ONE_F32))
        .expect("the `a + 1.0f` divisor");

    assert_eq!(
        initial_var_bit(&function, divisor, 0),
        Some((d0, 0)),
        "`a` is s0, the low half of d0; bit 32 would mean the sub-register \
         shift followed the data order rather than the sla's"
    );
}

/// The return value goes back into `s0`, so the write preserves `d0`'s HIGH
/// half. The complementary mask is what a big-endian register layout would
/// build, and a wrong preserve mask is a silent miscompile: the lift still
/// succeeds and still returns a float.
#[test]
fn a_sub_register_write_preserves_the_half_the_sla_names() {
    let function = analyze("floats", "f32_arith", Opt::Full);
    let d0 = initial_value(&function, "d0");
    let masks: Vec<u128> = function
        .value_uses(d0)
        .filter(|&(n, _)| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::And)
            )
        })
        .filter_map(|(n, _)| const_operand(&function, n))
        .collect();
    assert_eq!(
        masks,
        vec![0xffff_ffff_0000_0000u128],
        "writing s0 must preserve d0's high half"
    );
}

/// ```text
/// 100f4 <branch_on_const_string>:             # if (k_str[0] == 'y') a + b; else a - b;
///   10108: e59f3034  ldr     r3, [pc, #52]    # 10144
///   1010c: e5d33000  ldrb    r3, [r3]
///   10110: e3530079  cmp     r3, #121         # 'y'
///   ...
///   10144: 00010220  .word   0x00010220       # -> k_str
/// ```
///
/// Raw `.text` bytes: `1eff2fe1` at 0x10140 (`bx lr`, little-endian) then
/// `00010220` at 0x10144 (big-endian). `ld --be8` reversed the instruction and
/// not the pool word, so reading the word with the instruction order yields
/// 0x20020100, which maps nowhere and folds nothing.
#[test]
fn a_literal_pool_word_reads_big_endian_between_little_endian_instructions() {
    let function = analyze("globals", "branch_on_const_string", Opt::LoadReadOnlyOnly);
    let folded: Vec<u128> = function
        .walk()
        .filter_map(|n| function.first_value_output_of(n))
        .filter_map(|v| function.int_const_u128(v))
        .collect();
    assert!(
        folded.contains(&0x0001_0220),
        "the pool word must fold to its big-endian value; got {folded:x?}"
    );
    assert!(
        !folded.contains(&0x2002_0100),
        "0x20020100 is the pool word read in the instruction byte order"
    );
}

/// The folded condition is constant, so the whole function collapses to
/// `a + b`: no `If`, and no `Neg` from the `a - b` arm.
#[test]
fn the_folded_constant_condition_leaves_one_arm() {
    let function = analyze("globals", "branch_on_const_string", Opt::Full);
    let sum = function
        .walk()
        .find(|&n| {
            matches!(
                function.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            ) && function
                .node_inputs(n)
                .into_iter()
                .all(|v| matches!(function.kind_of_value(v), NodeKind::InitialVar(_)))
        })
        .expect("`a + b` over the two argument registers");
    // Add is commutative, so the slots carry no order.
    let operands: Vec<ValueId> = function.node_inputs(sum).into_iter().collect();
    for reg in ["r0", "r1"] {
        let arg = initial_value(&function, reg);
        assert!(
            operands.contains(&arg),
            "{reg} must be an operand of `a + b`"
        );
    }
    assert!(
        !function.walk().any(|n| matches!(
            function.node_kind(n),
            NodeKind::If | NodeKind::IntUnaryOp(strider_ir::IntUnaryOp::Neg)
        )),
        "the `a - b` arm must be gone"
    );
}

/// ```text
/// 10218 <pointer_chase>:                      # return **p;
///   10224: e50b0008  str     r0, [fp, #-8]
///   10228: e51b3008  ldr     r3, [fp, #-8]
///   1022c: e5933000  ldr     r3, [r3]
///   10230: e5933000  ldr     r3, [r3]
/// ```
///
/// The spill / reload pair round-trips, leaving the two real dereferences and
/// nothing else: a third `Load` would mean the reload did not see the store.
#[test]
fn a_spill_and_its_reload_round_trip() {
    let function = analyze("memory", "pointer_chase", Opt::Full);
    let loads = load_nodes(&function);
    assert_eq!(loads.len(), 2, "expected exactly the two dereferences");
    let r0 = initial_value(&function, "r0");
    let inner = loads
        .iter()
        .copied()
        .find(|&n| function.nth_input(n, 1) == Some(r0))
        .expect("a dereference of the argument register the spill held");
    let outer = loads.into_iter().find(|&n| n != inner).expect("the other");
    assert_eq!(
        function.nth_input(outer, 1),
        function.first_value_output_of(inner),
        "the outer dereference reads the inner one's result"
    );
}
