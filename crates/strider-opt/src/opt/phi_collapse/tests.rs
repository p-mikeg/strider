use super::*;
use strider_ir::IRBuilderExt;
use strider_ir::node::{NodeKind, ValueType};
use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn};

fn find_return(fg: &strider_ir::Function) -> NodeId {
    fg.graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Return))
        .expect("Return present")
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
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.build_branch(join)?;

    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let phi = find_var_phi(&fg, var);
    let phi_inputs = fg.node_inputs(phi);
    assert_eq!(phi_inputs.len(), 2, "token + 1 value");
    let lone_value = phi_inputs[1];

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "single-value phi must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
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
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let a = b.create_region_all()?;
    let bb = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    b.set_region(a);
    let v_a = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(bb);
    let v_b = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // Anchor on the phi the Return consumes.
    let phi = fg.producer(fg.node_inputs(find_return(&fg))[2]);
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

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "all-equal phi must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
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
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let mid = b.create_region_all()?;
    let tail = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.build_branch(mid)?;
    // Each read materialises a phi; tail's phi takes mid's phi as its value.
    b.set_region(mid);
    let _mid_read = b.read_variable(&var)?;
    b.build_branch(tail)?;
    b.set_region(tail);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
    assert!(changed, "chained trivial phis must collapse");

    let ret_val = fg.node_inputs(find_return(&fg))[2];
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
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let header = b.create_region_all()?;
    let exit = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    b.build_branch(header)?;

    // The back edge leaves `var` unwritten, so the header phi reads itself.
    b.set_region(header);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, header, exit)?;

    b.set_region(exit);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    assert!(
        fg.graph().all_node_ids().any(|n| {
            matches!(fg.node_kind(n), NodeKind::Phi)
                && fg.node_inputs(n).len() == 3
                && fg.node_inputs(n)[2] == fg.node_outputs(n)[0]
        }),
        "the header phi carries [token, initial, self-ref]"
    );

    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?;

    let ret_val = fg.node_inputs(find_return(&fg))[2];
    assert!(
        matches!(fg.node_kind(fg.producer(ret_val)), NodeKind::InitialVar(_)),
        "Return must rewire past the self-referential header phi to the entry value, got {:?}",
        fg.node_kind(fg.producer(ret_val))
    );
    Ok(())
}

