//! Producer-shape classifier for tier-2 indirect-branch resolution.
//!
//! Walks the producer node of a placeholder anchor's value-input and
//! classifies it into a [`ResolvedTargets`].  The arms here are the
//! soundness-checked subset for round R2 (R4 will add the
//! jump-table arm).

use cfg::test_api::ResolvedTargets;
use ir::BuiltFunctionGraph;
use ir::node::{NodeKind, NodeOutputId};

/// Classify a placeholder anchor's producer node into a
/// [`ResolvedTargets`].  Returns `None` when the producer doesn't
/// match any of the known sound shapes — the orchestrator (R3)
/// interprets `None` as "still unresolved at this iteration; try
/// again or surface as `UnresolvedIndirectBranch` at fixed point."
///
/// `link_register_vn` is the calling convention's link register
/// varnode (`None` on stack-push ABIs like x86 / x86_64 where there
/// is no architectural link register).  When `None`, the
/// `InitialVar(lr) → LinkRegister` arm is short-circuited — there
/// can be no LR match without a known LR varnode.
///
/// # Soundness
///
/// Every arm in this match must be a producer shape that, on the
/// optimised IR, **unambiguously** identifies the indirect branch's
/// runtime target.  Shapes the prior in-place heuristic tried
/// (`Load(InitialVar(sp))` for `pop pc`-style returns) are
/// deliberately NOT included here: a `push X; pop pc` tail call
/// has the same Load-shape and would be misclassified as a return.
/// We rely on `StackLoadForward` having already simplified
/// properly-popped return addresses to `InitialVar(lr_vn)` directly
/// — that's the shape the LinkRegister arm matches.  See R2.4 for
/// the explicit soundness tests.
#[must_use]
pub fn classify_anchor(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    let producer_id = graph.graph.get_node_from_output(anchor_output);
    let kind = *graph.graph.node_kind(producer_id);
    match kind {
        // SOUND: a literal constant in the IR comes from one of:
        //   - a tracked IntConst pcode insn in the source region,
        //   - constant folding (`ConstantFold`),
        //   - a `LoadReadOnly` resolution against the binary's rodata.
        // All three are deterministic functions of the function's
        // pcode, so the same address is the only possible runtime
        // target of this BranchIndirect.  IntConst stores a u128;
        // truncate to u64 because virtual-address space is 64-bit
        // and any higher bits are noise (e.g. a 128-bit SIMD vn used
        // as a target).
        NodeKind::IntConst(k) => {
            #[allow(clippy::cast_possible_truncation)]
            let truncated = k as u64;
            Some(ResolvedTargets::Single(truncated))
        }
        // SOUND: `InitialVar(vn)` is the function-entry value of
        // varnode `vn`.  When `vn == lr_vn`, the indirect branch
        // dispatches to the caller-provided return address — i.e. a
        // standard return.  This is the shape `StackLoadForward`
        // produces for properly-popped return addresses (R2.4 has
        // the explicit `pop pc` test).  The `link_register_vn ==
        // None` case (x86 / x86_64) short-circuits to None because
        // there is no architectural link register on those ABIs.
        NodeKind::InitialVar(vn) if Some(vn) == link_register_vn => {
            Some(ResolvedTargets::LinkRegister)
        }
        // R2.1 / R2.2: every other producer shape is "still
        // unresolved" — the orchestrator will try again on a later
        // iteration or surface `UnresolvedIndirectBranch` at fixed
        // point.  R2.3 adds the `ValuePhi-of-IntConsts` arm; R4
        // adds the jump-table arm.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`classify_anchor`].
    //!
    //! Each test constructs a minimal [`BuiltFunctionGraph`] via
    //! [`ir::FunctionBuilder::new_raw`], appends nodes directly via
    //! `graph.create_node` to control the producer shape exactly,
    //! and then invokes the classifier on the targeted output.
    //! These tests intentionally bypass the strider IR-lift path so
    //! the classifier's match arms are exercised in isolation
    //! without depending on the optimiser pipeline producing the
    //! expected shape.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use cfg::test_api::ResolvedTargets;
    use ir::FunctionBuilder;
    use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

    /// VN constructor for a fixed-offset register: the unit tests
    /// invent register VNs out of thin air; the actual register
    /// space mapping is irrelevant here because the classifier only
    /// compares VNs structurally.
    fn fake_reg_vn(offset: u64, size: u32) -> rsleigh::Vn {
        rsleigh::Vn {
            addr: rsleigh::VnAddr {
                space: rsleigh::VnSpace::REGISTER,
                off: offset,
            },
            size,
        }
    }

    /// Build a minimal `BuiltFunctionGraph` with one tracked
    /// variable and an empty body region terminated by a Return
    /// whose single value-input is the caller-supplied
    /// `NodeOutputId`.  Used as a scaffold for the unit tests so
    /// the classifier sees a real, validation-passing graph.
    fn empty_graph_returning(
        anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> NodeOutputId,
    ) -> (BuiltFunctionGraph, NodeOutputId) {
        // No tracked variables, no calling convention plumbing.
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let anchor = anchor_inputs(&mut builder);
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        let graph = builder.build().expect("build");
        // Re-locate the anchor in the built graph: the build step
        // is a move, but `NodeOutputId` is a stable cranelift-entity
        // index so the same id continues to point at the same
        // output in the resulting graph.
        (graph, anchor)
    }

    #[test]
    fn classify_int_const_returns_single() {
        let (graph, anchor) = empty_graph_returning(|fb| {
            // Single IntConst node.  Output type is U64 — chosen
            // because BranchIndirect targets are pointer-sized on
            // every supported 64-bit arch; smaller widths would
            // also fold via the `as u64` cast in the classifier.
            fb.build_int_const(0x1234u64, NodeOutputType::U64)
        });
        let result = classify_anchor(&graph, anchor, None);
        assert_eq!(result, Some(ResolvedTargets::Single(0x1234)));
    }

    #[test]
    fn classify_int_const_when_lr_unset_still_returns_single() {
        // Pinned: the IntConst arm does not consult
        // `link_register_vn`.  A None lr (x86 / x86_64) must not
        // suppress IntConst classification.
        let (graph, anchor) = empty_graph_returning(|fb| {
            fb.build_int_const(0xfeed_face_u64, NodeOutputType::U64)
        });
        assert_eq!(
            classify_anchor(&graph, anchor, None),
            Some(ResolvedTargets::Single(0xfeed_face)),
        );
    }

    #[test]
    fn classify_initial_var_with_matching_lr_returns_link_register() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        // Build a graph where the only tracked variable IS the
        // link register; reading it produces an `InitialVar(lr)`
        // output.
        let mut builder = FunctionBuilder::new_raw(
            vec![lr_vn],
            &[],
            &[],
            &[],
            None,
            0,
        )
        .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        // `read_variable` in the entry region's only predecessor
        // (the function entry) returns the InitialVar.
        let anchor = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        let graph = builder.build().expect("build");

        // RedundantPhis hasn't run, so the producer might be a
        // ControlPhi rather than InitialVar directly.  Inspect.
        // For the LinkRegister arm to match, the producer must be
        // `InitialVar(lr_vn)` — we walk past a single-input
        // ControlPhi in the test if we hit one, since
        // RedundantPhis would have done that in production.
        let mut producer_output = anchor;
        while let NodeKind::ControlPhi(_) = graph
            .graph
            .node_kind(graph.graph.get_node_from_output(producer_output))
        {
            // ControlPhi inputs: [phi_token, ...per-pred values].
            // With one predecessor, slot 1 is the value.
            let pid = graph.graph.get_node_from_output(producer_output);
            let inputs: Vec<_> = graph.graph.node_inputs(pid).into_iter().collect();
            if inputs.len() != 2 {
                break;
            }
            producer_output = inputs[1];
        }

        let result = classify_anchor(&graph, producer_output, Some(lr_vn));
        assert_eq!(result, Some(ResolvedTargets::LinkRegister));
    }

    #[test]
    fn classify_initial_var_with_non_matching_vn_returns_none() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        let other_vn = fake_reg_vn(0x10, 4);
        // Track only `other_vn`; reading it gives `InitialVar(other_vn)`.
        // Pass `lr_vn` as the link register — they don't match, so
        // the classifier must return None.
        let mut builder = FunctionBuilder::new_raw(
            vec![other_vn],
            &[],
            &[],
            &[],
            None,
            0,
        )
        .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let anchor = builder
            .read_variable(&other_vn)
            .expect("read_variable(other)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        let graph = builder.build().expect("build");

        let mut producer_output = anchor;
        while let NodeKind::ControlPhi(_) = graph
            .graph
            .node_kind(graph.graph.get_node_from_output(producer_output))
        {
            let pid = graph.graph.get_node_from_output(producer_output);
            let inputs: Vec<_> = graph.graph.node_inputs(pid).into_iter().collect();
            if inputs.len() != 2 {
                break;
            }
            producer_output = inputs[1];
        }

        let result = classify_anchor(&graph, producer_output, Some(lr_vn));
        assert_eq!(result, None);
    }

    #[test]
    fn classify_initial_var_with_lr_unset_returns_none() {
        // InitialVar(lr_vn), but `link_register_vn = None` (the
        // x86 / x86_64 case).  The `Some(vn) == None` guard fails;
        // classifier returns None.
        let lr_vn = fake_reg_vn(0x4c, 4);
        let mut builder = FunctionBuilder::new_raw(
            vec![lr_vn],
            &[],
            &[],
            &[],
            None,
            0,
        )
        .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let anchor = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        let graph = builder.build().expect("build");

        let mut producer_output = anchor;
        while let NodeKind::ControlPhi(_) = graph
            .graph
            .node_kind(graph.graph.get_node_from_output(producer_output))
        {
            let pid = graph.graph.get_node_from_output(producer_output);
            let inputs: Vec<_> = graph.graph.node_inputs(pid).into_iter().collect();
            if inputs.len() != 2 {
                break;
            }
            producer_output = inputs[1];
        }

        let result = classify_anchor(&graph, producer_output, None);
        assert_eq!(result, None);
    }

    #[test]
    fn classify_unrelated_node_kind_returns_none() {
        // An IntAdd node — not IntConst, not InitialVar, not
        // ValuePhi — must classify as None.
        let (graph, anchor) = empty_graph_returning(|fb| {
            let lhs = fb.build_int_const(1u64, NodeOutputType::U64);
            let rhs = fb.build_int_const(2u64, NodeOutputType::U64);
            fb.build_int_binary_operation(lhs, rhs, ir::IntBinaryOp::Add, NodeOutputType::U64)
                .expect("build_int_binary_operation")
        });
        // Note: ConstantFold would turn 1+2 into IntConst(3), but
        // we don't run the optimiser here — the unit tests use the
        // raw builder output.  The returned anchor's producer is
        // an IntBinaryOp node, which the `_ => None` arm catches.
        let producer_kind = *graph
            .graph
            .node_kind(graph.graph.get_node_from_output(anchor));
        assert!(
            matches!(producer_kind, NodeKind::IntBinaryOp(_)),
            "fixture must produce an IntBinaryOp; got {producer_kind:?}"
        );
        assert_eq!(classify_anchor(&graph, anchor, None), None);
    }
    // Note: NodeOutputKind is unused at the unit-test level because
    // every node we synthesise here goes through the high-level
    // `FunctionBuilder` API which takes a `NodeOutputType` directly.
    // The `use` above keeps the import path documented for future
    // unit tests that hand-construct `Graph::create_node` calls
    // (e.g. R4's jump-table arm).
    #[allow(dead_code)]
    fn _unused_to_keep_imports() {
        let _ = NodeOutputKind::Memory;
    }
}
