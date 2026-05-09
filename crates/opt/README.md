# `opt` — IR optimization passes

The optimizer for [`ir::BuiltFunctionGraph`](../ir). Passes are added to an
[`OptimizerPipeline`](#) and run in a shared fixed-point loop until no pass
reports a change. Three pre-built pipelines cover the common cases.

## Public surface

- `Optimizer` — most passes implement this trait. `run(&mut graph) ->
  Result<OptimizationResult>` returns whether the graph changed.
- `OptimizerOnBuilt` — variant that runs on a `BuiltFunctionGraph` (used for
  passes that need function-level metadata, e.g. `IfCondInversion` for
  control-flow surgery, and post-passes like `FunctionArgDetect`).
- `OptimizationResult` — `Changed { … }` | `Unchanged`.
- `OptimizerPipeline` — `add(pass)`, `add_post_pass(pass)`, `run(&mut
  graph)`. Calls `ir::validate::validate` at the end.
- `default_pipeline()` / `stable_default_pipeline()` /
  `destructive_default_pipeline()` — the three pre-built pipelines.
- Passes (each one is a unit-struct that implements `Optimizer` and lives in
  its own submodule):
  - `ConstantFold` — constant evaluation; algebraic identities (`x+0→x`,
    `x^x→0`, AND-mask merging, …).
  - `KnownBits` — bit-level propagation of statically known zeros / ones.
  - `FlagCmpCanonicalize` — flag-tree → single `IntCmpOp` rewrite (AArch64
    NZCV-style flag chains).
  - `IfCondInversion` — `If(BoolNeg(C)) {A}{B}` → `If(C){B}{A}`.
  - `RedundantPhis` — eliminates `VarPhi` / `MemPhi` / `ControlState` nodes
    with a single reachable predecessor.
  - `DeadBranchElimination` — strips `If` whose cond is `BoolConst`.
  - `LoadReadOnly` — folds constant-address loads via a caller-supplied
    `ReadOnlyMemory`.
  - `StackStoreDetect`, `StackLoadForward`, `CallStackArgCollect` — stack-frame
    analyses (in `stack_store/` and `stack_load_forward/`).
  - `FunctionArgDetect` — canonicalises arg-position reads at the function
    boundary into `FunctionArg` nodes.
  - `IndirectBranchResolve` (`indirect_branch_resolve/`) — producer-shape
    classifier for `BranchIndirect` placeholders. Exposes `classify_anchor`,
    `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`,
    `apply_link_register`, `apply_tail_call`, plus the result types
    `AnchorAddr`, `AnchorCallingContext`, `ResolvedTargets`,
    `find_placeholder_return_for_anchor`.
- `KnownBits`-flavour utilities: `Kb`, `analyze_known_bits`.
- Re-exports: `reader::ReadOnlyMemory` (so callers don't need a direct
  `reader` dep).

## Architecture

Each pass lives in its own submodule (`constant_fold/`, `known_bits/`,
`stack_store/`, …). Per-pass work units live in `tests.rs` next to the
implementation. Cross-cutting state lives at the crate root: `pipeline.rs`
(the fixed-point driver), `worklist.rs` (a thin wrapper over
`entity_utils::Worklist<NodeId>`), `sp_expr.rs` (stack-pointer expression
classification used by `StackStoreDetect` and the indirect resolver).

`pipeline::OptimizerPipeline::run` runs every `add(_)` pass in source order in
a shared fixed-point loop, then runs every `add_post_pass(_)` pass exactly
once after convergence. The whole sequence ends with `ir::validate::validate`,
so a malformed graph is reported as `opt::Error::IrError(ValidationFailed(_))`.

The strider tier-2 fixed-point splits passes into **stable** vs **destructive**
subsets. `stable_default_pipeline()` contains `ConstantFold`, `KnownBits`,
`FlagCmpCanonicalize`, `IfCondInversion` — all rewrite-only. The strider
orchestrator runs this pipeline mid-iteration while the IR is still growing.
`destructive_default_pipeline()` contains `RedundantPhis` and
`DeadBranchElimination` — these *remove* nodes and rewire consumers, so
running them mid-iteration would invalidate the orchestrator's per-iteration
`RegionIndex`. They run exactly once at the fixed-point exit.

`indirect_branch_resolve/` is the structural classifier for indirect-branch
placeholders. It does not modify the graph itself; instead it inspects the
producer shape behind a `BranchIndirect` anchor and returns a typed verdict
(link-register return, tail call, jump table, stack-array dispatch). The
strider orchestrator (`strider::indirect_resolve::inplace`) uses these
verdicts to rewrite the IR.

## Key invariants

- `OptimizerPipeline::run` calls `ir::validate::validate` at the end. A
  validation failure becomes an `opt::Error::IrError(_)` rather than a panic.
- Every pass must implement the **asm-fingerprint superset rule**: any
  replacement node's fingerprint includes (at minimum) the union of all
  contributors' fingerprints. Tests in `tests/asm_fingerprint_propagation.rs`
  pin this.
- The stable pipeline's passes never *remove* phi / `ControlState` / `If`
  nodes — they only rewrite operands. This is what makes them safe to run
  while strider's outer fixed-point is still adding new phi inputs.
- `LoadReadOnly` is stable per the spec but is not in `default_pipeline()`
  because it requires a caller-supplied ROM image. The strider crate's
  `Strider::build_optimizer_pipeline` layers it on top.

## Tests

Per-pass inline tests in `src/<pass>/tests.rs`. Cross-pass integration tests
in `crates/opt/tests/` (pipeline ordering, fixed-point convergence,
asm-fingerprint propagation, validator integration).

```
cargo test --package opt
cargo test --package opt <test_name>
cargo bench --package opt --bench default_pipeline
```

## Gotchas

- The fixed-point loop is **shared** across all `add(_)` passes: a
  simplification by one pass is immediately visible to the next pass in the
  same iteration. Passes don't need to converge individually.
- Order matters in `default_pipeline()`: `ConstantFold` must run before
  `IfCondInversion` so `BoolNeg(BoolNeg(_))` collapses first; `IfCondInversion`
  must run before `DeadBranchElimination` so the latter sees a canonical
  layout.
- Passes that detach nodes (`RedundantPhis`, `DeadBranchElimination`) leave
  zombie node ids in the arena — the validator's Layer A is reachability-scoped
  so this is fine, but graph walks must use `ir::walk::walk_graph` (not
  `Graph::iter_nodes`) to avoid touching the zombies.
- `KnownBits` is annotation-driven: the `Kb` map it produces is recomputed
  from scratch on every pipeline iteration. Don't cache results across runs.
- Depends on [`ir`](../ir), [`pattern`](../pattern), [`reader`](../reader),
  [`target`](../target), and `rsleigh`.
