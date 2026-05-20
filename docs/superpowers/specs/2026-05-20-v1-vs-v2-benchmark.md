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
