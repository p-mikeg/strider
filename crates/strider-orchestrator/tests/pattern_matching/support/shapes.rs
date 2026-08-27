//! A shape lives here iff at least two test modules need it; single-use shapes
//! stay inline.

use strider_ir::{Function, IntCmpOp};

use strider_ir_test_utils::{Tb, reg_vn, stack_vn_x86_64 as stack_vn};

// Shared with the other crate's copy of this module, so they live in the
// dev-dependency both already use.
pub(crate) use strider_ir_test_utils::{if_cmp_then_return, single_initial_var};

/// Compiler-inverted equivalent of [`if_cmp_then_return`]: same source
/// program, but the cond is wrapped in a negation and the branches swapped,
/// so the literal IR shape is `if (!(c == 1)) { return 20 } else { return 10 }`.
pub(crate) fn if_cmp_then_return_inverted(c: u64) -> Function {
    let mut t = Tb::bare(vec![], &[], &[], &[], None, 0);
    let entry = t.region();
    let true_r = t.region();
    let false_r = t.region();
    t.set_entry(entry);

    t.enter(true_r);
    let twenty = t.u64(20);
    t.fb_mut()
        .build_return(Some(twenty), &[])
        .expect("build_return");

    t.enter(false_r);
    let ten = t.u64(10);
    t.fb_mut()
        .build_return(Some(ten), &[])
        .expect("build_return");

    t.enter(entry);
    let c_node = t.u64(c);
    let one = t.u64(1);
    let inner = t.int_cmp(c_node, one, IntCmpOp::Equal);
    let cond = t.bool_not(inner);
    t.build_if(cond, true_r, false_r);
    t.finish()
}

/// Graph that, after `opt::FunctionArgDetect`, has `reg` registered as the
/// carrier for arg 0 in `SideTables::arg_index_to_values`. The underlying
/// `InitialVar(reg)` node stays in place.
pub(crate) fn function_arg_reg() -> (Function, rsleigh::Vn) {
    use strider_orchestrator::opt::{FunctionArgDetect, Optimizer};
    let reg = reg_vn(0x38, 8);
    let sp = stack_vn();
    // The pass reads its arg layout from the function's own CC, so the
    // fixture must carry `reg` as an arg-passing register and `sp` as the SP.
    let mut t = Tb::raw(vec![reg, sp], &[reg], &[reg], &[reg], Some(sp), 0);
    let v = t.read_var(&reg);
    let mut function = t.ret_val(v);
    strider_orchestrator::opt::run_post(
        &FunctionArgDetect,
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )
    .expect("FunctionArgDetect");
    (function, reg)
}
