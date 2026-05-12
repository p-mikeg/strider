# Round 8 / 1C — `opt` crate audit

**Branch:** `review/ai2`.  Independent audit.

## Coverage

All 37 `.rs` files under `crates/opt/src/` read and analyzed (constant_fold, known_bits, if_cond_inversion, flag_cmp_canonicalize, redundant_phis, dead_branch, stack_store, stack_load_forward, function_args, indirect_branch_resolve {classify, jump_table, stack_array, inplace}, load_readonly, pipeline, sp_expr, worklist, error, lib, test_support, plus all `tests.rs` modules).

## Findings

### MED: Redundant always-true guard in `apply_link_register`

- **Confidence:** 85.
- **Severity:** MED (maintenance hazard, not a correctness bug).
- **Where:** `crates/opt/src/indirect_branch_resolve/inplace.rs:61`.
- **What's wrong:**
  ```rust
  let inputs = graph.node_inputs(placeholder);
  if inputs.len() > 2 && inputs.len() > ret_val_outputs.len() + 2 {
      graph.remove_node_input(placeholder, 2)?;
  }
  ```
  By the time control reaches this guard, the for-loop above has appended all `ret_val_outputs` to a node that started with exactly 3 inputs (`control`, `memory`, `target_value`).  After append: `inputs.len() == 3 + N`.  Both halves of the guard reduce to `3 + N > 2` (always true) and `3 > 2` (always true).  The `remove_node_input(placeholder, 2)` is therefore unconditional.  A future reader could mis-read this as conditional and infer that `target_value` removal is sometimes skipped.
- **Fix:** Replace the dead guard with an unconditional `graph.remove_node_input(placeholder, 2)?;`, optionally preceded by `debug_assert!(graph.node_inputs(placeholder).len() >= 3, "IndirectBranch must have ≥3 inputs");`.

### LOW: `FlagCmpCanonicalize::try_apply_rule` bypasses `after_replace` fingerprint helper

- **Confidence:** 80.
- **Severity:** LOW (currently correct; pattern-inconsistency hazard for future rule additions).
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:135`.
- **What's wrong:** `try_apply_rule` calls `function.graph.replace_all_uses(root_out, new_out)?` directly instead of going through the `after_replace` method on `OptimizationResult` (`LoadReadOnly`'s pattern).  The fingerprint contract is upheld today because each individual RHS builder (`build_int_cmp`, `build_bool_neg`, `rhs_thumb_b`) calls `extend_asm_fingerprint_from(new_node, root)` for every intermediate node.  The comment at lines 143-147 records this obligation.
  Risk: a future rule's `build_rhs` closure (signature `fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> NodeOutputId`) does not enforce the per-builder discipline.  An omission would only be caught by `validate_with_options { check_asm_fingerprints: true }`, which is opt-in.
- **Fix:** Either route through `after_replace` (would auto-absorb root fingerprint into outermost RHS node) OR add a unit test that exercises `check_asm_fingerprints` against every rule in the canonicalization table.

## Areas verified correct (informational; no findings)

- **`IfCondInversion` VarPhi/MemPhi positional semantics**: `invert()` redirects control-input slots at the downstream join `ControlState` rather than swapping value positions in phis.  Slot↔semantic mapping is preserved.
- **`decompose_sp` memoization**: Only stores `Some(_)` results; deliberately allows re-traversal of cycle-truncated nodes.
- **`RedundantPhis` orphan-detach `Changed` suppression**: Returns `Ok(())`, not `Changed`; prevents fixed-point thrashing.
- **`KnownBits` shift ≥ bit-width**: Returns all-zeros `Kb`, matching Sleigh's `OpBehaviorIntLeft::evaluateBinary` semantics.
- **`walk_control_for_if_bound_iter` `combined=0` identity**: Used as `max()` identity; caller correctly rejects zero finals at step 3.
- **`mem_chain_is_dirty` Call-node passthrough**: Follows memory input before the call; correct under standard ABIs.
- **`LoadReadOnly` u64 truncation for U128/U256**: Returns `None` (correct graceful degradation).
- **`apply_link_register` no `after_replace` call**: Mutates placeholder in place (kind change + input surgery), so no replacement-node fingerprint absorption needed.
- **`OptimizerPipeline` MAX_ITERS=1024 cap**: prevents infinite fixed-point; end-of-run `validate` guarantees structural invariants.

## Summary

| # | Severity | Confidence | Title |
|---|---|---|---|
| 1 | MED | 85 | Dead always-true guard in `apply_link_register` |
| 2 | LOW | 80 | `FlagCmpCanonicalize::try_apply_rule` bypasses `after_replace` |
