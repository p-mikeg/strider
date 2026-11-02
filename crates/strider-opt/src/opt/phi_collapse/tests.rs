use super::*;
use crate::test_support::run_pass;
use strider_ir::IRBuilderExt;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::{IrWalkerEx, RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

fn find_return(fg: &strider_ir::Function) -> NodeId {
    fg.first_kind(|k| matches!(k, NodeKind::Return))
}

fn find_var_phi(fg: &strider_ir::Function, var: rsleigh::Vn) -> NodeId {
    fg.graph()
        .all_node_ids()
        .find(|&n| {
            matches!(fg.node_kind(n), NodeKind::Phi)
                && fg.get_vn_for_value(fg.node_outputs(n)[0]) == Some(var)
        })
        .expect("VarPhi present")
}

#[test]
fn single_value_phi_collapses() -> crate::Result<()> {
    let mut fg = build_blocks(&[(None, &[1]), (None, &[])])?;
    let phi = find_var_phi(&fg, reg_vn(0x1000, 8));
    let phi_inputs = fg.node_inputs(phi);
    assert_eq!(phi_inputs.len(), 2, "token + 1 value");
    let lone_value = phi_inputs[1];

    let changed = run_pass(&PhiCollapse, &mut fg)?;
    assert!(changed, "single-value phi must collapse");

    let ret_val = fg.returned();
    assert_eq!(
        ret_val, lone_value,
        "Return must rewire to the phi's only value input"
    );
    Ok(())
}

/// A real 2-predecessor join whose value inputs are the same `ValueId`.
///
/// The builder dedups two structurally-equal writes into one phi input, so the
/// `[token, V, V]` shape has to be produced by surgery on a 2-distinct join.
#[test]
fn multi_value_all_equal_phi_collapses() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (None, &[1, 2]),
        (Some(1), &[3]),
        (Some(2), &[3]),
        (None, &[]),
    ])?;

    // Anchor on the phi the Return consumes.
    let phi = fg.producer(fg.returned());
    assert!(matches!(fg.node_kind(phi), NodeKind::Phi));
    assert_eq!(fg.node_inputs(phi).len(), 3, "token + 2 values");

    let first_value = fg.node_inputs(phi)[1];
    let input2_id = fg.graph().node_input_id_at(phi, 2)?;
    fg.graph_mut().update_input(input2_id, first_value);
    assert_eq!(
        fg.node_inputs(phi)[1],
        fg.node_inputs(phi)[2],
        "both values equal"
    );

    let changed = run_pass(&PhiCollapse, &mut fg)?;
    assert!(changed, "all-equal phi must collapse");

    let ret_val = fg.returned();
    assert_eq!(
        ret_val, first_value,
        "Return must rewire to the shared value"
    );
    Ok(())
}

/// `entry -> mid -> tail`, a phi per join. Collapsing mid's phi leaves tail's
/// trivial too, so the cascade must reach all the way to the base `InitialVar`
/// with no `Phi` left on the value path.
#[test]
fn chained_single_pred_phis_cascade_to_base_value() -> crate::Result<()> {
    // Each read materialises a phi; tail's phi takes mid's phi as its value.
    let mut fg = build_blocks(&[(None, &[1]), (None, &[2]), (None, &[])])?;
    let var = reg_vn(0x1000, 8);

    let changed = run_pass(&PhiCollapse, &mut fg)?;
    assert!(changed, "chained trivial phis must collapse");

    let ret_val = fg.returned();
    let producer = fg.producer(ret_val);
    assert!(
        matches!(fg.node_kind(producer), NodeKind::InitialVar(v) if fg.initial_vn(*v) == var),
        "cascade must land on the base InitialVar, got {:?}",
        fg.node_kind(producer)
    );
    Ok(())
}

/// `[token, x, phi_self]` collapses to `x`; Braun's rule discards the self-ref.
#[test]
fn loop_carried_self_ref_phi_collapses() -> crate::Result<()> {
    // The back edge leaves `var` unwritten, so the header phi reads itself.
    let mut fg = build_blocks(&[(None, &[1]), (None, &[1, 2]), (None, &[])])?;

    assert!(
        fg.graph().all_node_ids().any(|n| {
            matches!(fg.node_kind(n), NodeKind::Phi)
                && fg.node_inputs(n).len() == 3
                && fg.node_inputs(n)[2] == fg.node_outputs(n)[0]
        }),
        "the header phi carries [token, initial, self-ref]"
    );

    run_pass(&PhiCollapse, &mut fg)?;

    let ret_val = fg.returned();
    assert!(
        matches!(fg.node_kind(fg.producer(ret_val)), NodeKind::InitialVar(_)),
        "Return must rewire past the self-referential header phi to the entry value, got {:?}",
        fg.node_kind(fg.producer(ret_val))
    );
    Ok(())
}

