# Round 10 — `opt` crate

Reviewing 51 .rs files in `crates/opt/src/`. All findings derived from code shape rather than comments.

---

## CRITICAL

### C-1: `IndirectBranchResolve::optimize_built` — pre-computed KB cache invalidated by in-place edits

- **Severity:** HIGH (Confidence: 88)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:347`
- **What's wrong:** `known = analyze_known_bits(fg.as_view())` is called once, before the loop. The loop then calls `apply_link_register` (appends inputs to the placeholder, mutates its `NodeKind` from `IndirectBranch` to `Return`) and `apply_tail_call` (detaches the placeholder, emits fresh `IntConst → Call → Return` chain). For the LinkRegister arm the cache stays valid (only appends). For the tail-call arm, `apply_tail_call` calls `graph.detach_node_inputs(placeholder)` which modifies the use-list of the placeholder's inputs (including the value-input which may share sub-expressions with another anchor's address chain). A sub-expression shared between the first anchor's index chain and the second anchor's index chain now has a different use-list; the `known` entry for that output is stale. A second-batch jump-table arm may proceed with an over-narrow bound, enumerate fewer targets than the actual set, and produce a `Multiple` that excludes valid runtime addresses — soundness violation.
- **Verified against:** `inplace.rs:apply_tail_call` calls `graph.detach_node_inputs(placeholder)`; `analyze_known_bits` propagates via `output_uses` — a detached use is no longer in `output_uses`.
- **Fix:** Move `known = analyze_known_bits(fg.as_view())` inside the per-anchor loop body (cheap per-anchor; safe; most anchors are singletons anyway). Or: only apply the cache for anchors processed before any in-place edit fires, and re-analyze after the first edit.
- **Regression test:** Build a graph with two `IndirectBranch` anchors; the first resolves to `IntConst(K)` (tail call), the second is a jump-table Load. Assert that the second anchor's resolved target set after the combined pass equals what `analyze_known_bits` returns from a fresh analysis after the first in-place edit.

---

### C-2: `FunctionArgDetect::detect_register_args` — `replace_all_uses` result discarded; fingerprint absorption missing for the exact-width case

- **Severity:** HIGH (Confidence: 85)
- **Where:** `crates/opt/src/function_args/mod.rs:198`
- **What's wrong:** The exact-width register-arg path calls `fg.replace_all_uses(old_out, new_out)?` and discards the `bool` return. The `new_node` (`FunctionArg`) is created at line 189 but `fg.extend_asm_fingerprint_from(new_node, initial_var)` is never called for the exact-width path. The narrower path (stack-arg `detect_stack_args`, line 335) also discards the bool but does call `extend_asm_fingerprint_from(trunc, load)` (line 347). The exact-width path skips both. CLAUDE.md says "every pass that calls `replace_all_uses(old, new)` must extend new's fingerprint with old's contributors." `FunctionArg` is exempt from non-empty checks, but the superset-only contract still requires that the downstream consumers of `new_out` don't lose attribution that was on `old_out`'s producer.
- **Verified against:** `pipeline.rs::OptimizationResult::after_replace` documents the correct pattern: call `extend_asm_fingerprint_from(new_node, old_node)` THEN `replace_all_uses`. Line 198 skips both steps.
- **Fix:** Before line 198, add `fg.extend_asm_fingerprint_from(new_node, initial_var);`. Also consider `result |= OptimizationResult::from_changed(fg.replace_all_uses(...)?)` to avoid spurious `Changed`.

---

## IMPORTANT

### I-1: `pipeline.rs::OptimizerOnBuilt` doc comment self-contradiction
- **Severity:** LOW (Confidence: 90)
- **Where:** `crates/opt/src/pipeline.rs:136-137`
- **What's wrong:** The doc reads: "parameter type was migrated from `&mut pattern::RewriteCtx<'_>` to `&mut pattern::RewriteCtx<'_>`." Both "from" and "to" types are identical — copy-paste error. Should say "migrated from `&mut ir::BuiltFunctionGraph` to `&mut pattern::RewriteCtx<'_>`."
- **Fix:** Correct the doc comment.

