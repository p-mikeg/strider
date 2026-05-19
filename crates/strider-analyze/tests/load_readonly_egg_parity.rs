//! Phase 3 Task 3.6 parity test.
//!
//! `LoadReadOnly` (v1, imperative) vs. `LoadReadOnlyEgg` (v2,
//! egg-informed) MUST produce structurally identical IR for every
//! supported shape: Load(constant_addr, space) where ROM has bytes is
//! folded to IntConst; everything else is left alone.
//!
//! Scope (Phase 3.6):
//!   * Constant addr present in ROM → fold for U8/U16/U32/U64.
//!   * Constant addr NOT in ROM → leave alone.
//!   * Non-constant addr (e.g. Add of two consts where ConstantFold
//!     hasn't run) → leave alone.
//!   * Wrong space (RAM-only ROM, Load from REGISTER) → leave alone.
//!   * Multiple folds in one invocation.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used)]

use strider_analyze::opt::{LoadReadOnly, OptimizerRaw, load_readonly_egg::LoadReadOnlyEgg};
use strider_ir::node::{NodeKind, NodeOutputType};
use strider_ir::test_utils::make_empty_fn;
use strider_ir::{BuiltFunctionGraph, FunctionBuilder, IntBinaryOp, ReadOnlyMemory, Value};

/// Test ROM with a small in-RAM lookup table.
struct TestRom;
impl ReadOnlyMemory for TestRom {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, _size: usize) -> Option<u64> {
        if space != rsleigh::VnSpace::RAM {
            return None;
        }
        match addr {
            0x1000 => Some(42),
            0x2000 => Some(0xFF),
            0x3000 => Some(0xDEAD_BEEF),
            _ => None,
        }
    }
}

fn return_kind(fg: &BuiltFunctionGraph) -> NodeKind {
    let ret = fg
        .graph
        .all_node_ids()
        .find(|&n| matches!(fg.graph.node_kind(n), NodeKind::Return))
        .expect("must have a Return");
    let inputs = fg.graph.node_inputs(ret);
    let val_out = inputs[2];
    let producer = fg.graph.get_node_from_output(val_out);
    *fg.graph.node_kind(producer)
}

fn reachable_loads(fg: &BuiltFunctionGraph) -> usize {
    strider_ir::walk::walk_graph(&fg.graph, fg.entry)
        .filter(|&n| matches!(fg.graph.node_kind(n), NodeKind::Load(_)))
        .count()
}

fn assert_parity<F>(label: &str, f: F)
where
    F: Fn(&mut FunctionBuilder) -> anyhow::Result<Value> + Clone,
{
    let mut fg_v1 = make_empty_fn(f.clone()).expect("build v1 fixture");
    let mut fg_v2 = make_empty_fn(f).expect("build v2 fixture");

    let v1_pass = LoadReadOnly(TestRom);
    let v2_pass = LoadReadOnlyEgg::new(TestRom);

    let v1_res = v1_pass
        .optimize_raw(&mut fg_v1.graph, fg_v1.entry)
        .expect("v1 must not error");
    let v2_res = v2_pass
        .optimize_raw(&mut fg_v2.graph, fg_v2.entry)
        .expect("v2 must not error");

    assert_eq!(
        v1_res.changed(),
        v2_res.changed(),
        "{label}: changed flag mismatch v1={v1:?} v2={v2:?}",
        v1 = v1_res.changed(),
        v2 = v2_res.changed(),
    );
    let v1_kind = return_kind(&fg_v1);
    let v2_kind = return_kind(&fg_v2);
    assert_eq!(
        v1_kind, v2_kind,
        "{label}: return_kind mismatch v1={v1_kind:?} v2={v2_kind:?}"
    );
    let v1_loads = reachable_loads(&fg_v1);
    let v2_loads = reachable_loads(&fg_v2);
    assert_eq!(
        v1_loads, v2_loads,
        "{label}: reachable load count mismatch v1={v1_loads} v2={v2_loads}"
    );
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn parity_load_const_addr_u64() {
    assert_parity("Load(0x1000) U64 → 42", |b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    });
}

#[test]
fn parity_load_const_addr_u8() {
    // U8 load from 0x2000 → ROM returns 0xFF → masked to 0xFF.
    assert_parity("Load(0x2000) U8 → 0xFF", |b| {
        let addr = b.build_int_const(0x2000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U8)
    });
}

#[test]
fn parity_load_const_addr_u32() {
    assert_parity("Load(0x3000) U32 → 0xDEADBEEF", |b| {
        let addr = b.build_int_const(0x3000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U32)
    });
}

#[test]
fn parity_load_const_addr_u16() {
    // 0xDEAD_BEEF masked to U16 = 0xBEEF.
    assert_parity("Load(0x3000) U16 → 0xBEEF", |b| {
        let addr = b.build_int_const(0x3000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U16)
    });
}

#[test]
fn parity_load_addr_not_in_rom() {
    // 0xDEAD is not in the ROM → neither v1 nor v2 folds.
    assert_parity("Load(0xDEAD) U64 → leave alone", |b| {
        let addr = b.build_int_const(0xDEADu64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    });
}

#[test]
fn parity_load_non_const_addr() {
    // addr = 0x1000 + 0 (Add — ConstantFold would simplify, but we
    // don't run it here).  Neither LoadReadOnly fires because the
    // addr's e-class isn't a literal IntConst.
    assert_parity("Load(Add) → leave alone", |b| {
        let base = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        let off = b.build_int_const(0u64, NodeOutputType::U64)?;
        let addr = b.build_int_binary_operation(base, off, IntBinaryOp::Add, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::RAM, NodeOutputType::U64)
    });
}

#[test]
fn parity_load_wrong_space() {
    // Load from REGISTER space — TestRom only serves RAM.
    assert_parity("Load(REGISTER) → leave alone", |b| {
        let addr = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        b.build_load(addr, rsleigh::VnSpace::REGISTER, NodeOutputType::U64)
    });
}

#[test]
fn parity_multiple_loads_one_pass() {
    // Two folded loads, sum them.
    assert_parity("Load(0x1000) + Load(0x2000)", |b| {
        let a1 = b.build_int_const(0x1000u64, NodeOutputType::U64)?;
        let a2 = b.build_int_const(0x2000u64, NodeOutputType::U64)?;
        let l1 = b.build_load(a1, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        let l2 = b.build_load(a2, rsleigh::VnSpace::RAM, NodeOutputType::U64)?;
        b.build_int_binary_operation(l1, l2, IntBinaryOp::Add, NodeOutputType::U64)
    });
}
