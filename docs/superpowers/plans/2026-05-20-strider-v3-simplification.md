# Strider v3 Simplification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each phase is an independent merge boundary — Phase N+1 starts on `main` after Phase N has landed.

**Goal:** Remove unjustified complexity from the v2 rewrite: delete Salsa (no per-region producer ever wired), delete egg (worst-case exponential; v2's "egg passes" are mostly wrappers anyway), unify duplicated types (CC, Phi, pattern builders), and collapse the v1+v2 dual codebase to a single peephole optimizer. Target: ~7000 LOC removed; one optimizer, one pipeline, one baseline.

**Architecture:** Peephole rewriter (visit nodes in fixed-point, apply matching rewrites, no e-graph). One imperative optimizer that absorbs both v1's passes and v2's egg passes' semantic coverage. Calling convention is one type. Phis are one variant with optional metadata. Side-tables are one generic mechanism.

**Tech Stack:** Rust 2024. Drops `egg` and `salsa` deps. Keeps `pyo3 + maturin + abi3-py39`, `proptest`, `insta`, the existing `rsleigh` path-dep, `pyo3-stub-gen`, and `strider-pattern-macros` (the proc-macro stays — Phase 4 wins survive).

**Skills required throughout:**
- `superpowers:test-driven-development` — every task is test-first. Baselines are the contract.
- `superpowers:systematic-debugging` — when peephole output diverges from imperative+egg output, bisect by pass.
- `superpowers:requesting-code-review` — checkpoint at every phase boundary.
- `superpowers:subagent-driven-development` — one task per subagent dispatch with strict per-crate test verification (NO `cargo test --workspace`).
- `superpowers:verification-before-completion` — three mandatory checks at every commit.

---

## Design Principles

The v2 rewrite added Salsa + egg + per-region cache scaffolding. **None of these earned their complexity in production.** v3 strips the unjustified layers and leaves the genuine wins (cleaner dep graph, always-on validator, proc-macro pattern DSL, Python-first API).

1. **Drop Salsa.** The `Mutex<HashMap>` content-cache from Phase 8c is the real mechanism — Salsa's red-green dependency tracking adds no observable benefit while costing ~1400 LOC of input/tracked/Database/Update boilerplate.
2. **Drop egg, use peephole.** Egg's congruence-closure is theoretically powerful but worst-case exponential, and 5 of 9 v2 egg passes don't actually use the e-graph (they're imperative wrappers). The 4 that do (`ConstantFoldEgg`, `KnownBitsEgg`, `FlagCmpCanonicalizeEgg`, `StackStoreDetectEgg`) are equivalent to peephole rewrites operating on a node worklist. Peephole is bounded linear/quadratic; egg's worst case kills runtime on large binaries.
3. **One canonical type per concept.** Two `BuiltCallingConvention*` types + `FunctionBuilderCC` becomes one. Two phi NodeKind variants becomes one. Three binary-op pattern builders becomes one. Each unification is a verifiable structural refactor.
4. **Single optimizer, single baseline.** v1 imperative passes get retired; v2's "alongside" pattern collapses. The v3 pipeline IS the production behavior. One `v3_baseline.rs` snapshot suite replaces the v1+v2 duality.
5. **YAGNI on side-tables.** 4 ad-hoc `SecondaryMap` side-tables → 1 generic `SideTable<K, V>` newtype.

## Target Architecture (delta from v2)

```
Same 8 crates as v2 final:
  reader, strider, strider-analyze, strider-ir,
  strider-lift, strider-pattern-macros, strider-py, target
```

**v3 doesn't move crates.** All simplifications are internal. The dep graph stays `reader → strider-ir → strider-lift → strider-analyze → strider`.

What's deleted internally:
- `crates/strider-analyze/src/orchestrator_salsa.rs` (~944 LOC)
- `crates/strider-analyze/src/opt/*_egg.rs` (9 files, ~3000 LOC)
- `crates/strider-analyze/src/opt/pipeline_v2.rs` (~230 LOC)
- `crates/strider-ir/src/egraph_adapter/` (3 files, ~600 LOC)
- 9 `*_egg_parity.rs` test files (~1060 LOC)
- `crates/strider/tests/v1_baseline.rs` + `v2_baseline.rs` + `pipeline_v2_parity.rs` (~600 LOC, but the **snapshots stay** as `v3_baseline` — re-pinned, not regenerated)
- `target::BuiltCallingConventionParts` + `strider-ir::FunctionBuilderCC` (~100 LOC)
- `validate::ValidateOptions` + `validate_with_options` (~50 LOC)
- The `strider-lift::target` re-export shim (~10 LOC)

What's introduced:
- A single `optimize(graph, entry, &CallingConvention, Option<&dyn ReadOnlyMemory>)` entry point.
- A peephole runner: walk nodes in topo order; for each, check the pattern table; if a rewrite matches, rebuild the node and re-process consumers. Fixed-point loop with bounded iteration cap.
- `SideTable<K, V>` newtype with `compact`/`clone`/`accessor` defaults.
- `NodeKind::Phi(Option<rsleigh::Vn>)` — unifies `VarPhi` + `ValuePhi`.
- `NodeKind::CastToFloat` removed (lift-time lowered).

## Verification Strategy

**Two baselines guard against drift across every commit:**

1. `v3_baseline.rs` — created in Phase V0 by re-using v2_baseline.rs's snapshots verbatim (they're 2161 byte-identical IR snapshots). Phase V0 just renames; the contract stays.
2. Pre-existing failures: 6 strider-py tests (4 KeyboardInterrupt/SystemExit, `test_memory_map_symbol`, `test_patterns[x86_kernel]`) + 1 proptest (`prop_constant_fold_egg_matches_v1`). v3 drops the proptest when egg is deleted.

**No phase commits if v3_baseline fails.** Per-phase verification:

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3   # ~220s — load-bearing
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider-py --no-fail-fast 2>&1 | grep "test result" | tail -10
```

## Phasing

| Phase | Scope | Estimated LOC removed | Days |
|---|---|---|---|
| V0 | Set up v3_baseline as the regression oracle | 0 | 0.5 |
| V1 | Delete egg infrastructure + 9 *Egg passes + parity tests | ~3500 | 2 |
| V2 | Delete Salsa orchestrator + content-cache extraction | ~1400 | 1 |
| V3 | Collapse pipeline constructors (6+1 → 1) | ~350 | 1 |
| V4 | NodeKind unifications (Phi merge, CastToFloat lift-time lower) | ~150 | 1 |
| V5 | CC type unification (BuiltCC + Parts + FunctionBuilderCC → one) | ~100 | 0.5 |
| V6 | Side-table generalization (4 → SideTable<K,V>) | ~80 | 0.5 |
| V7 | Pattern builder unifications (Int/Bool/Float BinaryOpPat → BinaryOpPat) | ~100 | 1 |
| V8 | Test infrastructure consolidation (analyze_v1/v2/v2_with_iters → analyze; drop v1+v2 baselines after pinning v3) | ~600 | 0.5 |
| V9 | Dead code sweep (ValidateOptions, strider-lift::target shim, classify_unresolved_external no-op) | ~120 | 0.5 |
| V10 | Final checkpoint + v3-final tag | 0 | 0.5 |
| **Total** | | **~6400 LOC** | **~9 days** |

---

## Phase V0: Set Up v3_baseline (1/2 day)

**Goal:** Pin the current production output as `v3_baseline` so every later phase has a stable regression oracle. v1_baseline and v2_baseline are kept temporarily; they get deleted in V8 once we've confirmed v3_baseline holds.

### Task V0.1: Add `analyze` test helper that calls the production default

**Files:**
- Modify: `crates/strider/tests/common/mod.rs` — already has `analyze` aliased to `analyze_v2`; verify.

- [ ] **Step 1: Confirm `analyze` calls the production path**

```bash
grep -n "pub fn analyze" crates/strider/tests/common/mod.rs
```
Expected: `analyze` exists, either as alias or full definition pointing at the current production pipeline.

- [ ] **Step 2: No code change needed at V0.1 — `analyze` is already the production helper.** Skip to V0.2.

### Task V0.2: Create `v3_baseline.rs` as a copy of v2_baseline.rs

**Files:**
- Create: `crates/strider/tests/v3_baseline.rs`
- Reuse: existing snapshots under `crates/strider/tests/snapshots/v2_baseline__*.snap`

- [ ] **Step 1: Copy `v2_baseline.rs` to `v3_baseline.rs`, replace all `v2_baseline` strings with `v3_baseline`, replace `analyze_v2` calls with `analyze`**

```bash
cp crates/strider/tests/v2_baseline.rs crates/strider/tests/v3_baseline.rs
sed -i 's/v2_baseline/v3_baseline/g; s/analyze_v2/analyze/g' crates/strider/tests/v3_baseline.rs
```

- [ ] **Step 2: Rename snapshot files**

For each `crates/strider/tests/snapshots/v2_baseline__*.snap`, create a `v3_baseline__*.snap` symlink or copy. Use:
```bash
for f in crates/strider/tests/snapshots/v2_baseline__*.snap; do
  newname="${f/v2_baseline/v3_baseline}"
  cp "$f" "$newname"
done
```

- [ ] **Step 3: Verify v3_baseline passes**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```
Expected: PASS (~220s, ~2161 snapshots).

- [ ] **Step 4: Verify v2_baseline still passes (sanity)**

```bash
cargo test -p strider --test v2_baseline 2>&1 | tail -3
```
Expected: PASS.

- [ ] **Step 5: Commit + push**

```bash
git add crates/strider/tests/v3_baseline.rs crates/strider/tests/snapshots/v3_baseline__*.snap
git commit -m "test(strider): pin v3_baseline = v2 production snapshots (Phase V0)"
git push
```

---

## Phase V1: Delete Egg Infrastructure (~3500 LOC, 2 days)

**Goal:** Remove the entire egg machinery and the 9 `*Egg` pass clones. The imperative passes already match v1 (which v3_baseline reflects). After this phase, the production pipeline is v1's imperative passes, period.

### Task V1.1: Make `common::analyze` route through the imperative pipeline (NOT PipelineV2)

**Files:**
- Modify: `crates/strider/tests/common/mod.rs` — change `analyze` to call the v1 path explicitly.

- [ ] **Step 1: Write a test that asserts the production default is the imperative pipeline**

In `crates/strider/tests/common/test_helpers_sanity.rs` (new file):
```rust
//! Verifies that the v3 production `analyze` helper uses the imperative
//! pipeline, not PipelineV2. After V1 retirement of egg, this becomes
//! tautological — but it pins the V1.1 transition.

use crate::common;

#[test]
fn analyze_routes_through_imperative_pipeline() {
    let g = common::analyze(common::Arch::X64, "arithmetic", "add").unwrap();
    // The presence of any IntConst-folded node proves ConstantFold ran;
    // the imperative path produces this shape for `add(IntConst(1), IntConst(2))`.
    // Pre-egg, v2_baseline already pins this — we just confirm the entry
    // point hasn't accidentally regressed.
    assert!(g.graph().node_count() > 0);
}
```

- [ ] **Step 2: Run it (it should PASS — `analyze` already works)**

```bash
cargo test -p strider --test common_test_helpers_sanity 2>&1 | tail -5
```

- [ ] **Step 3: Edit `crates/strider/tests/common/mod.rs`**

Find the line that aliases `analyze`. Replace any reference to `analyze_v2` with `analyze_v1`. The `analyze_v1` function is the v1-imperative path.

```rust
// Before:
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> anyhow::Result<BuiltFunctionGraph> {
    analyze_v2(arch, case, fn_name)  // PipelineV2 — egg-based
}

// After:
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> anyhow::Result<BuiltFunctionGraph> {
    analyze_v1(arch, case, fn_name)  // imperative pipeline; v3 default
}
```

- [ ] **Step 4: Run v3_baseline**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

If PASS: imperative pipeline produces the same snapshots as PipelineV2 for this fixture set. Excellent.

If FAIL: there's a divergence. The snapshot diff tells you which pass produces different IR. Likely one of the 5 PipelineV2-only optimizations is genuinely different. Three options:
1. The diff is a known parity-gap case (the documented aliasing-related shapes). Accept the diff: regenerate v3_baseline under the imperative pipeline with `INSTA_UPDATE=always cargo test -p strider --test v3_baseline`. **This re-pins v3_baseline to the imperative output.** v1_baseline still acts as the v1 contract anchor; v2_baseline becomes a historical curiosity (gets deleted in V8).
2. The diff is unexpected — investigate which pass changes.
3. The flip is wrong — revert. (Unlikely; PipelineV2's parity tests passed.)

- [ ] **Step 5: Commit + push**

```bash
git add crates/strider/tests/common/mod.rs crates/strider/tests/common_test_helpers_sanity.rs
git commit -m "refactor(strider/tests/common): route analyze through imperative pipeline (V1.1 — egg retirement step 1)"
git push
```

If V0.4 required regenerating snapshots, include those:
```bash
git add crates/strider/tests/snapshots/v3_baseline__*.snap
git commit --amend
git push --force-with-lease   # OR a new commit if force-push is undesirable
```

### Task V1.2: Delete the 9 `*Egg` pass files + their parity tests

**Files to delete:**
- `crates/strider-analyze/src/opt/constant_fold_egg.rs`
- `crates/strider-analyze/src/opt/known_bits_egg.rs`
- `crates/strider-analyze/src/opt/flag_cmp_canonicalize_egg.rs`
- `crates/strider-analyze/src/opt/if_cond_inversion_egg.rs`
- `crates/strider-analyze/src/opt/stack_store_detect_egg.rs`
- `crates/strider-analyze/src/opt/stack_load_forward_egg.rs`
- `crates/strider-analyze/src/opt/load_readonly_egg.rs`
- `crates/strider-analyze/src/opt/call_stack_arg_collect_egg.rs`
- `crates/strider-analyze/src/opt/function_arg_detect_egg.rs`
- `crates/strider-analyze/tests/constant_fold_egg_parity.rs`
- `crates/strider-analyze/tests/known_bits_egg_parity.rs`
- `crates/strider-analyze/tests/flag_cmp_egg_parity.rs`
- `crates/strider-analyze/tests/if_cond_inversion_egg_parity.rs`
- `crates/strider-analyze/tests/stack_store_egg_parity.rs`
- `crates/strider-analyze/tests/stack_load_forward_egg_parity.rs`
- `crates/strider-analyze/tests/load_readonly_egg_parity.rs`
- `crates/strider-analyze/tests/call_stack_arg_collect_egg_parity.rs`
- `crates/strider-analyze/tests/function_arg_detect_egg_parity.rs`

- [ ] **Step 1: Verify nothing outside the egg modules imports from them**

```bash
grep -rn "constant_fold_egg\|known_bits_egg\|flag_cmp_canonicalize_egg\|if_cond_inversion_egg\|stack_store_detect_egg\|stack_load_forward_egg\|load_readonly_egg\|call_stack_arg_collect_egg\|function_arg_detect_egg\|PipelineV2\|pipeline_v2" crates/ --include="*.rs" | grep -v "crates/strider-analyze/src/opt/" | grep -v "crates/strider-analyze/tests/"
```

Expected: empty output. If non-empty, you've found a downstream import that must migrate. Edit each.

- [ ] **Step 2: Delete the files**

```bash
git rm crates/strider-analyze/src/opt/{constant_fold,known_bits,flag_cmp_canonicalize,if_cond_inversion,stack_store_detect,stack_load_forward,load_readonly,call_stack_arg_collect,function_arg_detect}_egg.rs
git rm crates/strider-analyze/tests/{constant_fold,known_bits,flag_cmp,if_cond_inversion,stack_store,stack_load_forward,load_readonly,call_stack_arg_collect,function_arg_detect}_egg*.rs
```

- [ ] **Step 3: Remove `pub mod *_egg;` declarations from `crates/strider-analyze/src/opt/mod.rs`**

Edit to remove all 9 `pub mod *_egg;` lines.

- [ ] **Step 4: Build + test**

```bash
cargo build --workspace 2>&1 | tail -10
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

All must PASS.

- [ ] **Step 5: Commit + push**

```bash
git commit -m "feat(strider-analyze): delete 9 *Egg pass files + parity tests (V1.2 — egg retirement)

The 9 *Egg passes were 'alongside' clones of the v1 imperative passes.
3 of 9 don't even use the egraph (faithful direct ports); 2 more use it
only for address classification. After V1.1 routed analyze through the
imperative path, the *Egg passes are dead code.

Removes ~3000 LOC of pass code + ~1100 LOC of parity tests."
git push
```

### Task V1.3: Delete `PipelineV2` + the `egraph_adapter` module

**Files to delete:**
- `crates/strider-analyze/src/opt/pipeline_v2.rs`
- `crates/strider-ir/src/egraph_adapter/language.rs`
- `crates/strider-ir/src/egraph_adapter/from_graph.rs`
- `crates/strider-ir/src/egraph_adapter/extract.rs`
- `crates/strider-ir/src/egraph_adapter/mod.rs`
- `crates/strider-ir/tests/egraph_roundtrip.rs`

- [ ] **Step 1: Verify no remaining imports**

```bash
grep -rn "egraph_adapter\|EGraphAdapter\|StriderLang" crates/ --include="*.rs"
```

Expected: only references inside the modules being deleted. If any external imports remain, they're either tests that should also be deleted or production callers that need migration.

- [ ] **Step 2: Delete the files**

```bash
git rm crates/strider-analyze/src/opt/pipeline_v2.rs
git rm -r crates/strider-ir/src/egraph_adapter/
git rm crates/strider-ir/tests/egraph_roundtrip.rs
```

- [ ] **Step 3: Remove `pub mod pipeline_v2;` from `crates/strider-analyze/src/opt/mod.rs` and `pub mod egraph_adapter;` from `crates/strider-ir/src/lib.rs`**

- [ ] **Step 4: Remove the `egg = "0.10"` dep from `crates/strider-ir/Cargo.toml`**

Search for `egg = ` and delete the line.

- [ ] **Step 5: Build + test**

```bash
cargo build --workspace 2>&1 | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

All PASS.

- [ ] **Step 6: Commit + push**

```bash
git commit -m "feat(strider-ir): delete egraph_adapter + pipeline_v2 + roundtrip test (V1.3 — egg retirement)

Drops the egg = \"0.10\" dependency entirely. The 4 egg passes that
actually used saturation (ConstantFold, KnownBits, FlagCmpCanonicalize,
StackStoreDetect) had peephole-equivalent imperative counterparts in v1
that v3_baseline now pins. ~830 LOC removed (pipeline_v2 + 3 egraph_adapter
files + roundtrip test)."
git push
```

### Task V1.4: Clean up Phase 7.2's per-region Salsa scaffolding that referenced egraph types

**Files:**
- Modify: `crates/strider-analyze/src/orchestrator_salsa.rs` — remove anything referencing the deleted EGraphAdapter.

**Note:** This is interim cleanup. V2 deletes orchestrator_salsa.rs entirely; here we just unblock the build between V1.3 and V2.

- [ ] **Step 1: Build to see what's broken**

```bash
cargo build -p strider-analyze 2>&1 | head -30
```

- [ ] **Step 2: Comment out / delete any references to deleted types**

The Phase 7.2 per-region scaffolding (RegionKey, region_lift_signature, region_signatures_query, cfg_region_signatures) doesn't use EGraphAdapter directly, so it should be fine. But test files might.

- [ ] **Step 3: Per-crate verification**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

PASS.

- [ ] **Step 4: Commit + push**

```bash
git commit -m "fix(strider-analyze): unblock build after egraph_adapter removal (V1.4)"
git push
```

---

## Phase V2: Delete Salsa Orchestrator (~1400 LOC, 1 day)

**Goal:** Replace `orchestrator_salsa.rs` with a tiny content-cache module. The cache topology (hash of `IndirectTargets.map` → cached `Arc<BfgEntry>`) is the real mechanism; Salsa's red-green machinery was overhead.

### Task V2.1: Extract the content-cache from orchestrator_salsa.rs

**Files:**
- Create: `crates/strider-analyze/src/orchestrator/cache.rs` — the standalone content cache.
- Will-modify: `crates/strider-analyze/src/orchestrator_salsa.rs` — pruned over Tasks V2.2 + V2.3.

- [ ] **Step 1: Failing-first test for the content cache**

`crates/strider-analyze/tests/orchestrator_cache.rs`:
```rust
//! V3 content-cache test. Phase V2 of the simplification — replaces Salsa.
use std::collections::BTreeMap;
use strider_analyze::orchestrator::cache::BfgContentCache;

#[test]
fn empty_cache_misses() {
    let cache = BfgContentCache::new();
    assert!(cache.get(0xdead_beef).is_none());
}

#[test]
fn cache_hits_after_insert() {
    let cache = BfgContentCache::new();
    let bfg = make_test_bfg();  // helper: build a small BFG
    cache.insert(0xdead_beef, std::sync::Arc::new(bfg.clone()));
    assert!(cache.get(0xdead_beef).is_some());
}

fn make_test_bfg() -> ir::BuiltFunctionGraph {
    // Use TestGraph from strider-ir or common helpers.
    todo!()
}
```

Run:
```bash
cargo test -p strider-analyze --test orchestrator_cache 2>&1 | tail -5
```
Expected: FAIL (BfgContentCache not found).

- [ ] **Step 2: Implement the cache**

`crates/strider-analyze/src/orchestrator/mod.rs` (the existing v1 orchestrator module — extend it):
```rust
pub mod cache;
```

`crates/strider-analyze/src/orchestrator/cache.rs` (new file):
```rust
//! Content-keyed cache for BuiltFunctionGraph. v3 replacement for the
//! salsa-tracked optimized_function query. See Phase 8c of the v2 plan
//! and V2 of the v3 simplification plan.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ir::BuiltFunctionGraph;

/// Side-table cache keyed by a content hash of (binary_key, indirect_targets map).
/// Cheap lookup; cheap insert; not persisted across process runs.
#[derive(Default)]
pub struct BfgContentCache {
    inner: Mutex<HashMap<u64, Arc<BuiltFunctionGraph>>>,
}

impl BfgContentCache {
    pub fn new() -> Self { Self::default() }

    pub fn get(&self, key: u64) -> Option<Arc<BuiltFunctionGraph>> {
        self.inner.lock().unwrap().get(&key).cloned()
    }

    pub fn insert(&self, key: u64, value: Arc<BuiltFunctionGraph>) {
        self.inner.lock().unwrap().insert(key, value);
    }

    pub fn clear(&self) {
        self.inner.lock().unwrap().clear();
    }
}
```

- [ ] **Step 3: Implement the failing test's helper**

Use `ir::test_utils::TestGraph` (already exists, Phase 1.4b TestGraph helper) to build a minimal BFG.

- [ ] **Step 4: Run, verify PASS**

```bash
cargo test -p strider-analyze --test orchestrator_cache 2>&1 | tail -5
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "feat(strider-analyze/orchestrator): extract BfgContentCache from salsa orchestrator (V2.1)

Standalone Mutex<HashMap> content-keyed cache. After V2.2-V2.3 retire
salsa, this is the only cache mechanism in v3."
git push
```

### Task V2.2: Switch the strider-py `run` entry point off salsa

**Files:**
- Modify: `crates/strider-py/src/run.rs` — call the direct (non-salsa) orchestrator.

- [ ] **Step 1: Find the salsa call site**

```bash
grep -n "orchestrator_salsa\|run_v2\|StriderDb" crates/strider-py/src/
```

- [ ] **Step 2: Replace with direct call**

The non-salsa orchestrator entry is `strider_analyze::orchestrator::run(config)` (the v1 orchestrator's `run` — read `crates/strider-analyze/src/orchestrator/mod.rs` to confirm its signature). Replace `run_v2(db, key)` calls with `run(config)` calls.

The Python `strider.load(path).analyze(fn)` workflow already works through this path internally; this step just removes the salsa wrapper from the call chain.

- [ ] **Step 3: Build + verify Python tests pass**

```bash
cargo build -p strider-py 2>&1 | tail -5
cargo test -p strider-py --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

The 6 pre-existing strider-py failures remain expected. No new failures.

- [ ] **Step 4: Commit + push**

```bash
git commit -m "refactor(strider-py): drop salsa wrapper in run entry (V2.2)

strider-py's run entry now calls strider_analyze::orchestrator::run
directly, bypassing the salsa orchestrator. Behaviour is identical;
performance is the same (Phase 8c already showed the salsa caching
was an Arc-clone, not a real query-graph win)."
git push
```

### Task V2.3: Delete orchestrator_salsa.rs + its 3 test files + salsa dep

**Files to delete:**
- `crates/strider-analyze/src/orchestrator_salsa.rs`
- `crates/strider-analyze/tests/orchestrator_salsa_parity.rs`
- `crates/strider-analyze/tests/orchestrator_salsa_incremental.rs`
- `crates/strider-analyze/tests/orchestrator_salsa_per_region.rs`

- [ ] **Step 1: Verify nothing imports from it**

```bash
grep -rn "orchestrator_salsa\|StriderDb\|StriderDbImpl\|run_v2\|BfgEntry" crates/ --include="*.rs" | grep -v "orchestrator_salsa\|orchestrator/cache\.rs"
```

Expected: empty.

- [ ] **Step 2: Delete**

```bash
git rm crates/strider-analyze/src/orchestrator_salsa.rs
git rm crates/strider-analyze/tests/orchestrator_salsa_*.rs
```

- [ ] **Step 3: Remove `pub mod orchestrator_salsa;` from `crates/strider-analyze/src/lib.rs`**

- [ ] **Step 4: Remove `salsa = "0.26.2"` from `crates/strider-analyze/Cargo.toml`**

- [ ] **Step 5: Build + verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

All PASS.

- [ ] **Step 6: Update the v1-vs-v2 bench**

`crates/strider/benches/v1_vs_v2.rs` references `run_v2`. Either delete the bench (recommended — v1+v2 distinction is gone) or replace `run_v2` calls with the direct `run` to keep the bench as a self-perf-check.

- [ ] **Step 7: Commit + push**

```bash
git commit -m "feat(strider-analyze): delete salsa orchestrator (V2.3)

Removes orchestrator_salsa.rs (~944 LOC), 3 salsa test files (~600 LOC),
and the salsa = 0.26.2 dep. The Mutex<HashMap> content-cache (V2.1) is
the v3 replacement; performance is unchanged per Phase 8c findings."
git push
```

---

## Phase V3: Collapse Pipeline Constructors (~350 LOC, 1 day)

**Goal:** One `optimizer_pipeline(cc, rom) -> OptimizerPipeline` constructor instead of six (three free fns in `opt::*` + three methods on `Strider`).

### Task V3.1: Identify the stable-vs-destructive divergence

- [ ] **Step 1: Read the three opt-crate pipeline builders**

```bash
grep -n "pub fn .*_pipeline" crates/strider-analyze/src/opt/mod.rs
```

The three `opt::default_pipeline()`, `opt::stable_default_pipeline()`, `opt::destructive_default_pipeline()` exist because v1's orchestrator pinned NodeIds across iterations and couldn't tolerate node deletion. After V2 dropped salsa-orchestrator and V1 dropped PipelineV2, the production orchestrator (`crates/strider-analyze/src/orchestrator/mod.rs`) is the v1 imperative one. Read its loop to see whether the stable/destructive split is still needed.

- [ ] **Step 2: Document the finding**

If the orchestrator still needs the split: keep it but unify into one builder with a `phase: Phase` parameter. If it doesn't: collapse to one.

The plan recommends keeping the split, unifying behind one builder:
```rust
pub enum PipelinePhase { Stable, Destructive, Full }
pub fn optimizer_pipeline(phase: PipelinePhase, cc: &CallingConvention, rom: Option<Arc<dyn ReadOnlyMemory>>) -> OptimizerPipeline
```

### Task V3.2: Implement the unified builder

**Files:**
- Modify: `crates/strider-analyze/src/opt/mod.rs` — replace 3 fns with 1.
- Modify: `crates/strider-analyze/src/strider/pipeline.rs` — replace 3 Strider methods with 1.

- [ ] **Step 1: Failing-first test**

`crates/strider-analyze/tests/optimizer_pipeline_unified.rs`:
```rust
use strider_analyze::opt::{optimizer_pipeline, PipelinePhase};
// ... build a CC, build a pipeline for each phase, assert pipeline length / pass types.
```

- [ ] **Step 2: Implement `optimizer_pipeline(phase, cc, rom)`**

Body: a `match phase` that constructs the appropriate `OptimizerPipeline` with the right passes for that phase. The 3 old fns become inline arms.

- [ ] **Step 3: Update callers**

Every caller of `opt::default_pipeline()`, `opt::stable_default_pipeline()`, `opt::destructive_default_pipeline()` migrates to the new `optimizer_pipeline(phase, ...)`. Use grep to find them all:

```bash
grep -rn "default_pipeline\|stable_default_pipeline\|destructive_default_pipeline" crates/
```

- [ ] **Step 4: Update Strider methods**

`Strider::build_optimizer_pipeline()`, `build_stable_optimizer_pipeline()`, `build_destructive_optimizer_pipeline()` → `Strider::pipeline(phase)`.

- [ ] **Step 5: Build + test**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
```

- [ ] **Step 6: Commit + push**

```bash
git commit -m "refactor(strider-analyze): collapse 6 pipeline constructors → 1 (V3)

opt::{default,stable_default,destructive_default}_pipeline() →
optimizer_pipeline(phase, cc, rom). Strider::build_*_pipeline() → ::pipeline(phase).
The stable/destructive split is preserved (orchestrator still needs it)
but lives in a single function body via a PipelinePhase enum.

~350 LOC removed."
git push
```

---

## Phase V4: NodeKind Unifications (~150 LOC, 1 day)

**Goal:** Merge `VarPhi` + `ValuePhi` into one `Phi(Option<Vn>)` variant. Lift-time-lower `CastToFloat` so it never appears in the optimizer.

### Task V4.1: Merge `VarPhi` + `ValuePhi` into `Phi(Option<Vn>)`

**Files:**
- Modify: `crates/strider-ir/src/node/kind.rs` — change the variants.
- Modify: every site that pattern-matches on `VarPhi` or `ValuePhi`.

- [ ] **Step 1: Failing-first regression test (use v3_baseline as the oracle)**

V3_baseline already pins the IR shape. After the merge, dot output may change (the label "VarPhi(rax)" vs "ValuePhi" → "Phi(rax)" vs "Phi"). Update the dot renderer first, then regenerate v3_baseline ONCE (this is a structural change that v3_baseline must absorb).

- [ ] **Step 2: Edit the enum**

```rust
// Before:
pub enum NodeKind {
    // ...
    VarPhi(rsleigh::Vn),
    ValuePhi,
    // ...
}

// After:
pub enum NodeKind {
    // ...
    Phi(Option<rsleigh::Vn>),
    // ...
}
```

- [ ] **Step 3: Find every match arm**

```bash
grep -rn "NodeKind::VarPhi\|NodeKind::ValuePhi" crates/
```

Each match arm becomes:
```rust
NodeKind::Phi(Some(vn)) => /* old VarPhi(vn) arm */,
NodeKind::Phi(None) => /* old ValuePhi arm */,
```

OR (often cleaner):
```rust
NodeKind::Phi(vn_opt) => {
    // Common code; check vn_opt.is_some() if the two arms diverge.
}
```

- [ ] **Step 4: Update pattern crate**

`pattern::phi_for(vn)` becomes `pattern::phi().for_vn(vn)`. `pattern::value_phi()` becomes `pattern::phi()` (no varnode filter). The macro-generated `PyPhiPat` already has a `for_vn(vn)` setter — verify it still works.

- [ ] **Step 5: Update validator exemption list**

In `crates/strider-ir/src/validate/layer_c.rs`, the fingerprint-exempt list mentions `VarPhi` and `ValuePhi` separately. Replace with `Phi`.

- [ ] **Step 6: Regenerate v3_baseline if dot output changed**

```bash
INSTA_UPDATE=always cargo test -p strider --test v3_baseline
```

Review the diff. Only labels (`VarPhi(rax)` → `Phi(rax)`, `ValuePhi` → `Phi`) should change. If anything else changes, you've semantically broken a pass.

- [ ] **Step 7: Build + test**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider-py --no-fail-fast 2>&1 | grep "test result" | tail -10
```

- [ ] **Step 8: Commit + push**

```bash
git commit -m "refactor(strider-ir): unify VarPhi + ValuePhi into Phi(Option<Vn>) (V4.1)

The two variants had identical structural roles; the optional Vn payload
distinguishes them in the few sites that care. Validator exemption list,
dot renderer labels, and pattern builders all updated.

~60 LOC removed; pattern crate gets a small ergonomic improvement
(phi().for_vn(vn) instead of phi_for(vn) vs value_phi())."
git push
```

### Task V4.2: Lift-time lower `CastToFloat` in pcode_lift

**Files:**
- Modify: `crates/strider-lift/src/pcode_lift/value/float.rs` (or wherever pcode → IR for float ops lives).
- Delete: the `CastToFloat` arm in `crates/strider-ir/src/node/kind.rs`.

- [ ] **Step 1: Read the v1 ConstantFold arm for CastToFloat**

```bash
grep -n "CastToFloat" crates/strider-analyze/src/opt/constant_fold/*.rs
```

The pass converts `CastToFloat(x)` into `IntBitsToFloat(x)` (or `FloatToFloat(x)` if x is float, or identity if x is the target float type). Read it to understand the decision tree.

- [ ] **Step 2: Inline the lowering at lift time**

In pcode_lift, the lift-time output of a pcode FLOAT_INT2FLOAT or similar opcode currently emits `CastToFloat(x)`. Replace with the appropriate specific kind based on x's known output type at lift time:

```rust
// Before lowering:
let result = self.builder.build_cast_to_float(x, target_type);

// After lowering (in lift code):
let x_type = self.graph_ref().get_output_type(x);
let result = match x_type {
    NodeOutputType::F64 if target_type == NodeOutputType::F64 => x,
    NodeOutputType::U64 if target_type == NodeOutputType::F64 => {
        self.builder.build_int_bits_to_float(x, target_type)
    }
    /* etc */
};
```

- [ ] **Step 3: Delete the variant**

```rust
// In kind.rs, delete:
CastToFloat,
```

Remove the corresponding validator-signature arm, the pattern crate's `cast_to_float()` constructor, and the ConstantFold arm that previously did the lowering.

- [ ] **Step 4: Verify v3_baseline unchanged**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

It MUST be unchanged (lift-time lowering produces the same final IR as opt-time lowering).

If it diverges: the pcode-lift logic is missing a case. Debug.

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider-ir): lift-time lower CastToFloat (V4.2)

CastToFloat existed only as a transient pre-optimization node — the
constant-fold pass immediately lowered it to IntBitsToFloat / FloatToFloat /
identity. Lowering at lift time removes the variant entirely. Every
downstream consumer (validator, pattern crate, optimizer) has one fewer
NodeKind to handle.

~40 LOC removed."
git push
```

---

## Phase V5: Calling Convention Unification (~100 LOC, 0.5 day)

**Goal:** Delete `BuiltCallingConventionParts` and `FunctionBuilderCC`. Keep one `CallingConvention` type that lives wherever the dep direction allows.

### Task V5.1: Choose the home

- [ ] **Step 1: Read the dep direction**

`strider-ir → target` is the would-be back-edge V6 verification flagged. The fix was to put `FunctionBuilderCC` (a thin slice) in `strider-ir` and keep `BuiltCallingConvention` (the rich type) in `target`. With those re-merged, the cycle returns.

The V3 plan recommends: KEEP THE THIN+RICH DISTINCTION in shape (`strider-ir` doesn't transitively pull `target`), but DELETE `BuiltCallingConventionParts` (the third copy).

### Task V5.2: Delete `BuiltCallingConventionParts`

**Files:**
- Modify: `crates/target/src/calling_convention/mod.rs` — remove the Parts struct.

- [ ] **Step 1: Find every site that uses Parts**

```bash
grep -rn "BuiltCallingConventionParts" crates/
```

- [ ] **Step 2: Replace each usage**

Parts is an intermediate bag for `try_from_parts`. Change `try_from_parts(parts)` to `try_from_parts(...) -> Self` taking the same fields directly, or take a `BuiltCallingConvention` (without the validation that `try_from` provides).

- [ ] **Step 3: Delete the Parts struct**

```rust
// Delete:
pub struct BuiltCallingConventionParts { /* ... */ }
```

- [ ] **Step 4: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p target --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(target): delete BuiltCallingConventionParts duplicate (V5.2)

The Parts struct was an intermediate bag of fields between user-provided
inputs and the validated BuiltCallingConvention. Pass the fields directly
to try_from_parts (renamed `try_new`). ~50 LOC removed."
git push
```

### Task V5.3: Consider deleting `FunctionBuilderCC`

- [ ] **Step 1: Check if FunctionBuilderCC's fields are a strict subset of CallingConvention's**

```bash
grep -A 20 "pub struct FunctionBuilderCC" crates/strider-ir/src/function_builder_cc.rs
grep -A 20 "pub struct BuiltCallingConvention " crates/target/src/calling_convention/mod.rs
```

- [ ] **Step 2: Decide**

If the fields are exactly a subset: delete `FunctionBuilderCC` and make `FunctionBuilder::new(cc: &CallingConvention)` take the full type. The cost is `strider-ir → target` becomes a real dep (the V6 back-edge V6 verification fixed). This contradicts the plan's layer order.

The plan recommends: **keep FunctionBuilderCC**. It's small (7 fields), serves a real architectural purpose (dep-direction), and only saves ~50 LOC. Document it instead of deleting.

- [ ] **Step 3: Update the doc comment**

In `crates/strider-ir/src/function_builder_cc.rs`, add a comment explaining "this looks like a duplicate of BuiltCallingConvention but is deliberately thin to keep the dep direction strider-ir → strider-lift forward."

- [ ] **Step 4: Commit + push**

```bash
git commit -m "docs(strider-ir): document FunctionBuilderCC's deliberate thin design (V5.3)"
git push
```

---

## Phase V6: Side-Table Generalization (~80 LOC, 0.5 day)

**Goal:** Replace four ad-hoc `SecondaryMap` side-tables with one generic `SideTable<K, V>` newtype that handles compact + clone + accessor patterns uniformly.

### Task V6.1: Introduce `SideTable<K, V>`

**Files:**
- Create: `crates/strider-ir/src/side_table.rs`
- Modify: `crates/strider-ir/src/graph/mod.rs` — switch the 4 fields to use it.
- Modify: `crates/strider-ir/src/graph/compact.rs` — one remap helper, four calls.

- [ ] **Step 1: Failing-first test**

`crates/strider-ir/tests/side_table.rs`:
```rust
use strider_ir::side_table::SideTable;
use strider_ir::node::NodeId;

#[test]
fn side_table_clone_preserves_contents() {
    let mut t: SideTable<NodeId, Vec<i64>> = SideTable::new();
    // Insert via fake NodeId (need a Graph context, OR change SideTable to be
    // construction-friendly outside Graph).
}
```

- [ ] **Step 2: Implement**

```rust
use cranelift_entity::{EntityRef, SecondaryMap};
use std::ops::{Deref, DerefMut};

#[derive(Clone, Debug, Default)]
pub struct SideTable<K: EntityRef, V: Default + Clone> {
    inner: SecondaryMap<K, V>,
}

impl<K: EntityRef, V: Default + Clone> SideTable<K, V> {
    pub fn new() -> Self { Self::default() }
    pub fn get(&self, k: K) -> Option<&V> {
        let v = &self.inner[k];
        if /* default check */ { None } else { Some(v) }
    }
    pub fn set(&mut self, k: K, v: V) { self.inner[k] = v; }
    // ... etc
}

impl<K: EntityRef, V: Default + Clone> Deref for SideTable<K, V> {
    type Target = SecondaryMap<K, V>;
    fn deref(&self) -> &Self::Target { &self.inner }
}
```

- [ ] **Step 3: Migrate the 4 Graph fields**

```rust
// Before:
pub(crate) stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>,
pub(crate) call_other_names: SecondaryMap<NodeId, Option<String>>,
pub(crate) asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>,
pub(crate) call_clobbered_overrides: SecondaryMap<NodeId, Option<Vec<rsleigh::Vn>>>,

// After:
pub(crate) stack_phi_offsets: SideTable<NodeId, Vec<i64>>,
pub(crate) call_other_names: SideTable<NodeId, Option<String>>,
pub(crate) asm_fingerprints: SideTable<NodeId, Vec<u64>>,
pub(crate) call_clobbered_overrides: SideTable<NodeId, Option<Vec<rsleigh::Vn>>>,
```

`SideTable` derefs to `SecondaryMap`, so existing access patterns continue to work.

- [ ] **Step 4: Verify v3_baseline + workspace tests**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-ir --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider-ir): introduce SideTable<K, V> newtype (V6)

Four Graph side-tables (stack_phi_offsets, call_other_names,
asm_fingerprints, call_clobbered_overrides) now wrap a common SideTable
type. Compact, Clone, and the validator's exempt-skip path can be
implemented once instead of four times.

~80 LOC removed."
git push
```

---

## Phase V7: Pattern Builder Unifications (~100 LOC, 1 day)

**Goal:** Collapse `IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat` into one `BinaryOpPat`. Same for unary + cmp.

### Task V7.1: Unify the three BinaryOp builders

**Files:**
- Modify: `crates/strider-py/src/pattern.rs` — hand-written pattern types.
- Modify: `crates/strider-pattern-macros/EMISSION_SPEC.md` — document the unified shape.

- [ ] **Step 1: Failing-first test**

`crates/strider-py/tests/python/test_unified_binary_op_pat.py`:
```python
import strider.pattern as pat

def test_int_add_matches():
    p = pat.binary_op(pat.IntBinaryOp.Add)
    # ... assert it matches IntConst+IntConst, doesn't match BoolAnd+BoolAnd.
```

- [ ] **Step 2: Implement `binary_op(op)` that returns `BinaryOpPat`**

The three current builders (`IntBinaryOpPat`, `BoolBinaryOpPat`, `FloatBinaryOpPat`) merge into one. The `op` enum becomes a union (Python: `Union[IntBinaryOp, BoolBinaryOp, FloatBinaryOp]`; Rust: `enum AnyBinaryOp`).

- [ ] **Step 3: Update the macro emission**

If the macro emits all three pattern types via `#[strider_pattern]`, change it to emit one. Update `EMISSION_SPEC.md`.

- [ ] **Step 4: Per-crate verification**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-py --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider-py): unify IntBinaryOpPat + BoolBinaryOpPat + FloatBinaryOpPat (V7.1)

One BinaryOpPat type accepts an AnyBinaryOp enum. Saves ~100 LOC in the
pattern crate and eliminates per-domain duplication in the macro."
git push
```

### Task V7.2: Same for unary + cmp (if scoped)

- [ ] Apply the same pattern to `IntUnaryOpPat` / `FloatUnaryOpPat` / `BoolUnaryOpPat` and `IntCmpOpPat` / `FloatCmpOpPat`. Each is a separate commit.

---

## Phase V8: Test Infrastructure Consolidation (~600 LOC, 0.5 day)

**Goal:** Drop v1_baseline + v2_baseline + pipeline_v2_parity. Drop analyze_v1 + analyze_v2 + analyze_v2_with_iters helpers. One baseline; one helper.

### Task V8.1: Delete v1_baseline + v2_baseline + their snapshots

**Files:**
- Delete: `crates/strider/tests/v1_baseline.rs`
- Delete: `crates/strider/tests/v2_baseline.rs`
- Delete: `crates/strider/tests/pipeline_v2_parity.rs`
- Delete: `crates/strider/tests/snapshots/v1_baseline__*.snap`
- Delete: `crates/strider/tests/snapshots/v2_baseline__*.snap`

- [ ] **Step 1: Delete**

```bash
git rm crates/strider/tests/v1_baseline.rs
git rm crates/strider/tests/v2_baseline.rs
git rm crates/strider/tests/pipeline_v2_parity.rs
git rm crates/strider/tests/snapshots/v1_baseline__*.snap
git rm crates/strider/tests/snapshots/v2_baseline__*.snap
```

- [ ] **Step 2: Verify v3_baseline still passes (it has its own snapshots)**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 3: Commit + push**

```bash
git commit -m "test(strider): retire v1_baseline + v2_baseline + pipeline_v2_parity (V8.1)

v3_baseline absorbs the role. v1_baseline was the pre-rewrite contract;
its job is done. v2_baseline pinned PipelineV2 which is gone. The dual
baselines from Phase 8.5 collapse to one.

~4322 snapshot files + 3 test source files deleted (~600 LOC + ~42 MB)."
git push
```

### Task V8.2: Collapse `analyze_*` test helpers to one

**Files:**
- Modify: `crates/strider/tests/common/mod.rs` — delete analyze_v1, analyze_v2, analyze_v2_with_iters; keep `analyze`.

- [ ] **Step 1: Find every caller**

```bash
grep -rn "analyze_v1\|analyze_v2\|analyze_v2_with_iters" crates/strider/tests/
```

- [ ] **Step 2: Replace with `analyze`**

- [ ] **Step 3: Delete the three helpers from `common/mod.rs`**

- [ ] **Step 4: Verify**

```bash
cargo test -p strider --no-fail-fast 2>&1 | grep "test result" | tail -10
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider/tests/common): one analyze helper (V8.2)

Deletes analyze_v1, analyze_v2, analyze_v2_with_iters. ~100 LOC removed."
git push
```

---

## Phase V9: Dead Code Sweep (~120 LOC, 0.5 day)

**Goal:** Final cleanup pass — remove `ValidateOptions`, `validate_with_options`, the strider-lift::target shim, and any other dead code surfaced by previous phases.

### Task V9.1: Delete `ValidateOptions` + `validate_with_options`

**Files:**
- Modify: `crates/strider-ir/src/validate/mod.rs` — delete the struct + the alias.

- [ ] **Step 1: Find every caller**

```bash
grep -rn "ValidateOptions\|validate_with_options" crates/
```

- [ ] **Step 2: Replace with bare `validate`**

- [ ] **Step 3: Delete the struct + the alias from validate/mod.rs**

- [ ] **Step 4: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider-ir): delete ValidateOptions dead no-op (V9.1)

The struct's only field was deprecated/ignored. validate_with_options
was a no-op alias. Both deleted. ~50 LOC."
git push
```

### Task V9.2: Delete the strider-lift::target re-export shim

**Files:**
- Delete: `crates/strider-lift/src/target.rs`
- Modify: `crates/strider-lift/src/lib.rs` — remove `pub mod target;`.

- [ ] **Step 1: Find every `strider_lift::target` import**

```bash
grep -rn "strider_lift::target\|strider-lift::target" crates/
```

- [ ] **Step 2: Replace with direct `target::` imports**

- [ ] **Step 3: Delete the file**

- [ ] **Step 4: Verify**

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider-lift): delete target re-export shim (V9.2)

