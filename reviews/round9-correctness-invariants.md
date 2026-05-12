# Round 9 — Ask-8 R2: Correctness / Invariants pass

**Branch:** `feature/ai` (clean working tree, HEAD `c7a2903`)

## Critical

### Finding 1 — Asm-fingerprint superset contract violated: `FunctionArgDetect::detect_stack_args` drops the rewritten Load's fingerprint when `load_ty == max_type`

**Confidence:** 85.

**Where:** `crates/opt/src/function_args/mod.rs:329-336`.

When `load_ty == max_type` (direct-width replacement, no Truncate inserted), the code calls `fg.replace_all_uses(old_out, new_out)?` and `fg.detach_node_inputs(load)` without ever calling `extend_asm_fingerprint_from` on any surviving node. `new_node` is `FunctionArg` (exempt). When downstream non-exempt consumers exist, their fingerprint does not automatically acquire the Load's contributing addresses — superset invariant violated for those consumers.

The Truncate path (lines 340-349) correctly absorbs into the `trunc` node before redirecting uses. The exact-width path skips this step.

**Fix:** Before `replace_all_uses(old_out, new_out)` in the `load_ty == max_type` arm, propagate the load's fingerprint into every surviving consumer (iterate use-list of `old_out` before redirect, call `extend_asm_fingerprint_from(consumer_node, load)` for each).

### Finding 2 — `check_layer_c_control_state` applies the reachability gate only to the zero-predecessor early-exit case, not to the per-input loop

**Confidence:** 82.

**Where:** `crates/ir/src/validate/layer_c.rs:56-91`.

Outer loop iterates `graph.nodes.keys()` (all arena nodes including unreachable zombies). When `inputs.is_empty()`, code correctly gates on `reachable.contains(node)` before emitting `EmptyControlStatePredecessors`. For non-empty input lists, the `reachable` check is skipped. If a future pass leaves a `ControlState` zombie with stale non-`Control` inputs, the validator emits false-positive `ControlStateNonControlPredecessor` and masks real errors.

**Fix:** Add `if !reachable.contains(node) { continue; }` at the top of the `ControlState` branch inside the loop, consistent with `check_layer_c_phis`'s pattern.

## Important

### Finding 7 — Stall budget consumes on count-stable iterations, not just on count-grew iterations

**Confidence:** 80.

**Where:** `crates/strider/src/orchestrator.rs:400-408`.

The stall guard fires when `!edge_set_changed && unresolved_after_edits.len() >= prev_unresolved_len`. The intent is "in-place-only iterations must make progress" measured as strict decrease. But the budget decrements when a single anchor is resolved and a new placeholder materializes in the same `StableOnly` step (count stays the same). Pathological anchor-replacement cycles exhaust the budget prematurely with a false "resolver stalled" error.

**Fix:** Change the stall condition to fire only when `unresolved_after_edits.len() > prev_unresolved_len` (strictly grew), or track the *set* of placeholder NodeIds rather than the count.

## Verified Correct

1. **Dedup-cache structural equivalence** (`create_node`, `update_input`, `detach_node_inputs`, `add_node_input`, `remove_node_input`): all mutation sites correctly call `evict_cache_entry_if_cacheable` before mutating.
2. **Layer A/B reachability scoping**: Layer A and Layer B correctly reachability-scoped. `check_layer_c_uniqueness` intentionally scans full arena. Other Layer C checks reachability-scoped except the gap noted in Finding 2.
3. **Single Entry / single InitialMemory**: `check_layer_c_uniqueness` scans the full arena and reports duplicates.
4. **Memory chain monotonicity**: `build_call_with_cc` advances `cur_region_memory` only when `!no_memory_clobber`. `build_store` always emits a new Memory output. `build_call_other_modeled` does not advance memory by design.
5. **Phi-token → ControlState ownership**: `check_layer_c_phis` validates input[0] is `PhiToken` from a `ControlState`. `StackStorePhi` uses the `VarPhi`'s existing phi token (already valid).
6. **`StackStorePhi` per-predecessor offsets**: created only by `StackStoreDetect::try_detect_stack_store`; `set_stack_phi_offsets` always called.
7. **`from_graph_and_entry_for_rewrite` partial state**: only `graph` and `entry` accessed by any caller chain.
8. **Compact GC completeness**: `retain_reachable` remaps all four SecondaryMap side-tables; `gc_wide_consts` covers wide consts. All six covered.
9. **`ConstantFold` / `KnownBits` / `RewriteRule` fingerprint absorption**: `pattern::rewrite_rule:91-93`, `KnownBits:466-469`, all confirmed.
10. **Iteration cap formula**: `cap = 2 * pending_at_iter_0 + 4` reasonable for the monotonic-growth argument.
