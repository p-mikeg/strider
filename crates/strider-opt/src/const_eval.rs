use strider_ir::node::{ExtendOp, NodeId, NodeKind, OperandWidth, ValueId, ValueType};
use strider_ir::{Function, IRViewer, ReadOnlyMemory};
use strider_target::Endianness;

use strider_ir::eval;

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

/// Right-shift in bits that brings the big-endian `load_ty`-width slice at
/// the start of a wider `store_ty` integer into the low end. A little-endian
/// slice is low already.
///
/// `load_ty` must be no wider than `store_ty`, else the subtraction underflows.
#[inline]
pub(crate) fn big_endian_slice_shift_bits(store_ty: ValueType, load_ty: ValueType) -> u32 {
    (store_ty.byte_size() - load_ty.byte_size()) as u32 * 8
}

/// Whether every operand carries the width [`NodeKind::operand_width`] says
/// `kind` evaluates it at: the output's for `OfOutput`, operand 0's for
/// `OfFirstOperand`. A fold masks those operands to that width, which is only
/// correct when they already carry it; a free slot, such as a shift count, is
/// read at its own width.
fn operands_carry(
    function: &Function,
    kind: &NodeKind,
    operands: &[ValueId],
    out: ValueType,
) -> Option<()> {
    let width = |v: ValueId| function.value_type_opt(v).map(ValueType::bit_width);
    let first = width(*operands.first()?)?;
    operands
        .iter()
        .enumerate()
        .all(|(slot, &v)| {
            let want = match kind.operand_width(slot) {
                OperandWidth::OfOutput => out.bit_width(),
                OperandWidth::OfFirstOperand => first,
                OperandWidth::Conversion(_) | OperandWidth::Free => return true,
            };
            width(v) == Some(want)
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
            operands_carry(function, &kind, &[l, r], ty)?;
            eval::int_binary(op, resolve(l)?, resolve(r)?, ty)
        }
        NodeKind::IntUnaryOp(op) => {
            let ty = out_ty?;
            let v = ins.get(0).copied()?;
            // Checked like the binary arm: `IntUnaryOp` pins its operand to
            // the output width, and `validate` rejects a node that does not,
            // but this folds mid-pipeline where nothing has validated yet.
            operands_carry(function, &kind, &[v], ty)?;
            eval::int_unary(op, resolve(v)?, ty)
        }
        NodeKind::Truncate | NodeKind::Extend(ExtendOp::ZeroExtend) => {
            out_ty?.get_unsigned_int(resolve(ins.get(0).copied()?)?)
        }
        NodeKind::Extend(ExtendOp::SignExtend) => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval::sign_extend(resolve(ins[0])?, in_ty, out_ty?)
        }
        NodeKind::Popcount => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval::popcount(resolve(ins[0])?, in_ty)
        }
        NodeKind::Lzcount => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval::lzcount(resolve(ins[0])?, in_ty)
        }
        NodeKind::IntCmpOp(op) => {
            let (l, r) = (ins.get(0).copied()?, ins.get(1).copied()?);
            let in_ty = function.value_type_opt(l)?;
            operands_carry(function, &kind, &[l, r], in_ty)?;
            Some(u128::from(eval::int_cmp(
                op,
                resolve(l)?,
                resolve(r)?,
                in_ty,
            )?))
        }
        // Only a RAM load reads the image.  A register / unique / const-space
        // load names a different address space.  A nested load reaches here
        // from `post_opt::indirect_branch_resolve::eval`, whose `resolve` reads
        // the evaluator's memo; `load_readonly` passes a `resolve` that never
        // recurses into another node.
        NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
            let rom = rom?;
            let addr = u64::try_from(resolve(function.load_addr(node))?).ok()?;
            read_rom_const(rom, addr, out_ty?, function.endianness())
        }
        _ => None,
    }
}

/// `(x, c)` for a two-operand node with exactly one operand an integer
/// constant that fits `u128`, `c` its value.
pub(crate) fn var_and_const(function: &Function, node: NodeId) -> Option<(ValueId, u128)> {
    let [a, b] = function.node_inputs_exact::<2>(node).ok()?;
    match (function.int_const_u128(a), function.int_const_u128(b)) {
        (None, Some(c)) => Some((a, c)),
        (Some(c), None) => Some((b, c)),
        _ => None,
    }
}

/// `(x, c)` for `Add(x, c)` with `c` a constant, read signed at `x`'s width.
pub(crate) fn const_addend(function: &Function, add: NodeId) -> Option<(ValueId, i128)> {
    let (x, c) = var_and_const(function, add)?;
    Some((x, function.value_type_opt(x)?.get_signed_int(c)?))
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
            let addr = b.build_int_const(0x2000u64, ValueType::int(64))?;
            let ram = b.build_load(addr, rsleigh::VnSpace::RAM, ValueType::int(64))?;
            let reg = b.build_load(addr, rsleigh::VnSpace::REGISTER, ValueType::int(64))?;
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

    /// The split names the variable whichever side the constant sits on, and
    /// takes no side when both operands, or neither, are constants.
    #[test]
    fn var_and_const_needs_exactly_one_constant() {
        use strider_ir::IntBinaryOp;
        use strider_ir_test_utils::make_empty_fn;
        let ty = ValueType::int(64);
        let mut built = None;
        let function = make_empty_fn(|b| {
            let addr = b.build_int_const(0x2000u64, ty)?;
            let x = b.build_load(addr, rsleigh::VnSpace::RAM, ty)?;
            let c = b.build_int_const(8u64, ty)?;
            let d = b.build_int_const(16u64, ty)?;
            let mut add = |l, r| b.build_int_binary_operation(l, r, IntBinaryOp::Add, ty);
            let sums = [add(x, c)?, add(c, x)?, add(c, d)?, add(x, x)?];
            built = Some((x, sums));
            sums.into_iter().try_fold(x, add)
        })
        .expect("make_empty_fn");
        let (x, [xc, cx, cc, xx]) = built.expect("built");
        let split = |v| super::var_and_const(&function, function.producer(v));
        assert_eq!(split(xc), Some((x, 8)));
        assert_eq!(split(cx), Some((x, 8)));
        assert_eq!(split(cc), None, "two constants leave no variable side");
        assert_eq!(split(xx), None, "no constant side");
    }

    fn build_add_5_100() -> (strider_ir::Function, strider_ir::node::ValueId) {
        use strider_ir::IntBinaryOp;
        use strider_ir_test_utils::make_empty_fn;
        let mut sum_val = None;
        let function = make_empty_fn(|b| {
            let c5 = b
                .build_int_const(5u64, ValueType::int(64))
                .expect("build_int_const");
            let c100 = b
                .build_int_const(100u64, ValueType::int(64))
                .expect("build_int_const");
            let sum = b
                .build_int_binary_operation(c5, c100, IntBinaryOp::Add, ValueType::int(64))
                .expect("build_int_binary_operation");
            sum_val = Some(sum);
            Ok(sum)
        })
        .expect("make_empty_fn");
        (function, sum_val.expect("sum_val set"))
    }
}
