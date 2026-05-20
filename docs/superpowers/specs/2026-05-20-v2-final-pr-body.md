# Pull Request Body — strider v2 rewrite

Use this when opening the PR `rewrite/strdier` → `feature/ai` via GitHub's web UI (https://github.com/p-mikeg/strider/pull/new/rewrite/strdier).

**Suggested title:**
> strider v2 rewrite: 5-crate consolidation + egraph optimizer + Salsa orchestrator + Python-first API

**Body:**

---

## Summary

Six-phase rewrite of strider (88 commits, ~5 days). Consolidates the 12-crate v1 layout into 5 main crates + 1 proc-macro crate, introduces an `egg`-based optimizer with phi-as-opaque-leaves slicing, adds a Salsa-driven orchestrator, exposes a high-level Python API, and makes the asm-fingerprint Layer-C validator unconditional.

**v1 contract preserved throughout:** all 2161 v1_baseline IR snapshots are byte-identical to the v1-final tag at every commit. `crates/strider/tests/v1_baseline.rs` is the load-bearing regression oracle.

## Phases

| Phase | Scope | Outcome |
|---|---|---|
| 0 | Pin v1 baseline (2161 snapshots, cross-arch shape, v1-final tag) | Done |
| 1 | `strider-ir` consolidation: absorbs ir+graphwalk+entity-utils+dot+graphmock; V6.A `ReadOnlyMemory` trait + V6.B `FunctionBuilderCC` plain struct; always-on Layer-C asm-fingerprint validator (G3); TestGraph helper; EGraphAdapter spike (V1 verification: egg works) | Done |
| 2 | `strider-lift` consolidation: absorbs pcode-lift + cfg (target re-promoted standalone for cycle-break); Lifter facade; RegionDriver | Done |
| 3 | `strider-analyze`: orchestrator + 9 v2 egg-based passes (ConstantFoldEgg, KnownBitsEgg, FlagCmpCanonicalizeEgg, IfCondInversionEgg, StackStoreDetectEgg, StackLoadForwardEgg, LoadReadOnlyEgg, CallStackArgCollectEgg, FunctionArgDetectEgg) all parity-tested alongside v1; PipelineV2 interleaved destructive+nondestructive fixed-point; Salsa-0.26.2 orchestrator (wrapper-mode) | Done |
| 4 | `strider-pattern-macros` proc-macro DSL; 10/14 PyPat types migrated (byte-identical .pyi vs hand-written reference); 4 hand-written exceptions documented | Done |
| 5 | High-level Python API: `strider.load(path).analyze(fn).find(pattern)`; ELF arch auto-detection for 11+ variants; `Analysis.fingerprint(node)` proof-of-correctness helper | Done |
| 6 | CLAUDE.md rewrite for v2; v1-vs-v2 benchmark with honest results; 3000-iteration proptest suite (0 failures) | Done |

## V2 layer order (V6 verified, no back-edges)

```
strider-binary → strider-ir → strider-lift → strider-analyze → strider
```

The v1 `cfg → opt` back-edge was resolved via the `IndirectTargetResolver` callback trait (G9). The `ir → target` back-edge was resolved by moving the thin `FunctionBuilderCC` plain-data struct into `strider-ir` (V6.B).

## Tests

- `cargo test -p strider --test v1_baseline` → PASS (~210s, 2161 snapshots).
- 45+ pass parity tests (`*_egg_parity.rs`) covering all 9 v2 passes.
- 5 `pipeline_v2_parity.rs` integration tests on real fixtures.
- 16 `test_high_level_api.py` Python tests.
- 3 proptest properties × 1000 iterations = 3000 random graphs, 0 failures.

## Honest reality checks

- **Performance:** v2 is currently 1.1–1.6× SLOWER than v1 (`docs/superpowers/specs/2026-05-20-v1-vs-v2-benchmark.md`). The Salsa orchestrator is wrapper-mode (Phase 3.9 scope); per-region granularity work is the documented follow-up. The plan's ≥10× projection was overstated.
- **LOC:** Phase 4 proc-macro added +977 LOC; pattern.rs only shrank by 92 LOC. Net +42% growth. The value-prop is per-new-pattern maintenance cost, not raw LOC reduction.

## Out of scope (deferred follow-ups)

