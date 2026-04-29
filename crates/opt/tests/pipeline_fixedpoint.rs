//! Convergence and idempotency: the pipeline reaches fixed point in bounded
//! iterations and running it twice yields no further change.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

mod common;

use ir::node::NodeOutputType;
use ir::IntBinaryOp;
use opt::default_pipeline;
use opt::{OptimizationResult, Optimizer, OptimizerPipeline};

use common::{make_fn, make_fn_with_var, reg_vn};

/// Running the default pipeline a second time on an already-optimized graph
/// must not change the node count.
#[test]
fn default_pipeline_idempotent() -> opt::Result<()> {
    let vn = reg_vn(0x1000, 8);
    let (mut fg, _x) = make_fn_with_var(vn, |b, x| {
        let c1 = b.build_int_const(1u64, NodeOutputType::U64);
        let c2 = b.build_int_const(2u64, NodeOutputType::U64);
        let a = b.build_int_binary_operation(x, c1, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_int_binary_operation(a, c2, IntBinaryOp::Add, NodeOutputType::U64)
    })?;

    default_pipeline().run(&mut fg.graph, fg.entry)?;
    let count1 = fg.all_node_ids().count();
    default_pipeline().run(&mut fg.graph, fg.entry)?;
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
            let one = b.build_int_const(1u64, NodeOutputType::U64);
            acc = b.build_int_binary_operation(acc, one, IntBinaryOp::Add, NodeOutputType::U64)?;
        }
        Ok(acc)
    })?;
    default_pipeline().run(&mut fg.graph, fg.entry)?;
    Ok(())
}

/// A non-monotone pass that always claims the graph changed must be caught
/// by the pipeline's iteration cap rather than spinning forever.
#[test]
fn fixed_point_limit_exceeded() -> opt::Result<()> {
    struct AlwaysChanged;
    impl Optimizer for AlwaysChanged {
        fn optimize(
            &self,
            _graph: &mut ir::Graph,
            _entry: ir::node::NodeId,
        ) -> opt::Result<OptimizationResult> {
            Ok(OptimizationResult::Changed)
        }
    }

    let mut fg = make_fn(|b| {
        Ok(b.build_int_const(0u64, NodeOutputType::U64))
    })?;

    let mut pipeline = OptimizerPipeline::new();
    pipeline.add(AlwaysChanged);

    match pipeline.run(&mut fg.graph, fg.entry) {
        Ok(()) => Err(anyhow::anyhow!(
            "assertion failed: expected the pipeline to bail out, got Ok"
        )),
        Err(err) => {
            assert!(
                err.to_string().contains("did not converge"),
                "expected FixedPointLimitExceeded, got {err:?}"
            );
            Ok(())
        }
    }
}
