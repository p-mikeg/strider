//! Convergence and idempotency: the pipeline reaches fixed point in bounded
//! iterations and running it twice yields no further change.

mod common;

use ir::node::NodeOutputType;
use ir::IntBinaryOp;
use opt::default_pipeline;

use common::{make_fn_with_var, reg_vn};

/// Running the default pipeline a second time on an already-optimized graph
/// must not change the node count.
#[test]
fn default_pipeline_idempotent() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c1 = b.build_int_const(1, NodeOutputType::U64);
        let c2 = b.build_int_const(2, NodeOutputType::U64);
        let a = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(b.build_int_binary_operation(a, c2, IntBinaryOp::Add, NodeOutputType::U64)?)
    })?;

    default_pipeline().run(&mut fg)?;
    let count1 = fg.all_node_ids().count();
    default_pipeline().run(&mut fg)?;
    let count2 = fg.all_node_ids().count();
    assert_eq!(count1, count2, "second run must not change node count");
    Ok(())
}

/// A 50-deep chain of `+ 1` reductions must reach fixed point — no infinite
/// loop in the pipeline.
#[test]
fn long_reassoc_chain_converges() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let mut acc = x;
        for _ in 0..50 {
            let one = b.build_int_const(1, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64)?;
        }
        Ok(acc)
    })?;
    default_pipeline().run(&mut fg)?;
    Ok(())
}
