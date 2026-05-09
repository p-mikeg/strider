---
name: strider-orchestrator-extend
description: Extend the strider indirect-branch fixed-point orchestrator — add a new Decision variant, wire a new pipeline placement, or fix a cross-arch CFG-builder dispatch bug.
---

# strider-orchestrator-extend

## When to invoke

User wants to modify the top-level driver in `crates/strider/src/orchestrator.rs`, `LoopState`, `Decision`, the indirect-branch fixed-point, or the way pipelines are layered into the loop. Triggers include:

- "Add a new step to `strider::run`."
- "The orchestrator passes the wrong `ArchPreset` for `<arch>`."
- "Make the indirect-branch fixed-point recognise <new placeholder shape>."
- "Wire a new optimizer pipeline into `LoopState`."
- "Add a new `Decision` variant for <new convergence behaviour>."
- After `cargo test --package strider` shows an `UnknownCallOtherError` for a non-x86_64 arch on a previously-passing user-op (this is the canonical symptom of the arch-preset delivery bug — see round-8 cross-arch finding §1).

## When NOT to invoke

- Adding an opt pass with no orchestrator interaction → use `strider-opt-pass-author`.
- Adding a new shape to the indirect-branch resolver only (classifier or `inplace::*`) → use `strider-indirect-shape-author`.
- Adding a calling-convention preset → use `strider-cc-preset-extend`.

## Files this skill operates on

- `crates/strider/src/orchestrator.rs` — the canonical entry (`strider::run`), `LoopState`, `Decision`, `RegionIndex`, the `for_arch` CFG-construction call site (around line 837).
- `crates/strider/src/strider/pipeline.rs` — `Strider::build_optimizer_pipeline`, `build_stable_optimizer_pipeline`, `build_destructive_optimizer_pipeline`. Touch only when changing pipeline composition.
- `crates/strider/tests/common/mod.rs` (around line 220) — duplicate CFG-builder call site already migrated to `Builder::for_arch` post-round-8. Confirm any future builder-API change is mirrored here too.
- `crates/strider/benches/scaling.rs` (around line 93) — same duplication, also already on `for_arch`. Mirror future API changes here.
- `crates/cfg/src/cfg/builder/mod.rs` — only if extending the `Builder::for_arch` constructor surface.
- `crates/strider-py/src/strider_cls.rs` and `crates/strider-py/src/run.rs` — Python parity for any new public knob.

## Procedure

1. **Identify the CFG construction call site.** The orchestrator constructs CFGs at `crates/strider/src/orchestrator.rs:908`. The canonical post-round-8 form is:

   ```rust
   let cfg: Cfg<R> = Builder::for_arch(opts.strider.arch(), sleigh, opts.start_addr, cfg_opts)
       .with_known_targets(known_targets.clone())
       .with_decode_cache(decode_cache.clone())
       .build()?;
   ```

   Always use `Builder::for_arch(arch, sleigh, addr, opts)` — never `Builder::with_endianness(...)` directly. `with_endianness` hardcodes `preset: ArchPreset::X86_64` and is the source of the round-8 cross-arch CallOther-classification bug: ARM `swi`, AArch64 `CallHyperVisor` / `CallSecureMonitor`, MIPS `syscall` are dispatched against the X86_64 row of `target::call_other_abi::classify` and silently misclassify or raise `UnknownCallOtherError`.

2. **Sanity-check the two duplicate CFG-build sites.** `crates/strider/tests/common/mod.rs:220` and `crates/strider/benches/scaling.rs:93` build their own CFGs outside `strider::run`. Both are already on `Builder::for_arch` post-round-8. Any future change to the builder API must be mirrored at all three sites or tests / benches will diverge from production lift behaviour.

3. **Decide which `Decision` variant your change extends.** Today: `FixedPoint` (loop exit), `StableOnly` (in-place edits, no CFG rebuild), `Rebuild` (edge-set change forces re-lift). New variants must be exhaustively handled in `LoopState::step`, the dispatch in `orchestrator::run` (around lines 179-181), and any convergence accounting (e.g. the no-progress safety counter).

4. **Use `match` not `if let`.** The dispatch around lines 178-184 uses a `match` with all three arms; preserve that. `if let Decision::Rebuild = ...` would silently drop new variants if they are added later.