#[test]
fn genuine_two_value_phi_unchanged() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (None, &[1, 2]),
        (Some(1), &[3]),
        (Some(2), &[3]),
        (None, &[]),
    ])?;

    // The builder may layer phis, so anchor on the value the Return actually
    // consumes rather than on `find_var_phi`.
    let phi_value = fg.returned();
    let phi = fg.producer(phi_value);
    assert!(
        matches!(fg.node_kind(phi), NodeKind::Phi),
        "Return's value must be produced by a VarPhi, got {:?}",
        fg.node_kind(phi)
    );
    let phi_inputs = fg.node_inputs(phi);
    assert_eq!(phi_inputs.len(), 3, "token + 2 distinct values");
    assert_ne!(phi_inputs[1], phi_inputs[2], "the two values are distinct");

    // Not asserting on the pass result: other single-pred phis (entry/branch
    // MemPhis) do collapse, so `Changed` is expected either way.
    run_pass(&PhiCollapse, &mut fg)?;

    let ret_val_after = fg.returned();
    assert_eq!(
        ret_val_after, phi_value,
        "Return must still read the genuine 2-distinct phi output"
    );
    assert!(
        matches!(fg.node_kind(phi), NodeKind::Phi),
        "the genuine join phi must still be a Phi"
    );
    Ok(())
}

#[test]
fn single_value_mem_phi_collapses() -> crate::Result<()> {
    let mut b = strider_ir_test_utils::empty_builder()?;
    let entry = b.create_region_all()?;
    let body = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(body)?;
    b.set_region(body);
    let addr = b.build_int_const(0x1000u64, ValueType::int(64))?;
    let data = b.build_int_const(0x42u64, ValueType::int(64))?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    let mut fg = b.build()?;

    let store = fg.first_kind(|k| matches!(k, NodeKind::Store(_)));
    // Store inputs: [mem, addr, data]; the memory input is slot 0.
    let store_mem_value_before = fg.node_inputs(store)[0];
    let body_mem_phi = fg.producer(store_mem_value_before);
    assert!(
        matches!(fg.node_kind(body_mem_phi), NodeKind::MemPhi),
        "Store's memory input must be a MemPhi pre-pass"
    );
    assert_eq!(
        fg.node_inputs(body_mem_phi).len(),
        2,
        "token + 1 memory value"
    );

    let changed = run_pass(&PhiCollapse, &mut fg)?;
    assert!(changed, "single-value MemPhi must collapse");

    // The cascade collapses body's MemPhi and then entry's, so the Store ends
    // up reading InitialMemory directly.
    let store_mem_value = fg.node_inputs(store)[0];
    let mem_producer = fg.producer(store_mem_value);
    assert!(
        !matches!(fg.node_kind(mem_producer), NodeKind::MemPhi),
        "Store's memory input must rewire past every collapsed MemPhi, got {:?}",
        fg.node_kind(mem_producer)
    );
    assert!(
        matches!(fg.node_kind(mem_producer), NodeKind::InitialMemory),
        "Store's memory input must reach InitialMemory, got {:?}",
        fg.node_kind(mem_producer)
    );
    Ok(())
}

/// The pipeline loop over a chain of trivial phis, each a consumer of the one
/// before: the first sweep collapses them all, so the second reports nothing.
#[test]
fn a_cascading_sweep_converges_in_one_iteration() -> crate::Result<()> {
    use crate::pipeline::Optimizer;
    let mut fg = build_blocks(&[
        (None, &[1]),
        (None, &[2]),
        (None, &[3]),
        (None, &[4]),
        (None, &[5]),
        (None, &[]),
    ])?;
    let phis = |fg: &strider_ir::Function| fg.count_kind(|k| matches!(k, NodeKind::Phi));
    assert!(phis(&fg) >= 5, "setup expects a phi per join");

    let ctx = crate::OptCtx::new(None);
    let mut edit = crate::EditFunction::new(&mut fg);
    edit.cull_dead();
    let mut sweeps = Vec::new();
    loop {
        let generation = edit.generation();
        PhiCollapse.apply(&mut edit, &ctx)?;
        let changed = edit.generation() != generation;
        sweeps.push(changed);
        edit.clean();
        if !changed || sweeps.len() > 8 {
            break;
        }
    }
    drop(edit);
    assert_eq!(sweeps, [true, false]);
    assert_eq!(phis(&fg), 0);
    Ok(())
}

/// One block of [`build_blocks`]: an optional constant written to `var`, then
/// no successor (return `var`), one (branch) or two (`If` on `var == 0`).
type Block<'a> = (Option<u64>, &'a [usize]);

