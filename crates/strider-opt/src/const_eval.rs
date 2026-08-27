use strider_ir::node::{ExtendOp, NodeKind, ValueId, ValueType};
use strider_ir::{Function, IRViewer, ReadOnlyMemory};
use strider_target::Endianness;

use crate::opt::constant_fold::eval_int::{
    eval_int_binary, eval_int_cmp, eval_int_unary, eval_lzcount, eval_popcount, eval_sign_extend,
};

/// Decodes `ty` bytes at `addr` into an integer masked to `ty`. `None` for
/// widths > 16 bytes, an unmapped read, or a non-integer `ty`.
pub(crate) fn read_rom_const(
    rom: &dyn ReadOnlyMemory,
    addr: u64,
    ty: ValueType,
    endianness: Endianness,
) -> Option<u128> {
    let size = ty.byte_size();
    if size > 16 {
        return None;
    }
    let mut bytes = [0u8; 16];
    rom.read(addr, &mut bytes[..size]).ok()?;
    let loaded = endianness.read_uint(&bytes[..size]);
    ty.get_unsigned_int(loaded)
}

/// `None` unless every operand already carries `expected`'s bit width.
///
/// The folds below mask all operands to one width, which is only correct when
/// they already carry it. `check_function_invariants_arith_widths` covers the
/// arithmetic operands; a shift COUNT is exempt there and masked here.
/// Declining is the safe direction in this evaluator in particular: its answer
/// becomes a decoded branch target.
fn operands_carry(function: &Function, operands: &[ValueId], expected: ValueType) -> Option<()> {
    let bits = expected.bit_width();
    operands
        .iter()
        .all(|v| {
            function
                .value_type_opt(*v)
                .is_some_and(|t| t.bit_width() == bits)
        })
        .then_some(())
}

/// The constant value of `value`, given `resolve` for its inputs' constants.
/// `None` for anything not foldable to one integer constant. Does NOT handle
/// `Phi` or any stack-relative address.
pub(crate) fn eval_node_const(
    function: &Function,
    value: ValueId,
    resolve: &dyn Fn(ValueId) -> Option<u128>,
    rom: Option<&dyn ReadOnlyMemory>,
) -> Option<u128> {
    let node = function.producer(value);
    let kind = *function.node_kind(node);
    let out_ty = function.value_type_opt(value);
    let ins = function.node_inputs(node);
    match kind {
        NodeKind::IntConst(_) => function.int_const_u128(value),
        NodeKind::IntBinaryOp(op) => {
            let ty = out_ty?;
            let (l, r) = (ins.get(0).copied()?, ins.get(1).copied()?);
            operands_carry(function, &[l, r], ty)?;
            eval_int_binary(op, resolve(l)?, resolve(r)?, ty)
        }
        NodeKind::IntUnaryOp(op) => eval_int_unary(op, resolve(ins.get(0).copied()?)?, out_ty?),
        NodeKind::Truncate | NodeKind::Extend(ExtendOp::ZeroExtend) => {
            out_ty?.get_unsigned_int(resolve(ins.get(0).copied()?)?)
        }
        NodeKind::Extend(ExtendOp::SignExtend) => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval_sign_extend(resolve(ins[0])?, in_ty, out_ty?)
        }
        NodeKind::Popcount => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval_popcount(resolve(ins[0])?, in_ty)
        }
        NodeKind::Lzcount => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval_lzcount(resolve(ins[0])?, in_ty)
        }
        NodeKind::IntCmpOp(op) => {
            let (l, r) = (ins.get(0).copied()?, ins.get(1).copied()?);
            let in_ty = function.value_type_opt(l)?;
            operands_carry(function, &[l, r], in_ty)?;
            Some(u128::from(eval_int_cmp(
                op,
                resolve(l)?,
                resolve(r)?,
                in_ty,
            )?))
        }
        // Only a RAM load reads the image.  A register / unique / const-space
        // load names a different address space, and the root gate in
        // `load_readonly` checks the root alone, so a nested one reaches here.
        NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
            let rom = rom?;
            let addr = u64::try_from(resolve(function.load_addr(node))?).ok()?;
            read_rom_const(rom, addr, out_ty?, function.endianness())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::eval_node_const;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer};

    #[test]
    fn folds_add_of_two_constants() {
        let (function, sum) = build_add_5_100();
        let resolve = |v| function.int_const_u128(v);
        let got = eval_node_const(&function, sum, &resolve, None);
        assert_eq!(got, Some(105));
    }

    /// The ROM answers for RAM. A register-space load names a different address
    /// space, so folding it against the image would read the wrong memory; the
    /// root gate in `load_readonly` checks the root only, so a nested one
    /// arrives here.
    #[test]
    fn a_non_ram_load_is_not_folded_against_the_rom() {
        use strider_ir_test_utils::{MockRom, make_empty_fn};
        let mut loads = None;
        let function = make_empty_fn(|b| {
            let addr = b.build_int_const(0x2000u64, ValueType::I64)?;
            let ram = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::I64)?;
            let reg = b.build_load(addr, rsleigh::VnSpace::REGISTER, ValueType::I64)?;
            loads = Some((ram, reg));
            Ok(ram)
        })
        .expect("make_empty_fn");
        let (ram, reg) = loads.expect("loads set");
        let rom = MockRom::fixed_table(&[(0x2000, 0xDEAD)]);
        let resolve = |v| function.int_const_u128(v);

        assert_eq!(
            eval_node_const(&function, ram, &resolve, Some(&rom)),
            Some(0xDEAD),
            "a RAM load still folds"
        );
        assert_eq!(
            eval_node_const(&function, reg, &resolve, Some(&rom)),
            None,
            "a register-space load must not read the image"
        );
    }

    fn build_add_5_100() -> (strider_ir::Function, strider_ir::node::ValueId) {
        use strider_ir::IntBinaryOp;
        use strider_ir_test_utils::make_empty_fn;
        let mut sum_val = None;
        let function = make_empty_fn(|b| {
            let c5 = b
                .build_int_const(5u64, ValueType::I64)
                .expect("build_int_const");
            let c100 = b
                .build_int_const(100u64, ValueType::I64)
                .expect("build_int_const");
            let sum = b
                .build_int_binary_operation(c5, c100, IntBinaryOp::Add, ValueType::I64)
                .expect("build_int_binary_operation");
            sum_val = Some(sum);
            Ok(sum)
        })
        .expect("make_empty_fn");
        (function, sum_val.expect("sum_val set"))
    }
}
