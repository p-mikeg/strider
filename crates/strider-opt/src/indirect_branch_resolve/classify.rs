//! Producer-shape classifier for indirect-branch resolution.
//!
//! Walks the producer node of a placeholder anchor's value-input and
//! classifies it into a [`ResolvedTargets`].  Each arm is a
//! soundness-checked shape (`IntConst`, `InitialVar(lr)`, anonymous `Phi`
//! of constants, jump-table load, stack-array load) — the comments on
//! each arm spell out why the runtime target set is constrained.
//!
//! [`ResolvedTargets`] is re-exported from `cfg`, so callers can pass
//! results from the classifier directly into
//! `strider_lift::cfg::Builder::with_known_targets`.

use strider_ir::node::{NodeKind, ValueId};

use strider_lift::cfg::ResolvedTargets;

use super::jump_table::classify_jump_table;
use crate::ReadOnlyMemory;

/// Classify a placeholder anchor's producer node into a
/// [`ResolvedTargets`], given an optional [`ReadOnlyMemory`] (for the
/// rodata jump-table arm) and an optional stack-pointer varnode (for
/// the stack-array-of-labels arm).
///
/// Returns:
/// - `Some(_)` — successful classification.
/// - `None` — producer doesn't match any of the known sound shapes;
///   the orchestrator interprets this as "still unresolved at this
///   iteration; try again or surface as `UnresolvedIndirectBranch`
///   at fixed point."
///
/// `link_register_vn` is the calling convention's link register
/// varnode (`None` on stack-push ABIs like x86 / x86_64 where there
/// is no architectural link register).  When `None`, the
/// `InitialVar(lr) → LinkRegister` arm is short-circuited — there
/// can be no LR match without a known LR varnode.
///
/// When `rom` is `None`, the rodata-jump-table arm is short-circuited.
/// When `stack_vn` is `None`, the stack-array arm is
/// short-circuited.
///
/// The orchestrator passes both: the rom for the binary-image rodata,
/// and the calling convention's stack-pointer varnode for the
/// stack-array shape.  Callers compute the known-bits analysis once
/// via [`crate::analyze_known_bits`] and pass the cached map; the
/// graph doesn't change between iterations of the resolver's outer
/// loop, so a single KB pass suffices for every anchor.
///
/// # Soundness
///
/// Every arm in this match must be a producer shape that, on the
/// optimised IR, **unambiguously** identifies the indirect branch's
/// runtime target.  Shapes the prior in-place heuristic tried
/// (`Load(InitialVar(sp))` for `pop pc`-style returns) are
/// deliberately NOT included here: a `push X; pop pc` tail call
/// has the same Load-shape and would be misclassified as a return.
/// We rely on `LoadForward` having already simplified
/// properly-popped return addresses to `InitialVar(lr_vn)` directly
/// — that's the shape the LinkRegister arm matches.
///
/// Both rom- and stack-pointer-driven arms preserve the classifier's
/// overall contract: the resulting `ResolvedTargets::Multiple`
/// enumerates the *full* set of possible runtime targets.  Failing
/// closed (returning `None`) on any partial proof defers the branch
/// to a later iteration or to `UnresolvedIndirectBranch` at fixed
/// point — never under-approximating.
#[must_use]
pub fn classify_anchor(
    ctx: &strider_ir::Function,
    anchor_value: ValueId,
    link_register_vn: Option<rsleigh::Vn>,
    rom: Option<&dyn ReadOnlyMemory>,
    endianness: strider_target::Endianness,
    stack_vn: Option<rsleigh::Vn>,
    known: &crate::KnownBitsMap,
) -> Option<ResolvedTargets> {
    let graph = ctx.graph();
    let function = ctx;
    let producer_id = graph.producer(anchor_value);
    let kind = *graph.node_kind(producer_id);
    match kind {
        // SOUND: a literal constant in the IR comes from one of:
        //   - a tracked IntConst pcode insn in the source region,
        //   - constant folding (`ConstantFold`),
        //   - a `LoadReadOnly` resolution against the binary's rodata.
        // All three are deterministic functions of the function's
        // pcode, so the same address is the only possible runtime
        // target of this BranchIndirect.
        NodeKind::IntConst(k) => {
            // Wide constants (high 64 bits set) are never valid branch
            // targets on any 64-bit ISA — defer to UnresolvedIndirectBranch
            // rather than silently routing to a truncated wrong address.
            Some(ResolvedTargets::Single(
                crate::indirect_branch_resolve::u128_to_branch_target(k)?,
            ))
        }
        // SOUND: `InitialVar(vn)` is the function-entry value of
        // varnode `vn`.  When `vn == lr_vn`, the indirect branch
        // dispatches to the caller-provided return address — i.e. a
        // standard return.  This is the shape `LoadForward`
        // produces for properly-popped return addresses.
        NodeKind::InitialVar(vn) if Some(vn) == link_register_vn => {
            Some(ResolvedTargets::LinkRegister)
        }
        // SOUND: an anonymous `Phi`'s output is the merge of one
        // per-predecessor value input (slot 0 is the phi token,
        // slots 1.. are the values).  When *every* value input folds
        // to `IntConst(k_i)`, the runtime target set is exactly
        // `{k_i}` for the predecessors that ever reach this branch.
        //
        // Vn-tagged phis are excluded: their register-identity
        // semantics must not be folded into a target-set computation.
        NodeKind::Phi if function.phi_var_tag(producer_id).is_none() => {
            let inputs = graph.node_inputs(producer_id);
            let mut targets = Vec::with_capacity(inputs.len().saturating_sub(1));
            for val in inputs.into_iter().skip(1) {
                match graph.kind_of_value(val) {
                    NodeKind::IntConst(k) => {
                        // Same wide-const guard as the IntConst arm
                        // above: defer the whole Phi if any value input
                        // doesn't fit u64.
                        targets.push(crate::indirect_branch_resolve::u128_to_branch_target(*k)?);
                    }
                    _ => return None,
                }
            }
            targets.sort_unstable();
            targets.dedup();
            // SOUND: an empty `Multiple` would silently advertise zero
            // runtime targets, making the dispatch site appear
            // unreachable.  Defer instead.
            if targets.is_empty() {
                None
            } else {
                Some(ResolvedTargets::Multiple(targets))
            }
        }
        // Jump-table arm.  Producer is a Load — a candidate for
        // the canonical `Load(IntAdd(IntConst(base), IntMul(idx,
        // IntConst(stride))))` jump-table dispatch shape.
        //
        // when the rodata jump-table arm doesn't match and
        // an SP varnode is supplied, fall through to
        // `stack_array::classify_stack_array` which handles the
        // computed-goto-via-local-stack-array shape.  Both arms fail
        // closed (return None) on any partial proof.
        NodeKind::Load(_) => {
            if let Some(r) = classify_jump_table(ctx, anchor_value, rom, endianness, known) {
                return Some(r);
            }
            if let Some(sp) = stack_vn {
                return super::stack_array::classify_stack_array(ctx, anchor_value, sp, known);
            }
            None
        }
        // ARM / arm-thumb / arm-be lifters wrap the
        // dispatch target in `IntBinaryOp(And)` with a constant mask
        // (`& 0xFFFFFFFE` for 32-bit ARM Thumb-interworking).  The
        // stack_array classifier transparently strips the mask, so
        // route And-anchors through the same arm — but only when the
        // SP varnode is supplied.
        NodeKind::IntBinaryOp(strider_ir::IntBinaryOp::And) => {
            if let Some(sp) = stack_vn {
                return super::stack_array::classify_stack_array(ctx, anchor_value, sp, known);
            }
            None
        }
        // No dedicated `Truncate(IntConst)` / `Extend(IntConst)` arm:
        // ConstantFold rules 4-6 fold those shapes to `IntConst` before
        // the classifier runs, and `truncate_if_needed` /
        // `extend_if_needed` fold them at build time.  The folded
        // `IntConst` flows through the `NodeKind::IntConst` arm above.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for [`classify_anchor`].
    //!
    //! Each test constructs a minimal [`strider_ir::Graph`]
    //! via [`strider_ir::FunctionBuilder::new_raw`], appends nodes
    //! directly via `graph.create_node` to control the producer shape
    //! exactly, and then invokes the classifier on the targeted output.
    //! These tests intentionally bypass the strider IR-lift path so the
    //! classifier's match arms are exercised in isolation without
    //! depending on the optimiser pipeline producing the expected
    //! shape.

    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use strider_ir::IRBuilderExt;
    use strider_ir::FunctionBuilder;
    use strider_ir::node::{NodeKind, ValueKind, ValueType};
    use strider_ir_test_utils::{RegisterSet, SENTINEL_LIFT_ADDR, reg_vn as fake_reg_vn};

    /// Unit-test convenience: recomputes `analyze_known_bits` and
    /// calls [`classify_anchor`] with no rom and no SP varnode.  The
    /// integration-style tests in `tests/indirect_resolve_classify.rs`
    /// drive the rom/SP arms; these unit tests only exercise the
    /// IntConst / InitialVar / Phi / Load-fallthrough arms.
    fn classify_anchor_bare(
        ctx: &strider_ir::Function,
        anchor_value: ValueId,
        link_register_vn: Option<rsleigh::Vn>,
    ) -> anyhow::Result<Option<ResolvedTargets>> {
        let known = crate::analyze_known_bits(ctx)?;
        Ok(classify_anchor(
            ctx,
            anchor_value,
            link_register_vn,
            None,
            strider_target::Endianness::Little,
            None,
            &known,
        ))
    }

    /// Build a minimal `Graph` with one tracked
    /// variable and an empty body region terminated by a Return
    /// whose single value-input is the caller-supplied
    /// `ValueId`.  Used as a scaffold for the unit tests so
    /// the classifier sees a real, validation-passing graph.
    fn empty_graph_returning(
        anchor_inputs: impl FnOnce(&mut FunctionBuilder) -> ValueId,
    ) -> (strider_ir::Function, ValueId) {
        // No tracked variables, no calling convention plumbing.
        let mut builder = strider_ir_test_utils::empty_builder().expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        let anchor = anchor_inputs(&mut builder);
        // Re-stamp in case `anchor_inputs` cleared the lift_addr.
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");
        // Re-locate the anchor in the built graph: the build step
        // is a move, but `ValueId` is a stable cranelift-entity
        // index so the same id continues to point at the same
        // output in the resulting graph.
        (function, anchor)
    }

    #[test]
    fn classify_int_const_returns_single() {
        let (function, anchor) = empty_graph_returning(|fb| {
            // Single IntConst node.  Output type is I64 — chosen
            // because BranchIndirect targets are pointer-sized on
            // every supported 64-bit arch; smaller widths would
            // also fold via the `as u64` cast in the classifier.
            fb.build_int_const(0x1234u64, ValueType::I64).unwrap()
        });
        let result = classify_anchor_bare(
            &function,
            anchor,
            None,
        )
        .expect("classify");
        assert_eq!(result, Some(ResolvedTargets::Single(0x1234)));
    }

    #[test]
    fn classify_int_const_when_lr_unset_still_returns_single() {
        // Pinned: the IntConst arm does not consult
        // `link_register_vn`.  A None lr (x86 / x86_64) must not
        // suppress IntConst classification.
        let (function, anchor) = empty_graph_returning(|fb| {
            fb.build_int_const(0xfeed_face_u64, ValueType::I64).unwrap()
        });
        assert_eq!(
            classify_anchor_bare(
                &function,
                anchor,
                None
            )
            .expect("classify"),
            Some(ResolvedTargets::Single(0xfeed_face)),
        );
    }

    #[test]
    fn classify_initial_var_with_matching_lr_returns_link_register() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        // Build a graph where the only tracked variable IS the
        // link register; reading it produces an `InitialVar(lr)`
        // output.
        let mut builder = RegisterSet::new()
            .tracked(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        // `read_variable` in the entry region's only predecessor
        // (the function entry) returns the InitialVar.
        let anchor = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        // PhiCollapse hasn't run, so the producer might be a
        // VarPhi rather than InitialVar directly.  Inspect.
        // For the LinkRegister arm to match, the producer must be
        // `InitialVar(lr_vn)` — we walk past a single-input
        // VarPhi in the test if we hit one, since
        // PhiCollapse would have done that in production.
        let mut producer_value = anchor;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function.phi_var_tag(pid).is_some();
            if !is_var_phi {
                break;
            }
            // VarPhi inputs: [phi_token, ...per-pred values].
            // With one predecessor, slot 1 is the value.
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.graph().nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_anchor_bare(
            &function,
            producer_value,
            Some(lr_vn),
        )
        .expect("classify");
        assert_eq!(result, Some(ResolvedTargets::LinkRegister));
    }

    #[test]
    fn classify_initial_var_with_non_matching_vn_returns_none() {
        let lr_vn = fake_reg_vn(0x4c, 4);
        let other_vn = fake_reg_vn(0x10, 4);
        // Track only `other_vn`; reading it gives `InitialVar(other_vn)`.
        // Pass `lr_vn` as the link register — they don't match, so
        // the classifier must return None.
        let mut builder = RegisterSet::new()
            .tracked(other_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let anchor = builder
            .read_variable(&other_vn)
            .expect("read_variable(other)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        let mut producer_value = anchor;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function.phi_var_tag(pid).is_some();
            if !is_var_phi {
                break;
            }
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.graph().nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_anchor_bare(
            &function,
            producer_value,
            Some(lr_vn),
        )
        .expect("classify");
        assert_eq!(result, None);
    }

    #[test]
    fn classify_initial_var_with_lr_unset_returns_none() {
        // InitialVar(lr_vn), but `link_register_vn = None` (the
        // x86 / x86_64 case).  The `Some(vn) == None` guard fails;
        // classifier returns None.
        let lr_vn = fake_reg_vn(0x4c, 4);
        let mut builder = RegisterSet::new()
            .tracked(lr_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let anchor = builder.read_variable(&lr_vn).expect("read_variable(lr)");
        builder
            .build_return(Some(anchor), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let function = builder.build().expect("build");

        let mut producer_value = anchor;
        loop {
            let pid = function.producer(producer_value);
            let is_var_phi = matches!(function.node_kind(pid), NodeKind::Phi)
                && function.phi_var_tag(pid).is_some();
            if !is_var_phi {
                break;
            }
            if function.node_inputs(pid).len() != 2 {
                break;
            }
            let Some(slot1) = function.graph().nth_input(pid, 1) else {
                break;
            };
            producer_value = slot1;
        }

        let result = classify_anchor_bare(
            &function,
            producer_value,
            None,
        )
        .expect("classify");
        assert_eq!(result, None);
    }

    /// Helper for the ValuePhi tests: build a minimal graph
    /// containing a Region (to provide the phi token), a set
    /// of value-producing nodes (each one fed in as a per-pred
    /// input), and a `ValuePhi` whose first input is the phi token
    /// and whose remaining inputs are the per-pred values.
    /// Returns the graph and the value-phi's output id.
    ///
    /// Synthesises the ValuePhi via `graph.create_node` directly
    /// after `build()` has returned — bypassing the validator's
    /// per-predecessor-arity check (the graph-invariants phi check requires phi inputs
    /// to match `Region`'s predecessor count, which we don't
    /// satisfy here).  This is intentional: the unit tests
    /// exercise `classify_anchor` against fully synthetic shapes
    /// that the validator would reject in production.  The
    /// integration tests in `tests/indirect_resolve_classify.rs` cover the
    /// validation-passing path end-to-end.
    fn build_value_phi_graph(per_pred_consts: &[u64]) -> (strider_ir::Function, ValueId) {
        let mut builder = strider_ir_test_utils::empty_builder().expect("FunctionBuilder::new_raw");
        let region = builder.create_region().expect("create_region");
        builder.set_entry_region(region).expect("set_entry_region");
        builder.set_region(region);
        builder.set_lift_addr(Some(SENTINEL_LIFT_ADDR));

        // Build all per-predecessor IntConst nodes; remember their
        // output ids so we can wire them into the ValuePhi below.
        let const_values: Vec<ValueId> = per_pred_consts
            .iter()
            .map(|k| builder.build_int_const(*k, ValueType::I64).unwrap())
            .collect();

        // Use a dummy IntConst as the synthetic placeholder anchor
        // so `build_return` succeeds and `build()` validates.  The
        // ValuePhi we synthesise after build is unreachable from
        // entry, so it can have any shape we want.
        let dummy = builder.build_int_const(0u64, ValueType::I64).unwrap();
        builder
            .build_return(Some(dummy), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let mut function = builder.build().expect("build");

        // Synthesise a fake phi-token node.  VarPhi nodes
        // produce PhiToken outputs, but the dedup cache keys
        // them by (NodeKind, inputs, outputs), so we can hand-
        // construct one with no inputs.  We need the phi-token
        // output kind for the ValuePhi's first input slot to
        // typecheck against `expected_signature`'s `PHI` slot.
        let fake_token_node =
            function
                .graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::PhiToken]);
        function.set_phi_var_tag(
            fake_token_node,
            rsleigh::Vn {
                addr_off: 0xdead,
                addr_space: rsleigh::VnSpace::REGISTER,
                size: 8,
            },
        );
        let [token_value] = function
            .node_outputs_exact::<1>(fake_token_node)
            .expect("token output");

        // Build the ValuePhi: inputs = [phi_token, ...vals]; output
        // is a single value (I64 for definiteness).
        let vp_node = function.graph_mut().create_node(
            NodeKind::Phi,
            std::iter::once(token_value).chain(const_values.iter().copied()),
            [ValueKind::Typed(ValueType::I64)],
        );
        let [vp_value] = function
            .node_outputs_exact::<1>(vp_node)
            .expect("value-phi output");
        (function, vp_value)
    }

    #[test]
    fn classify_value_phi_of_consts_returns_multiple_dedup_sorted() {
        // Phi(IntConst(7), IntConst(3), IntConst(7)) →
        //   Multiple(sorted, deduped) = Multiple([3, 7]).
        let (function, anchor) = build_value_phi_graph(&[7, 3, 7]);
        let result = classify_anchor_bare(
            &function,
            anchor,
            None,
        )
        .expect("classify");
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
        let (function, anchor) = build_value_phi_graph(&[42]);
        let result = classify_anchor_bare(
            &function,
            anchor,
            None,
        )
        .expect("classify");
        assert_eq!(result, Some(ResolvedTargets::Multiple(vec![42])));
    }

    #[test]
    fn classify_value_phi_with_one_non_const_returns_none() {
        // Build a ValuePhi with mixed inputs: one IntConst and
        // one InitialVar.  The arm must return None — we cannot
        // soundly enumerate the target set when any input is a
        // runtime value.
        let other_vn = fake_reg_vn(0x10, 8);
        let mut builder = RegisterSet::new()
            .tracked(other_vn)
            .build_fn_single_region()
            .expect("RegisterSet::build_fn_single_region");
        let const_value = builder.build_int_const(0x1234u64, ValueType::I64).unwrap();
        let var_value = builder.read_variable(&other_vn).expect("read_variable");
        let dummy = builder.build_int_const(0u64, ValueType::I64).unwrap();
        builder
            .build_return(Some(dummy), &[])
            .expect("build_return");
        builder.set_lift_addr(None);
        let mut function = builder.build().expect("build");
        let fake_token_node =
            function
                .graph_mut()
                .create_node(NodeKind::Phi, [], [ValueKind::PhiToken]);
        function.set_phi_var_tag(
            fake_token_node,
            rsleigh::Vn {
                addr_off: 0xdead,
                addr_space: rsleigh::VnSpace::REGISTER,
                size: 8,
            },
        );
        let [token_value] = function
            .node_outputs_exact::<1>(fake_token_node)
            .expect("token output");
        let vp_node = function.graph_mut().create_node(
            NodeKind::Phi,
            [token_value, const_value, var_value],
            [ValueKind::Typed(ValueType::I64)],
        );
        let [vp_value] = function
            .node_outputs_exact::<1>(vp_node)
            .expect("value-phi output");

        // No lr supplied: the InitialVar arm doesn't accidentally
        // classify as LinkRegister either.
        assert_eq!(
            classify_anchor_bare(
                &function,
                vp_value,
                None
            )
            .expect("classify"),
            None
        );
    }

    #[test]
    fn classify_value_phi_empty_returns_none() {
        // A ValuePhi with no value inputs must not classify as
        // Multiple(vec![]).  An empty target set would
        // silently feed the orchestrator a Switch{targets:[]}
        // terminator with no successor edges, making the dispatch
        // site appear unreachable.  We treat the degenerate case as
        // "still unresolved at this iteration" — None — so the
        // orchestrator either retries on a later iteration or, at
        // fixed point, surfaces `UnresolvedIndirectBranch` cleanly.
        // A degenerate zero-value-input ValuePhi cannot arise from
        // the normal lift path, but DeadBranchElim's input-detach
        // can leave a zero-input phi observable transiently.
        let (function, anchor) = build_value_phi_graph(&[]);
        let result = classify_anchor_bare(
            &function,
            anchor,
            None,
        )
        .expect("classify");
        assert_eq!(result, None);
    }

    #[test]
    fn classify_unrelated_node_kind_returns_none() {
        // An IntAdd node — not IntConst, not InitialVar, not
        // ValuePhi — must classify as None.
        let (function, anchor) = empty_graph_returning(|fb| {
            let lhs = fb.build_int_const(1u64, ValueType::I64).unwrap();
            let rhs = fb.build_int_const(2u64, ValueType::I64).unwrap();
            fb.build_int_binary_operation(lhs, rhs, strider_ir::IntBinaryOp::Add, ValueType::I64)
                .expect("build_int_binary_operation")
        });
        // Note: ConstantFold would turn 1+2 into IntConst(3), but
        // we don't run the optimiser here — the unit tests use the
        // raw builder output.  The returned anchor's producer is
        // an IntBinaryOp node, which the `_ => None` arm catches.
        let producer_kind = *function.kind_of_value(anchor);
        assert!(
            matches!(producer_kind, NodeKind::IntBinaryOp(_)),
            "fixture must produce an IntBinaryOp; got {producer_kind:?}"
        );
        assert_eq!(
            classify_anchor_bare(
                &function,
                anchor,
                None
            )
            .expect("classify"),
            None
        );
    }
}
