//! Producer-shape classifier shim.  Delegates to
//! [`opt::indirect_branch_resolve`].  Retained as a strider-side entry
//! point so the orchestrator and integration tests can call into the
//! classifier under a stable strider path; the underlying logic
//! lives in `opt`.

use cfg::test_api::ResolvedTargets;
use ir::BuiltFunctionGraph;
use ir::node::NodeOutputId;
use opt::ReadOnlyMemory;

/// Classify a placeholder anchor's producer node into a
/// [`ResolvedTargets`].  Delegates to
/// [`opt::classify_anchor`].
#[must_use]
pub fn classify_anchor(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    opt::classify_anchor(graph, anchor_output, link_register_vn)
}

/// Classify a placeholder anchor with an optional [`ReadOnlyMemory`]
/// for the jump-table arm.  Delegates to
/// [`opt::classify_anchor_with_rom`].
#[must_use]
pub fn classify_anchor_with_rom(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
) -> Option<ResolvedTargets> {
    opt::classify_anchor_with_rom(graph, anchor_output, link_register_vn, rom)
}

/// Classify a placeholder anchor with both an optional
/// [`ReadOnlyMemory`] (for the rodata jump-table arm) and an optional
/// stack-pointer varnode (for the BUG-30 stack-array-of-labels arm).
/// Delegates to [`opt::classify_anchor_with_rom_and_sp`].
#[must_use]
pub fn classify_anchor_with_rom_and_sp(
    graph: &BuiltFunctionGraph,
    anchor_output: NodeOutputId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    stack_ptr_vn: Option<rsleigh::Vn>,
) -> Option<ResolvedTargets> {
    let known = opt::analyze_known_bits(graph).ok()?;
    opt::classify_anchor_with_rom_and_sp(
        graph,
        anchor_output,
        link_register_vn,
        rom,
        stack_ptr_vn,
        &known,
    )
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
        while let NodeKind::ControlPhi(_) = graph.graph.kind_of_output(producer_output) {
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
        while let NodeKind::ControlPhi(_) = graph.graph.kind_of_output(producer_output) {
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
        while let NodeKind::ControlPhi(_) = graph.graph.kind_of_output(producer_output) {
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

    /// Helper for the ValuePhi tests: build a minimal graph
    /// containing a ControlState (to provide the phi token), a set
    /// of value-producing nodes (each one fed in as a per-pred
    /// input), and a `ValuePhi` whose first input is the phi token
    /// and whose remaining inputs are the per-pred values.
    /// Returns the graph and the value-phi's output id.
    ///
    /// Synthesises the ValuePhi via `graph.create_node` directly
    /// after `build()` has returned — bypassing the validator's
    /// per-predecessor-arity check (Layer C requires phi inputs
    /// to match `ControlState`'s predecessor count, which we don't
    /// satisfy here).  This is intentional: the unit tests
    /// exercise `classify_anchor` against fully synthetic shapes
    /// that the validator would reject in production.  The
    /// integration tests in `tests/tier2_classify.rs` cover the
    /// validation-passing path end-to-end.
    fn build_value_phi_graph(
        per_pred_consts: &[u64],
    ) -> (BuiltFunctionGraph, NodeOutputId) {
        let mut builder = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);

        // Build all per-predecessor IntConst nodes; remember their
        // output ids so we can wire them into the ValuePhi below.
        let const_outputs: Vec<NodeOutputId> = per_pred_consts
            .iter()
            .map(|k| builder.build_int_const(*k, NodeOutputType::U64))
            .collect();

        // Use a dummy IntConst as the synthetic placeholder anchor
        // so `build_return` succeeds and `build()` validates.  The
        // ValuePhi we synthesise after build is unreachable from
        // entry, so it can have any shape we want.
        let dummy = builder.build_int_const(0u64, NodeOutputType::U64);
        builder.build_return(Some(dummy), &[]).expect("build_return");
        let mut graph = builder.build().expect("build");

        // Synthesise a fake phi-token node.  ControlPhi nodes
        // produce ControlPhi outputs, but the dedup cache keys
        // them by (NodeKind, inputs, outputs), so we can hand-
        // construct one with no inputs.  We need the phi-token
        // output kind for the ValuePhi's first input slot to
        // typecheck against `expected_signature`'s `PHI` slot.
        let fake_token_node = graph.graph.create_node(
            NodeKind::ControlPhi(rsleigh::Vn {
                addr: rsleigh::VnAddr {
                    space: rsleigh::VnSpace::REGISTER,
                    off: 0xdead,
                },
                size: 8,
            }),
            [],
            [NodeOutputKind::ControlPhi],
        );
        let [token_out] = graph
            .graph
            .node_outputs_exact::<1>(fake_token_node)
            .expect("token output");

        // Build the ValuePhi: inputs = [phi_token, ...vals]; output
        // is a single value (U64 for definiteness).
        let vp_node = graph.graph.create_node(
            NodeKind::ValuePhi,
            std::iter::once(token_out).chain(const_outputs.iter().copied()),
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [vp_out] = graph
            .graph
            .node_outputs_exact::<1>(vp_node)
            .expect("value-phi output");
        (graph, vp_out)
    }

    #[test]
    fn classify_value_phi_of_consts_returns_multiple_dedup_sorted() {
        // Phi(IntConst(7), IntConst(3), IntConst(7)) →
        //   Multiple(sorted, deduped) = Multiple([3, 7]).
        let (graph, anchor) = build_value_phi_graph(&[7, 3, 7]);
        let result = classify_anchor(&graph, anchor, None);
        match result {
            Some(ResolvedTargets::Multiple(ts)) => assert_eq!(ts, vec![3, 7]),
            other => panic!("expected Multiple([3, 7]); got {other:?}"),
        }
    }

    #[test]
    fn classify_value_phi_of_one_const_returns_multiple_singleton() {
        // Single-input ValuePhi (degenerate, but the classifier
        // must still produce a Multiple([K]) — we don't second-
        // guess by collapsing to Single, since the orchestrator
        // treats Multiple-of-len-1 identically).
        let (graph, anchor) = build_value_phi_graph(&[42]);
        let result = classify_anchor(&graph, anchor, None);
        assert_eq!(result, Some(ResolvedTargets::Multiple(vec![42])));
    }

    #[test]
    fn classify_value_phi_with_one_non_const_returns_none() {
        // Build a ValuePhi with mixed inputs: one IntConst and
        // one InitialVar.  The arm must return None — we cannot
        // soundly enumerate the target set when any input is a
        // runtime value.
        let other_vn = fake_reg_vn(0x10, 8);
        let mut builder = FunctionBuilder::new_raw(vec![other_vn], &[], &[], &[], None, 0)
            .expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        let const_out = builder.build_int_const(0x1234u64, NodeOutputType::U64);
        let var_out = builder.read_variable(&other_vn).expect("read_variable");
        let dummy = builder.build_int_const(0u64, NodeOutputType::U64);
        builder.build_return(Some(dummy), &[]).expect("build_return");
        let mut graph = builder.build().expect("build");
        let fake_token_node = graph.graph.create_node(
            NodeKind::ControlPhi(rsleigh::Vn {
                addr: rsleigh::VnAddr {
                    space: rsleigh::VnSpace::REGISTER,
                    off: 0xdead,
                },
                size: 8,
            }),
            [],
            [NodeOutputKind::ControlPhi],
        );
        let [token_out] = graph
            .graph
            .node_outputs_exact::<1>(fake_token_node)
            .expect("token output");
        let vp_node = graph.graph.create_node(
            NodeKind::ValuePhi,
            [token_out, const_out, var_out],
            [NodeOutputKind::OutputType(NodeOutputType::U64)],
        );
        let [vp_out] = graph
            .graph
            .node_outputs_exact::<1>(vp_node)
            .expect("value-phi output");

        // No lr supplied: the InitialVar arm doesn't accidentally
        // classify as LinkRegister either.
        assert_eq!(classify_anchor(&graph, vp_out, None), None);
    }

    #[test]
    fn classify_value_phi_empty_returns_none() {
        // M1 (review fix): a ValuePhi with no value inputs MUST NOT
        // classify as Multiple(vec![]).  An empty target set would
        // silently feed the orchestrator a Switch{targets:[]}
        // terminator with no successor edges, making the dispatch
        // site appear unreachable.  We treat the degenerate case as
        // "still unresolved at this iteration" — None — so the
        // orchestrator either retries on a later iteration or, at
        // fixed point, surfaces `UnresolvedIndirectBranch` cleanly.
        // A degenerate zero-value-input ValuePhi cannot arise from
        // the normal lift path, but DeadBranchElim's input-detach
        // can leave a zero-input phi observable transiently.
        let (graph, anchor) = build_value_phi_graph(&[]);
        let result = classify_anchor(&graph, anchor, None);
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
        let producer_kind = *graph.graph.kind_of_output(anchor);
        assert!(
            matches!(producer_kind, NodeKind::IntBinaryOp(_)),
            "fixture must produce an IntBinaryOp; got {producer_kind:?}"
        );
        assert_eq!(classify_anchor(&graph, anchor, None), None);
    }
}
