---
name: strider-opt-pass-author
description: Scaffold a new strider opt pass under crates/opt/src/, choose stable/destructive/post-pass placement, propagate asm-fingerprints, and add Python parity.
---

# strider-opt-pass-author

## When to use

User wants to add a new IR rewrite pass to the `opt` crate. Triggers include "add an optimisation pass that folds X into Y", "rewrite NodeKind A into B", "scaffold a new pass under `crates/opt/src/`", "I want a pass that canonicalises ...".

## When NOT to use

- The rewrite is a single match-and-substitute over one matchable subtree — `pattern::rewrite_rule` plus `strider::GraphRewriter` (`crates/strider/src/rewrite.rs`) is simpler than a full pass.
- The user is fixing a bug in an existing pass — use the `superpowers:systematic-debugging` skill first.

## Inputs the skill expects

- A description of the rewrite (input shape to output shape).
- Whether the pass is stable (rewrites that survive new phi inputs in a later strider iteration), destructive (node removal, only safe at fixed point), or a post-pass (runs once after convergence).
- Whether the pass needs the calling convention or a ROM image (e.g. uses SP, endianness, or `.rodata`).

## Procedure

1. Decide the trait. **Prefer `OptimizerOnBuilt`** — its `optimize_built(&mut pattern::RewriteCtx<'_>)` signature is the higher-level surface (`RewriteCtx` Derefs to `Graph` and exposes `preorder()` / `preorder_kind()` ergonomically), and the blanket `impl<T: OptimizerOnBuilt> Optimizer for T` adapts via `with_rewrite_ctx` so it slots into the same pipeline as raw `Optimizer` impls. Implement `Optimizer` directly only when you need the lower-level `(&mut Graph, NodeId)` pair (e.g. `IndirectBranchResolve`, whose in-place edits straddle multiple `with_rewrite_ctx` boundaries). Templates: `crates/opt/src/if_cond_inversion/` and `crates/opt/src/constant_fold/` are both `OptimizerOnBuilt`.
2. Create file layout: `crates/opt/src/<pass>/{mod.rs, tests.rs}`. Match neighbouring passes (`if_cond_inversion`, `redundant_phis`, `flag_cmp_canonicalize`).
3. Register the type in `crates/opt/src/lib.rs`: `mod <pass>;` then `pub use <pass>::<Type>;`.
4. Pick a pipeline placement in `crates/opt/src/pipeline.rs`. `default_pipeline()` runs in the strider top-level fixed point. `stable_default_pipeline()` must NOT remove `VarPhi` / `MemPhi` / `ControlState` / `If` nodes that the orchestrator's `RegionIndex` pins. `destructive_default_pipeline()` runs once at fixed-point exit. `add_post_pass` runs once after convergence (e.g. `CallStackArgCollect`, `FunctionArgDetect`).
5. Implement the rewrite. Prefer `pattern::rewrite_rule` for matchable subtrees with a single output replacement. Hand-write surgery only when you need use-list edits the rewrite engine cannot express (e.g. branch swap in `IfCondInversion`). The pass must be idempotent — the fixed-point loop calls it repeatedly. Stable passes must respect the phi-input contract: pre-existing phi nodes get new predecessors in later strider iterations, so do not delete a phi or `ControlState` whose `NodeId` the orchestrator tracks.
6. Propagate asm-fingerprints (REQUIRED). Every newly-created `NodeId` that replaces or derives from an existing node must inherit contributors via `Graph::extend_asm_fingerprint_from(new, contributor)` (`crates/ir/src/graph/store.rs:184`). The contract is superset-only: never shrink, never replace with a node whose fingerprint is a strict subset of an ancestor's. If the rewrite has multiple contributors, call `extend_asm_fingerprint_from` once per contributor. Canonical example: `crates/opt/src/flag_cmp_canonicalize/mod.rs`.

   **Trap: multi-node `pattern::rewrite_rule` rewrites.** `rewrite_rule` only attributes the *outermost* RHS node from the matched root. If your rule's RHS produces fresh interior nodes (e.g. `((a&C1)|(b&C2))&C3 → (a&(C1&C3)) | (b&(C2&C3))` creates two new `And` nodes), those interior nodes are non-exempt and will fail `validate_with_options(check_asm_fingerprints: true)`. For multi-node RHS rules: either use `Graph::create_node_attributed(..., &[contributor])` directly when building the RHS, or post-walk the freshly-built subtree and union the root's fingerprint into each new non-exempt node.
7. Tests in `tests.rs` must include happy-path rewrite, no-op (a graph that does NOT match stays bit-identical), idempotency (running the pass twice equals once), interaction with `ConstantFold` / `KnownBits` / the next pass without infinite loops, and a fingerprint test asserting `extend_asm_fingerprint_from` propagated correctly.
8. Python parity if the pass is user-facing. Add a wrapper class in `crates/strider-py/src/opt.rs` and update `PipelineState::from_default()`. There is no compile-time sync — manually mirror.

## Verification

- `cargo test --package opt <pass>` — the new pass's unit tests.
- `cargo test --package strider` — regression on the orchestrator's pipeline.
- `cargo test --package ir asm_fingerprint` — fingerprint contract.
- `cargo clippy --workspace -- -D warnings`.
- If touching Python: `uv run maturin develop && uv run pytest crates/strider-py/tests/python/test_optimizer_pipeline.py`.

## Exit criteria

- The pass is reachable via `default_pipeline()` / `stable_default_pipeline()` / `destructive_default_pipeline()` (or registered as a post-pass) as appropriate.
- All existing strider tests pass — no destabilisation of the indirect-branch fixed point.
- `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` passes after the pass on at least one fixture.
- Python wrapper added (if applicable) and parity test green.

## Pitfalls

- Adding a destructive rewrite to `stable_default_pipeline` invalidates the orchestrator's per-iteration `RegionIndex`. Always test against the switch-jump-table fixture in `cargo test --package strider`.
- Forgetting `extend_asm_fingerprint_from` silently breaks the proof-of-correctness contract. Pin with a test that asserts a non-empty fingerprint on the rewritten root.
- Production `expect()` / `unwrap()` is rejected by `clippy::expect_used` / `unwrap_used` in non-test code. Use `?` propagation with `anyhow::anyhow!` context.
- Recursive graph-walking helpers blow the 8 MB Rust stack on pathological inputs. Convert to iterative `Vec<...>` worklists or add a `MAX_DEPTH` guard.
- Forgetting Python parity: `PipelineState::from_default()` reconstructs Rust's `default_pipeline` by hand; adding a Rust pass without updating Python silently desyncs.

## Related skills

- `strider-fingerprint-audit` — mandatory final step before completion.
- `strider-pattern-author` — when the rewrite predicate uses a `Pat`.
- `strider-py-binding` — when adding the Python wrapper.