/// A function over one tracked `var` whose regions are `blocks`, block 0 the
/// entry.
fn build_blocks(blocks: &[Block<'_>]) -> crate::Result<strider_ir::Function> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let regions = blocks
        .iter()
        .map(|_| b.create_region_all())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    b.set_entry_region_all(regions[0])?;
    for (&(write, succs), &region) in blocks.iter().zip(&regions) {
        b.set_region(region);
        if let Some(k) = write {
            let value = b.build_int_const(k, ValueType::int(64))?;
            b.write_variable(&var, value)?;
        }
        let current = b.read_variable(&var)?;
        match *succs {
            [] => b.build_return(Some(current), &[])?,
            [next] => b.build_branch(regions[next])?,
            [taken, not_taken] => {
                let zero = b.build_int_const(0u64, ValueType::int(64))?;
                let cond = b.build_int_cmp_operation(
                    current,
                    zero,
                    strider_ir::IntCmpOp::Equal,
                    ValueType::int(64),
                )?;
                b.build_if(cond, regions[taken], regions[not_taken])?;
            }
            _ => unreachable!("a block has at most two successors"),
        }
    }
    b.build()
}

/// A loop nest resetting `var` to 0 in the inner body:
/// `h1 = phi(0, h2)`, `h2 = phi(h1, 0)`.
#[test]
fn two_phi_cycle_with_one_outside_value_collapses() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (Some(0), &[1]),
        (None, &[2]),
        (None, &[3, 4]),
        (Some(0), &[2]),
        (None, &[1, 5]),
        (None, &[]),
    ])?;
    assert!(run_pass(&PhiCollapse, &mut fg)?);
    assert_eq!(fg.count_kind(|k| matches!(k, NodeKind::Phi)), 0);
    assert!(matches!(
        *fg.kind_of_value(fg.returned()),
        NodeKind::IntConst(_)
    ));
    Ok(())
}

/// The inner body joins a reset and a pass-through arm:
/// `h1 = phi(0, h2)`, `h2 = phi(h1, j)`, `j = phi(0, h2)`.
#[test]
fn three_phi_cycle_over_two_loop_headers_collapses() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (Some(0), &[1]),
        (None, &[2]),
        (None, &[3, 6]),
        (None, &[4, 5]),
        (Some(0), &[5]),
        (None, &[2]),
        (None, &[1, 7]),
        (None, &[]),
    ])?;
    assert!(run_pass(&PhiCollapse, &mut fg)?);
    assert_eq!(fg.count_kind(|k| matches!(k, NodeKind::Phi)), 0);
    assert!(matches!(
        *fg.kind_of_value(fg.returned()),
        NodeKind::IntConst(_)
    ));
    Ok(())
}

/// `h1 = phi(0, h2)`, `h2 = phi(h1, 1)` is a genuine merge.
#[test]
fn cycle_with_two_outside_values_stays() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (Some(0), &[1]),
        (None, &[2]),
        (None, &[3, 4]),
        (Some(1), &[2]),
        (None, &[1, 5]),
        (None, &[]),
    ])?;
    run_pass(&PhiCollapse, &mut fg)?;
    assert_eq!(fg.count_kind(|k| matches!(k, NodeKind::Phi)), 2);
    assert!(matches!(*fg.kind_of_value(fg.returned()), NodeKind::Phi));
    assert!(
        !run_pass(&PhiCollapse, &mut fg)?,
        "a second run finds nothing to collapse"
    );
    Ok(())
}

/// The outer cycle `h1 = phi(0, o)`, `o = phi(h2, 1)` merges two values, but
/// the inner loop never writes `var`: `h2 = phi(h1, p)`, `p = phi(h2, q)`,
/// `q = phi(h2, p)` take only `h1` from outside themselves.
#[test]
fn inner_scc_of_a_genuine_merge_collapses() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (Some(0), &[1]),
        (None, &[2]),
        (None, &[3, 7]),
        (None, &[4, 5]),
        (None, &[5, 6]),
        (None, &[4]),
        (None, &[2]),
        (None, &[8, 9]),
        (Some(1), &[9]),
        (None, &[1, 10]),
        (None, &[]),
    ])?;
    assert!(run_pass(&PhiCollapse, &mut fg)?);
    assert_eq!(fg.count_kind(|k| matches!(k, NodeKind::Phi)), 2);
    let o = fg.producer(fg.returned());
    let h1 = fg.producer(fg.node_inputs(o)[1]);
    assert!(matches!(fg.node_kind(h1), NodeKind::Phi));
    assert_eq!(fg.node_inputs(h1)[2], fg.node_outputs(o)[0]);
    Ok(())
}

/// Two joins entered from one block and from each other, with no store:
/// `p = memphi(m, q)`, `q = memphi(m, p)`.
#[test]
fn mem_phi_cycle_with_one_outside_token_collapses() -> crate::Result<()> {
    let mut fg = build_blocks(&[
        (None, &[1]),
        (None, &[2, 3]),
        (None, &[3, 4]),
        (None, &[2]),
        (None, &[]),
    ])?;
    assert!(run_pass(&PhiCollapse, &mut fg)?);
    assert_eq!(fg.count_kind(|k| matches!(k, NodeKind::MemPhi)), 0);
    let memory = fg.node_inputs(find_return(&fg))[1];
    assert!(matches!(
        fg.node_kind(fg.producer(memory)),
        NodeKind::InitialMemory
    ));
    Ok(())
}

