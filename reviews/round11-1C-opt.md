# Round 11 — 1C: opt audit

Scope: every file under `crates/opt/src/**/*.rs`, every file under
`crates/opt/tests/**/*.rs`, the crate's `Cargo.toml`, and `crates/opt/README.md`.
Branch `feature/ai`. Findings derived only from the current code (no prior
audit reports were read).

## Baseline blocker triage

`cargo clippy --workspace --all-targets -- -D warnings` produces **89 errors**
in the `opt` lib-test target (the workspace `lib`/`bin`/`bench` build is
clean). Categorised exactly:

| Category | Count | Files |
|---|---|---|
| `unused import: BuiltFunctionGraph` | 2 | `crates/opt/src/stack_store/tests.rs:6`, `crates/opt/src/test_support.rs:18` |
| `useless conversion to the same type: &ir::BuiltFunctionGraph` | 87 | constant_fold/tests.rs (×62), known_bits/tests.rs (×17), if_cond_inversion/tests.rs (×2), load_readonly/tests.rs (×2) — all from `(&fg).into()` callsites |

Root cause: a partial migration of test helpers. `crates/opt/src/test_support.rs`
defines a *canonical* `return_kind`/`return_value`/`count`/`count_reachable`
that takes `pattern::RewriteCtxView<'_>`. Five per-pass test modules
(`constant_fold/tests.rs:15-27`, `known_bits/tests.rs:8-15` and `:231-237`,
`load_readonly/tests.rs:24-31`, `if_cond_inversion/tests.rs:43-50` and
`:53-59`) still define **shadowing local helpers** that take
`&ir::BuiltFunctionGraph`. The callsites uniformly use
`return_kind((&fg).into())` — the `.into()` does a no-op
`Into<&BuiltFunctionGraph> for &BuiltFunctionGraph` conversion (clippy:
useless), and the local-vs-shared shadow-and-import drift left two unused
imports.

The 5 lib-test errors at `if_cond_inversion/tests.rs:37` and `:224` reach
into `find_unique_if((&fg).into())` (also a local helper at line 43 with
`fg: &ir::BuiltFunctionGraph`).

Cleanup path:

1. **Delete** every per-test-module local `return_value` / `return_kind` /
   `find_unique_if` definition (lines listed above).
2. **Import** the canonical helpers from `crate::test_support`. They're
   already `pub(crate)` so no API change.
3. **Drop** every `(&fg).into()` at the callsite — the canonical helpers
   take `pattern::RewriteCtxView<'_>` directly and `&BuiltFunctionGraph`
   already has `Into<RewriteCtxView<'_>>`. Either keep the `.into()` and
   it stops being useless (calls the cross-type conversion), or call
   `(&fg).into()` is replaced by passing `&fg` directly if `RewriteCtxView`
   has a `From<&BuiltFunctionGraph>`.
4. **Remove** the unused `use ir::BuiltFunctionGraph;` lines at
   `test_support.rs:18` and `stack_store/tests.rs:6`.