5. **Choose the right pipeline placement.** Three top-level pipelines live in `strider::Strider`:
   - `build_stable_optimizer_pipeline` — passes whose rewrites survive a later iteration that grows phi inputs (`ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`, plus `FunctionArgDetect` post-pass). Run **per iteration** during the indirect-branch loop.
   - `build_destructive_optimizer_pipeline` — node-removal passes (`RedundantPhis`, `DeadBranchElimination`, plus `CallStackArgCollect` post-pass). Run **once** at fixed-point exit. Running these mid-iteration invalidates the per-iteration `RegionIndex` `NodeId` pins and is a hard correctness bug.
   - `build_optimizer_pipeline` — full pipeline used outside the orchestrator (e.g. by the example).

6. **If extending `LoopState` state**, audit every method that consumes it: `step`, `run_stable_only`, `rebuild`, `finalize`. Any new field needs an init in the constructor and a reset rule on `Rebuild` if it is iteration-scoped.

7. **`RegionIndex` invariant.** `LoopState::region_index` is rebuilt on every `Rebuild` decision from the per-iteration `outcome.region_handles` snapshot. Do NOT cache `NodeId`s across iterations without re-mapping them through `RegionIndex` — the IR `Graph` is shared but new `VarPhi` / `MemPhi` node IDs are minted per iteration.

8. **Indirect-branch resolver hand-off.** After the stable pipeline runs, the loop calls `indirect_resolve::classify_anchor` on each placeholder. New placeholder shapes go through `strider-indirect-shape-author`; the orchestrator only routes the `inplace::*` rewrite call.

9. **Mirror new public knobs into `strider-py`.** Anything reachable from `strider::run`'s `RunOptions` needs a Python counterpart in `crates/strider-py/src/run.rs`. The Python `strider.run(...)` test surface fans out to non-x86_64 targets; missing parity hides cross-arch regressions.

10. **CallOther dispatch sanity check.** After the patch, a quick way to confirm `ArchPreset` propagation is correct on every arch is to lift one fixture per arch family that emits an arch-specific user-op (ARM `swi`, AArch64 `CallHyperVisor`, MIPS `syscall`, PPC `sc`) and confirm no `UnknownCallOtherError`. The full fixture matrix is built via `make -C fixtures`.

## Verification

- `cargo test --package strider --workspace` (focus on `test_orchestrator_*`, `test_run_*`, and the `per_arch_test!` expansions).
- Cross-arch sanity: `cargo test --package strider arm`, `cargo test --package strider aarch64`, `cargo test --package strider mips`, `cargo test --package strider ppc`. None should raise `UnknownCallOtherError` on a previously-classified user-op.
- `uv run pytest crates/strider-py/tests/python/test_smoke.py crates/strider-py/tests/python/test_arch.py`.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- All three orchestrator-equivalent CFG construction sites (`orchestrator.rs:908`, `tests/common/mod.rs:220`, `benches/scaling.rs:93`) use the same canonical constructor (`Builder::for_arch` post-round-8). Any change here propagates to all three.
- New `Decision` variants are exhaustively handled in `LoopState::step` and the top-level dispatch.
- No `if let Decision::...` shortcuts that would silently drop new variants.
- `RegionIndex` is rebuilt on every `Rebuild` (no stale `NodeId` references).
- Destructive passes run only at the fixed-point exit, never mid-iteration.
- `cargo clippy --workspace -- -D warnings` clean.
- Python parity exists for any new `RunOptions` knob.

## Pitfalls

- **`Builder::new` and `Builder::with_endianness` both hardcode `ArchPreset::X86_64`.** This is the round-8 cross-arch finding §1 bug. Any non-x86_64 caller that doesn't chain `.with_preset(arch.preset)` after `with_endianness` will silently misclassify CallOthers. The `for_arch` constructor was added precisely to remove this foot-gun — use it. Concretely, `crates/cfg/src/cfg/builder/mod.rs:113` sets `preset: target::ArchPreset::X86_64` unconditionally in `with_endianness`; only `for_arch` (lines 129-146) reads `arch.preset` and `arch.endianness` together.
- **Running destructive passes mid-iteration.** `RedundantPhis` and `DeadBranchElimination` remove `NodeId`s. The per-iteration `RegionIndex` pins those IDs as anchor handles for the indirect-branch resolver. Mid-iteration removal is silent corruption — the resolver later walks dangling `NodeId`s. Hard rule: destructive passes go in `build_destructive_optimizer_pipeline` and run exactly once, at the fixed-point exit.
- **Forgetting to re-snapshot `RegionIndex` after `Rebuild`.** A `Rebuild` mints a fresh CFG, fresh region handles, and fresh phi node IDs. The previous `RegionIndex` is stale.
- **Skipping the `tests/common` and `benches/scaling` mirror.** Round-8 already migrated these to `for_arch`, but any future builder-API change must propagate to all three sites or tests will silently diverge from production.
- **Adding a `Decision` variant without exhaustive `match`.** The current dispatch is `match` on three variants; an `if let` shortcut here would silently drop a new variant and the loop would loop forever or exit prematurely.

