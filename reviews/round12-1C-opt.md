# Round 12 — Agent 1C — `opt` crate audit

**Scope.** Every file under `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/**/*.rs` + `crates/opt/tests/**/*.rs` + `Cargo.toml` + `README.md` (≈45 production .rs files, ~10 test-only).  Branch `feature/ai`, `review/ai6`.  Trust model: strict — round7..round11 reviews not read.  Threshold: confidence ≥ 80.

## Methodology

Read each pass + the trait split + the shared `sp_expr` machinery; cross-checked the focus-area fixes against the source text; spot-checked external usage in `strider`, `strider-py`, and `cfg` for the trait-object boundary; grep'd for production panics, struct-literal misuse, and stale-name leftovers.

## Files reviewed

- Pipeline + crate scaffolding: `lib.rs`, `pipeline.rs`, `error.rs`, `worklist.rs`, `test_support.rs`, `Cargo.toml`, `README.md`
- Shared SP machinery: `sp_expr.rs`
- Per-pass: `constant_fold/{mod,eval_int,eval_float,rules}.rs`, `known_bits/mod.rs`, `flag_cmp_canonicalize/mod.rs`, `if_cond_inversion/mod.rs`, `redundant_phis/mod.rs`, `dead_branch/mod.rs`, `load_readonly/mod.rs`, `stack_store/{mod,detect,call_args}.rs`, `stack_load_forward/mod.rs`, `function_args/mod.rs`
- Indirect-branch: `indirect_branch_resolve/{mod,classify,inplace,jump_table,stack_array}.rs`

## Focus areas — verified clean

1. **Each pass — rewrite + no-op + idempotency + ordering.**  All passes implement `Optimizer`; all return `OptimizationResult::NoChange` on no-progress paths (verified `FlagCmpCanonicalize` zero-uses bail at `flag_cmp_canonicalize/mod.rs:155`, `StackStoreDetect` only-if-rewired at `stack_store/detect.rs:77-79`, `IfCondInversion` strictly removes one `BoolNeg` per application).  Ordering in `default_pipeline()` correctly serialises `ConstantFold → KnownBits → FlagCmpCanonicalize → IfCondInversion → RedundantPhis → DeadBranchElimination`.
2. **`Optimizer` / `OptimizerRaw` split.**  Pipeline stores `Box<dyn OptimizerRaw>`; blanket `impl<T: Optimizer> OptimizerRaw for T` adapts via `with_rewrite_ctx` (`pipeline.rs:154-162`).  Every pass implements `Optimizer` (verified by `grep`); strider-py's `ForwardPass` is the sole direct `OptimizerRaw` impl (`strider-py/src/opt.rs:35`), justifying the trait's continued existence.
3. **`Kb::ones()` / `Kb::zeros()` accessors + `pub(crate)` fields.**  Internal struct-literal `Kb { ones, zeros }` use is confined to `known_bits/mod.rs`'s per-op derivation (lines 192-396) where the invariants are enforced by construction, plus one test (`known_bits/tests.rs:230-231`).  No external (out-of-crate) construction surface.
4. **`FlagCmpCanonicalize` zero-uses early bail (W2 fix).**  `flag_cmp_canonicalize/mod.rs:155` — `if output_uses(root_out).next().is_none() { return Ok(false); }` is placed BEFORE `build_rhs` runs, so no orphan IntCmp/BoolNeg/IntAdd nodes leak into the arena.
5. **`KnownBits` `ZeroExtend`/`SignExtend` wide-input bail (W1 fix).**  Both arms bail via `let Some(input_mask) = u64_type_mask(input_ty) else { return Ok(None); };` (`known_bits/mod.rs:329-331, 346-348`) — the previous `unwrap_or(0)` silent-corruption mode is gone.
6. **`StackLoadForward` `StackStorePhi` arm (W1 fix).**  `stack_load_forward/mod.rs:256-272` — disjoint per-pred offsets walk through, may-alias bails.
7. **`LoadReadOnly` `size > 8` bail (W2 fix).**  `load_readonly/mod.rs:82-84` — exact `if size > 8 { continue; }` guard before calling `ReadOnlyMemory::read` (whose return is `Option<u64>`).
8. **`function_args::mem_chain_is_dirty` Err discipline (W6 fix).**  Malformed `MemPhi` (zero preds) and malformed `Call`/`CallOther` (< 2 inputs) raise `Err` rather than the unsafe `false` ("clean") verdict (`function_args/mod.rs:501-509, 515-528`).  Final result-stack invariant violations are also surfaced as `Err` (`function_args/mod.rs:545-558`).
9. **`indirect_branch_resolve` public API.**  `pub use` block (`indirect_branch_resolve/mod.rs:49-54`) exposes: `classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`, `apply_link_register`, `apply_tail_call`, `classify_jump_table`, `classify_stack_array`, plus the types `ResolvedTargets`, `AnchorCallingContext`, and `find_placeholder_return_for_anchor`.  No stale S1.1 surface remains in the source.
10. **`OptimizerPipeline::run` convergence.**  `pipeline.rs:265-281` — counter increments only on full Changed iterations; cap is `1024`; check is `if iters >= MAX_ITERS` after increment, so a stale Changed loop is bailed out with a typed `anyhow::bail!` rather than spinning.  Final `ir::validate::validate` runs only when every pass + post-pass succeeded — correct ordering.
11. **`decompose_sp` iterative form + no-None memoisation.**  `sp_expr.rs:263-403` — spine-driven loop walks Add/And chains, dispatches to `decompose_sp_phi` only on `VarPhi(sp)`.  No `None` is memoised (comment at `sp_expr.rs:394-399` + regression test at line 698-721).  Stack-safe up to 5000-node chains (pin test at line 871-894).
12. **`fg → ctx` rename complete in production code.**  All non-test source uses `ctx`/`function`/`graph` as the local name; `fg` only appears inside `#[cfg(test)]` blocks (`sp_expr.rs` tests, etc.), which is fine.
13. **Production panics.**  All `unwrap()`/`expect()`/`panic!()` outside `#[cfg(test)]` are confined to:
    - `flag_cmp_canonicalize/mod.rs:134-145, 184-186, 198-200` — five `expect("…must bind to a value output")` / `expect("…produces 1 output")` calls, each `#[allow(clippy::expect_used)]`-annotated and reasoned in nearby comments.  Every site is downstream of a successful `match_at` whose LHS pattern guarantees the binding, and every `create_node` upstream uses a single-`OutputType` shape — sound invariants.
    No `unreachable!()` / `todo!()` / `unimplemented!()` in production code.

