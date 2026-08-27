//! [`strider_opt::apply_rules_count`] driven through the orchestrator's
//! pipeline: rewrite, then re-optimize, against a Sleigh-lifted or hand-built
//! function.

use strider_ir::node::{NodeKind, ValueType};
use strider_ir::{Function, IRBuilderExt, IRViewer, IRWalker, IntBinaryOp};
use strider_ir_test_utils::IrWalkerEx;
use strider_opt::{EditFunction, apply_rules_count, rewrite_rule};
use strider_pattern::{Capture, int_add, int_const, var};

mod common;

fn count_switches(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::Switch))
}

fn find_unique_switch(function: &Function) -> strider_ir::node::NodeId {
    let mut iter = function
        .walk()
        .filter(|&nid| matches!(function.node_kind(nid), NodeKind::Switch));
    let first = iter
        .next()
        .expect("fixture must contain exactly one Switch node");
    assert!(
        iter.next().is_none(),
        "fixture has more than one Switch node"
    );
    first
}

/// Builds `fn() -> u64 { return Add(K, 0); }` without any Sleigh fixture.
fn add_k_plus_zero(k: u64) -> Function {
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let region = b.create_region_all().unwrap();
    b.set_entry_region_all(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let lhs = b.build_int_const(k, ValueType::I64).unwrap();
    let rhs = b.build_int_const(0u64, ValueType::I64).unwrap();
    let sum = b
        .build_int_binary_operation(lhs, rhs, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(sum), &[]).unwrap();
    b.set_lift_addr(None);
    b.build().unwrap()
}

fn count_adds(function: &Function) -> usize {
    function.count_kind(|k| matches!(k, NodeKind::IntBinaryOp(IntBinaryOp::Add)))
}

/// A 3-target `build_switch`-lifted dispatch produces one `NodeKind::Switch`
/// (inputs `[ctrl, address]`), never an if-ladder. `Switch` has no value
/// output, so a `rewrite_rule` can't root on it; rewriting the selector goes
/// through `EditFunction`'s low-level input-rewrite primitive instead.
#[test]
fn replace_switch_address_with_const_collapses_switch_after_reoptimize() -> anyhow::Result<()> {
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(3);
    let (mut g, _strider, _cc) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        common::count_ifs(&g),
        0,
        "3-target dispatch produces zero If nodes"
    );
    assert_eq!(
        count_switches(&g),
        1,
        "3-target dispatch lifts to exactly one Switch node"
    );
    let switch_id = find_unique_switch(&g);
    assert_eq!(
        g.node_outputs(switch_id).len(),
        3,
        "Switch has one Control output per target",
    );

    // Rewrite the Switch's `address` input (slot 1) to IntConst(K_0). The
    // displaced idx-read is an exempt InitialVar with no asm history, so
    // stamp the new const's fingerprint from the Switch node (which traces
    // to the real `jmp rax`) to satisfy the fingerprint check.
    let addr_use = g.node_input_id_at(switch_id, 1)?;
    {
        let mut ctx = EditFunction::new(&mut g);
        let k0 = ctx.build_int_const(targets[0], ValueType::I64)?;
        let k0_node = ctx.function().producer(k0);
        ctx.function_mut()
            .side_tables_mut()
            .extend_asm_fingerprint_from(k0_node, switch_id);
        ctx.redirect_input(addr_use, k0);
        ctx.clean();
    }

    let pipeline = strider_orchestrator::opt::default_pipeline();
    pipeline.run(&mut g, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    assert_eq!(
        count_switches(&g),
        0,
        "constant-address Switch collapses to its matching arm (DeadBranchElimination)",
    );
    assert_eq!(
        common::count_ifs(&g),
        0,
        "Switch-lowered dispatch never produces If nodes, even after the collapse",
    );
    strider_ir::validate::validate(&g).map_err(|e| {
        anyhow::anyhow!("assertion failed: validate failed after switch-address rewrite: {e}")
    })?;
    Ok(())
}

/// A rewrite rule shaped for an if-ladder (match any `Eq(_, K)`, replace it
/// with `bool_const(false)`) is a safe no-op against a `Switch`-lowered
/// dispatch: `handle_switch` emits a single `Switch` node, so the rule's
/// `IntCmpOp::Equal` root has nothing to bind to and the dispatch survives
/// re-optimizing unchanged.
#[test]
fn rewrite_rule_targeting_old_if_ladder_shape_is_a_no_op_against_switch_dispatch()
-> anyhow::Result<()> {
    let (bytes, base, ba, targets) = common::synth_jmp_rax_with_targets(3);
    let (mut g, _strider, _cc) = common::analyze_with_known_targets(&bytes, base, ba, &targets);
    assert_eq!(
        common::count_ifs(&g),
        0,
        "3-target Switch produces zero Ifs"
    );
    assert_eq!(
        count_switches(&g),
        1,
        "3-target dispatch lifts to exactly one Switch node"
    );

    let pipeline = strider_orchestrator::opt::default_pipeline();
    let rule_all_false = rewrite_rule(
        strider_pattern::int_eq(
            strider_pattern::anything(),
            strider_pattern::int_const(strider_pattern::Capture::new()),
        ),
        strider_pattern::bool_const(false),
    );
    let fired = {
        let mut ctx = EditFunction::new(&mut g);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule_all_false))?
    };
    assert_eq!(
        fired, 0,
        "an if-ladder-shaped rewrite rule must not match anything against a Switch-lowered dispatch",
    );
    pipeline.run(&mut g, &mut strider_orchestrator::opt::OptCtx::new(None))?;

    assert_eq!(
        count_switches(&g),
        1,
        "Switch node must be untouched by the no-op rewrite + re_optimize",
    );
    assert_eq!(
        common::count_ifs(&g),
        0,
        "Switch-lowered dispatch produces zero If nodes before or after the no-op rewrite",
    );
    Ok(())
}