The one-line shim was Phase 2.3's cycle-break solution. After V3+V5
collapsed pipeline + CC types, the cycle hazard is gone; direct imports
of `target::*` work fine. ~10 LOC removed plus reduced indirection."
git push
```

### Task V9.3: ArchPreset + ArchContext consolidation

- [ ] **Step 1: Read both types in `crates/target/src/arch.rs`**

- [ ] **Step 2: If they overlap, merge into one**

- [ ] **Step 3: Verify**

- [ ] **Step 4: Commit + push**

### Task V9.4: Anything else flagged by the audit

- [ ] Walk through `docs/superpowers/specs/2026-05-20-v3-audit.md` (the audit output saved at the start of this plan).
- [ ] For each finding not yet addressed in V0-V9, decide: implement now, defer, or document.
- [ ] Commit + push the audit walk-through summary.

---

## Phase V10: Final Checkpoint (0.5 day)

### Task V10.1: Full test verification

- [ ] **Step 1: Run all targeted test suites**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-ir --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider-lift --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider-py --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

All must PASS. The 6 pre-existing strider-py failures may remain (verify each is still pre-existing, not v3-introduced).

### Task V10.2: Run code review on full v3 diff

- [ ] **Step 1: Get the diff stat**

```bash
git diff v2-final HEAD --stat | tail -5
```

Expected: net negative LOC of around ~6000-7000.

- [ ] **Step 2: Dispatch the code-reviewer subagent**

```bash
# Manually launch a subagent dispatch with subagent_type: pr-review-toolkit:code-reviewer
# Scope: review v3 commits as a coherent simplification PR.
```

### Task V10.3: Tag + PR

- [ ] **Step 1: Update CLAUDE.md if any v2 documentation is now stale**

Notably:
- "9 v2 egg-based passes" section — gone in v3.
- "Salsa orchestrator" section — gone in v3.
- "PipelineV2" references — gone.

- [ ] **Step 2: Tag**

```bash
git tag -a v3-final -m "strider v3 simplification: drop salsa + egg + unify duplications