## HIGH findings (confidence ≥ 80)

### Important: 80-89

**[Stale README API surface] `crates/opt/README.md:46` lists `AnchorAddr` among `indirect_branch_resolve`'s public types.**  Confidence: 88.

```
- `IndirectBranchResolve` (`indirect_branch_resolve/`) — producer-shape
  classifier for `BranchIndirect` placeholders. Exposes `classify_anchor`,
  `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`,
  `apply_link_register`, `apply_tail_call`, plus the result types
  `AnchorAddr`, `AnchorCallingContext`, `ResolvedTargets`,
  `find_placeholder_return_for_anchor`.
```

`AnchorAddr` does not exist in the crate (`grep -rn AnchorAddr crates/opt` finds only this README mention; nothing in `src/`).  The type appears to have been removed (likely a leftover from the W9 S1.1 cleanup) but the doc was not updated.

**Fix.**  Remove the `AnchorAddr,` token from the README's bullet:

```diff
-    `AnchorAddr`, `AnchorCallingContext`, `ResolvedTargets`,
+    `AnchorCallingContext`, `ResolvedTargets`,
```

This is the only doc-vs-code drift I can attribute high confidence to; everything else in the README matches the source.

## Summary

The `opt` crate is in excellent shape.  Each of the eleven passes (`ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`, `RedundantPhis`, `DeadBranchElimination`, `LoadReadOnly`, `StackStoreDetect`, `StackLoadForward`, `FunctionArgDetect`, `CallStackArgCollect`, plus the indirect-branch helpers) is rewrite-clean, idempotent, and ordered correctly in the public pipelines.  The W1/W2/W4/W6/W15 fixes are all verifiably in place at the cited line numbers.  The trait split is sound — `OptimizerRaw` retains its purpose for the type-erased `Box<dyn …>` adapter in `strider-py/src/opt.rs`, and every pass takes the ergonomic `Optimizer` path with the blanket impl.

The single HIGH finding is a one-token stale name in `README.md` (`AnchorAddr`) — code is correct, only the doc lags.

## Relevant absolute paths

- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/README.md` (HIGH — line 46)
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/pipeline.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/flag_cmp_canonicalize/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/load_readonly/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/stack_load_forward/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/function_args/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/sp_expr.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/opt.rs` (cross-crate confirmation of `OptimizerRaw`'s direct-impl consumer)