## Worked example: adding a new `Decision` variant

Suppose you want to add `Decision::PartialResolve` for the case where some placeholders resolved but the resolver wants the loop to retry the stable pipeline before deciding `FixedPoint` vs `Rebuild`. Concrete steps:

1. Extend the enum at `crates/strider/src/orchestrator.rs:188` (the `enum Decision { ... }` block).
2. Update the dispatch around lines 178-184 to add a `Decision::PartialResolve => state.run_stable_only()?` arm (or whatever the new semantics are). Keep the `match` exhaustive — clippy `-D warnings` will flag a missed arm only if you forget `#[deny(non_exhaustive_omitted_patterns)]`, so be explicit.
3. Update `LoopState::step` to *return* the new variant under whatever condition triggers it.
4. Update the no-progress safety counter so the new arm advances iteration count if it represents work.
5. Update `LoopState::run` if it has its own dispatch (it usually delegates to `step` + the top-level dispatch — verify).
6. Add a test in `crates/strider/tests/` that drives the orchestrator into the new arm.

## Background: the indirect-branch fixed-point

The loop in `strider::run` is the heart of the orchestrator. Each iteration:

1. Builds (or rebuilds) a CFG using the current `known_targets` map (resolved indirect-branch destinations from prior iterations).
2. Lifts every region to IR via `Strider::analyze_cfg`, producing `(graph, unresolved_branches, region_handles)`.
3. Runs the **stable** optimizer pipeline (passes whose rewrites survive a later iteration adding new phi inputs).
4. Calls `indirect_resolve::classify_anchor` on each placeholder. The classifier inspects the producer shape after stable rewrites.
5. Decides:
   - `FixedPoint` — every placeholder is classified and `known_targets` did not grow → exit.
   - `StableOnly` — placeholders were resolved by in-place edits (`apply_link_register` for returns, `apply_tail_call` for tail calls) without changing the CFG edge set → re-run the stable pipeline only.
   - `Rebuild` — the edge set changed (a new resolved target adds new CFG edges) → re-lift everything from a fresh CFG.
6. On exit, runs the **destructive** pipeline once.

A new `Decision` variant must answer: which of these three semantics does it belong to? If it advances state without changing edges, it is a `StableOnly` cousin. If it changes edges, it is a `Rebuild` cousin. If it requests an early exit, it is a `FixedPoint` cousin.

## Edge cases worth flagging

- **No-progress safety counter.** `LoopState` carries an iteration counter that aborts after a configurable maximum to prevent unbounded looping on a buggy resolver. New variants must not bypass this counter.
- **Region-split races.** A `Rebuild` may split an existing region (e.g. when a previously-unresolved indirect target lands inside an already-explored region). The Vn cache used for `analyze_cfg_with` is conservatively scoped — see the field doc on `LoopState::vn_cache_region_count`. Re-lifting on iteration N+1 may scan slightly more regions than strictly necessary; that's safe but slow if mis-shaped.
- **Per-address calling conventions.** `RunOptions::per_address_built_ccs` lets specific addresses use different CCs (e.g. a kernel hook). Any new `Decision` arm that re-lifts must thread this map through unchanged.

## Related skills

- `strider-indirect-shape-author` — when the orchestrator change is to recognise a new placeholder shape produced by `IndirectBranchResolve`.
- `strider-opt-pass-author` — when the change is just adding a pass, not a Decision/loop-control change.
- `strider-cc-preset-extend` — when the orchestrator extension is driven by a new CC preset's needs.
- `strider-py-binding` — for mirroring new `RunOptions` knobs into Python.