1. **Phase 6.2 shim crate deletion** — the 9 shim crates (`ir`, `graphwalk`, `entity-utils`, `dot`, `graphmock`, `pcode-lift`, `cfg`, `opt`, `pattern`) still exist as one-line `pub use` re-exports. Safe to delete in a follow-up.
2. **Salsa per-region granularity** — split `optimized_function` into per-region tracked queries to unlock the cache-reuse benefit hidden by the current wrapper-mode out-of-band rebuild.
3. **`BuiltFunctionGraph: Clone`** via Arc-wrapped internals — would eliminate the v2 double-rebuild.
4. **The remaining 4 hand-written PyPat types** (FunctionArg enum-dispatch + 3 binary-op required-construction) — need macro extensions: `#[field(enum_dispatch)]` and required-field construction support.

## Phase 8.5: Production flip

PipelineV2 is now the production default for the test-helper convenience entry `common::analyze` (and therefore for every `per_arch_test!` and assertion-based system test that doesn't pin its pipeline explicitly). The two-baseline contract guards against drift:

- `crates/strider/tests/v1_baseline.rs` pins `common::analyze_v1` (the explicit v1 imperative path, `Strider::build_optimizer_pipeline` + `LoadReadOnly`). Snapshots = the historical v1 contract. **Frozen.**
- `crates/strider/tests/v2_baseline.rs` pins `common::analyze_v2` (the explicit v2 path, `opt::pipeline_v2::PipelineV2`). Snapshots = the pinned PipelineV2 contract. **Updates here track v2 IR evolution.**
- `common::analyze` is a thin call to `analyze_v2` — every default-routed test exercises PipelineV2.

The split was necessary because PipelineV2 applies more-aggressive canonicalisation than v1's imperative pipeline (e.g. eliminating `StackStore` nodes of zero-init constants on x86, the "egraph-aliasing-gap" family documented in `pipeline_v2_parity.rs`); a single baseline could only pin one shape.

### Bench numbers (post-flip)

`cargo bench -p strider --bench v1_vs_v2` measures the salsa-orchestrator path (`run_v2`) vs v1's direct `strider::run`, so it is orthogonal to the `common::analyze` flip; both numbers reflect production wiring as of `bbe7a006`:

| Workload | v1 | v2 (salsa) | Ratio |
|---|---|---|---|
| Single function (cold) | 84.4 ms | 89.2 ms | 1.06× slower |
| Multi-function | 685 ms | 705 ms | 1.03× slower |
| 10× repeat-query | 875 ms | 502 ms | **1.74× faster** |

The 1.74× win on repeat-query is the Salsa wrapper-mode cache reuse. The 1.03-1.06× slowdown on cold paths is the documented PipelineV2 overhead vs v1's imperative passes — Phase 3.9 per-region granularity work would close the gap.

### Commits

- `e3cf9eca` — `refactor(strider/tests/common): expose explicit analyze_v1 + analyze_v2 entry points (Phase 8.5a)`
- `1dfdf5b4` — `refactor(strider/tests/v1_baseline): pin to analyze_v1 explicitly (Phase 8.5b)`
- `af1929cf` — `perf(strider): flip production default to PipelineV2 (Phase 8.5c)`
- `bbe7a006` — `test(strider): add v2_baseline pinning the PipelineV2 IR shape (Phase 8.5d)`

### Verification matrix

- `cargo test -p strider --test v1_baseline` → PASS (~225s, all 2161 v1 snapshots byte-identical)
- `cargo test -p strider --test v2_baseline` → PASS (~223s, all 2161 v2 snapshots agree)
- `cargo test -p strider-analyze --no-fail-fast` → all suites green
- `cargo build --workspace` → clean

Future v2 IR changes show up as v2_baseline snapshot diffs; v1_baseline stays frozen as the historical v1 contract — any divergence there is a real regression in the v1 imperative pipeline, never a v2 issue.

## Tags

- `v1-final` — baseline (commit `5b0f7a8`, before this rewrite).
- `v2-final` — final state of this branch (commit `e670a621`).

## Test plan

- [ ] `cargo build --workspace` — clean.
- [ ] `cargo test -p strider --test v1_baseline` — 1 passed (v1 contract preserved).
- [ ] `cargo test -p strider --test v2_baseline` — 1 passed (v2 contract pinned).
- [ ] `cargo test -p strider-analyze --no-fail-fast` — all suites green.
- [ ] `cargo bench -p strider --bench v1_vs_v2` — v2 still wins on repeat_query (~1.74×).
- [ ] Manual: `cargo run -p strider --example strider` produces the cfg/graph/graph-opt HTML files as in v1.
- [ ] Python: `import strider; s = strider.load("fixtures/out/x64/arithmetic.elf"); a = s.analyze("add"); list(a.find(strider.pattern.call()))` — works end-to-end.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