### I-2: `OptimizerPipeline::run` iteration counter pre-incremented; off-by-one in error message
- **Severity:** LOW (Confidence: 82)
- **Where:** `crates/opt/src/pipeline.rs:270-283`
- **What's wrong:** `iters` starts at 0 and is incremented only after a changed iteration. The guard fires when `iters >= 1024`, meaning up to 1025 total iterations. Error message says "after 1024 iterations" — should be 1025.
- **Fix:** Reword the error or change the guard.

### I-3: `StackStoreDetect::try_detect_stack_store` — `replace_all_uses` result discarded; always returns `Changed`
- **Severity:** MED (Confidence: 83)
- **Where:** `crates/opt/src/stack_store/detect.rs:73-75`
- **What's wrong:** `fg.replace_all_uses(old_mem_out, new_mem_out)?;` discards the `bool`. If `old_mem_out` has no consumers, function still returns `Changed`, causing a spurious extra fixed-point iteration. `detach_node_inputs` is also called unconditionally, severing a node even when no rewiring happened.
- **Fix:** `let changed = fg.replace_all_uses(...)?; if changed { ... } else { Ok(NoChange) }`.

### I-4: `FunctionArgDetect::detect_stack_args` — same `replace_all_uses`-discard pattern as C-2
- **Severity:** MED (Confidence: 84)
- **Where:** `crates/opt/src/function_args/mod.rs:335,348`
- **Fix:** `result |= OptimizationResult::from_changed(fg.replace_all_uses(...)?);`.

### I-5: `decompose_sp` — `None` results never memoized
- **Severity:** LOW (Confidence: 80)
- **Where:** `crates/opt/src/sp_expr.rs:393-396`
- **What's wrong:** Memoization only inserts `Some(level_expr)`. The test `decompose_sp_does_not_cache_none_results` pins this as intentional ("never cache None") to avoid masking a cycle-truncated path. But for non-SP-rooted constants and pure integer arithmetic, `None` is stable and safe to cache. Performance issue, not correctness.
- **Fix (optional):** For leaf cases that reach `break None` (not via cycle guard), memoize `None`.

### I-6: `StackLoadForward::probe` — `StackStorePhi` not handled in inner match; falls through to `None`
- **Severity:** MED (Confidence: 85)
- **Where:** `crates/opt/src/stack_load_forward/mod.rs:214`
- **What's wrong:** `probe` handles `StackStore`, `Store`, `MemPhi` but not `StackStorePhi`. A `Load[sp + K]` whose memory chain passes through a `StackStorePhi` cannot be forwarded even when every per-predecessor offset is provably disjoint from K. Inconsistent with `function_args::mem_chain_is_dirty` which DOES handle `StackStorePhi` via `step_through_stack_store_phi`.
- **Verified against:** `sp_expr.rs::step_through_stack_store_phi` exists; `function_args/mod.rs` imports it; `stack_load_forward/mod.rs:17` does NOT.
- **Fix:** Add a `StackStorePhi` arm in `probe` that calls `step_through_stack_store_phi`.

### I-7: `FlagCmpCanonicalize::try_apply_rule` — fresh nodes built before `replace_all_uses` zero-uses check
- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:151-156`
- **What's wrong:** `build_rhs` is called first; the freshly-built nodes are kept even when the root has zero live uses (so `replace_all_uses` returns false). The fresh nodes leak into the arena as zombies. Benign (zombie-tolerant graph), but wasted allocations.
- **Fix:** Gate `build_rhs` behind `function.graph.output_uses(root_out).next().is_some()`.

### I-8: `FlagCmpCanonicalize` — `function.graph.replace_all_uses` direct access bypasses RewriteCtx
- **Severity:** LOW
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs:156`
- **What's wrong:** Calls `function.graph.replace_all_uses(...)` directly rather than `function.replace_all_uses(...)`. Currently identical because `RewriteCtx` derefs transparently, but sets a precedent for callers to reach around the ctx.
- **Fix:** Use `function.replace_all_uses(...)` for consistency.

