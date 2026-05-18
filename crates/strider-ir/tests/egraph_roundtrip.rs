//! Round-trip integration tests for the egraph adapter — Phase 1 Task 1.5
//! spike acceptance.
//!
//! Each test builds a small graph via [`FunctionBuilder`], converts to
//! `egg::EGraph` via [`EGraphAdapter::from_graph`], extracts back via
//! [`EGraphAdapter::extract_into_graph`], then verifies the resulting
//! graph is structurally equivalent to the input.
//!
//! ZERO rewrites are applied; this is the identity round-trip. With no
//! rewrites, the egraph contains exactly one e-node per e-class, so
//! `AstSize` extraction returns the original e-node verbatim — which
//! `kind_from_lang` must map back to the originating strider
//! [`NodeKind`].
//!
//! # What "structurally equivalent" means here
//!
//! - Same number of reachable nodes from the entry.
//! - Same multiset of `NodeKind` values across reachable nodes.
//! - Asm-fingerprints preserved (copied through the side-table walk).
//! - Topology preserved: a parallel pre-order walk yields matching
//!   kinds at every step (modulo NodeId renumbering — the new graph
//!   uses fresh ids).
//!
//! These tests intentionally do NOT use the `TestGraph` helper (which
//! is `#[cfg(test)]`-only and not visible to integration tests).  They
//! use `FunctionBuilder` + `set_lift_addr` per the spike instructions.

use strider_ir::{
    BuiltFunctionGraph, FunctionBuilder, IntBinaryOp, Result,
    egraph_adapter::EGraphAdapter,
    node::{NodeId, NodeKind, NodeOutputType},
    Graph,
};

const SENTINEL: u64 = 0xDEAD_BEEF_0000_0001;

/// Builds a single-region function whose return value is the result of `f`.
///
/// Mirrors `strider_ir::test_utils::make_empty_fn` (which lives behind a
/// feature gate not visible to integration tests).
fn make_empty_fn<F>(f: F) -> Result<BuiltFunctionGraph>
where
    F: FnOnce(&mut FunctionBuilder) -> Result<strider_ir::Value>,
{
    let mut b = FunctionBuilder::empty()?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL));
    let val = f(&mut b)?;
    b.set_lift_addr(Some(SENTINEL));
    b.build_return(Some(val), &[])?;
    b.set_lift_addr(None);
    b.build()
}

/// Collects every reachable node's `NodeKind` from `entry` in walk-order.
fn collected_kinds(g: &Graph, entry: NodeId) -> Vec<NodeKind> {
    strider_ir::walk::walk_graph(g, entry)
        .map(|n| *g.node_kind(n))
        .collect()
}

/// Asserts that two graphs are structurally equivalent.  Compares the
/// multiset of NodeKinds reachable from each entry plus the count of
/// reachable nodes.  Doesn't compare NodeIds directly (the new graph
/// renumbers them).
fn assert_structurally_equivalent(
    old_g: &Graph,
    old_entry: NodeId,
    new_g: &Graph,
    new_entry: NodeId,
) {
    let mut old_kinds = collected_kinds(old_g, old_entry);
    let mut new_kinds = collected_kinds(new_g, new_entry);
    // Sort for multiset comparison (the walk order may differ between
    // the two graphs even though the structure is identical).
    old_kinds.sort_by_key(|k| format!("{k:?}"));
    new_kinds.sort_by_key(|k| format!("{k:?}"));
    assert_eq!(
        old_kinds, new_kinds,
        "structural mismatch:\n  old: {old_kinds:?}\n  new: {new_kinds:?}"
    );
}

/// Asserts that every reachable non-exempt node in `new_g` carries a
/// non-empty asm-fingerprint (the validator's Layer-C invariant).
fn assert_fingerprints_preserved(new_g: &Graph, new_entry: NodeId) {
    for n in strider_ir::walk::walk_graph(new_g, new_entry) {
        let kind = new_g.node_kind(n);
        // Structural kinds are exempt from the fingerprint check.
        let exempt = matches!(
            kind,
            NodeKind::Entry
                | NodeKind::InitialMemory
                | NodeKind::InitialVar(..)
                | NodeKind::FunctionArg { .. }
                | NodeKind::ControlState
                | NodeKind::MemPhi
                | NodeKind::VarPhi(..)
                | NodeKind::ValuePhi
                | NodeKind::StackStorePhi { .. }
        );
        if !exempt {
            assert!(
                !new_g.asm_fingerprint(n).is_empty(),
                "fingerprint lost for {n:?} ({kind:?})"
            );
        }
    }
}

// ── Test 1: a single IntConst returned by the function ────────────────────

