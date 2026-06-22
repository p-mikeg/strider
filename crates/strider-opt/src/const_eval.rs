//! Shared "what constant does this node produce from constant inputs" SSoT,
//! used by the jump-table abstract evaluator and by `LoadReadOnly` so the ROM
//! decode and the per-op fold dispatch live in exactly one place. ConstFold
//! shares the leaf arithmetic (`eval_int_*`) directly and does not route
//! through here.

use strider_ir::Function;
use strider_ir::IRViewer;
use strider_ir::ReadOnlyMemory;
use strider_ir::node::{ExtendOp, NodeKind, ValueId, ValueType};
use strider_target::Endianness;

use crate::opt::constant_fold::eval_int::{
    eval_int_binary, eval_int_cmp, eval_int_unary, eval_lzcount, eval_popcount, eval_sign_extend,
};

/// Decode `ty` bytes at `addr` from a read-only image into an integer masked to
/// `ty`. The single ROM-decode site. `None` for widths > 16 bytes, an unmapped
/// read, or a non-integer `ty`.
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

/// The constant value of `value`, given `resolve` for its inputs' constants.
/// Covers `IntConst`, integer arithmetic/casts/compares (via the shared
/// `eval_int_*` helpers), and a constant-address `Load(RAM)` (via
/// [`read_rom_const`]). `None` for anything not foldable to one integer
/// constant from `resolve`d inputs. Does NOT handle `Phi` or any
/// stack-relative address — those stay in the jump-table evaluator.
pub(crate) fn eval_node_const(
    function: &Function,
    value: ValueId,
    resolve: &dyn Fn(ValueId) -> Option<u128>,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: Endianness,
) -> Option<u128> {
    let node = function.producer(value);
    let kind = *function.node_kind(node);
    let out_ty = function.value_type_opt(value);
    let ins = function.node_inputs(node);
    match kind {
        NodeKind::IntConst(_) => function.int_const_u128(value),
        NodeKind::IntBinaryOp(op) => {
            eval_int_binary(op, resolve(ins.get(0).copied()?)?, resolve(ins.get(1).copied()?)?, out_ty?)
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
            let in_ty = function.value_type_opt(ins.get(0).copied()?)?;
            Some(u128::from(
                eval_int_cmp(op, resolve(ins[0])?, resolve(ins.get(1).copied()?)?, in_ty).ok()?,
            ))
        }
        NodeKind::Load(_) => {
            let rom = rom?;
            let addr = u64::try_from(resolve(function.load_addr(node))?).ok()?;
            read_rom_const(rom, addr, out_ty?, endianness)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::eval_node_const;
    use strider_ir::IRBuilderExt;
    use strider_ir::IRViewer;
    use strider_ir::node::ValueType;

    // Build `Add(IntConst(5), IntConst(100)):I64`; eval_node_const with an
    // int-const resolver folds it to 105. Copy the builder pattern from
    // constant_fold/tests.rs.
    #[test]
    fn folds_add_of_two_constants() {
        let (function, sum) = build_add_5_100();
        let resolve = |v| function.int_const_u128(v);
        let got = eval_node_const(&function, sum, &resolve, None, function.endianness());
        assert_eq!(got, Some(105));
    }

    fn build_add_5_100() -> (strider_ir::Function, strider_ir::node::ValueId) {
        use strider_ir::IntBinaryOp;
        use strider_ir_test_utils::make_empty_fn;
        let mut sum_val = None;
        let function = make_empty_fn(|b| {
            let c5 = b.build_int_const(5u64, ValueType::I64).expect("build_int_const");
            let c100 = b.build_int_const(100u64, ValueType::I64).expect("build_int_const");
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