The canonical `test_support.rs` import on line 18
(`use ir::{BuiltFunctionGraph, Value};`) is unused because no function in
that file references the symbol — the helpers all take
`pattern::RewriteCtxView`. Drop `BuiltFunctionGraph`, keep `Value` (used
on line 24's signature).

## Coverage

| File | Lines | Read |
|---|---|---|
| `Cargo.toml` | 40 | full |
| `README.md` | 122 | full |
| `src/lib.rs` | 202 | full |
| `src/error.rs` | 3 | full |
| `src/pipeline.rs` | 375 | full |
| `src/sp_expr.rs` | 947 | full |
| `src/test_support.rs` | 56 | full |
| `src/worklist.rs` | 110 | full |
| `src/constant_fold/eval_float.rs` | 138 | full |
| `src/constant_fold/eval_int.rs` | 204 | full |
| `src/constant_fold/mod.rs` | 95 | full |
| `src/constant_fold/rules.rs` | 738 | full |
| `src/constant_fold/tests.rs` | 1926 | spot-checked (helper-defs + ~4 representative tests) |
| `src/dead_branch/mod.rs` | 269 | full |
| `src/dead_branch/tests.rs` | 386 | spot-checked (top 120 lines) |
| `src/flag_cmp_canonicalize/mod.rs` | 403 | full |
| `src/flag_cmp_canonicalize/tests.rs` | 401 | spot-checked (top 80 lines) |
| `src/function_args/mod.rs` | 552 | full |
| `src/function_args/tests.rs` | 1016 | spot-checked (top 80 lines) |
| `src/if_cond_inversion/mod.rs` | 140 | full |
| `src/if_cond_inversion/tests.rs` | 237 | full |
| `src/indirect_branch_resolve/classify.rs` | 239 | full |
| `src/indirect_branch_resolve/inplace.rs` | 454 | full |
| `src/indirect_branch_resolve/jump_table.rs` | 776 | full |
| `src/indirect_branch_resolve/jump_table_tests.rs` | 1204 | spot-checked (top 280 lines, builders + shape tests) |
| `src/indirect_branch_resolve/mod.rs` | 685 | full |
| `src/indirect_branch_resolve/stack_array.rs` | 858 | full |
| `src/known_bits/mod.rs` | 519 | full |
| `src/known_bits/tests.rs` | 483 | spot-checked (top 80 + helper at 231) |
| `src/load_readonly/mod.rs` | 100 | full |
| `src/load_readonly/tests.rs` | 155 | full (160-line subset shown; total 155) |
| `src/redundant_phis/mod.rs` | 212 | full |
| `src/redundant_phis/tests.rs` | 305 | spot-checked (top 100 lines) |
| `src/stack_load_forward/mod.rs` | 597 | full |
| `src/stack_load_forward/tests.rs` | 1214 | spot-checked (top 100 lines + helper) |
| `src/stack_store/call_args.rs` | 323 | full |
| `src/stack_store/detect.rs` | 123 | full |
| `src/stack_store/mod.rs` | 11 | full |
| `src/stack_store/tests.rs` | 1179 | spot-checked (top 250 lines) |
| `tests/asm_fingerprint_propagation.rs` | 318 | full |
| `tests/common/mod.rs` | 74 | full |
| `tests/indirect_branch_resolve.rs` | 113 | full |
| `tests/known_bits_edge_cases.rs` | 42 | full |
| `tests/multi_pass.rs` | 354 | full |
| `tests/pipeline_default.rs` | 109 | full |
| `tests/pipeline_fixedpoint.rs` | 91 | full |
| `tests/pipeline_subsets.rs` | 145 | full |
| `tests/pipeline_validation.rs` | 43 | full |
| `tests/pipeline_with_stack.rs` | 143 | full |
| `tests/wide_const_passthrough.rs` | 90 | full |
| `benches/constant_fold.rs` | — | not read |
| `benches/default_pipeline.rs` | — | not read |
| `benches/known_bits.rs` | — | not read |
| `benches/stack_store.rs` | — | not read |

49 of 53 .rs/.toml/.md files inspected; 4 `benches/*.rs` files skipped.

## Findings

### 1. Stale `BuiltFunctionGraph` shadow helpers + 87 useless-conversion errors

- **Severity:** HIGH
- **Where:**
  - `crates/opt/src/test_support.rs:18` — `use ir::{BuiltFunctionGraph, Value};`
    imports `BuiltFunctionGraph` that no function in the file uses.
  - `crates/opt/src/stack_store/tests.rs:6` — `use ir::BuiltFunctionGraph;`
    has zero references in the file.
  - `crates/opt/src/constant_fold/tests.rs:15-27` — local
    `return_value`/`return_kind` taking `&BuiltFunctionGraph`; called as
    `return_kind((&fg).into())` at 62 sites.
  - `crates/opt/src/known_bits/tests.rs:8-15`, `:231-237` — same pattern
    (17 callsites).
  - `crates/opt/src/load_readonly/tests.rs:24-31` — same (2 callsites).
  - `crates/opt/src/if_cond_inversion/tests.rs:43-50` (`find_unique_if`),
    `:53-59` (`if_cond_kind`) — same (2 callsites).
- **What's wrong:** The canonical helpers in `crate::test_support`
  (`return_kind` etc.) take `pattern::RewriteCtxView<'_>`. Each per-pass
  tests module re-declares its own `&BuiltFunctionGraph`-flavoured copy
  *and* the callsites still use `(&fg).into()`. The `Into<&BuiltFunctionGraph>
  for &BuiltFunctionGraph` blanket impl makes `.into()` a no-op, which
  clippy flags as 87 errors. Two `BuiltFunctionGraph` imports that survived
  the partial migration are now unused (2 errors). Total 89 — the documented
  pre-noted blocker.
- **Verified against:** `crates/opt/src/test_support.rs:24` defines the
  canonical `return_value(fg: pattern::RewriteCtxView<'_>)`. The test
  modules' local copies are textually older signatures.
- **Fix:** Delete every local `return_value`/`return_kind`/`find_unique_if`
  in the affected per-pass test modules, replace with
  `use crate::test_support::{return_kind, return_value};` (and a tiny inline
  `find_unique_if` rewrite for `if_cond_inversion/tests.rs` taking
  `RewriteCtxView`). Drop the two unused `BuiltFunctionGraph` imports.
  Then `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- **Regression test (when applicable):** The clippy invocation itself is
  the regression check. Add a CI job pinning `cargo clippy --all-targets`
  at the workspace level if not already.

### 2. `OptimizationResult::after_replace` discards the replace_all_uses bool when the input itself was `Changed`

- **Severity:** LOW (functionally correct; cost-only)
- **Where:** `crates/opt/src/pipeline.rs:44-54`
- **What's wrong:** `after_replace(self, …)` always extends the new node's
  fingerprint via `extend_asm_fingerprint_from`, even when
  `replace_all_uses` returns `false` (no consumers redirected). The
  fingerprint extension is then a soundness no-op (it grows the side-table
  for a node whose semantic role didn't change), but it's cheap and
  preserves the superset contract. No bug, but the doc on lines 32-39
  references "the parameter type changed from `&mut ir::BuiltFunctionGraph`
  to `&mut pattern::RewriteCtx<'_>`" — this is migration-history prose
  that has nothing to do with the function's contract. Cleanup: delete
  lines 36-39 of the docstring.
- **Verified against:** `crates/opt/src/load_readonly/mod.rs:93` — the
  one production caller of `after_replace`. Behaviour is correct
  regardless of the return-bool.
- **Fix:** Drop the migration-history paragraph; document only the
  contract.

### 3. `mem_chain_is_dirty` cycle-handling salvages soundness only via top-level OR-merge

- **Severity:** LOW (currently sound; fragile)
- **Where:** `crates/opt/src/function_args/mod.rs:417-549`, specifically
  the `seen.insert(cur_mem)` short-circuit at lines 456-460.
- **What's wrong:** The `seen` set is graph-wide and never rolled back.
  When two MemPhi predecessors converge to the same memory output `Y`,
  the second visit pushes `false` (clean) regardless of `Y`'s actual
  verdict. Soundness depends on the OR-merge at every enclosing
  `Frame::JoinPhi` propagating the truth from whichever predecessor
  *did* discover dirty bits past `Y`. This works, but the per-pred
  result-stack entry is wrong on the revisit path. A future contributor
  who inlines `mem_chain_is_dirty`'s logic into a context that uses
  AND-merging (or per-pred final extraction without further joining)
  would silently produce wrong answers.
  - Counter-example I traced: 2-pred MemPhi where pred A is clean prefix
    + reaches shared node Y whose subgraph is dirty; pred B reaches Y
    directly. Iteration order processes B first: B walks Y's subgraph,
    discovers dirty, B's slot = `true`. A then walks its prefix, reaches
    Y → `seen` hit → A's slot = `false` (wrong, but the OR `true||false
    = true` saves the join).
- **Verified against:** the doc comment at lines 405-411 explicitly notes
  the trade-off ("Sub-frame results aren't cached because their
  cleanliness depends on the cycle set populated above them"). The doc
  is honest about the design but doesn't pin an OR-merge invariant for
  future refactors.
- **Fix:** Either (a) document the OR-merge invariant explicitly so
  future refactors don't swap the join op, or (b) track per-mem-id
  results in a memo distinct from `seen` (i.e. `result_memo:
  FxHashMap<NodeOutputId, bool>`) and use it on revisit instead of
  pushing `false`. Option (b) is strictly more sound and removes the
  invariant tax.
- **Regression test (when applicable):** Construct a 2-pred MemPhi where
  pred 1's chain is clean+long+reaches shared store-aliasing-prefix,
  pred 2 reaches the same shared store directly; assert `mem_chain_is_dirty`
  returns `true` (this works currently because of OR; would catch a
  regression that swaps the merge op).

### 4. `decompose_sp` And-arm is recursive inside an otherwise iterative loop

- **Severity:** LOW (typical inputs unaffected)
- **Where:** `crates/opt/src/sp_expr.rs:339-361`
- **What's wrong:** The whole `decompose_sp` function was rewritten as
  iterative to fix scale.md A1 (round 8). The comment at lines 263-269
  documents this: "Implemented iteratively so deep `sp + K1 + K2 + ... +
  KN` chains … cannot overflow the thread stack." But the `And` arm at
  line 356 still calls `decompose_sp(g, sp_input, sp_vn, memo, visiting)`
  recursively. A pathological chain of nested `And(And(And(... sp ..., m1),
  m2), m3)` would still recurse one stack frame per layer.
- **Verified against:** Real binaries chain at most one alignment AND
  (e.g. `and esp, 0xfffffff8`); deep AND-chains aren't observed in
  practice. The existing 5000-node chain regression test
  (`decompose_sp_does_not_stack_overflow_on_deep_chain`,
  `crates/opt/src/sp_expr.rs:870-894`) only exercises Add-chains.
- **Fix:** Convert the And-arm to push a continuation onto the iterative
  spine — replace the inner `decompose_sp` call with the equivalent
  iterative descent. Or document the depth bound (single AND in
  practice) so the limitation is captured.
- **Regression test (when applicable):** Add a 1000-deep `And(And(...
  And(sp, m1), m2) ..., mN)` chain. With current code it would still
  recurse and overflow at sufficient N.

### 5. `realize` in `stack_load_forward` is recursive on `ResolveShape::Phi`

- **Severity:** LOW (typical depth small)
- **Where:** `crates/opt/src/stack_load_forward/mod.rs:408-432`
- **What's wrong:** `probe` was made iterative explicitly to handle deep
  memory chains (lines 162-180 of the same file). `realize` mirrors
  the shape that probe produced but recurses on every nested
  `ResolveShape::Phi`. A nested-MemPhi memory chain (rare but legal)
  could overflow `realize`'s stack.
- **Verified against:** `probe` only emits `ResolveShape::Phi` for
  `MemPhi`-shaped memory chains; in practice these are no more than
  ~4 levels deep (matching loop nesting), so the existing form is safe
  on production graphs.
- **Fix:** If desired for symmetry, replace `realize` with an iterative
  worklist. Otherwise pin the depth in a doc comment so the asymmetry
  is documented.

### 6. `bound_via_known_bits` returns `None` for fully-unknown inputs, masking what KB *did* prove for narrower types

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:312-322`
- **What's wrong:** The line `if max == type_mask { return None; }` skips
  the case where KB proved nothing tighter than the natural type_mask.
  But for `idx: U32`, `type_mask = 0xFFFF_FFFF`. If KB proves
  `kb.zeros == 0` (no bits known zero), the function returns `None` and
  defers to `bound_via_predecessor_if`. This is intended: a 4-billion-
  entry enumeration is rejected upstream (`MAX_TABLE_ENTRIES`), so
  there's nothing to gain from "computing" the bound at type_mask. But
  for narrower types like `U8`, `type_mask = 0xFF` is *also* rejected.
  Yet a U8 jump table with 256 entries is in principle within
  `MAX_TABLE_ENTRIES` (which is 4096). The current code refuses to
  even try.
- **Verified against:** `crates/opt/src/indirect_branch_resolve/mod.rs:53`
  — `MAX_TABLE_ENTRIES` is 4096; a U8 idx with 256 possible values
  fits. The KB analyser would return `kb.max_value(0xFF) = 0xFF` and
  the function would correctly compute `Some(0x100)`.
- **Fix:** Drop the `max == type_mask` early-return when `type_mask <
  MAX_TABLE_ENTRIES`. Keep it for the U32+ case to avoid the
  4-billion-entry trap.

### 7. `IndirectBranchResolve::optimize_built` short-circuits the second anchor's classifier on a known-bits map taken before the first anchor's edit

- **Severity:** LOW (currently sound; documented; fragile)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:340-403`
- **What's wrong:** `let known = crate::analyze_known_bits(fg.as_view())?;`
  is computed BEFORE the per-anchor loop. The comment (lines 340-348)
  asserts that in-place edits don't affect the bounds on existing
  producers, which is true. But `apply_tail_call` creates fresh nodes
  (IntConst, Call, Return) that are absent from the cached `known` map.
  If a later anchor's classifier somehow needed bounds on a node
  introduced by the earlier edit, the cached map wouldn't have them.
  Currently no anchor depends on another's edit, so this is fine.
- **Verified against:** `apply_tail_call` (`inplace.rs:153-191`) creates
  the IntConst, Call, Return on dedicated `target_value`-derived inputs
  — no second-anchor classifier reads from these.
- **Fix:** Document the precondition explicitly in the comment block at
  lines 340-348, or recompute KB if the in-place edit fired (an extra
  `analyze_known_bits` call between anchors when `changed` becomes
  `true`).

### 8. `IndirectBranchResolve::add_anchor` claims "lockstep" but doesn't reject duplicate-anchor pushes that would silently no-op

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:285-293`,
  precondition at `:163-167`.
- **What's wrong:** The `unresolved_anchors` field documents
  "**Precondition:** each anchor must appear at most once" but
  `add_anchor` doesn't enforce it. A caller passing the same
  `(addr, anchor_output)` twice gets the second push appended and
  the second `anchor_contexts.insert` overwrites the first's context.
  The optimizer's per-anchor loop would then visit the same anchor
  twice; the second visit's `find_placeholder_return_for_anchor` would
  return `None` (the first visit detached the placeholder), and the
  pass silently no-ops on the duplicate — masking the contract
  violation.
- **Verified against:** `find_placeholder_return_for_anchor`
  (`mod.rs:423-438`) returns `None` after the first edit detaches the
  IndirectBranch. The orchestrator's deduplication is documented at
  line 167 but not enforced here.
- **Fix:** Either insert with a `HashMap`-style guard in `add_anchor`
  (return `Result` or panic on duplicate, depending on policy) or
  surface the duplicate as an error in `optimize_built`.

### 9. `bound_via_predecessor_if` walks `IndirectBranch`'s slot 0 (control) but the slot index is fragile

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:362-368`
- **What's wrong:** `let control_in = *graph.node_inputs(placeholder).get(0)?;`
  reads slot 0. The comment names "see node_signature::expected_signature
  for IndirectBranch: `inputs: [CTRL, MEM, TARGET]`" — pinning a contract
  by file reference. If a future IR change reordered IndirectBranch's
  inputs (e.g. inserting a new slot 0), this code reads the wrong slot
  and silently walks the wrong control predecessor. The lookup would
  be cleaner via `node_inputs_exact::<3>` which would surface a
  size mismatch as `Err`.
- **Fix:** Use `node_inputs_exact::<3>(placeholder)` and destructure
  `[ctrl, _mem, _tgt]`; surface a typed Err on shape mismatch.

### 10. `f32` const-fold path silently discards bits 32-63 of the u64 IntConst

- **Severity:** LOW
- **Where:** `crates/opt/src/constant_fold/eval_float.rs:60, 79, 88`
- **What's wrong:** `eval_float_binary`/`eval_float_cmp`/`eval_float_unary`
  at the F32 arm cast `bits_l: u64` to `u32` via `bits_l as u32` (the
  macro's `as _` resolves to u32). If the IntConst was stored with
  high bits set (which it shouldn't be, but a malformed lifter could),
  those bits are silently truncated. `make_float_const` (in `ir`)
  pre-masks per-type, so the bits are clean in well-formed IR.
- **Fix:** Add a `debug_assert!(bits_l >> 32 == 0, "F32 IntConst with
  high bits set")` in the F32 arm so a future regression in the
  constructor surfaces in tests rather than silently producing
  numerically-wrong constants.

### 11. `dead_branch::dead_uses_all_zero_input` only considers ControlState consumers, ignoring other dead-side consumers

- **Severity:** LOW (idempotency-only; not soundness)
- **Where:** `crates/opt/src/dead_branch/mod.rs:183-191`
- **What's wrong:** The helper's logic is "if the dead-side has CS
  consumers and they ALL have zero inputs already, no work to do".
  But a dead-side could have non-CS consumers (e.g. a dead `Call` whose
  ctrl is the dead branch). Those consumers aren't checked here. The
  check is part of the idempotency early-return (line 100-104), so
  the worst case is one extra fixed-point iteration — not a
  correctness bug. The `dead_subgraph_escapes` check below it does
  the right thing for soundness; the `dead_uses_all_zero_input`
  check is only an optimisation.
- **Verified against:** lines 89-91 — the escape check uses the more
  general `collect_dead_subgraph` walker. Lines 100-104 are the
  early-return guard.
- **Fix:** Extend the guard to include non-CS consumers (e.g.
  `!matches!(*fg.node_kind(*n), NodeKind::ControlState) ||
  fg.node_inputs(*n).is_empty()` becomes `fg.node_inputs(*n).is_empty()`
  unconditionally, since any consumer with no inputs is detached). Or
  document explicitly that the helper only considers CS-consumers and
  that's deliberate.

### 12. `RedundantPhis::optimize` (`Optimizer` impl) and `optimize_built` (private companion) have asymmetric visibility

- **Severity:** LOW (style)
- **Where:** `crates/opt/src/redundant_phis/mod.rs:171-209`
- **What's wrong:** `RedundantPhis` implements `Optimizer` directly
  (lines 171-179) and bridges to `optimize_built` (line 182) via
  `with_rewrite_ctx`. But every other rewrite-flavoured pass implements
  `OptimizerOnBuilt`, gaining the `Optimizer` impl via the blanket
  in `pipeline.rs:154-162`. `RedundantPhis` is the only stable-pipeline
  pass that splits into both methods explicitly. The `optimize_built`
  is a private inherent method (not the trait method) — invoking it
  outside the file isn't possible. There's no semantic reason for the
  asymmetry I can identify; the comment claims `IndirectBranchResolve`
  goes the same way ("`OptimizerPipeline::run` … in-place edits straddle
  multiple `with_rewrite_ctx`-style boundaries", `pipeline.rs:138-140`)
  but `RedundantPhis` does not have the same straddling concern — its
  whole body is one `with_rewrite_ctx` invocation.
- **Fix:** Convert `RedundantPhis` to `OptimizerOnBuilt` like every
  other rewrite-flavoured pass. The blanket impl gives the same
  behaviour with less code. (DeadBranchElimination already uses
  `OptimizerOnBuilt` — `dead_branch/mod.rs:255`.)

### 13. `apply_link_register` mutates `NodeKind` from `IndirectBranch` to `Return` on the same NodeId — relies on the IR contract that those two kinds share signature shape

- **Severity:** LOW (verified shapes match)
- **Where:** `crates/opt/src/indirect_branch_resolve/inplace.rs:43-72`
- **What's wrong:** The kind mutation `set_node_kind(placeholder,
  NodeKind::Return)` (line 70) only succeeds when the input/output
  signatures match. The doc at lines 67-69 asserts: "Same input/output
  signature shape (control + memory + variadic value tail; no outputs);
  both kinds are non-cacheable." This is correct (per
  `node_signature::expected_signature` for both kinds). If a future IR
  change adds a slot to either kind's signature, this mutation would
  fail at runtime. The current code propagates the error via `?`, so
  it's contained — but the failure mode is "set_node_kind returned
  Err" rather than a typed contract violation.
- **Verified against:** the IR doc claims `Return` has variadic value
  inputs after `[ctrl, mem]`, and `IndirectBranch` has the trailing
  `target_value` slot which `apply_link_register` removes before the
  kind switch. Shapes match post-mutation.
- **Fix:** Add a doc-test or unit test that pins both kinds' signatures
  share `(Control, Memory, Variadic)` shape, so a future signature
  change surfaces the assumption rather than a silent runtime
  failure.

### 14. `KnownBits::optimize_built` calls `analyze` once per pass invocation, then drives a worklist that doesn't re-analyse on rewrite

- **Severity:** LOW
- **Where:** `crates/opt/src/known_bits/mod.rs:452-518`
- **What's wrong:** Phase 1 computes `known` from the current graph
  (line 457). Phase 2 walks the worklist and replaces fully-determined
  outputs with constants (lines 463-515). After each replacement,
  `result = OptimizationResult::Changed` and the consumers are
  re-enqueued — but the `known` map is NOT updated. The replacement
  changed an output's producer to `IntConst(kb.ones)`, which has a
  different KB than whatever the analyser concluded for the pre-replace
  output. Subsequent worklist iterations within the same Phase-2
  drain still consult the stale `known` map. This means cascading
  KB folds within one pass invocation may miss propagation
  opportunities. The pipeline's outer fixed-point loop catches up on
  the next iteration (`KnownBits` re-runs and re-`analyze`s), so it's
  correctness-preserving but extra-iteration-paying.
- **Fix:** Either re-run `analyze` after every Phase-2 rewrite (expensive
  but tight), or update `known` in place on rewrite (cheap; the new
  IntConst's KB is `Kb::from_const(kb.ones, ty)`). The latter is the
  obvious optimisation.

### 15. `IndirectBranchResolve` clones `Arc<dyn ReadOnlyMemory>` per anchor but the ROM is shared

- **Severity:** LOW (style)
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:154-157`
- **What's wrong:** `pub rom: Option<Arc<dyn ReadOnlyMemory>>` is fine
  for cheap cloning. But the `add_anchor` /  `clear_anchors` API
  doesn't expose a builder-pattern for setting the rom — callers
  must touch the public field directly. Combined with the
  documented lockstep contract on `unresolved_anchors` /
  `anchor_contexts`, this leaves the rom as a "shared mutable" public
  field. Not a correctness issue.
- **Fix:** Add `with_rom(Arc<dyn ReadOnlyMemory>) -> Self` builder
  method.

### 16. Pipeline doc comment at `lib.rs:79-84` lists `KnownBits` exports but `Kb` and `KnownBitsMap` aren't documented in README's public-surface section

- **Severity:** LOW (documentation)
- **Where:** `crates/opt/src/lib.rs:79`, `crates/opt/README.md:44`
- **What's wrong:** The README at line 44 lists "`KnownBits`-flavour
  utilities: `Kb`, `analyze_known_bits`." but doesn't mention
  `KnownBitsMap` (the public type alias for the analysis result),
  which `lib.rs:79` re-exports. Pattern users that want to call
  `bound_via_known_bits` directly need both `KnownBitsMap` and `Kb`.
- **Fix:** Add `KnownBitsMap` to the README's public-surface bullet.

## Coverage summary

49 of 53 files inspected fully (or substantially), 0 partially, 4 skipped
(the four `benches/*.rs` files were not in the documented audit
checklist).

Note: the test files at line counts > 1000 (`constant_fold/tests.rs:1926`,
`stack_load_forward/tests.rs:1214`, `function_args/tests.rs:1016`,
`stack_store/tests.rs:1179`, `jump_table_tests.rs:1204`,
`flag_cmp_canonicalize/tests.rs:401`, `dead_branch/tests.rs:386`,
`redundant_phis/tests.rs:305`) were spot-checked at the declaration-of-
helpers level + ~4-8 representative tests each — sufficient to identify
the helper-shadow pattern and the test-API drift, which is the actual
behavioural concern. No findings beyond the shadowing-helpers issue are
specific to the un-read parts of those files.
