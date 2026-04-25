//! `OptimizerPipeline::run` always calls `ir::validate::validate` at the end.
//! If any pass leaves an invalid graph, run returns Err.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]

mod common;

use ir::node::NodeOutputType;
use opt::*;

use common::{make_fn, sp_vn};

#[test]
fn run_validates_after_default_pipeline() -> opt::Result<()> {
    let mut fg = make_fn(|b| Ok(b.build_int_const(0, NodeOutputType::U64).unwrap()))?;
    default_pipeline().run(&mut fg)?;
    Ok(())
}

#[test]
fn run_with_post_passes_validates() -> opt::Result<()> {
    use ir::FunctionBuilder;
    let sp = sp_vn();
    let mut fg = {
        let mut b = FunctionBuilder::new_raw(vec![sp], &[], &[sp], &[], None, 0)?;
        let region = b.create_region()?;
        b.set_entry_region(region)?;
        b.set_region(region);
        b.build_return(None, &[])?;
        b.build()?
    };
    let mut p = OptimizerPipeline::new();
    p.add(ConstantFold);
    p.add(StackStoreDetect::new(sp));
    p.add_post_pass(CallStackArgCollect::new(vec![0]));
    p.run(&mut fg)?;
    Ok(())
}
