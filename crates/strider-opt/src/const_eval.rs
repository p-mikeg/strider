use strider_ir::node::{ExtendOp, NodeId, NodeKind, ValueId, ValueType, WidthTie, width_tie};
use strider_ir::{Function, IRViewer, ReadOnlyMemory};
use strider_target::Endianness;

use strider_ir::UBig;
use strider_ir::eval;

/// Decodes `ty` bytes at `addr` into an integer masked to `ty`. `None` for
/// widths > 16 bytes, an unmapped read, or a non-integer `ty`.
fn read_rom_const(
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

/// Right-shift in bits that brings the `load_ty`-width slice a load in
/// `space` reads at the start of a wider `store_ty` value into the low end,
/// in that space's byte order: [`strider_target::Endianness::slice_shift_bits`]
/// at offset 0. `None` for a load wider than the store.
pub(crate) fn slice_shift_bits(
    order: strider_target::ByteOrder,
    space: rsleigh::VnSpace,
    store_ty: ValueType,
    load_ty: ValueType,
) -> Option<u32> {
    let at = |ty: ValueType| rsleigh::Vn {
        addr_space: space,
        addr_off: 0,
        size: u32::try_from(ty.byte_size()).unwrap_or(u32::MAX),
    };
    let bits = order
        .of(space)
        .slice_shift_bits(&at(load_ty), &at(store_ty))?;
    u32::try_from(bits).ok()
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
    let ty = |v: ValueId| function.value_type_opt(v);
    let first = ty(*operands.first()?)?;
    operands
        .iter()
        .enumerate()
        .all(
            |(slot, &v)| match width_tie(kind, slot, Some(out), Some(first)) {
                WidthTie::Tied(want) => ty(v).map(ValueType::bit_width) == Some(want.bit_width()),
                WidthTie::Conversion(_) | WidthTie::Free | WidthTie::NoHead => true,
            },
        )
        .then_some(())
}

/// The constant value of `value`, given `resolve` for its inputs' constants.
/// `None` for anything not foldable to one integer constant. Does NOT handle
/// `Phi` or any stack-relative address.
pub(crate) fn eval_node_const(
    function: &Function,
    value: ValueId,
    resolve: &dyn Fn(ValueId) -> Option<UBig>,
    rom: Option<&dyn ReadOnlyMemory>,
) -> Option<UBig> {
    let node = function.producer(value);
    let kind = *function.node_kind(node);
    let out_ty = function.value_type_opt(value);
    let ins = function.node_inputs(node);
    match kind {
        NodeKind::IntConst(_) => function.int_const(value).cloned(),
        NodeKind::IntBinaryOp(op) => {
            let ty = out_ty?;
            let (l, r) = (ins.get(0).copied()?, ins.get(1).copied()?);
            operands_carry(function, &kind, &[l, r], ty)?;
            eval::int_binary(op, &resolve(l)?, &resolve(r)?, ty)
        }
        NodeKind::IntUnaryOp(op) => {
            let ty = out_ty?;
            let v = ins.get(0).copied()?;
            // Checked like the binary arm: `IntUnaryOp` pins its operand to
            // the output width, and `validate` rejects a node that does not,
            // but this folds mid-pipeline where nothing has validated yet.
            operands_carry(function, &kind, &[v], ty)?;
            eval::int_unary(op, &resolve(v)?, ty)
        }
        NodeKind::Truncate | NodeKind::Extend(ExtendOp::ZeroExtend) => {
            // `eval::mask` answers 0 for a float, so the refusal is explicit.
            let ty = out_ty.filter(|t| t.is_integer())?;
            Some(eval::mask(&resolve(ins.get(0).copied()?)?, ty))
        }
        NodeKind::Extend(ExtendOp::SignExtend) => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval::sign_extend(&resolve(ins[0])?, in_ty, out_ty?)
        }
        NodeKind::Popcount => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval::popcount(&resolve(ins[0])?, in_ty)
        }
        NodeKind::Lzcount => {
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            eval::lzcount(&resolve(ins[0])?, in_ty)
        }
        NodeKind::IntCmpOp(op) => {
            let (l, r) = (ins.get(0).copied()?, ins.get(1).copied()?);
            let in_ty = function.value_type_opt(l)?;
            operands_carry(function, &kind, &[l, r], in_ty)?;
            Some(UBig::from(u8::from(eval::int_cmp(
                op,
                &resolve(l)?,
                &resolve(r)?,
                in_ty,
            )?)))
        }
        // Only a RAM load reads the image.  A register / unique / const-space
        // load names a different address space.  A nested load reaches here
        // from `post_opt::indirect_branch_resolve::eval`, whose `resolve` reads
        // the evaluator's memo; `load_readonly` passes a `resolve` that never
        // recurses into another node.
        NodeKind::Load(space) if space == rsleigh::VnSpace::RAM => {
            let rom = rom?;
            let addr = u64::try_from(&resolve(function.load_addr(node))?).ok()?;
            read_rom_const(rom, addr, out_ty?, function.endianness()).map(UBig::from)
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
    use strider_ir::UBig;
    use strider_ir::node::ValueType;
    use strider_ir::{IRBuilderExt, IRViewer};

    #[test]
    fn folds_add_of_two_constants() {
        let (function, sum) = build_add_5_100();
        let resolve = |v| function.int_const(v).cloned();
        let got = eval_node_const(&function, sum, &resolve, None);
        assert_eq!(got, Some(UBig::from(105u8)));
    }

    /// `(2^256 - 1) + 2` at I256 is 1: the fold masks at the declared width,
    /// not at a 128-bit carrier, where the same sum would be 1 for the wrong
    /// reason and the operands would not even be readable.
    #[test]
    fn folds_an_i256_add_that_wraps_at_256_bits() {
        use strider_ir::IntBinaryOp;
        use strider_ir_test_utils::make_empty_fn;
        let ty = ValueType::int(256);
        let mut built = None;
        let function = make_empty_fn(|b| {
            let near_max = b.build_int_const((UBig::ONE << 256usize) - UBig::from(3u8), ty)?;
            let two = b.build_int_const(2u8, ty)?;
            let sum = b.build_int_binary_operation(near_max, two, IntBinaryOp::Add, ty)?;
            let wrapped = b.build_int_binary_operation(sum, two, IntBinaryOp::Add, ty)?;
            built = Some((sum, wrapped));
            Ok(wrapped)
        })
        .expect("make_empty_fn");
        let (sum, wrapped) = built.expect("built");
        let resolve = |v| function.int_const(v).cloned();
        assert_eq!(
            eval_node_const(&function, sum, &resolve, None),
            Some((UBig::ONE << 256usize) - UBig::ONE),
            "no wrap yet"
        );
        // `resolve` only reads constants, so feed the inner sum back by hand.
        let outer = |v| {
            if v == sum {
                Some((UBig::ONE << 256usize) - UBig::ONE)
            } else {
                function.int_const(v).cloned()
            }
        };
        assert_eq!(
            eval_node_const(&function, wrapped, &outer, None),
            Some(UBig::ONE),
            "(2^256 - 1) + 2 wraps to 1 at I256"
        );
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
        let resolve = |v| function.int_const(v).cloned();

        assert_eq!(
            eval_node_const(&function, ram, &resolve, Some(&rom)),
            Some(UBig::from(0xDEADu16)),
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