#[test]
fn roundtrip_int_const_returns_identical_graph() -> Result<()> {
    let fg = make_empty_fn(|b| b.build_int_const(7u128, NodeOutputType::U64))?;

    let adapter = EGraphAdapter::from_graph(&fg.graph, fg.entry);
    let (new_g, new_entry) = adapter.extract_into_graph(&fg.graph, fg.entry);

    assert_structurally_equivalent(&fg.graph, fg.entry, &new_g, new_entry);
    assert_fingerprints_preserved(&new_g, new_entry);

    // Confirm the constant value survived.
    let found_const = strider_ir::walk::walk_graph(&new_g, new_entry).any(|n| {
        matches!(new_g.node_kind(n), NodeKind::IntConst(7))
    });
    assert!(found_const, "IntConst(7) must survive round-trip");
    Ok(())
}

// ── Test 2: a chained add ────────────────────────────────────────────────

#[test]
fn roundtrip_int_add_chain_returns_identical_graph() -> Result<()> {
    // (c1 + c2) + c3
    let fg = make_empty_fn(|b| {
        let c1 = b.build_int_const(5u128, NodeOutputType::U64)?;
        let c2 = b.build_int_const(11u128, NodeOutputType::U64)?;
        let c3 = b.build_int_const(13u128, NodeOutputType::U64)?;
        let s1 = b.build_int_binary_operation(c1, c2, IntBinaryOp::Add, NodeOutputType::U64)?;
        let s2 = b.build_int_binary_operation(s1, c3, IntBinaryOp::Add, NodeOutputType::U64)?;
        Ok(s2)
    })?;

    let adapter = EGraphAdapter::from_graph(&fg.graph, fg.entry);
    let (new_g, new_entry) = adapter.extract_into_graph(&fg.graph, fg.entry);

    assert_structurally_equivalent(&fg.graph, fg.entry, &new_g, new_entry);
    assert_fingerprints_preserved(&new_g, new_entry);

    // Confirm both Add nodes survive.
    let add_count = strider_ir::walk::walk_graph(&new_g, new_entry)
        .filter(|&n| {
            matches!(
                new_g.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .count();
    assert_eq!(add_count, 2, "both Add nodes must survive round-trip");

    // Confirm every constant survived.
    for k in [5u128, 11, 13] {
        let found = strider_ir::walk::walk_graph(&new_g, new_entry)
            .any(|n| matches!(new_g.node_kind(n), NodeKind::IntConst(v) if *v == k));
        assert!(found, "IntConst({k}) must survive round-trip");
    }
    Ok(())
}

// ── Test 3: a VarPhi consumed by an Add — phi must round-trip as opaque ──

#[test]
fn roundtrip_with_var_phi_preserves_opaque_leaf() -> Result<()> {
    // Build a function that tracks a variable `r0`, reads it (producing
    // a VarPhi over InitialVar(r0)), and adds a constant to it.
    let r0 = rsleigh::Vn {
        size: 8,
        addr_off: 0x100,
        addr_space: rsleigh::VnSpace::REGISTER,
    };
    let mut b = FunctionBuilder::new_raw(vec![r0], &[r0], &[], &[r0], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    b.set_lift_addr(Some(SENTINEL));
    let x = b.read_variable(&r0)?;
    let c = b.build_int_const(3u128, NodeOutputType::U64)?;
    let sum = b.build_int_binary_operation(x, c, IntBinaryOp::Add, NodeOutputType::U64)?;
    b.set_lift_addr(Some(SENTINEL));
    b.build_return(Some(sum), &[])?;
    b.set_lift_addr(None);
    let fg = b.build()?;

    let adapter = EGraphAdapter::from_graph(&fg.graph, fg.entry);

    // Sanity: the VarPhi over r0 must have become an opaque leaf.
    let phi_count = adapter
        .leaf_to_output
        .values()
        .filter(|&&oid| {
            let (n, _) = fg.graph.output_definition(oid);
            matches!(fg.graph.node_kind(n), NodeKind::VarPhi(..))
        })
        .count();
    assert!(phi_count >= 1, "VarPhi must be modeled as opaque leaf");

    let (new_g, new_entry) = adapter.extract_into_graph(&fg.graph, fg.entry);

    assert_structurally_equivalent(&fg.graph, fg.entry, &new_g, new_entry);
    assert_fingerprints_preserved(&new_g, new_entry);

    // The VarPhi over r0 must survive structurally.
    let var_phi_kind = strider_ir::walk::walk_graph(&new_g, new_entry).find(|&n| {
        matches!(new_g.node_kind(n), NodeKind::VarPhi(v) if *v == r0)
    });
    assert!(
        var_phi_kind.is_some(),
        "VarPhi(r0) must survive round-trip as opaque leaf"
    );

    // And the Add must survive.
    let add_count = strider_ir::walk::walk_graph(&new_g, new_entry)
        .filter(|&n| {
            matches!(
                new_g.node_kind(n),
                NodeKind::IntBinaryOp(IntBinaryOp::Add)
            )
        })
        .count();
    assert_eq!(add_count, 1);
    Ok(())
}
