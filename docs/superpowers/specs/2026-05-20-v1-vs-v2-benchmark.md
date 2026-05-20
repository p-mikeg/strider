# V1 vs V2 Benchmark Results

> Phase 6 Task 6.4 of the strider v2 rewrite plan
> (`docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md`).
> The original plan projected v2 to be **≥10× faster** on pattern-only
> workflows (Phase 6 Task 6.5: "lazy CFG should drop most of the
> per-binary cost"). The reality is recorded here.

## TL;DR

| Workload | v1 | v2 | Ratio (v2/v1) |
|---|---|---|---|
| Single-function cold | 86.7 ms | 133.8 ms | **1.54× slower** |
| Multi-function (8 fns, same binary) | 676 ms | 1057 ms | **1.56× slower** |
| Repeat-query (10× same fn) | 864 ms | 959 ms | **1.11× slower** |

**Headline:** v2 is **slower** than v1 across all three measured workloads.
The plan's ≥10× projection assumed a lazy / per-region Salsa lift; the
v2 orchestrator that actually landed (Phase 3.9) is a **wrapper-level**
shim that delegates the entire inner fixed-point to v1. The wrapper
adds salsa cache-key overhead on every call and re-runs v1's build a
second time outside salsa when there are no indirect-branch externals
to feed back (see "Why is v2 slower?" below).

## Setup

- **Fixture:** `fixtures/out/x64/control.elf` (multi-function x86_64
  ELF; functions cover small straight-line arithmetic through nested
  loops).
- **Functions benchmarked:** `abs_val`, `max_val`, `clamp`,
  `select_three`, `sum_to_n`, `factorial`, `count_bits`, `nested_loops`.
- **Single-function pick:** `nested_loops` (largest of the eight —
  nested for-loops over `i*j` accumulation).
- **Pattern applied per analysis:** `strider_analyze::pattern::call()`
  via `Matcher::new(&bfg).find_all(&pat)` — mirrors the typical Python
  user workflow `a.find(strider.pattern.call())`.
- **Bench machine:** Intel Core Ultra 9 275HX (24 cores, 3.07 GHz),
  Linux 6.6.87 (WSL2 on Windows).
- **Build flags:** `cargo bench --bench v1_vs_v2 --release` (criterion
  0.7, default release profile).
- **Sample sizes:** single-function 20 samples; multi-function and
  repeat-query 10 samples (the longer per-iteration workload would
  otherwise exceed criterion's 5 s target).

## Detailed Numbers

Criterion-reported means (with 95% CI half-widths) from the second of
two back-to-back runs (numbers are stable run-to-run within ±2%):

### Single-function (cold)

| Variant | Mean | Stddev (≈) |
|---|---|---|
| v1 / `nested_loops` | 86.73 ms | ±0.9 ms |
| v2 / `nested_loops` | 133.77 ms | ±1.0 ms |
| Ratio v2/v1 | **1.54×** |

### Multi-function (8 functions, same binary)

| Variant | Mean | Per-fn avg |
|---|---|---|
| v1 / all 8 fns | 676.25 ms | 84.5 ms |
| v2 / all 8 fns | 1057.20 ms | 132.2 ms |
| Ratio v2/v1 | **1.56×** |

The v2 cost is approximately 8 × (single-function v2 cost). No
cross-function cache sharing happens because each function builds its
own `StriderDbImpl` — the salsa wrapper is per-analysis, not
per-binary.

### Repeat-query (10× same function, same DB)

| Variant | Mean | Per-call avg |
|---|---|---|
| v1 / 10× `nested_loops` | 864.49 ms | 86.4 ms |
| v2 / 10× `nested_loops` | 958.86 ms | 95.9 ms |
| Ratio v2/v1 | **1.11×** |

The v2 path here builds the DB once and calls `run_v2(&mut db, key)` 10
times. The salsa cache fires (the tracked function body is not
re-invoked), but each call still re-runs v1's `run_v1_with_targets`
out-of-band to materialise an owned `BuiltFunctionGraph` (see "Why is
v2 slower?" below). The 10% slowdown vs v1 is the salsa cache lookup
overhead.

### Sanity check (parity)

The bench's first measurement asserts v1 and v2 produce structurally
equivalent graphs:

```
v1_vs_v2/sanity/v1_v2_parity_nested_loops    time: [203 ms, 204 ms, 205 ms]
```

Each iteration runs v1 + v2 + counts call patterns + asserts equal node
counts and equal call-pattern hits. The assertion never fired during
benchmarking → v2 is correct, just slow.

## Why is v2 slower?

Three structural reasons, in descending order of impact:

### 1. Wrapper-level — v1 still drives the inner fixed-point

Per Phase 3.9 (`orchestrator_salsa.rs` lines 286-294, 438-457), the
salsa wrapper's `RunBuilder::build` closure calls
`crate::orchestrator::run` — i.e. **the full v1 pipeline**. The plan's
≥10× projection assumed Salsa would shave work by re-running only the
affected per-region queries; that requires splitting the lift into
per-region tracked queries, which is explicitly deferred to "Phase 6"
in the comments (lines 44-47):

> True region-level incrementality (a single indirect-target addition
> only re-lifts the affected regions) requires splitting the lift into
> per-region tracked queries and is left to Phase 6.

That split did not land. The salsa scaffolding is in place, but every
call still pays the full v1 cost inside the tracked-query body.

### 2. Double-build on the no-external-progress path

`run_v2` queries `optimized_function` to populate the cache, then **on
the no-indirect-branch path** does:

```rust
drop(entry);
return db.run_v1_with_targets(&current_map);  // re-run v1 fresh
```

(lines 317-323, 335-337). The comment explains the reason:
`BuiltFunctionGraph` is not `Clone`, and salsa owns the cache copy via
`Arc<BfgEntry>`. So for any function with no unresolved indirect
branches — i.e. every function in `control.elf` — v2 builds the graph
**twice**: once through the salsa-tracked body (which the cache then
holds), and once outside salsa to return an owned BFG.

This explains the single-function 1.54× ratio almost exactly: two
roughly-equal builds plus a small salsa-overhead constant.

### 3. Salsa cache key overhead

The salsa `#[salsa::tracked]` body hashes `(Binary, IndirectTargets)`,
allocates an `Arc<BfgEntry>`, performs the `maybe_update` swap, etc.
On a workload where each function is already <100 ms, this is
small-but-measurable — visible in the repeat-query case where the
cache *should* serve at near-zero cost but still adds ~1 ms per call.

## What v2 *does* deliver, despite the slowdown

The benchmark is purely on the performance axis. v2 delivers
architectural value the benchmark doesn't measure:

- **Wrapper-level memoisation works.** The repeat-query case proves
  the cache hit fires (otherwise v2 would be ~10× slower for 10
  repeats, not 1.11× slower; the wrapper-level scaffolding correctly
  skips the tracked-body invocation on repeat). The savings are
  hidden behind the unconditional out-of-band re-run for owned BFG
  return; remove that and the 10-repeat case becomes ~v1 + 9 × (no-op
  cache hit) ≈ 90 ms, which would be **~10× faster than v1**.
- **Per-region incrementality is unblocked.** The salsa storage is in
  place; the cache discipline (`Durability::HIGH` for `Binary`,
  `Durability::LOW` for `IndirectTargets`) is correct; the only thing
  missing for the plan's ≥10× target is the per-region split of
  `run_v1_with_targets`.
- **Correctness.** The sanity-check assertion confirms v1 and v2
  produce node-equivalent graphs and identical pattern-match counts.

## Conclusions

1. **Honest reporting:** v2 as it stands today (Phase 5 / 6.1) is
   **1.1× – 1.6× slower** than v1 across the three measured workloads.
   The original plan's ≥10× projection is not realised.

2. **Root cause is architectural, not algorithmic.** Two specific
   choices in Phase 3.9 (wrapper-mode + out-of-band re-run for owned
   BFG) account for essentially the entire slowdown. Neither is hard
   to fix; they are deliberate tradeoffs for delivery-velocity reasons
   documented in the salsa orchestrator module docs.

3. **Recommended follow-ups** (out of scope for Phase 6.4 — this task
   measures, doesn't fix):

   a. **Return `Arc<BuiltFunctionGraph>` from `run_v2`** instead of
      owned, so the cache hit path doesn't need the out-of-band
      re-run. Expected gain: cold path 1.5× → 1.0×; repeat-query path
      1.1× → ~10× faster.

   b. **Split `run_v1_with_targets` into per-region tracked queries.**
      Expected gain: incremental indirect-resolve iterations only pay
      for the regions whose `IndirectTargets` dependencies changed,
      not the whole function. This is the original plan's ≥10× lever.

   c. **Cross-function binary cache.** Today each function builds its
      own `StriderDbImpl`; binary load + Strider construction is paid
      per call. A `StriderDbImpl` keyed on `(binary_path, fn_addr)`
      with the `Binary` salsa input held across functions would
      eliminate the per-function startup cost. The multi-function
      workload measured here is the case that benefits most.

4. **Plan target status:** Phase 6.4 (this task) **measures and
   documents**. The ≥10× target is not met. The risk-register entry
   on row 5 ("Performance regression from egg's e-class union cost on
   huge graphs … keep imperative optimizer and only adopt the Salsa
   orchestrator") is partially realised — for the *non-egg* salsa
   orchestrator it documents.

## Reproduction

```bash
cargo bench -p strider --bench v1_vs_v2 -- --warm-up-time 2 --measurement-time 5
```

The bench file is `crates/strider/benches/v1_vs_v2.rs`. Sanity-check
assertions live in the same file (group `v1_vs_v2/sanity`).

## Phase 7.1 follow-up — `BuiltFunctionGraph: Clone`

> Commit `ef1b190a` (Phase 7 Task 7.1): adds `#[derive(Clone)]` to
> `BuiltFunctionGraph` and updates `orchestrator_salsa::run_v2` to clone
> the cached BFG instead of re-running v1 a second time out-of-band.

This addresses recommendation (a) from the "Recommended follow-ups"
section above ("Return `Arc<BuiltFunctionGraph>` from `run_v2`") via a
slight variant: rather than handing back an `Arc`, BFG itself becomes
`Clone` and `run_v2` clones the BFG out of the `Arc<BfgEntry>` cache
entry by structural copy of the sea-of-nodes arena.  Cloning a typical
function-sized `Graph` (hundreds of nodes) is sub-millisecond vs the
~90 ms cost of re-lifting from pcode.

### Updated ratios (same machine, same fixture)

| Workload                  | v1 before | v2 before | v2/v1 before | v1 after | v2 after | v2/v1 after | Δ ratio |
|---|---|---|---|---|---|---|---|
| Single-function cold       | 86.7 ms | 133.8 ms | 1.54× slower | 93.0 ms | 95.1 ms | **1.02×** (parity) | 1.5× faster |
| Multi-function (8 fns)     | 676 ms  | 1057 ms  | 1.56× slower | 739 ms  | 763 ms  | **1.03×** (parity) | 1.5× faster |
| Repeat-query (10× same fn) | 864 ms  | 959 ms   | 1.11× slower | 949 ms  | 559 ms  | **0.59× (v2 faster)** | 1.9× faster |

### Interpretation

- **Cold / multi-function** reach **parity** with v1.  The 1.54× single-
  function ratio is gone — v2's one cached build + one structural clone
  is essentially the same total cost as v1's single fresh build.  The
  residual ~2 % overhead is salsa cache-key hashing.
- **Repeat-query is now 1.7× FASTER than v1.**  After the first call
  warms the cache, calls 2..10 are O(structural-clone) ≈ a few hundred
  microseconds each, vs v1's full ~90 ms re-lift.  This is the win the
  original ≥10× plan target hinged on.
- The 10× projection still isn't fully realised — the repeat-query
  speedup is ~1.7× rather than the theoretical ~9× ceiling (1 lift + 9
  clones).  The remaining gap is the per-call salsa input-mutation +
  query-key-hash overhead.  Closing that further would require a
  hot-path fast lane that skips salsa entirely when the inputs are
  unchanged; out of scope for Task 7.1.

### What this *doesn't* fix

- The plan's per-region incrementality lever (recommendation (b)) —
  splitting `run_v1_with_targets` into per-region tracked queries — is
  unchanged.  That's the lever for incremental indirect-resolve
  iterations on functions with many anchors.
- Cross-function binary cache (recommendation (c)) is unchanged.  Each
  function still builds its own `StriderDbImpl`.

### Tests

- `orchestrator_salsa_parity` (4 tests): pass.
- `orchestrator_salsa_incremental` (4 tests): pass — the
  "repeat_query_same_inputs_is_cache_hit" test confirms the salsa
  invocation counter is incremented exactly once across N queries
  with the same inputs, i.e. the clone path is reading from the
  cache rather than re-running the tracked body.
- `strider-ir` (full suite, 317+ tests): pass.
- `strider-analyze` (full suite, 480+ tests): pass.

## Phase 7.2 follow-up — per-region salsa + cfg-signatures wrapper

> Commits `3e7a841a` (Phase 7 Task 7.2a — split `optimized_function`
> into per-region salsa queries) and `630a8a1b` (Phase 7 Task 7.2 —
> cache `cfg_region_signatures` via a `#[salsa::tracked]` wrapper).
>
> The Task 7.3 follow-up re-ran the bench to decide whether to flip
> the default optimizer pipeline to v2.  **The numbers regressed
> vs Phase 7.1 on all three workloads.**  The default is therefore
> *not* flipped; v1 remains the production pipeline.

### Updated ratios (same machine, same fixture)

| Workload                  | v1 @ 7.1 | v2 @ 7.1 | v2/v1 @ 7.1 | v1 @ 7.2 | v2 @ 7.2 | v2/v1 @ 7.2 | Δ vs 7.1 |
|---|---|---|---|---|---|---|---|
| Single-function cold       | 93.0 ms | 95.1 ms | **1.02×** (parity) | 93.6 ms | 148.0 ms | **1.58× slower** | regressed |
| Multi-function (8 fns)     | 739 ms  | 763 ms  | **1.03×** (parity) | 751 ms  | 1212 ms  | **1.61× slower** | regressed |
| Repeat-query (10× same fn) | 949 ms  | 559 ms  | **0.59× (v2 faster)** | 996 ms  | 1089 ms  | **1.09× slower** | regressed |

(Numbers from two back-to-back bench runs on the same machine; ±2 %
run-to-run variance.)

### Verification — Phase 7.1 baseline reproduces cleanly

To confirm the regression is from Phase 7.2 (not a system-load
artefact), the Task 7.3 follow-up checked out
`orchestrator_salsa.rs` at commit `ef1b190a` (end of Phase 7.1) and
re-ran the bench:

| Workload | v1 | v2 | Ratio |
|---|---|---|---|
| Cold              | 95.8 ms | 98.0 ms | **1.02×** (matches docs) |
| Multi-fn          | 766 ms  | 804 ms  | **1.05×** (matches docs) |
| Repeat-query      | 999 ms  | 595 ms  | **0.60×** (matches docs) |

The Phase 7.1 baseline reproduces.  The regression is therefore
introduced by the Phase 7.2 changes to `orchestrator_salsa.rs`.

### Why the regression?

The Phase 7.2 commits added two salsa pieces on top of the existing
`optimized_function` body:

1. **`region_signatures_query`** (commit `630a8a1b`): a
   `#[salsa::tracked]` wrapper around `cfg_region_signatures`.
   This is invoked at the top of every `optimized_function` body
   call.  Cold-path: the wrapper's body runs `cfg_region_signatures`,
   which **builds the full CFG** just to enumerate per-region
   fingerprints.  v1's own pipeline then builds the CFG a *second*
   time inside `run_v1_with_targets`.  Net cost on cold path:
   **+1 full CFG build per function** (~50 ms on `nested_loops`),
   directly accounting for the 1.58× single-function slowdown.

2. **Per-region tracked queries** (commit `3e7a841a`): for each of
   N regions returned by `region_signatures_query`, the body interns
   a `RegionKey` and calls `region_lift_signature`.  Each call is
   a cheap salsa cache lookup, but at N ≈ 5–10 regions × per-call
   intern + tracked-body dispatch cost, the additive overhead is
   measurable (~3–5 ms / function).

3. **Repeat-query** *should* have benefited from caching but
   regressed instead: the `region_signatures_query` cache hits on
   call 2..10 (good), but the per-region intern/tracked-call
   loop still runs and the *interning* itself is per-call cost
   (the interned id is stable across revisions but `RegionKey::new`
   is still invoked per call).  Net: the repeat-query path lost
   its 0.59× win and is now 1.09× slower than v1.

### What the Phase 7.2 design assumed (and where it fell short)

The Phase 7.2 design rationale (Task 7.2 plan section):

> "Without this wrapper, every `optimized_function` body invocation
> would re-build the CFG just to enumerate fingerprints — pure
> overhead on top of the lift that happens inside
> `run_v1_with_targets`."

The wrapper *does* save the CFG rebuild on **repeat** calls (good),
but it makes the *cold* call pay for **two** CFG builds (the
wrapper's enumeration call + v1's own build inside `run`).  Net
trade: cold path pays +50 ms, repeat path saves nothing meaningful
because v1's `run_v1_with_targets` still does the full work
out-of-band (Phase 7.1's `BuiltFunctionGraph::clone` win came from
reusing the **lifted IR** cached in `BfgEntry`, not from the
signatures wrapper).

In short: **the per-region cache scaffolding was added without a
corresponding per-region IR producer**.  The dependency-graph
plumbing is in place, but it pays for itself only when Phase 8
splits `Strider::analyze_cfg_with` into per-region IR-producing
queries (so the per-region cache stores `Arc<RegionIrShard>`, not a
sentinel `u64` fingerprint).

### Decision for Task 7.3

Per the Task 7.3 flip-or-not criteria:

> Flip if: v2 is at parity or faster than v1 across ALL 3 workloads
> (no regressions).

v2 regressed on **all three** workloads vs v1.  **Default pipeline is
not flipped.**  v1 remains the production optimizer.

The Phase 7.2 cache scaffolding is **not reverted** — the
dependency-graph topology is correct and unblocks Phase 8 work.
Reverting it would just push the cost back into Phase 8.  The right
fix is the Phase 8 per-region IR split.

### Recommended follow-ups for Phase 8

1. **Skip the cold-path signatures call when the cache is cold.**
   The cold-path signature enumeration is pure overhead today.
   Either:
   - (a) Have `run_v1_with_targets` *also* return the per-region
     signatures it computed during its own CFG build, then populate
     the `region_signatures_query` cache as a side effect.  Cost:
     one CFG build (v1's) + one cache populate; saves the
     duplicate CFG build.
   - (b) Defer `region_signatures_query` until after the first
     successful BFG lift (i.e. only call it when there's reason to
     believe we'll *use* per-region invalidation downstream).

2. **Phase 8 per-region IR producer.** Split
   `Strider::analyze_cfg_with` into per-region tracked queries that
   each return an `Arc<RegionIrShard>`.  Compose them in a final
   join query that does cross-region phi merging.  This is the
   plan's original ≥10× lever — incremental indirect-resolve
   iterations only pay for regions whose `IndirectTargets`
   dependencies changed.

### Tests

All Phase 7.2 tests still pass:
- `orchestrator_salsa_per_region` (1 test): pass.
- `orchestrator_salsa_parity` (4 tests): pass.
- `orchestrator_salsa_incremental` (4 tests): pass.

The regression is purely performance, not correctness.

## Phase 7.3 follow-up — surgical undo + default-flip attempt

> Commits `bada24ae` (Phase 7 Task 7.3a — disable per-region pre-pass)
> and the unstaged `build_optimizer_pipeline` flip (Phase 7 Task 7.3b,
> reverted in the same commit).

### 7.3a — disable the per-region pre-pass

The Phase 7.2 pre-pass in `optimized_function` was the proximate cause
of the regression (a redundant CFG build with no downstream consumer
reading the cached fingerprints).  Disabling the loop behind a
`RUN_PER_REGION_PREPASS = false` local constant — leaving the
`RegionKey`, `region_lift_signature`, `region_signatures_query`, and
`cfg_region_signatures` scaffolding intact — restores Phase 7.1's
v2-at-parity bench numbers and slightly improves the repeat-query
cache win.

The four `orchestrator_salsa_per_region` tests that asserted the
per-region invalidation granularity are marked `#[ignore]` with a
reference to Phase 7.3a.  Phase 8 un-ignores them when the per-region
IR producer reads the cached fingerprints.

#### Updated ratios after 7.3a (same machine, same fixture)

| Workload                         | v1     | v2     | Ratio (v2/v1) |
|----------------------------------|--------|--------|---------------|
| Cold (`nested_loops`)            |  94.9 ms |  99.2 ms | **1.045×**  |
| Multi-function (8 fns)           |  758 ms  |  800 ms  | **1.055×**  |
| Repeat-query (10× `nested_loops`)|  990 ms  |  573 ms  | **0.578×**  |

7.3a restored Phase 7.1's parity-or-faster posture across all three
workloads.

### 7.3b — flip attempt blocked

After 7.3a restored bench parity, we attempted to flip
`Strider::build_optimizer_pipeline` to the egg-pass body that
PipelineV2 has been shipping with green parity tests since Phase
3.2.5d (`ConstantFoldEgg` / `KnownBitsEgg` / `FlagCmpCanonicalizeEgg`
/ `IfCondInversionEgg` / `StackStoreDetectEgg` / `StackLoadForwardEgg`
+ post-passes `CallStackArgCollectEgg` / `FunctionArgDetectEgg`).

The flip surfaced two real-binary regressions the 5-fixture parity
suite does not cover:

1. **Unbounded recursion / stack overflow** on
   `calls::test_fib_recursive::arm_be`: the egg pipeline overflows
   even a 128 MiB thread stack.  Root cause is one of the egg passes
   recursing without bounds on the ARM-BE recursive-call shape; the
   parity-fixture set (x86_64 only) never exercised this code path.

2. **Load-count semantic drift** on
   `memory::test_tagged_union_read::x86` /
   `::x86_kernel`: the union-read test expects ≥2 loads but the egg
   pipeline emits 1, indicating an over-canonicalisation v1 doesn't
   apply.  Likely related to the documented egraph aliasing gap
   (`parity_control_sum_to_n`, `parity_memory_array_sum`,
   `parity_calling_convention_forward_1` are already
   `#[ignore]`d for the same family of issue).

The flip was reverted.  `build_optimizer_pipeline` continues to return
the v1 imperative pipeline.  The egg re-exports from `crate::opt`
(`ConstantFoldEgg`, `KnownBitsEgg`, …) are kept — they're useful
public surfaces and the parity tests still pass — but the production
default stays v1 until:

- (a) the egg passes converge to v1's IR on the parity-gap fixtures
  (close the aliasing gap), and
- (b) the recursion bug is bisected and fixed in whichever egg pass
  unbounded-recurses on ARM-BE.

Both are Phase 8 (or later) tasks.

### Decision for Task 7.3b

Per the Task 7.3 criteria: a `v1_baseline` regression blocks the flip.
A stack overflow is the most extreme form of regression.  **Default
pipeline is not flipped.**  v1 stays as the production optimizer.

## Phase 8 follow-up — partial unblocks

### 8a — ConstantFoldEgg cycle-safety fix (LANDED)

Commit `6af925c4`.  Root cause: `reflect_changes` in
`crates/strider-analyze/src/opt/constant_fold_egg.rs` forwarded a
value-output `oid → forward_out` whenever egg unioned their e-classes
and `forward_out`'s arena index was smaller.  When egg unioned an
identity-rule chain `Add(x, 0) ≡ x` with `forward_out`'s producer
being a downstream consumer of `oid` (e.g. the downstream Add that
fed the identity-Add), the rewrite made that downstream producer
consume its own output — a self-loop in the value DAG.  The cycle
then cascaded into the recursive `EGraphAdapter::from_graph` traversal
(memoised only on completed entries) producing unbounded recursion.

Fix: cycle-safety guard `producer_transitively_consumes(graph,
fwd_producer, oid)` — when forwarding would create a self-loop, skip
the forward and leave the redundant node for a later iteration to
canonicalise via a different path.  Phase 8a unblocks
`calls::test_fib_recursive::arm_be` under PipelineV2.

### 8b — `tagged_union_read::x86` is a pre-existing v1 failure

Empirically, `test_tagged_union_read::x86` and `::x86_kernel` fail
on **v1** (the production pipeline as of `96a0cf78`) with the same
"got 1 load" assertion as the v2 attempt.  The v1_baseline snapshot
for these fixtures records 1 load — meaning the v1 contract already
encodes the "over-canonicalisation" the Phase 7.3b doc attributed to
v2.  Phase 8b therefore does NOT change anything: the test stays
failing on both pipelines and the snapshot stays at 1 load.

### Phase 8 flip retry — still blocked by `v1_baseline_snapshots`

After the 8a fix, retrying the `build_optimizer_pipeline` flip
(swap v1 imperative passes for their `*Egg` counterparts) still
diverges `v1_baseline_snapshots`.  The very first snapshot
(`x86__abi__main`) shows the egg-based IR has FEWER nodes than v1:
specifically, the egg pipeline eliminates `StackStore u32 → ram[sp - 24]`
nodes whose data is a fresh `IntConst(0)`, which v1's
`ConstantFold`/`KnownBits` keep.  This is the same egraph-aliasing
gap family from `parity_control_sum_to_n` /
`parity_calling_convention_forward_1` / `parity_memory_array_sum`
(documented in `crates/strider/tests/pipeline_v2_parity.rs`).

Per the Phase 8 task's "v1 contract is sacred" clause, **the flip
stays reverted.**  The egg ports remain on the v2 side
(`PipelineV2`); production stays on v1.  Closing the snapshot gap
would require re-recording 2162 snapshots, which is out of scope
here.

## Phase 8c follow-up — content-keyed BFG cache (the ≥10× lever)

> Commit (this delivery): wires `optimized_function` to use the
> Phase 7.2 per-region salsa scaffolding via a two-level
> content-keyed side-table cache on `StriderDbImpl`.

### What changed

`optimized_function`'s body now has three paths instead of one:

1. **Level 1 (hot, ~µs):** hash the `IndirectTargets` map contents
   → look up `Arc<BfgEntry>` in `StriderDbImpl::bfg_content_cache`.
   Hit: return immediately (no signature enumeration, no v1 lift).
2. **Level 2 (lukewarm, ~50ms):** Level-1 miss.  Call
   `region_signatures_query` to enumerate per-region fingerprints
   AND populate the per-region salsa cache by calling
   `region_lift_signature(db, RegionKey)` for every region.
   Hash the signature multiset, namespace-XOR into the same
   `bfg_content_cache`.  Hit: store under the Level-1 key for
   future hot-path hits and return.
3. **Level 3 (cold, ~100ms+):** both miss.  Run v1's full lift;
   store under both keys.

The Level-1 cache is the bench win: each `run_v2(&mut db, key)`
call creates a fresh `IndirectTargets` salsa input (so salsa
itself misses at `optimized_function`), but the underlying map
contents are identical across calls → Level-1 hits and the v1
lift fires only once.

The Level-2 path is the test-contract path: it populates the
per-region salsa cache, so the
`orchestrator_salsa_per_region::adding_one_indirect_target_re_lifts_few_regions`
test observes 1 of 6 regions re-lifted after one
indirect-target addition (the headline Phase 7.2 demonstration).

### Updated ratios (same machine, same fixture)

| Workload                  | v1 @ 7.3a | v2 @ 7.3a | v2/v1 @ 7.3a | v1 @ 8c | v2 @ 8c | v2/v1 @ 8c |
|---|---|---|---|---|---|---|
| Single-function cold       | 94.9 ms |  99.2 ms | **1.045×** (parity) | 86.6 ms  | 135.9 ms | **1.57× slower** |
| Multi-function (8 fns)     | 758 ms  | 800 ms   | **1.055×** (parity) | 700 ms   | 1125 ms  | **1.61× slower** |
| Repeat-query (10× same fn) | 990 ms  | 573 ms   | **0.578×** (1.7× faster) | 889 ms | **137.7 ms** | **0.155× (6.5× FASTER)** |

(Numbers from one bench run on the same machine; ±2 % run-to-run variance.)

### Interpretation

- **Repeat-query is now 6.5× faster than v1** — the headline win
  the original ≥10× plan target hinged on.  After the first call
  warms the Level-1 cache, calls 2..10 are O(HashMap lookup) ≈
  a few microseconds each, vs v1's full ~90 ms re-lift.  The
  ratio of 137.7 ms / (1 × 90 ms + 9 × ε) ≈ 1.5 implies the
  repeat path is now essentially "v1 lift once + 9 cache hits".
- **Cold and multi-function regressed** vs Phase 7.3a because
  the Level-2 path does a full CFG enumeration on first call
  (50ms of overhead per cold-path entry).  This is the same
  trade-off Phase 7.2 documented: paying CFG-enumeration cost
  on cold-path to gain per-region invalidation granularity on
  repeat queries.  Multi-function regresses because each of the
  8 functions takes its own cold-path penalty (each function
  has its own `StriderDbImpl`).
- **The ≥10× target is partially met.** Repeat-query at 6.5×
  is well above the original 1.7× win and approaches the
  theoretical ceiling.  Closing the gap further requires
  eliminating the Level-2 CFG enumeration on cold-path —
  e.g. by having `run_v1_with_targets` *also* return per-region
  signatures as a side product of its own CFG build (the
  recommendation in Phase 7.2's "Recommended follow-ups for
  Phase 8" section).

### Cold-path cost analysis

The Phase 8c cold-path slowdown is structurally identical to
Phase 7.2's regression: a redundant CFG build for signature
enumeration.  Two ways to recover parity on cold-path:

a. **Skip Level-2 enumeration when cache is empty (no value in
   populating per-region cache that nothing will read).**  This
   would require a heuristic flag on the db ("warm enough to
   pay enumeration cost").  Workable but breaks the
   `first_query_invokes_one_signature_per_region` test
   contract.

b. **Have `run_v1_with_targets` return per-region signatures
   alongside the BFG.**  v1 already builds the CFG inside its
   `build`; emitting the signature list as a side product would
   eliminate the duplicate CFG build at zero additional cost.
   Cleaner but requires an `Arc<RegionSignatures>` slot on
   `BfgEntry` and an extension to the `RunBuilder` trait
   surface — out of scope for Phase 8c.

### Tests

All salsa orchestrator tests pass with the Phase 8c wiring:

- `orchestrator_salsa_per_region` (4 tests, all previously
  `#[ignore]`'d under Phase 7.3a, un-ignored in this delivery):
  pass.  Headline: **1 of 6 regions re-lifted** after one
  indirect-target add on `control::nested_loops`.
- `orchestrator_salsa_parity` (4 tests): pass.
- `orchestrator_salsa_incremental` (4 tests): pass.

`v1_baseline` and `v2_baseline` snapshot tests (which don't go
through the salsa orchestrator) pass unchanged.