/// A `Region` whose only predecessor is its own `If` is unreachable from
/// entry, so an arm's definition need not dominate the phi: collapsing
/// `p = phi(token, Add(p, 2))` onto the `Add` makes the `Add` its own input.
/// `validate` accepts the input and rejects that result.  The detached `If`
/// also branches to a live region, which is what keeps it in the graph.
#[test]
fn a_phi_on_an_unreachable_region_is_left_alone() -> crate::Result<()> {
    let var = reg_vn(0x1000, 4);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    let detached = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.build_branch(join)?;

    b.set_region(detached);
    let p = b.read_variable(&var)?;
    let two = b.build_int_const(2u64, ValueType::int(32))?;
    let next =
        b.build_int_binary_operation(p, two, strider_ir::IntBinaryOp::Add, ValueType::int(32))?;
    b.write_variable(&var, next)?;
    let cond = b.build_boolean_const(true);
    b.build_if(cond, join, detached)?;

    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(None, &[read_back])?;
    let mut fg = b.build()?;

    // `run_one` panics on invalid output.
    run_pass(&PhiCollapse, &mut fg)?;
    Ok(())
}

/// Loop headers `h_0..=h_m`, each entered from the two below it and the one
/// above, so `a_k = phi(a_(k-1), a_(k-2), a_(k+1))`, with `0` into `a_0` and
/// `1` into `a_1` from outside, and a self-loop `t = phi(a_m, t)` at the top.
/// Each inner rerun peels two headers, so reaching `t`, which collapses
/// alone, costs about `m^2 / 4` visits.
fn peeling_nest(m: usize) -> crate::Result<strider_ir::Function> {
    let h = |k: usize| 1 + 2 * k;
    let x = |k: usize| 2 + 2 * k;
    let (top, exit, one) = (2 * m + 3, 2 * m + 4, 2 * m + 5);
    let mut succs: Vec<Vec<usize>> = vec![vec![h(0), one]];
    for k in 0..=m {
        succs.push(vec![x(k), if k == 0 { exit } else { h(k - 1) }]);
        succs.push(match m - k {
            0 => vec![top],
            1 => vec![h(m)],
            _ => vec![h(k + 1), h(k + 2)],
        });
    }
    succs.extend([vec![top, h(m)], vec![], vec![h(1)]]);
    let blocks: Vec<Block<'_>> = succs
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let write = match i {
                0 => Some(0),
                i if i == one => Some(1),
                _ => None,
            };
            (write, s.as_slice())
        })
        .collect();
    build_blocks(&blocks)
}

/// Whether `t`, the one two-arm phi reading itself, survives a PhiCollapse
/// over [`peeling_nest`]`(m)`.
fn top_survives(m: usize) -> crate::Result<bool> {
    let mut fg = peeling_nest(m)?;
    let reads_itself = |fg: &strider_ir::Function| {
        use strider_ir::IRWalker;
        fg.walk_kind(|k| matches!(k, NodeKind::Phi)).any(|phi| {
            let out = fg.node_outputs(phi)[0];
            let arms: Vec<ValueId> = fg.phi_data_inputs(phi).collect();
            arms.len() == 2 && arms.contains(&out)
        })
    };
    assert!(reads_itself(&fg));
    run_pass(&PhiCollapse, &mut fg)?;
    Ok(reads_itself(&fg))
}

/// The inner reruns stop once they have visited a few phis per phi, so a
/// nest that peels two headers per rerun keeps the trivial phi at its top;
/// a shallow one reaches and collapses it.
#[test]
fn inner_reruns_past_their_budget_stop() -> crate::Result<()> {
    assert!(!top_survives(6)?, "the shallow nest collapses `t`");
    assert!(top_survives(40)?, "the deep nest stops before `t`");
    Ok(())
}

/// `And(phi(a, b), 0x1f)` over a diamond writing `a` and `b`: the value the
/// Return reads afterwards.
fn masked_diamond(a: u64, b: u64) -> crate::Result<(strider_ir::Function, ValueId)> {
    let var = reg_vn(0x1000, 4);
    let ty = ValueType::int(32);
    let mut fb = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = fb.create_region_all()?;
    let left = fb.create_region_all()?;
    let right = fb.create_region_all()?;
    let join = fb.create_region_all()?;
    fb.set_entry_region_all(entry)?;

    fb.set_region(entry);
    let current = fb.read_variable(&var)?;
    let zero = fb.build_int_const(0u64, ty)?;
    let cond = fb.build_int_cmp_operation(current, zero, strider_ir::IntCmpOp::Equal, ty)?;
    fb.build_if(cond, left, right)?;
    for (region, k) in [(left, a), (right, b)] {
        fb.set_region(region);
        let value = fb.build_int_const(k, ty)?;
        fb.write_variable(&var, value)?;
        fb.build_branch(join)?;
    }

    fb.set_region(join);
    let merged = fb.read_variable(&var)?;
    let masked = fb.build_const_binop(0x1fu32, merged, strider_ir::IntBinaryOp::And, ty)?;
    fb.build_return(Some(masked), &[])?;
    let mut fg = fb.build()?;
    run_pass(&PhiCollapse, &mut fg)?;
    let ret = fg.returned();
    Ok((fg, ret))
}

#[test]
fn a_mask_every_constant_arm_agrees_on_folds_to_that_constant() -> crate::Result<()> {
    let (fg, ret) = masked_diamond(0x13, 0x33)?;
    assert_eq!(fg.int_const_u128(ret), Some(0x13));
    Ok(())
}