#[test]
fn genuine_two_value_phi_unchanged() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let a = b.create_region_all()?;
    let bb = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;

    b.set_region(entry);
    let cond = b.build_boolean_const(true);
    b.build_if(cond, a, bb)?;

    b.set_region(a);
    let v_a = b.build_int_const(1u64, ValueType::I64)?;
    b.write_variable(&var, v_a)?;
    b.build_branch(join)?;

    b.set_region(bb);
    let v_b = b.build_int_const(2u64, ValueType::I64)?;
    b.write_variable(&var, v_b)?;
    b.build_branch(join)?;

    b.set_region(join);
    let merged = b.read_variable(&var)?;
    b.build_return(Some(merged), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    // The builder may layer phis, so anchor on the value the Return actually
    // consumes rather than on `find_var_phi`.
    let phi_value = fg.node_inputs(find_return(&fg))[2];
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
    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?;

    let ret_val_after = fg.node_inputs(find_return(&fg))[2];
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
    b.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
    b.build_branch(body)?;
    b.set_region(body);
    let addr = b.build_int_const(0x1000u64, ValueType::I64)?;
    let data = b.build_int_const(0x42u64, ValueType::I64)?;
    b.build_store(addr, data, rsleigh::VnSpace::RAM)?;
    b.build_return(None, &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    let store = fg
        .graph()
        .all_node_ids()
        .find(|&n| matches!(fg.node_kind(n), NodeKind::Store(_)))
        .expect("Store present");
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

    let changed =
        crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?.changed();
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

#[test]
fn collapse_then_validates() -> crate::Result<()> {
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let entry = b.create_region_all()?;
    let join = b.create_region_all()?;
    b.set_entry_region_all(entry)?;
    b.set_region(entry);
    b.build_branch(join)?;
    b.set_region(join);
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;

    crate::pipeline::run_one(&PhiCollapse, &mut fg, &mut crate::OptCtx::new(None))?;
    strider_ir::validate::validate(&fg)
        .map_err(|e| anyhow::anyhow!("post-PhiCollapse validation failed: {e:?}"))?;
    Ok(())
}

/// The pipeline loop over a chain of trivial phis, each a consumer of the one
/// before: the first sweep collapses them all, so the second reports nothing.
#[test]
fn a_cascading_sweep_converges_in_one_iteration() -> crate::Result<()> {
    use crate::pipeline::Optimizer;
    use strider_ir_test_utils::IrWalkerEx;
    let var = reg_vn(0x1000, 8);
    let mut b = RegisterSet::new().tracked(var).arg(var).build_fn()?;
    let regions = (0..6)
        .map(|_| b.create_region_all())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    b.set_entry_region_all(regions[0])?;
    for pair in regions.windows(2) {
        b.set_region(pair[0]);
        b.build_branch(pair[1])?;
        b.set_region(pair[1]);
        let _ = b.read_variable(&var)?;
    }
    let read_back = b.read_variable(&var)?;
    b.build_return(Some(read_back), &[])?;
    b.set_lift_addr(None);
    let mut fg = b.build()?;
    let phis = |fg: &strider_ir::Function| fg.count_kind(|k| matches!(k, NodeKind::Phi));
    assert!(phis(&fg) >= 5, "setup expects a phi per join");

    let mut ctx = crate::OptCtx::new(None);
    let mut edit = crate::EditFunction::new(&mut fg);
    edit.cull_dead();
    let mut sweeps = Vec::new();
    loop {
        let changed = PhiCollapse.apply(&mut edit, &mut ctx)?.changed();
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
type Block = (Option<u64>, &'static [usize]);

/// A function over one tracked `var` whose regions are `blocks`, block 0 the
/// entry.
fn build_blocks(blocks: &[Block]) -> crate::Result<strider_ir::Function> {
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
            let value = b.build_int_const(k, ValueType::I64)?;
            b.write_variable(&var, value)?;
        }
        let current = b.read_variable(&var)?;
        match *succs {
            [] => b.build_return(Some(current), &[])?,
            [next] => b.build_branch(regions[next])?,
            [taken, not_taken] => {
                let zero = b.build_int_const(0u64, ValueType::I64)?;
                let cond = b.build_int_cmp_operation(
                    current,
                    zero,
                    strider_ir::IntCmpOp::Equal,
                    ValueType::I64,
                )?;
                b.build_if(cond, regions[taken], regions[not_taken])?;
            }
            _ => unreachable!("a block has at most two successors"),
        }
    }
    b.set_lift_addr(None);
    b.build()
}

fn count_live(fg: &strider_ir::Function, pred: impl Fn(&NodeKind) -> bool) -> usize {
    use strider_ir_test_utils::IrWalkerEx;
    fg.count_kind(pred)
}

fn run(fg: &mut strider_ir::Function) -> crate::Result<bool> {
    Ok(crate::pipeline::run_one(&PhiCollapse, fg, &mut crate::OptCtx::new(None))?.changed())
}

fn return_value_kind(fg: &strider_ir::Function) -> NodeKind {
    *fg.node_kind(fg.producer(fg.node_inputs(find_return(fg))[2]))
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
    assert!(run(&mut fg)?);
    assert_eq!(count_live(&fg, |k| matches!(k, NodeKind::Phi)), 0);
    assert!(matches!(return_value_kind(&fg), NodeKind::IntConst(_)));
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
    assert!(run(&mut fg)?);
    assert_eq!(count_live(&fg, |k| matches!(k, NodeKind::Phi)), 0);
    assert!(matches!(return_value_kind(&fg), NodeKind::IntConst(_)));
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
    run(&mut fg)?;
    assert_eq!(count_live(&fg, |k| matches!(k, NodeKind::Phi)), 2);
    assert!(matches!(return_value_kind(&fg), NodeKind::Phi));
    assert!(!run(&mut fg)?, "a second run finds nothing to collapse");
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
    assert!(run(&mut fg)?);
    assert_eq!(count_live(&fg, |k| matches!(k, NodeKind::Phi)), 2);
    let o = fg.producer(fg.node_inputs(find_return(&fg))[2]);
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
    assert!(run(&mut fg)?);
    assert_eq!(count_live(&fg, |k| matches!(k, NodeKind::MemPhi)), 0);
    let memory = fg.node_inputs(find_return(&fg))[1];
    assert!(matches!(
        fg.node_kind(fg.producer(memory)),
        NodeKind::InitialMemory
    ));
    Ok(())
}