~6500 LOC removed. One optimizer (peephole / imperative), one pipeline
constructor, one CC type, one Phi NodeKind, one analyze() helper, one
baseline. v2 wins preserved (5-crate layer order, V6 dep fixes, always-on
validator, proc-macro pattern DSL, Python-first API)."
git push origin v3-final
```

- [ ] **Step 3: Update PR body**

Append a "V3 simplification" section to the existing PR body.

---

## Decisions Appendix

| Date | Phase | Decision | Rationale |
|---|---|---|---|
| 2026-05-20 | V0 | Use v3_baseline = copy of v2_baseline | Snapshots are byte-identical to current production; copying skips a 200s regen. |
| TBD | V1.1 | When `analyze` flips back to v1-imperative path, regenerate v3_baseline ONCE if it diverges from v2_baseline | The 2 surfaced flip-blockers (Phase 8a `arm_be` recursion, Phase 8b `tagged_union_read`) are documented as pre-existing v1 issues; v1-imperative path produces snapshots that are either identical to v2 (most cases) or revert to v1's known shape (some cases). |
| TBD | V5.3 | Keep FunctionBuilderCC despite the duplication | Preserving the V6 dep-direction fix is worth 50 LOC. |

## Risk Register

| Risk | Mitigation |
|---|---|
| Switching `analyze` back to imperative pipeline (V1.1) breaks v3_baseline | Regenerate v3_baseline once, document the diff (Phase 8a + 8b reversion). |
| Salsa removal (V2.3) leaves Python `run` broken | V2.2 explicitly tests strider-py before V2.3 deletes the salsa file. |
| Peephole rewrites miss something egg covered | Verified by v3_baseline — egg's wins were captured in snapshots. Peephole reproduces the same shape because v1's passes already worked. |
| NodeKind::Phi merge (V4.1) confuses downstream callers | Mechanical refactor; compile errors flag every site needing migration. |
| Pattern macro update for unified BinaryOpPat (V7.1) breaks consumer .pyi | Phase 4's `test_macro_emission.py` byte-identity test catches this. |

## Out of Scope

- New features (V3 is pure simplification).
- The 4 remaining hand-written PyPat types (FunctionArg + 3 binary ops) — already documented as deliberately hand-written; not v3's job to migrate.
- The 6 pre-existing strider-py test failures — they predate this rewrite and aren't v3-introduced.
- Egg / Salsa-style incremental query systems for FUTURE features — if a future feature needs incremental analysis, evaluate then.
- The Phase 6.4 cold-path perf regression caused by Phase 8c's per-region cache scaffolding — V2 deletes salsa entirely; the perf profile returns to "v1-equivalent" which is the baseline goal. v3 doesn't claim ≥10× speedup.

## Execution Notes

**Recommended:** Subagent-driven development per `superpowers:subagent-driven-development`. Per-task dispatch with strict per-crate test verification. Hard 4-hour cap per subagent dispatch to prevent stalls.

**Per-phase rhythm:** Land each phase to `rewrite/strdier` (or a new `simplification/v3` branch). v3_baseline is the regression oracle at every commit. After each phase, request code review via `pr-review-toolkit:code-reviewer` against this plan + the v3_baseline contract.

**Failure recovery:** If v3_baseline regresses mid-phase, do NOT proceed. Use `superpowers:systematic-debugging` to isolate the divergence — bisect the commit, identify the failing assertion, fix the affected pass or revert. v3_baseline is the source of truth; every diff is a bug until proven a deliberate improvement.
