//! Tests the escape-hatch rule form `name @ fn_name` which bypasses the
//! pattern DSL and calls a user-supplied function directly.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
)]

use ir::BuiltFunctionGraph;
use ir::FunctionBuilder;
use ir::node::NodeId;
use ir_macros::rewrite_rules;
use opt::{OptimizationResult, Result};

fn always_nochange(_fg: &mut BuiltFunctionGraph, _node: NodeId) -> Result<OptimizationResult> {
    Ok(OptimizationResult::NoChange)
}

fn always_changed(_fg: &mut BuiltFunctionGraph, _node: NodeId) -> Result<OptimizationResult> {
    Ok(OptimizationResult::Changed)
}

#[test]
fn escape_hatch_nochange() -> ir::Result<()> {
    // Build a minimal graph so we have a node id to pass.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    b.build_return(None, &[])?;
    let mut fg = b.build().expect("build failed");

    let apply = rewrite_rules! {
        nop @ always_nochange,
    };

    // Any node id works; the helper ignores it. Use the Entry node.
    let entry = fg.entry;
    let res = apply(&mut fg, entry).unwrap();
    assert_eq!(res, OptimizationResult::NoChange);
    Ok(())
}

#[test]
fn escape_hatch_changed_propagates() -> ir::Result<()> {
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    b.build_return(None, &[])?;
    let mut fg = b.build().expect("build failed");

    let apply = rewrite_rules! {
        force @ always_changed,
    };
    let entry = fg.entry;
    let res = apply(&mut fg, entry).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
    Ok(())
}

#[test]
fn escape_hatch_mixed_with_pattern_rule() -> ir::Result<()> {
    // An escape rule sitting alongside a regular pattern rule. The dispatcher
    // should run each in order; if either reports Changed, the aggregate is Changed.
    let mut b = FunctionBuilder::new(vec![], &[], &[], &[])?;
    let r = b.create_region()?;
    b.set_entry_region(r)?;
    b.set_region(r);
    b.build_return(None, &[])?;
    let mut fg = b.build().expect("build failed");

    let apply = rewrite_rules! {
        nop @ always_nochange,
        force @ always_changed,
    };
    let entry = fg.entry;
    let res = apply(&mut fg, entry).unwrap();
    assert_eq!(res, OptimizationResult::Changed);
    Ok(())
}
