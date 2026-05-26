# `opt` crate review, simplification, scaling, and testing

**Date:** 2026-04-25
**Branch:** `feature/opt-review` (off `feature/ai`)
**Worktree:** `.worktrees/opt-review`

## Goals

1. **Verify correctness** of every existing pass against the IR validator and the documented invariants. No silent behavior regressions.
2. **Reorganize** for clean separation: shared SP machinery in its own module, oversized files split by responsibility, test helpers centralized.
3. **Test every optimization** with both basic (single-rule) and complex (interaction / fixed-point / edge-case) cases — white-box per-pass plus black-box pipeline-level.
4. **Scale to large IR graphs** — sub-quadratic where the algorithm allows, memoize repeated walks, cut redundant allocation, switch the per-pass fixed-point sweep to a worklist.
5. **Zero clippy warnings** on `cargo clippy -p opt --all-targets -- -D warnings`.
6. **End-to-end smoke** via `cargo run --example analyzer` produces semantically-equivalent output before vs after.

## Non-goals

- No new optimization passes.
- No new external functionality (no new public types beyond what's needed for the reorganization).
- No public API breakage to `Optimizer` / `OptimizerPipeline` — the worklist refactor is per-pass internal.
- No changes outside `crates/opt/` (other than docs and possibly minimal test-only helpers if reused by other crates — none currently expected).

## Current state (baseline)

```
crates/opt/src/
├── constant_fold/{mod,rules,eval_int,eval_float,tests}.rs      ~1900 lines
├── dead_branch.rs                                                246
├── error.rs                                                       61
├── function_args.rs                                             1203
├── known_bits.rs                                                 375
├── lib.rs                                                         77
├── load_readonly.rs                                              151
├── pipeline.rs                                                   126
├── redundant_phis.rs                                             274
├── load_forward.rs                                         853
└── stack_store.rs                                               1023
                                                          total ~6100 lines
```

- 92 tests pass in the worktree baseline.
- Existing test coverage is uneven: `constant_fold` is rich (~50 tests in a sibling `tests.rs`), every other pass keeps its tests inline as `#[cfg(test)] mod tests` mixed with the implementation.
- Clippy on `opt --all-targets`: 15 warnings (mostly `must_use_candidate`, a few `match_same_arms` and `map(...).unwrap_or(...)`).
- The `make_fn` / `return_kind` / `return_value` / `sp_vn` / `count` test helpers are duplicated verbatim across 5+ test modules.
- `decompose_sp`, `SpExpr`, `ranges_disjoint` live in `stack_store.rs` and are imported from there by `function_args` and `load_forward` — re-exported `pub(crate)` from a module whose primary purpose is something else.

## Final structure

```
crates/opt/
├── src/
│   ├── lib.rs                       # public exports + default_pipeline
│   ├── error.rs
│   ├── pipeline.rs
│   ├── sp_expr.rs                   # NEW: SpExpr, decompose_sp (memoized), ranges_disjoint, int_const_signed
│   ├── constant_fold/{mod,rules,eval_int,eval_float,tests}.rs
│   ├── known_bits/{mod,tests}.rs        # split out
│   ├── dead_branch/{mod,tests}.rs       # split out
│   ├── redundant_phis/{mod,tests}.rs    # split out
│   ├── load_readonly/{mod,tests}.rs     # split out
│   ├── stack_store/
│   │   ├── mod.rs                       # re-exports both passes
│   │   ├── detect.rs                    # StackStoreDetect (was 1st half of stack_store.rs)
│   │   ├── call_args.rs                 # CallStackArgCollect (was 2nd half)
│   │   └── tests.rs                     # white-box tests for both passes
│   ├── load_forward/{mod,tests}.rs
│   └── function_args/
│       ├── mod.rs                       # FunctionArgDetect entry + struct
│       ├── register_args.rs             # detect_register_args
│       ├── stack_args.rs                # detect_stack_args + shadow walk
│       └── tests.rs
├── tests/                                # NEW: black-box integration tests
│   ├── common/
│   │   └── mod.rs                       # shared make_fn / return_kind / sp_vn / count
│   ├── pipeline_default.rs              # default_pipeline end-to-end
│   ├── pipeline_with_stack.rs           # detect+forward+args interactions
│   ├── pipeline_fixedpoint.rs           # convergence + idempotency
│   └── pipeline_validation.rs           # OptimizerPipeline::run final-validate guarantees
└── benches/                              # NEW: criterion benches
    ├── constant_fold.rs
    ├── known_bits.rs
    ├── stack_store.rs
    └── default_pipeline.rs
```

## Module contracts

### `sp_expr` (new)

Hosts:
- `pub(crate) enum SpExpr { Terminal{...}, Phi{...} }`
- `pub(crate) fn decompose_sp(fg, out, sp_vn, memo) -> Option<SpExpr>`
- `pub(crate) fn ranges_disjoint(a_off, a_size, b_off, b_size) -> bool`
- `pub(crate) fn int_const_signed(fg, out) -> Option<i64>`

The memo is a `&mut FxHashMap<NodeOutputId, Option<SpExpr>>` owned by the calling pass and reused across all calls within one `optimize` invocation.

Cycle handling via `visiting: &mut FxHashSet<NodeId>` is preserved; the memo only stores definitive results (cycles are not cached so a different call path can resolve them).

### `pipeline` (unchanged API)

- `Optimizer::optimize(&self, &mut BuiltFunctionGraph) -> Result<OptimizationResult>` — signature stays.
- `OptimizerPipeline::add` / `add_post_pass` / `run` — unchanged.
- `OptimizerPipeline::run` continues to call `validate::validate` at the very end.

### Per-pass internal worklist (the core scaling change)

Each pass that today does:
```rust
let nodes: Vec<_> = function.preorder().collect();
for node_id in nodes { ... }
```
becomes:
```rust
let mut work: WorkSet = WorkSet::seeded(function.preorder());
let mut result = OptimizationResult::NoChange;
while let Some(node_id) = work.pop() {
    let r = try_apply(function, node_id, &mut ctx)?;
    if r.changed() {
        result |= r;
        work.extend(consumers_of_changed_outputs(&ctx, function));
    }
}
Ok(result)
```

`WorkSet` is a small private helper (set + queue, prevents double-enqueue) used by every pass. `consumers_of_changed_outputs` reads back the list of `(NodeOutputId)` that were the LHS of a `replace_all_uses` and re-enqueues each consumer node.

### `KnownBits` worklist

The Phase-1 propagation loop today is `for_each_node × until_no_change`. Replace with: dirty queue seeded with all nodes; on each `Kb::merge(...)` returning `true`, enqueue every consumer node that uses this output.

## Test plan

For every pass, three tiers:

1. **Basic (white-box, in `<pass>/tests.rs`)**
   - Single-rule fires.
   - Single-rule no-fires (the canonical untouched case).
   - Trivial input shape.

2. **Complex (white-box)**
   - Rule chains hitting the worklist fixed-point.
   - Edge cases: integer width edges (U8/U16/U32/U64), explicit no-op on U128/U256, NaN/inf for floats, overflow.
   - Malformed / degenerate inputs that should be no-ops (single-input ControlState, zero-input phi, etc.).

3. **Pipeline (black-box, in `crates/opt/tests/`)**
   - Pass interaction inside `default_pipeline`.
   - Pass interaction inside `Analyzer::build_optimizer_pipeline` (for SP-aware passes).

### Concrete additions per pass (gaps relative to today)

| Pass | New basic | New complex |
|---|---|---|
| `constant_fold` | (already rich) | shifts at width boundaries, double-bitcast roundtrip on f32, NaN propagation through binary ops |
| `known_bits` | shifts for ShiftLeft/ShiftRight, ZeroExtend propagation, Truncate of partially-known | popcount-then-and chains, lzcount range bounds, U128/U256 explicit no-op, partially-known propagation through chain of 5+ AND/OR |
| `dead_branch` | dead branch with side-effect-free body | cascading dead branches, nested `If(true)` inside live branch, two-deep ControlPhi cleanup |
| `redundant_phis` | MemPhi single-pred, ControlState-with-both-outputs-unused | loop back-edge phi, unreachable subgraph cleanup, MemPhi with identical data inputs |
| `load_readonly` | (already covered) | oversize read returning None, cross-`VnSpace`, mismatched output type masking |
| `stack_store::detect` | (already rich) | multi-region SP-arithmetic chain through MemPhi, cycle in SP defs, mixed Add/Sub of negative consts, non-SP base |
| `stack_store::call_args` | (cdecl 2-arg, AArch64 0-offset, missing slot) | chain broken by un-decomposed Store, chain broken by Call/MemPhi, multiple call frames in one function |
| `load_forward` | (rich) | forward through MemPhi to ValuePhi when a predecessor is `StackStorePhi` of the same offsets, aliasing store breaks forward, load width > store width must not forward |
| `function_args` | (rich) | register-arg with truncated reads, stack arg shadowed by same-offset store, gap-truncation, multiple call frames |
| pipeline (black-box) | default pipeline reaches fixed-point | running the pipeline twice is idempotent, `run` always validates IR, large-graph pipeline run completes in expected time bound |

## Scaling work (in scope)

1. **Memoize `decompose_sp`** — per-pass `FxHashMap<NodeOutputId, Option<SpExpr>>` cache. Threaded through every call site in `StackStoreDetect`, `LoadForward`, and `function_args::stack_args`.
2. **Worklist-based fixed-point inside each pass** — replace the full-rescan with a `WorkSet`-driven loop. `ConstantFold`, `KnownBits` (Phase 1), `RedundantPhis`, `DeadBranchElimination`, `LoadReadOnly`, `StackStoreDetect`, `LoadForward`. (`CallStackArgCollect` and `FunctionArgDetect` are post-passes and naturally one-shot — left as-is.)
3. **`KnownBits` worklist** — Phase-1 inner fixed-point becomes worklist-driven: re-evaluate node only when one of its inputs' `Kb` changed.
4. **Memoize `function_args` shadow walk** — per-pass-call cache keyed on `(NodeOutputId, offset, size)` for the shadow-walk DFS through `MemPhi`.
5. **`FxHashMap` / `FxHashSet` in hot paths** — replace `std::collections::HashMap` / `HashSet` with `rustc-hash` types in `KnownBits` (known map), `RedundantPhis` (reachability + live values), `decompose_sp` (visiting set), and the new memo caches.
6. **Minimize per-call allocation** — where benches show it matters: pre-allocate one `Vec<NodeOutputId>` buffer per pass call instead of `node_inputs(...).into_iter().collect()`. Only applied where it shows up in benches.

### Benchmarks

`benches/` uses criterion. Each bench builds a synthetic graph at three scales (100, 1k, 10k nodes) and measures one pass running to fixed point on it:

- `constant_fold` — long chain of `(((x + 1) + 1) + 1) ...` with N constants, plus a "diamond" CFG variant.
- `known_bits` — chain of `(x | C1) & C2 | C3 & C4 ...` exercising the propagation worklist.
- `stack_store` — straight-line N pushes (cdecl-style), plus a diamond with per-branch SP adjustments.
- `default_pipeline` — combined: linear chain of 1k arithmetic + 1k loads + 1k stores; runs `default_pipeline` and `Analyzer::build_optimizer_pipeline()` end-to-end.

Bench reports go in commit messages (delta vs baseline). No CI gating yet; the bench suite is just a tool we use during development.

## Correctness review methodology

For each pass, in this order, before touching code:

1. Read the implementation against `node_signature::expected_signature` and `validate::validate` to confirm the pass leaves a structurally-valid graph.
2. Confirm reachability scoping: any pass that detaches inputs of zombie nodes must keep `Layer A` of the validator happy via the existing reachability-based skip in `walk::walk_graph`.
3. Look for: missing `detach_node_inputs` after `replace_all_uses`, off-by-one on input indices around `phi_token = inputs[0]`, unsoundness in commutative-fold ordering vs `IfCase` non-commutativity, and the documented constraints in existing comments.
4. Run the analyzer example and diff `cfg.html` / `graph.html` artifacts before vs after.
5. Any bug found: fix in the same PR with a regression test in the relevant `<pass>/tests.rs`. Anything ambiguous: surface to the user before fixing.

## Process

1. Worktree `feature/opt-review` already created and rooted in `feature/ai` tip.
2. Write the design doc (this file) and commit it to the worktree.
3. Write the implementation plan (`docs/superpowers/plans/2026-04-25-opt-crate-review-plan.md`) via `superpowers:writing-plans` and commit.
4. Execute the plan; per-pass tasks are independent enough to dispatch in parallel via `superpowers:subagent-driven-development` where it's a clear win (e.g. white-box test extraction for the stable passes).
5. After each batch:
   - `cargo test --workspace`
   - `cargo clippy -p opt --all-targets -- -D warnings`
   - `cargo run --example analyzer`
6. Run `superpowers:requesting-code-review` on the final state.
7. Merge `feature/opt-review` back to `feature/ai`. Method: fast-forward if possible, else single merge commit.

## Risks and mitigations

- **Behavior regression from worklist refactor.** The worklist must enqueue every consumer of every replaced output, not just direct ones. Mitigation: regression tests that compose passes (the existing `constant_fold/tests.rs::reassoc_*` tests already cover this; we'll add explicit "this fold cascades 5 levels" tests).
- **Memoization breaks cycle handling.** `decompose_sp` uses a `visiting` set to break cycles; the memo must not cache "currently visiting" results. Mitigation: only cache definitive `Some(_)` and `None` results returned from the top of the recursion, never partial state.
- **Splitting `function_args.rs` may shift `pub(crate)` boundaries.** Mitigation: keep all currently-private helpers at `pub(crate)` only as needed, default to `pub(super)` within the new module hierarchy.
- **Big diff makes review hard.** Mitigation: implement in small, well-scoped commits — one per pass for the file-split + tests + scaling change, plus one for the shared `sp_expr` extraction, plus one for `tests/common` and integration tests, plus one for benches. Code-review skill runs on the final state.

## Open questions

None — all questions answered in brainstorming. Implementation plan will resolve any further specifics.