#[test]
fn a_mask_the_arms_disagree_on_stays() -> crate::Result<()> {
    let (fg, ret) = masked_diamond(0x13, 0x14)?;
    assert!(
        matches!(
            fg.node_kind(fg.producer(ret)),
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And)
        ),
        "{:?}",
        fg.node_kind(fg.producer(ret))
    );
    Ok(())
}

/// The values a [`mask_loop`] reads at entry: `x`, `y`, `z`, `w`.
type Args = [ValueId; 4];

type Build<'a, T> =
    Box<dyn FnOnce(&mut strider_ir::FunctionBuilder, T) -> crate::Result<ValueId> + 'a>;

/// A loop over `var` whose header reads `var & mask` and returns it on exit:
/// the entry writes `entry(args)` to `var`, or leaves the incoming `var`, and
/// the body writes `back(args, var & mask)`. Returns the function after the
/// pass, the value the Return reads and the initial `var`.
fn mask_loop(
    mask: u128,
    entry: Option<Build<'_, Args>>,
    back: Build<'_, (Args, ValueId)>,
) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    let var = reg_vn(0x1000, 4);
    let regs = [0x1004, 0x1008, 0x100c, 0x1010].map(|off| reg_vn(off, 4));
    let ty = ValueType::int(32);
    let mut set = RegisterSet::new().tracked(var).arg(var);
    for reg in regs {
        set = set.tracked(reg).arg(reg);
    }
    let mut fb = set.build_fn()?;
    let entry_region = fb.create_region_all()?;
    let header = fb.create_region_all()?;
    let body = fb.create_region_all()?;
    let exit = fb.create_region_all()?;
    fb.set_entry_region_all(entry_region)?;

    fb.set_region(entry_region);
    let mut args = [fb.read_variable(&var)?; 4];
    for (arg, reg) in args.iter_mut().zip(&regs) {
        *arg = fb.read_variable(reg)?;
    }
    if let Some(entry) = entry {
        let first = entry(&mut fb, args)?;
        fb.write_variable(&var, first)?;
    }
    fb.build_branch(header)?;

    fb.set_region(header);
    let carried = fb.read_variable(&var)?;
    let masked = fb.build_const_binop(mask, carried, strider_ir::IntBinaryOp::And, ty)?;
    let zero = fb.build_int_const(0u64, ty)?;
    let cond = fb.build_int_cmp_operation(masked, zero, strider_ir::IntCmpOp::Equal, ty)?;
    fb.build_if(cond, exit, body)?;

    fb.set_region(body);
    let again = back(&mut fb, (args, masked))?;
    fb.write_variable(&var, again)?;
    fb.build_branch(header)?;

    fb.set_region(exit);
    fb.build_return(Some(masked), &[])?;
    let mut fg = fb.build()?;
    run_pass(&PhiCollapse, &mut fg)?;
    let ret = fg.returned();
    let incoming = fg.initial_var_value(&var).expect("var is read");
    Ok((fg, ret, incoming))
}

fn bin(
    fb: &mut strider_ir::FunctionBuilder,
    op: strider_ir::IntBinaryOp,
    a: ValueId,
    b: ValueId,
) -> crate::Result<ValueId> {
    fb.build_int_binary_operation(a, b, op, ValueType::int(32))
}

fn konst(
    fb: &mut strider_ir::FunctionBuilder,
    op: strider_ir::IntBinaryOp,
    k: u128,
    a: ValueId,
) -> crate::Result<ValueId> {
    fb.build_const_binop(k, a, op, ValueType::int(32))
}

/// The value the Return reads is still `var & mask`.
fn stays(fg: &strider_ir::Function, ret: ValueId) -> bool {
    let producer = fg.producer(ret);
    matches!(
        fg.node_kind(producer),
        NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And)
    ) && fg
        .node_inputs(producer)
        .into_iter()
        .any(|v| matches!(fg.node_kind(fg.producer(v)), NodeKind::Phi))
}

