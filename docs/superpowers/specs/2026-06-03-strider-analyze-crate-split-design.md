# Split `strider-analyze` into `strider-opt` + `strider-orchestrator`

## Problem

`strider-analyze` bundles two distinct concerns into one ~25k-LOC crate:
the **optimization passes** (`opt/`, ~22k LOC across 49 files) and the
**orchestration** that drives the lifter / CFG / indirect-branch
fixed-point loop and runs those passes (`orchestrator/`, `strider/`,
`indirect_resolver.rs`). A consumer that only wants the graph→graph
passes still pulls in the whole lift-driver, and the boundary between
"transform the graph" and "drive the analysis" is implicit.

The spike confirmed the split is structurally clean: `opt/` has **zero**
references to `strider`/`orchestrator`/`indirect_resolver` — the
dependency is strictly one-way (`orchestrator/strider → opt`).

## Design

Two crates replace `strider-analyze`:

- **`strider-opt`** — the `opt` module promoted to crate root.
  Optimization passes, `OptimizerPipeline`, `OptCtx`, `AliasMode`, the
  pattern-based peepholes, and the `indirect_branch_resolve`
  classifiers / in-place editors (the *optimization* logic). Deps:
  `strider-ir`, `strider-pattern`, `strider-target`, `strider-lift`,
  `rsleigh`, `entity-utils`, plus the small utility crates.
  - `indirect_branch_resolve` **stays here**: it borrows `pub(crate)`
    optimizer internals (`sp_expr`, `memory_ssa`, `KnownBitsMap`).
    Moving it out would force those — the SP-alias oracle and the
    memory-SSA walker with the narrowing soundness contract — to become
    public API. Keeping it in preserves the tight surface; the only
    `strider-lift` coupling is the cfg type aliases `ResolvedTargets` /
    `IndirectResolverFn` (already depended on today).

- **`strider-orchestrator`** — `orchestrator/` + `strider/` (the lift
  driver) + `indirect_resolver.rs` (the cfg-time resolver **stub**: the
  `resolve_indirect_target` closure installed via
  `Builder::with_indirect_resolver`). Depends on `strider-opt`. This is
  the orchestration glue, not optimization.

### Public surface / consumer migration

`strider-orchestrator` re-exports the optimizer as a module:

```rust
pub use strider_opt as opt;
```

plus the existing top-level items (`run`, `RunConfig`, `RunOptions`,
`LiftDriver`, `AnalyzeOptions`, `AnalyzeOutcome`, `dump_per_region`,
`dump_neighborhood`, `indirect_resolver`). So `strider-orchestrator`'s
surface is a superset of the old `strider-analyze`, and consumers only
rename the crate:

- `strider-py` (Cargo + 5 src files) and the 3 examples: rename
  `strider_analyze → strider_orchestrator`; existing `::opt::X` and
  `::run` paths keep working.

`strider-opt` is also available as a direct dependency for any future
consumer that wants passes without the lift driver.

### Dependency graph (no cycles)

```
strider-opt          → strider-ir, strider-lift, strider-pattern,
                        strider-target, rsleigh, entity-utils
strider-orchestrator → strider-opt, strider-ir, strider-lift,
                        strider-pattern, strider-target, rsleigh, dot
strider-py           → strider-orchestrator (+ the rest, unchanged)
```

`strider-lift` defines `IndirectResolverFn` (a `Box<dyn Fn>` type alias)
and `ResolvedTargets`; the *implementation* lives in
`strider-orchestrator::indirect_resolver` and is installed at runtime —
so there is no `strider-lift → orchestrator` build edge, and no cycle.

## Mechanics (behavior-preserving)

1. `git mv` `opt/` → `crates/strider-opt/src/` (`opt/mod.rs` → `lib.rs`).
2. `git mv` `orchestrator/`, `strider/`, `indirect_resolver.rs`,
   `tests/`, `examples/`, `benches/` → `crates/strider-orchestrator/`.
3. Path rewrites:
   - inside `strider-opt`: `crate::opt::X → crate::X`.
   - inside `strider-orchestrator`: `crate::opt::X → strider_opt::X`;
     `crate::strider`/`crate::indirect_resolver` stay `crate::…`.
   - in `strider-py` + examples + moved tests:
     `strider_analyze → strider_orchestrator`.
4. Two new `Cargo.toml`s; root `[workspace.dependencies]` swaps
   `strider-analyze` for `strider-opt` + `strider-orchestrator`
   (`members = ["crates/*"]` picks up the dirs automatically).
5. Delete `crates/strider-analyze/`.
6. Update doc-comment cross-references in sibling crates (cosmetic) and
   the crate inventory in `CLAUDE.md`.

## Verification (the refactor's safety net)

This is a pure move — **no behavior changes**. The existing test suite
is the spec:

- `cargo build --workspace` clean.
- `cargo test -p strider-opt` and `-p strider-orchestrator` — the same
  unit + integration tests that pass today, now under the new crates,
  all green (same counts: ~405 lib + the integration binaries).
- `cargo clippy --workspace --all-targets` clean.
- `cargo build -p strider-py` clean.

Done incrementally with the compiler + test suite as the gate at each
step; no step is committed until the workspace builds and tests pass.