#[test]
fn replace_input_then_reoptimize_then_replace_again_works() -> anyhow::Result<()> {
    // Two Adds; rewriting must still work after a re-optimize runs in between.
    let mut b = strider_ir_test_utils::empty_builder().unwrap();
    let region = b.create_region_all().unwrap();
    b.set_entry_region_all(region).unwrap();
    b.set_region(region);
    b.set_lift_addr(Some(strider_ir_test_utils::SENTINEL_LIFT_ADDR));
    let a = b.build_int_const(7u64, ValueType::I64).unwrap();
    let z = b.build_int_const(0u64, ValueType::I64).unwrap();
    let one = b.build_int_const(1u64, ValueType::I64).unwrap();
    let add1 = b
        .build_int_binary_operation(a, z, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    let add2 = b
        .build_int_binary_operation(add1, one, IntBinaryOp::Add, ValueType::I64)
        .unwrap();
    b.build_return(Some(add2), &[]).unwrap();
    b.set_lift_addr(None);
    let mut function = b.build().unwrap();

    assert_eq!(count_adds(&function), 2, "fixture has two Adds");

    let x = Capture::new();
    let rule_x_plus_zero = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    let pipeline = strider_orchestrator::opt::default_pipeline();

    let n1 = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule_x_plus_zero))?
    };
    assert_eq!(n1, 1, "first rewrite collapses Add(7,0)");
    // Propagates the folded constant through the second Add.
    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )?;

    // ConstantFold already collapsed Add(7,1) to IntConst(8); the rewrite
    // finds nothing but must still succeed.
    let n2 = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule_x_plus_zero))?
    };
    assert_eq!(
        n2, 0,
        "second rewrite finds no Add-by-zero patterns after re_optimize collapsed everything",
    );
    Ok(())
}

#[test]
fn re_optimize_without_changes_is_no_op() -> anyhow::Result<()> {
    let mut function = add_k_plus_zero(7);
    let pipeline = strider_orchestrator::opt::default_pipeline();

    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )?; // first run: collapses Add(7,0)
    let count_after_first = function.walk().count();

    pipeline.run(
        &mut function,
        &mut strider_orchestrator::opt::OptCtx::new(None),
    )?; // second run: no-op
    let count_after_second = function.walk().count();

    assert_eq!(
        count_after_first, count_after_second,
        "re_optimize on an already-stable graph is a no-op",
    );
    Ok(())
}

#[test]
fn manual_rewrite_does_not_break_validate() -> anyhow::Result<()> {
    // Local typing + use-list consistency + graph invariants: a broken
    // use-list would only surface here.
    let mut function = add_k_plus_zero(42);
    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));

    {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule))?;
    }

    strider_ir::validate::validate(&function)
        .map_err(|e| anyhow::anyhow!("assertion failed: validate failed after rewrite: {e}"))?;
    Ok(())
}

#[test]
fn apply_rule_using_pattern_var_capture() -> anyhow::Result<()> {
    // add(var(x), int_const(0u128)) -> var(x): the capture binds the Add's
    // left input and the RHS reuses it as a passthrough.
    // Pins: apply_rules_count's fire count, and that Return ends up wired
    // directly to `x` once the Add becomes unreachable.
    let mut function = add_k_plus_zero(99);
    assert_eq!(count_adds(&function), 1, "fixture has one Add");

    let x = Capture::new();
    let rule = rewrite_rule(int_add(var(x), int_const(0u128)), var(x));
    let fired = {
        let mut ctx = EditFunction::new(&mut function);
        apply_rules_count(&mut ctx, std::slice::from_ref(&rule))?
    };
    assert_eq!(fired, 1, "Capture-capture rule fires exactly once");
    assert_eq!(
        count_adds(&function),
        0,
        "post-rewrite Add is unreachable: the Return feeds off `x` directly",
    );
    Ok(())
}