/// The interrupt-mask save shape with `op` joining the body's high part:
/// the entry writes `(x & 0xe0) | (y & 0x1f)` and the body
/// `(z & high) op (var & 0x1f)`. Returns the function, the value the Return
/// reads and the entry's `y & 0x1f`.
fn masked_loop(
    op: strider_ir::IntBinaryOp,
    high: u128,
) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or};
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        0x1f,
        Some(Box::new(|fb, [x, y, ..]| {
            let high0 = konst(fb, And, 0xe0, x)?;
            let low = konst(fb, And, 0x1f, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(move |fb, ([_, _, z, _], masked)| {
            let high = konst(fb, And, high, z)?;
            bin(fb, op, high, masked)
        }),
    )?;
    Ok((fg, ret, saved.get().expect("entry built")))
}

/// Each [`mask_loop`] shape, once with nothing the back edge writes
/// reaching the masked bits, where the mask folds to the value saved before
/// the loop, and once with something that can, where it stays.
#[test]
fn a_loop_carried_mask_folds_only_past_what_the_back_edge_writes() -> crate::Result<()> {
    use strider_ir::IntBinaryOp::{Or, Xor};
    #[rustfmt::skip]
    let cases = [
        // The interrupt-mask save, the body's high part joined by `Or` / `Xor`.
        ("or", masked_loop(Or, 0xe0)?, true),
        ("or, high part reaching the mask", masked_loop(Or, 0xf0)?, false),
        ("xor", masked_loop(Xor, 0xe0)?, true),
        ("xor, high part reaching the mask", masked_loop(Xor, 0xf0)?, false),
        ("shift clearing the mask", shifted_loop(8)?, true),
        ("shift keeping some of it", shifted_loop(4)?, false),
        // p-code shifts past the width to 0.
        ("shift past the width", shifted_loop(32)?, true),
        // A right shift vacates its top `shift` bits.
        ("mask above a right shift", shr_loop(24, 0xffff_0000)?, true),
        ("mask reaching under it", shr_loop(8, 0xffff_0000)?, false),
        ("mask above a zero-extended byte", extended_loop(0xffff_ff00)?, true),
        ("mask reaching into it", extended_loop(0xffff_ff80)?, false),
        ("or with a constant outside the mask", or_const_loop(0x40)?, true),
        ("or with a constant inside it", or_const_loop(0x50)?, false),
        // Both arms already within the mask: equal, then different.
        ("arms sharing their value", narrower_mask_loop(0x0f)?, true),
        ("arms that differ", narrower_mask_loop(0x07)?, false),
    ];
    for (case, (fg, ret, saved), folds) in cases {
        if folds {
            assert_eq!(ret, saved, "{case}");
        } else {
            assert!(
                stays(&fg, ret),
                "{case}: {:?}",
                fg.node_kind(fg.producer(ret))
            );
        }
    }
    Ok(())
}

/// The entry writes `(x << shift) | (y & 0xff)`, the body
/// `(z << shift) | (var & 0xff)`.
fn shifted_loop(shift: u128) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or, ShiftLeft};
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        0xff,
        Some(Box::new(|fb, [x, y, ..]| {
            let high0 = konst(fb, ShiftLeft, shift, x)?;
            let low = konst(fb, And, 0xff, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, ([_, _, z, _], masked)| {
            let high = konst(fb, ShiftLeft, shift, z)?;
            bin(fb, Or, high, masked)
        }),
    )?;
    Ok((fg, ret, saved.get().expect("entry built")))
}

/// The shift proves the bits the fold drops zero, so the folded value carries
/// both shifts' addresses.
#[test]
fn a_fold_through_a_shift_absorbs_the_shift() -> crate::Result<()> {
    use strider_ir::IntBinaryOp::{And, Or, ShiftLeft};
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        0xff,
        Some(Box::new(|fb, [x, y, ..]| {
            fb.set_lift_addr(Some(0xD1));
            let high0 = konst(fb, ShiftLeft, 8, x)?;
            fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
            let low = konst(fb, And, 0xff, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, ([_, _, z, _], masked)| {
            fb.set_lift_addr(Some(0xD2));
            let high = konst(fb, ShiftLeft, 8, z)?;
            fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
            bin(fb, Or, high, masked)
        }),
    )?;
    assert_eq!(Some(ret), saved.get());
    let fp = fg.side_tables().asm_fingerprint(fg.producer(ret));
    assert!(fp.contains(&0xD1) && fp.contains(&0xD2), "{fp:?}");
    Ok(())
}

/// The high half is itself an `Or` of shifts, so proving it clear under the
/// mask reads two levels below the peeled `Or`.
#[test]
fn a_mask_under_an_or_of_shifts_folds() -> crate::Result<()> {
    use strider_ir::IntBinaryOp::{And, Or, ShiftLeft};
    let saved = std::cell::Cell::new(None);
    let high = |fb: &mut strider_ir::FunctionBuilder, a, b| -> crate::Result<ValueId> {
        let a = konst(fb, ShiftLeft, 8, a)?;
        let b = konst(fb, ShiftLeft, 9, b)?;
        bin(fb, Or, a, b)
    };
    let (_, ret, _) = mask_loop(
        0xff,
        Some(Box::new(|fb, [x, y, z, _]| {
            let high0 = high(fb, x, z)?;
            let low = konst(fb, And, 0xff, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, ([x, _, z, _], masked)| {
            let high = high(fb, z, x)?;
            bin(fb, Or, high, masked)
        }),
    )?;
    assert_eq!(Some(ret), saved.get());
    Ok(())
}

/// The shifts two levels below the peeled `Or` are part of the proof that
/// its high half is clear, so the folded value carries their addresses.
/// Mutation: absorb only the dropped operand's producer.
#[test]
fn a_fold_through_an_or_of_shifts_absorbs_the_shifts() -> crate::Result<()> {
    use strider_ir::IntBinaryOp::{And, Or, ShiftLeft};
    let saved = std::cell::Cell::new(None);
    let high = |fb: &mut strider_ir::FunctionBuilder, a, b, at: [u64; 2]| {
        fb.set_lift_addr(Some(at[0]));
        let a = konst(fb, ShiftLeft, 8, a)?;
        fb.set_lift_addr(Some(at[1]));
        let b = konst(fb, ShiftLeft, 9, b)?;
        fb.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        bin(fb, Or, a, b)
    };
    let (fg, ret, _) = mask_loop(
        0xff,
        Some(Box::new(|fb, [x, y, z, _]| {
            let high0 = high(fb, x, z, [0xD1, 0xD2])?;
            let low = konst(fb, And, 0xff, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, ([x, _, z, _], masked)| {
            let high = high(fb, z, x, [0xD3, 0xD4])?;
            bin(fb, Or, high, masked)
        }),
    )?;
    assert_eq!(Some(ret), saved.get());
    let fp = fg.side_tables().asm_fingerprint(fg.producer(ret));
    assert!(
        [0xD1, 0xD2, 0xD3, 0xD4].iter().all(|a| fp.contains(a)),
        "{fp:?}"
    );
    Ok(())
}

/// The entry writes `(x >> shift) | (y & mask)`, the body
/// `(z >> shift) | (var & mask)`.
fn shr_loop(shift: u128, mask: u128) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or, ShiftRight};
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        mask,
        Some(Box::new(|fb, [x, y, ..]| {
            let high0 = konst(fb, ShiftRight, shift, x)?;
            let low = konst(fb, And, mask, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, ([_, _, z, _], masked)| {
            let high = konst(fb, ShiftRight, shift, z)?;
            bin(fb, Or, high, masked)
        }),
    )?;
    Ok((fg, ret, saved.get().expect("entry built")))
}

/// The entry writes `zext(x as i8) | (y & mask)`, the body
/// `zext(z as i8) | (var & mask)`.
fn extended_loop(mask: u128) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or};
    fn zext_byte(fb: &mut strider_ir::FunctionBuilder, v: ValueId) -> crate::Result<ValueId> {
        let byte = fb.truncate_if_needed(v, ValueType::int(8))?;
        fb.extend_if_needed(byte, ValueType::int(32), strider_ir::ExtendOp::ZeroExtend)
    }
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        mask,
        Some(Box::new(|fb, [x, y, ..]| {
            let low0 = zext_byte(fb, x)?;
            let high = konst(fb, And, mask, y)?;
            saved.set(Some(high));
            bin(fb, Or, low0, high)
        })),
        Box::new(|fb, ([_, _, z, _], masked)| {
            let low = zext_byte(fb, z)?;
            bin(fb, Or, low, masked)
        }),
    )?;
    Ok((fg, ret, saved.get().expect("entry built")))
}

/// The entry writes `(x & 0xe0) | (y & 0x1f)`, the body `(var & 0x1f) | k`.
fn or_const_loop(k: u128) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or};
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        0x1f,
        Some(Box::new(|fb, [x, y, ..]| {
            let high0 = konst(fb, And, 0xe0, x)?;
            let low = konst(fb, And, 0x1f, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, (_, masked)| konst(fb, Or, k, masked)),
    )?;
    Ok((fg, ret, saved.get().expect("entry built")))
}

/// Both arms are `(high & 0xe0) | low`: the entry's `low` is `y & 0x0f`, the
/// body's `y & body_low`. Returns the function, the value the Return reads
/// and the entry's `y & 0x0f`.
fn narrower_mask_loop(body_low: u128) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or};
    let saved = std::cell::Cell::new(None);
    let (fg, ret, _) = mask_loop(
        0x1f,
        Some(Box::new(|fb, [x, y, ..]| {
            let high0 = konst(fb, And, 0xe0, x)?;
            let low = konst(fb, And, 0x0f, y)?;
            saved.set(Some(low));
            bin(fb, Or, high0, low)
        })),
        Box::new(|fb, ([_, y, z, _], _)| {
            let high = konst(fb, And, 0xe0, z)?;
            let low = konst(fb, And, body_low, y)?;
            bin(fb, Or, high, low)
        }),
    )?;
    Ok((fg, ret, saved.get().expect("entry built")))
}

/// The entry leaves `var`; the body writes `(z & 0xe0) | low`, `low` the
/// loop's own `var & 0x1f` or the unrelated `w & 0x1f`.
fn bare_entry_loop(own: bool) -> crate::Result<(strider_ir::Function, ValueId, ValueId)> {
    use strider_ir::IntBinaryOp::{And, Or};
    mask_loop(
        0x1f,
        None,
        Box::new(move |fb, ([_, _, z, w], masked)| {
            let high = konst(fb, And, 0xe0, z)?;
            let low = if own {
                masked
            } else {
                konst(fb, And, 0x1f, w)?
            };
            bin(fb, Or, high, low)
        }),
    )
}

#[test]
fn a_bare_entry_arm_folds_to_a_new_mask_of_it() -> crate::Result<()> {
    let (fg, ret, incoming) = bare_entry_loop(true)?;
    let producer = fg.producer(ret);
    assert!(
        matches!(
            fg.node_kind(producer),
            NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And)
        ),
        "{:?}",
        fg.node_kind(producer)
    );
    let inputs: Vec<ValueId> = fg.node_inputs(producer).into_iter().collect();
    assert!(inputs.contains(&incoming), "{inputs:?}");
    assert!(
        inputs.iter().any(|&v| fg.int_const_u128(v) == Some(0x1f)),
        "{inputs:?}"
    );
    Ok(())
}

#[test]
fn a_bare_entry_arm_the_back_edge_replaces_stays() -> crate::Result<()> {
    let (fg, ret, _) = bare_entry_loop(false)?;
    assert!(stays(&fg, ret), "{:?}", fg.node_kind(fg.producer(ret)));
    Ok(())
}

/// A diamond writing `0x13` and `(x & 0xc0) | 0x33` to each of
/// `masks.len()` vars, the join reading each var under its masks, in order.
/// Returns, per var, each mask's value after the pass: `Some(k)` folded,
/// `None` left as `var & mask`.
fn masked_join(masks: &[&[u128]]) -> crate::Result<Vec<Vec<Option<u128>>>> {
    use strider_ir::IntBinaryOp::{And, Or};
    let vars: Vec<_> = (0..masks.len())
        .map(|i| reg_vn(0x1000 + 4 * i as u64, 4))
        .collect();
    let x = reg_vn(0x2000, 4);
    let ty = ValueType::int(32);
    let mut set = RegisterSet::new().tracked(x).arg(x);
    for &var in &vars {
        set = set.tracked(var).arg(var);
    }
    let mut fb = set.build_fn()?;
    let entry = fb.create_region_all()?;
    let left = fb.create_region_all()?;
    let right = fb.create_region_all()?;
    let join = fb.create_region_all()?;
    fb.set_entry_region_all(entry)?;

    fb.set_region(entry);
    let xv = fb.read_variable(&x)?;
    let zero = fb.build_int_const(0u64, ty)?;
    let cond = fb.build_int_cmp_operation(xv, zero, strider_ir::IntCmpOp::Equal, ty)?;
    fb.build_if(cond, left, right)?;

    fb.set_region(left);
    let k = fb.build_int_const(0x13u64, ty)?;
    for var in &vars {
        fb.write_variable(var, k)?;
    }
    fb.build_branch(join)?;

    fb.set_region(right);
    let high = konst(&mut fb, And, 0xc0, xv)?;
    let other = konst(&mut fb, Or, 0x33, high)?;
    for var in &vars {
        fb.write_variable(var, other)?;
    }
    fb.build_branch(join)?;

    fb.set_region(join);
    let mut uses = Vec::new();
    for (var, var_masks) in vars.iter().zip(masks) {
        let merged = fb.read_variable(var)?;
        for &mask in *var_masks {
            uses.push(fb.build_const_binop(mask, merged, And, ty)?);
        }
    }
    fb.build_return(None, &uses)?;
    let mut fg = fb.build()?;
    run_pass(&PhiCollapse, &mut fg)?;

    let mut returned = fg.node_inputs(find_return(&fg)).into_iter().skip(2);
    let mut out = Vec::new();
    for var_masks in masks {
        let mut row = Vec::new();
        for &mask in *var_masks {
            let v = returned.next().expect("one value per mask");
            let k = fg.int_const_u128(v);
            assert!(
                k.is_some() || stays(&fg, v),
                "mask {mask:#x}: {:?}",
                fg.node_kind(fg.producer(v))
            );
            row.push(k);
        }
        out.push(row);
    }
    Ok(out)
}

/// Each mask costs one peel step on the `0x13` arm and two on the other, so a
/// phi's budget of `2 * PEEL_VISITS_PER_ARM` folds that many masks over three,
/// each to `0x13 & mask`, and runs out inside the next one, which stays.
#[test]
fn masks_past_the_peel_budget_stay() -> crate::Result<()> {
    let budget = 2 * super::masked::PEEL_VISITS_PER_ARM;
    assert_ne!(budget % 3, 0, "the budget runs out inside a mask");
    // Clear of bit 5, where the arms differ, and of bits 6 and 7, which the
    // `x & 0xc0` arm leaves unknown.
    let masks: Vec<u128> = (1..32).chain((1..20).map(|i| i << 8)).collect();
    let folded = masked_join(&[&masks])?.remove(0);
    for (k, &mask) in folded.iter().zip(&masks) {
        if let Some(k) = *k {
            assert_eq!(k, 0x13 & mask, "mask {mask:#x}");
        }
    }
    let count = folded.iter().filter(|k| k.is_some()).count();
    assert_eq!(count, budget / 3);
    Ok(())
}

/// A phi whose masks all refuse spends only its own budget: a phi visited
/// after it still folds.
#[test]
fn a_refusing_phi_leaves_the_next_phi_its_budget() -> crate::Result<()> {
    let refusing: Vec<u128> = (1..64)
        .map(|i| (i & 0x1f) | 0x20 | ((i >> 5) << 8))
        .collect();
    // Refusing phis on both sides, whichever order the pass visits them in.
    let folded = masked_join(&[&refusing, &[0x1f], &refusing])?;
    for refused in [&folded[0], &folded[2]] {
        assert!(refused.iter().all(Option::is_none), "{refused:?}");
    }
    assert_eq!(folded[1], [Some(0x13)]);
    Ok(())
}