### I-9: `StackStorePhi` zombie key leakage in `Graph::stack_phi_offsets`
- **Severity:** MED (Confidence: 80)
- **Where:** `crates/opt/src/stack_store/detect.rs:59-69`
- **What's wrong:** `set_stack_phi_offsets(new_node, offsets)` writes to a `SecondaryMap<NodeId, Vec<i64>>`. The old detached node's prior entry is never cleared. `detach_unreachable_nodes` operates on inputs, not side-tables. A pass that later calls `step_through_stack_store_phi` on an old detached node would read stale data. Currently no caller does this (passes seed from `preorder_kind`), but it's a semantic gap.
- **Fix:** On `detach_node_inputs`, or in `StackStoreDetect`'s post-rewrite cleanup, clear the `stack_phi_offsets` entry for the old node ID.

### I-10: `KnownBits` phase 2 — verified correct
- **Severity:** N/A (False positive on initial review)
- **Where:** `crates/opt/src/known_bits/mod.rs:461-475`
- **Status:** Correct. The `consumers` are captured before `replace_all_uses`, the bool gates both `result` update and `work.push`. No issue.

### I-11: `classify_anchor` / `classify_anchor_with_rom` recompute KB on every call
- **Severity:** LOW (Confidence: 80)
- **Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:65,100`
- **What's wrong:** Both public helpers call `analyze_known_bits(fg)?` unconditionally. External callers cannot pass a pre-computed KB map. Internal `IndirectBranchResolve::optimize_built` correctly caches once (modulo C-1).
- **Fix:** Document that these helpers are single-anchor conveniences; recommend `classify_anchor_with_rom_and_sp` with pre-computed `KnownBitsMap` for multi-anchor loops.

### I-12: `ConstantFold::optimize_built` — consumers re-enqueued even when later rule fires
- **Severity:** LOW (Confidence: 80)
- **What's wrong:** The `consumers` vector is captured once at the start of node processing (before any rule runs). If a later rule fires after an earlier rule already changed the node, the consumers may include nodes that are now unreachable from the rewritten output. Re-enqueuing a now-useless node is wasteful but not incorrect.

---

## Coverage

| File | Status |
|------|--------|
| `lib.rs` | Fully |
| `pipeline.rs` | Fully |
| `worklist.rs` | Fully |
| `sp_expr.rs` | Fully |
| `error.rs` | Partially |
| `test_support.rs` | Not |
| `constant_fold/mod.rs` | Fully |
| `constant_fold/rules.rs` | Fully |
| `constant_fold/eval_int.rs` | Not |
| `constant_fold/eval_float.rs` | Not |
| `constant_fold/tests.rs` | Partially |
| `dead_branch/mod.rs` | Partially |
| `dead_branch/tests.rs` | Not |
| `flag_cmp_canonicalize/mod.rs` | Fully |
| `flag_cmp_canonicalize/tests.rs` | Not |
| `function_args/mod.rs` | Fully |
| `function_args/tests.rs` | Not |
| `if_cond_inversion/mod.rs` | Fully |
| `if_cond_inversion/tests.rs` | Not |
| `indirect_branch_resolve/mod.rs` | Fully |
| `indirect_branch_resolve/classify.rs` | Fully |
| `indirect_branch_resolve/jump_table.rs` | Partially |
| `indirect_branch_resolve/jump_table_tests.rs` | Not |
| `indirect_branch_resolve/stack_array.rs` | Not |
| `indirect_branch_resolve/inplace.rs` | Partially |
| `known_bits/mod.rs` | Fully |
| `known_bits/tests.rs` | Not |
| `load_readonly/mod.rs` | Not |
| `load_readonly/tests.rs` | Not |
| `redundant_phis/mod.rs` | Fully |
| `redundant_phis/tests.rs` | Not |
| `stack_load_forward/mod.rs` | Fully |
| `stack_store/detect.rs` | Fully |
| `stack_store/call_args.rs` | Fully |
| `stack_store/mod.rs` | Not |
| `stack_store/tests.rs` | Not |

**Coverage gap:** ~20 test-side and load_readonly/stack_array files not read. Round 7 should backfill.
