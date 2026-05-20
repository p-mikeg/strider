# Strider v3 Simplification Implementation Plan (Deeper)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax. Each theme is an independent merge boundary.

> **Replaces the earlier 2026-05-20 v3 plan** after the user pointed out the audit was too shallow. This version addresses ALL the user's named smells (test code in src, layer_a/b/c naming, dot/graphwalk/entity-utils in IR layer, FunctionBuilder+Graph+BuiltFunctionGraph triple, BuiltCC duplication, Phi duplication) PLUS deeper structural issues that the first audit missed (orchestrator sprawl, Cast variant sprawl, phase-leftover code, pattern-builder triples).

**Goal:** Aggressively simplify the codebase. Remove unjustified abstractions accumulated across v1 + v2. Target: ~10,000 LOC removed. Production behavior must be byte-identical to current state (v3_baseline is the contract).

**Architecture:** Same 8 crates but with cleaner boundaries. Test code moves OUT of src/. Generic utilities (dot, graphwalk, entity-utils) leave the IR layer. The orchestrator collapses from 6+ types to ~2. The Phi pair becomes one variant; the Cast trio becomes one; the BinaryOp triple's pattern-builder consolidates.

**Tech Stack:** Rust 2024. Drops `egg`, `salsa`. Keeps `pyo3 + maturin + abi3-py39`, `proptest`, `insta`, `rsleigh`, `pyo3-stub-gen`, `strider-pattern-macros`.

**Skills required throughout:**
- `superpowers:test-driven-development` — every refactor is test-anchored to v3_baseline.
- `superpowers:systematic-debugging` — when v3_baseline diverges, bisect immediately.
- `superpowers:requesting-code-review` — code-review subagent at every theme boundary.
- `superpowers:subagent-driven-development` — per-task dispatch with strict per-crate test verification (NEVER `cargo test --workspace`).
- `superpowers:verification-before-completion` — three mandatory checks (build + v3_baseline + targeted per-crate tests) at every commit.

---

## Aggressive Dead-Code Removal (apply to every theme)

Every theme actively removes dead code revealed by the change. Not just the items named in the audit — anything that becomes unreferenced as a side effect. Examples:
- After Theme B deletes the egg passes, types like `EGraphAdapter`, `StriderLang`, helper functions only used by egg passes — gone.
- After Theme C deletes Salsa, types like `BfgEntry`, `RegionKey`, `StriderDb` trait, the `unsafe impl salsa::Update` boilerplate — gone.
- After Theme G folds `BuiltFunctionGraph` into `Graph`, any `pub(crate)` methods that existed only to bridge the two — gone.
- After Theme H merges Phi variants, any helper like `is_var_phi` / `is_value_phi` — gone.
- After Theme I collapses orchestrator types, any builder/destructuring helpers — gone.

**Tools to apply at every theme boundary:**

```bash
cargo build --workspace 2>&1 | grep -E "warning: (unused|dead_code|never used)" | head -20
cargo machete 2>&1 | head -10            # finds unused deps
cargo +nightly udeps 2>&1 | head -10     # finds unused deps (alternative)
```

Final Theme K asserts zero `dead_code` / `unused_*` warnings. If a warning remains, the code it points at IS dead — delete it.

## Optimizer Architecture (peephole on top of the existing matcher)

The v3 optimizer rewrites the IR via peephole transformations. **Each peephole pass uses the existing `pattern` crate's `Matcher` infrastructure** — `Matcher::find_all(&pat)`, `match_at(node, &pat)`, `Capture`, commutative auto-matching, `.when(predicate)`, lift-time-canonical aliases. Passes do NOT roll their own matching logic per-pass.

**Why this matters for the plan:**
- Theme B deletes the 9 *Egg passes. The replacement is NOT "rewrite each in egg-style without egg" — it's "use the imperative passes that already exist". Those imperative passes' v1 incarnations already use ad-hoc matching; v3 cleans them up to consume the `pattern` crate's matcher uniformly.
- Theme J's optimizer-trait audit (Optimizer / OptimizerRaw / blanket impl / Box<dyn>) considers whether the pattern-matcher-based pass shape can be expressed via a simpler trait.
- The pattern crate stays — it's the load-bearing matcher for peepholes AND for user pattern queries (the public API). No churn.

A new peephole pass is structured:

```rust
pub struct ConstantFoldIdentityAdd;

impl Optimizer for ConstantFoldIdentityAdd {
    fn optimize(&self, ctx: &mut RewriteCtx<'_>) {
        let x = Capture::new();
        let pat = pattern::add(pattern::any().capture(x), pattern::int_const(0));
        for m in ctx.matcher().find_all(&pat) {
            let surviving = m.output(x).expect("any() always binds an output");
            ctx.graph().replace_all_uses(m.root(), surviving);
        }
    }
}
```

Multi-step rewrites (e.g. `(a + b) + c → a + (b + c)` for reassociation) use shared captures and `find_all_requirements(&[&p1, &p2])` for cross-pattern joins. Memory-chain rewrites (StackLoadForward) keep their imperative chain walks — the matcher covers value-DAG patterns, not memory threading.

## Naming Convention (mandatory)

**Code, doc comments, commit messages, file names, test names — none of them mention plan identifiers.** Never write "Theme G", "Phase V3", "Task H.3", "Step B.2", or audit-finding numbers like "F4 / F21" anywhere a future engineer reading the durable artifact would see them. The plan exists to organize the work; the work doesn't exist to advertise the plan.

The example commit messages in this plan use plain language that justifies the change on its own ("merge VarPhi and ValuePhi into Phi(Option<Vn>) — they had identical shape"), NOT plan-tagged labels ("VarPhi+ValuePhi merge — Theme H.3"). If a sample commit message in this plan accidentally includes a tag, replace it with the change's intrinsic description.

The plan's section headers (Theme A/B/C, Task X.Y) are workflow markers for the implementer to track progress — they STAY in this plan file. They DON'T leak into the working tree.

## Design Principles (Restated)

1. **YAGNI.** Every type, trait, and abstraction must earn its complexity by saving more code than it costs. The v2 rewrite added several layers that did not earn theirs (Salsa, egg, the v2-alongside-v1 dual codebase, three CC types where one would do).
2. **Test code lives in tests/.** Production `src/` is for production code. Test helpers move to `tests/common/`, dev-dep-feature-gated test crates, or to integration test bodies directly.
3. **Crate boundaries reflect responsibility.** `strider-ir` is the IR. Rendering (dot), generic graph algorithms (graphwalk), and arena helpers (entity-utils) are not IR concerns. They move out.
4. **Identifiers are readable.** `layer_a` / `layer_b` / `layer_c` become `local_typing` / `use_list_consistency` / `graph_invariants`. Other cryptic names get audited (Vn, Bfg, Cfg, IrStrider, etc.).
5. **One type per concept.** If two structs have the same fields, they're the same type. If two enum variants have the same shape, they're the same variant with optional payload. If two builder methods do the same thing with different operand widths, they're one method.
6. **Single optimizer, single orchestrator, single baseline.** v2 left v1 + v2 coexisting; v3 collapses them.

## v3_baseline as the regression oracle

Every commit in this plan must satisfy:
```bash
cargo build --workspace 2>&1 | tail -5         # exit 0
cargo test -p strider --test v3_baseline       # PASS (~220s)
```
plus per-crate sanity tests for the crate just touched.

**`v3_baseline` is created in Theme A by copying `v2_baseline.rs` and re-keying snapshots.** After Theme B retires PipelineV2, v3_baseline may need a ONE-TIME regeneration to capture the imperative-pipeline output for fixtures where the egg pipeline diverged. The regeneration is documented in Theme B; after that, v3_baseline is frozen.

## Open question: what is `crates/strider/` for?

The user surfaced this: `crates/strider/src/lib.rs` is a ~50-line re-export shim around `strider_analyze::*`. The crate has tests (`tests/v3_baseline.rs`, `tests/common/mod.rs`) and an example (`examples/strider.rs`) but no actual source code.

**Options:**
1. **Delete `crates/strider/` entirely.** Move `tests/v3_baseline.rs` + `tests/common/mod.rs` into `strider-analyze`. Move `examples/strider.rs` into `strider-analyze/examples/`. The crate disappears.
2. **Rename `strider-py` → `strider`** (per the v2 plan's deferred Phase 5.1). The Python bindings move into `crates/strider/`, giving the crate real source code. The current empty shim's tests + example also live there.
3. **Keep the shim**, treating it as a stable user-facing import path while internals live in strider-analyze.

**Recommendation: option 1.** It's the simplest. `strider-analyze` is the orchestrator crate; its example showing how to use the orchestrator and its integration tests verifying the orchestrator's behavior both belong with the orchestrator. The "strider" name is preserved as the Python package's distribution name (handled by `strider-py`'s `[lib].name = "strider"` configuration). The Rust crate-name "strider" goes away.

**Caveat:** anyone with `use strider::run` or `use strider::pattern::*` imports needs to migrate to `strider_analyze::run` / `strider_analyze::pattern::*`. The Python API surface (`import strider`) is unaffected.

This is added as Theme L below.

## Themes (13 themes, ~14 days)

| Theme | Scope | LOC removed | Days |
|---|---|---|---|
| A | Set up v3_baseline | 0 | 0.5 |
| B | Drop egg | ~3500 | 2 |
| C | Drop Salsa | ~1400 | 1 |
| D | Move test code out of src/ | ~600 | 1 |
| E | Eject dot + graphwalk + entity-utils from strider-ir; consolidate the 3 walkers | ~200 (split, not removed) | 1 |
| F | Rename non-descriptive identifiers | ~50 (mostly renames) | 0.5 |
| G | Collapse the FunctionBuilder + Graph + BuiltFunctionGraph triple | ~300 | 1.5 |
| H | Unify duplicate types (CC, Phi, Cast, BinaryOp pattern builders, NodeOutputType+Kind) | ~600 | 2 |
| I | Collapse orchestrator sprawl (Strider/IrStrider/RegionLiftHandles/AnalyzeOptions/AnalyzeOutcome/RunConfig) | ~700 | 1.5 |
| J | Drop v1+v2 dual code + leftover scaffolding + final cleanup | ~1400 | 2.5 |
| M | Scrub plan-identifier references from surviving code | ~0 (renames only) | 0.5 |
| L | Delete `crates/strider/` shim (move tests + example into strider-analyze) | ~50 | 0.5 |
| N | Crate-prefix consistency: rename `reader` → `strider-reader`, `target` → `strider-target` | ~0 (rename) | 0.5 |
| K | Final checkpoint + tag | 0 | 0.5 |
| **Total** | | **~8750 LOC** | **~14 days** |

---

## Theme A: v3_baseline Setup (0.5 day)

### Task A.1: Create v3_baseline.rs from v2_baseline.rs

**Files:**
- Create: `crates/strider/tests/v3_baseline.rs`
- Reuse: existing `crates/strider/tests/snapshots/v2_baseline__*.snap` → `v3_baseline__*.snap`

- [ ] **Step 1: Copy and rekey**

```bash
cp crates/strider/tests/v2_baseline.rs crates/strider/tests/v3_baseline.rs
sed -i 's/v2_baseline/v3_baseline/g; s/analyze_v2(/analyze(/g' crates/strider/tests/v3_baseline.rs

for f in crates/strider/tests/snapshots/v2_baseline__*.snap; do
    newname="${f/v2_baseline/v3_baseline}"
    cp "$f" "$newname"
done
```

- [ ] **Step 2: Verify both baselines pass**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
cargo test -p strider --test v2_baseline 2>&1 | tail -3
```
Both must PASS (~220s each).

- [ ] **Step 3: Commit + push**

```bash
git add crates/strider/tests/v3_baseline.rs crates/strider/tests/snapshots/v3_baseline__*.snap
git commit -m "test(strider): pin v3_baseline (Theme A — copy of v2 snapshots)"
git push
```

---

## Theme B: Drop Egg Infrastructure (~3500 LOC, 2 days)

### Task B.1: Flip `common::analyze` to the imperative pipeline (v1 path)

**Files:**
- Modify: `crates/strider/tests/common/mod.rs` — change `analyze` to call `analyze_v1`.

- [ ] **Step 1: Find the alias**

```bash
grep -n "pub fn analyze\b" crates/strider/tests/common/mod.rs
```

- [ ] **Step 2: Edit the alias**

```rust
// Before:
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> anyhow::Result<BuiltFunctionGraph> {
    analyze_v2(arch, case, fn_name)
}

// After:
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> anyhow::Result<BuiltFunctionGraph> {
    analyze_v1(arch, case, fn_name)
}
```

- [ ] **Step 3: Re-run v3_baseline**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

**If it FAILS:** the imperative pipeline produces different IR than PipelineV2 for some fixtures. This was anticipated (Phase 8a/8b had this issue). The diff is the documented egraph-aliasing-gap family.

Regenerate v3_baseline ONCE to capture the imperative output:
```bash
INSTA_UPDATE=always cargo test -p strider --test v3_baseline
```

Review the diff. The expected pattern: v1 keeps a few `Add` and `Neg` nodes that PipelineV2 over-canonicalised. Acceptable; the contract going forward is v1's shape.

- [ ] **Step 4: Commit + push**

```bash
git add crates/strider/tests/common/mod.rs crates/strider/tests/snapshots/v3_baseline__*.snap
git commit -m "refactor(strider/tests/common): analyze routes through v1 imperative pipeline

v3_baseline regenerated for fixtures where v1 and PipelineV2 differed
(the documented egraph-aliasing-gap family). v1's shape is the v3 contract."
git push
```

### Task B.2: Delete the 9 `*Egg` pass files + their parity tests + pipeline_v2

**Files to delete (verbatim list):**

```
crates/strider-analyze/src/opt/constant_fold_egg.rs
crates/strider-analyze/src/opt/known_bits_egg.rs
crates/strider-analyze/src/opt/flag_cmp_canonicalize_egg.rs
crates/strider-analyze/src/opt/if_cond_inversion_egg.rs
crates/strider-analyze/src/opt/stack_store_detect_egg.rs
crates/strider-analyze/src/opt/stack_load_forward_egg.rs
crates/strider-analyze/src/opt/load_readonly_egg.rs
crates/strider-analyze/src/opt/call_stack_arg_collect_egg.rs
crates/strider-analyze/src/opt/function_arg_detect_egg.rs
crates/strider-analyze/src/opt/pipeline_v2.rs
crates/strider-analyze/tests/constant_fold_egg_parity.rs
crates/strider-analyze/tests/known_bits_egg_parity.rs
crates/strider-analyze/tests/flag_cmp_egg_parity.rs
crates/strider-analyze/tests/if_cond_inversion_egg_parity.rs
crates/strider-analyze/tests/stack_store_egg_parity.rs
crates/strider-analyze/tests/stack_load_forward_egg_parity.rs
crates/strider-analyze/tests/load_readonly_egg_parity.rs
crates/strider-analyze/tests/call_stack_arg_collect_egg_parity.rs
crates/strider-analyze/tests/function_arg_detect_egg_parity.rs
```

- [ ] **Step 1: Verify no outside imports**

```bash
grep -rn "_egg\|PipelineV2\|pipeline_v2" crates/ --include="*.rs" | grep -v "crates/strider-analyze/src/opt/\|crates/strider-analyze/tests/"
```

Expected: empty.

- [ ] **Step 2: Delete**

```bash
git rm crates/strider-analyze/src/opt/{constant_fold,known_bits,flag_cmp_canonicalize,if_cond_inversion,stack_store_detect,stack_load_forward,load_readonly,call_stack_arg_collect,function_arg_detect}_egg.rs
git rm crates/strider-analyze/src/opt/pipeline_v2.rs
git rm crates/strider-analyze/tests/{constant_fold,known_bits,flag_cmp,if_cond_inversion,stack_store,stack_load_forward,load_readonly,call_stack_arg_collect,function_arg_detect}_egg*.rs
```

- [ ] **Step 3: Remove `pub mod *_egg` and `pub mod pipeline_v2` from `crates/strider-analyze/src/opt/mod.rs`**

- [ ] **Step 4: Build + test**

```bash
cargo build --workspace 2>&1 | tail -10
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```
All PASS.

- [ ] **Step 5: Commit + push**

```bash
git commit -m "feat(strider-analyze): delete 9 *Egg passes + pipeline_v2 + parity tests

~3000 LOC of source + ~1100 LOC of parity tests removed. The imperative
pipeline (v1) is now the only optimizer. v3_baseline locked at B.1."
git push
```

### Task B.3: Delete `egraph_adapter` + `egg` dep

**Files to delete:**
- `crates/strider-ir/src/egraph_adapter/` (3 files + mod.rs)
- `crates/strider-ir/tests/egraph_roundtrip.rs`

- [ ] **Step 1: Delete + remove deps**

```bash
git rm -r crates/strider-ir/src/egraph_adapter/
git rm crates/strider-ir/tests/egraph_roundtrip.rs
```

Edit `crates/strider-ir/src/lib.rs`: remove `pub mod egraph_adapter;`.
Edit `crates/strider-ir/Cargo.toml`: remove `egg = "0.10"`.

- [ ] **Step 2: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 3: Commit + push**

```bash
git commit -m "feat(strider-ir): delete egraph_adapter + drop egg dep

~830 LOC removed. egg = '0.10' gone from strider-ir's Cargo.toml."
git push
```

### Task B.4: Delete `v1_baseline.rs` + `v2_baseline.rs` + their snapshots

After B.1 + B.2 + B.3, the v1/v2 dual baselines are obsolete. `v3_baseline.rs` carries the v3 contract.

- [ ] **Step 1: Delete**

```bash
git rm crates/strider/tests/v1_baseline.rs
git rm crates/strider/tests/v2_baseline.rs
git rm crates/strider/tests/pipeline_v2_parity.rs
git rm crates/strider/tests/snapshots/v1_baseline__*.snap
git rm crates/strider/tests/snapshots/v2_baseline__*.snap
```

- [ ] **Step 2: Verify**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 3: Commit + push**

```bash
git commit -m "test(strider): retire v1_baseline + v2_baseline

v3_baseline is the single contract. ~4322 snapshot files + 3 test
sources deleted (~600 LOC + ~42 MB of snapshot data)."
git push
```

---

## Theme C: Drop Salsa Orchestrator (~1400 LOC, 1 day)

### Task C.1: Extract `BfgContentCache` from orchestrator_salsa.rs

**Files:**
- Create: `crates/strider-analyze/src/orchestrator/cache.rs`

- [ ] **Step 1: Failing-first test**

`crates/strider-analyze/tests/orchestrator_cache.rs`:
```rust
//! Theme C — content-keyed cache replacing salsa.
use std::sync::Arc;
use strider_analyze::orchestrator::cache::BfgContentCache;

#[test]
fn empty_cache_misses() {
    let c = BfgContentCache::new();
    assert!(c.get(0xdead_beef).is_none());
}

#[test]
fn cache_hits_after_insert() {
    let c = BfgContentCache::new();
    let bfg = strider_ir::test_helpers::TestGraph::new()
        .with_int_const(0, ir::NodeOutputType::U64)
        .build();
    c.insert(0xdead_beef, Arc::new(bfg));
    assert!(c.get(0xdead_beef).is_some());
}
```

Run; expect FAIL (`BfgContentCache` not defined).

- [ ] **Step 2: Implement**

`crates/strider-analyze/src/orchestrator/cache.rs`:
```rust
//! Content-keyed cache for BuiltFunctionGraph. v3 replacement for salsa.
//! See Theme C of the v3 plan.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ir::BuiltFunctionGraph;

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

Add `pub mod cache;` to `crates/strider-analyze/src/orchestrator/mod.rs`.

- [ ] **Step 3: Run, verify PASS**

```bash
cargo test -p strider-analyze --test orchestrator_cache 2>&1 | tail -5
```

- [ ] **Step 4: Commit + push**

```bash
git commit -m "feat(strider-analyze/orchestrator): extract BfgContentCache"
git push
```

### Task C.2: Switch strider-py off salsa (point at direct `orchestrator::run`)

**Files:**
- Modify: `crates/strider-py/src/run.rs` (or wherever the salsa wrapper is invoked)

- [ ] **Step 1: Find salsa call sites**

```bash
grep -n "run_v2\|orchestrator_salsa\|StriderDb" crates/strider-py/src/
```

- [ ] **Step 2: Replace `run_v2(db, key)` with direct `orchestrator::run(config)`**

The signature of the direct path is in `crates/strider-analyze/src/orchestrator.rs:169`. Pass the equivalent `RunConfig`.

- [ ] **Step 3: Verify**

```bash
cargo build -p strider-py 2>&1 | tail -5
cargo test -p strider-py --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

The 6 pre-existing strider-py failures remain expected; no new failures.

- [ ] **Step 4: Commit + push**

```bash
git commit -m "refactor(strider-py): use orchestrator::run directly"
git push
```

### Task C.3: Delete `orchestrator_salsa.rs` + 3 salsa test files + `salsa` dep

- [ ] **Step 1: Verify nothing imports**

```bash
grep -rn "orchestrator_salsa\|StriderDb\|StriderDbImpl\|run_v2\|BfgEntry" crates/ --include="*.rs"
```

Expected: empty (or only orchestrator_salsa.rs itself).

- [ ] **Step 2: Delete**

```bash
git rm crates/strider-analyze/src/orchestrator_salsa.rs
git rm crates/strider-analyze/tests/orchestrator_salsa_*.rs
```

- [ ] **Step 3: Remove `pub mod orchestrator_salsa;` from `crates/strider-analyze/src/lib.rs`**

- [ ] **Step 4: Remove `salsa = "0.26.2"` from `crates/strider-analyze/Cargo.toml`**

- [ ] **Step 5: Update bench**

`crates/strider/benches/v1_vs_v2.rs` references `run_v2`. Either delete (recommended — v1+v2 distinction is gone) or replace with direct `run`.

- [ ] **Step 6: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-analyze --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 7: Commit + push**

```bash
git commit -m "feat(strider-analyze): delete salsa orchestrator + salsa dep

~944 LOC of orchestrator_salsa.rs + 3 salsa test files (~600 LOC) +
the salsa = 0.26.2 workspace dep — all gone. Replaced by the
BfgContentCache from C.1 and direct calls to orchestrator::run."
git push
```

---

## Theme D: Move Test Code Out of src/ (~600 LOC, 1 day)

The user identified this as a critical structural smell. Test helpers in production `src/` files are gated by `#[cfg(any(feature = "test-utils", test))]`, but this is a workaround. The right home for test helpers is either:

- **`tests/common/`** of the same crate (when only that crate's integration tests need them).
- **A dedicated `strider-ir-test-utils` crate** when multiple downstream crates need them as dev-deps.

### Task D.1: Audit the test-code-in-src footprint

- [ ] **Step 1: List every test-only file in src/**

```bash
find crates/*/src -name "test_*.rs" -o -name "*_test.rs" -o -name "mock*.rs" -o -name "graphmock*.rs"
```

Expected output:
```
crates/strider-ir/src/test_helpers.rs       (TestGraph)
crates/strider-ir/src/test_utils.rs         (make_empty_fn, make_fn_with_var, SENTINEL_LIFT_ADDR)
crates/strider-ir/src/graphmock.rs          (mock graph for walker tests)
crates/strider/src/test_utils.rs            (test helpers for Strider construction)
crates/strider-analyze/src/opt/test_support.rs (optimizer test scaffold)
```

- [ ] **Step 2: Categorize each by consumer**

For each file, list which crates' tests use it:
```bash
for f in TestGraph make_empty_fn SENTINEL_LIFT_ADDR graphmock; do
    echo "=== $f ==="
    grep -rln "$f" crates/*/tests/ crates/*/src/ 2>/dev/null | head
done
```

Categorize:
- **Cross-crate dev-dep users:** TestGraph (used by strider-analyze tests, strider tests). Needs to be a public crate or feature-gated.
- **Single-crate users:** make_empty_fn (only strider-ir's own tests). Can move to `crates/strider-ir/tests/common/mod.rs`.
- **Single-crate users:** opt::test_support (only strider-analyze's own tests). Can move to `crates/strider-analyze/tests/common/mod.rs`.

### Task D.2: Move single-crate test helpers to `tests/common/`

**Files:**
- Move: `crates/strider-ir/src/test_utils.rs` → `crates/strider-ir/tests/common/test_utils.rs`
- Move: `crates/strider-ir/src/graphmock.rs` → `crates/strider-ir/tests/common/graphmock.rs`
- Move: `crates/strider/src/test_utils.rs` → `crates/strider/tests/common/test_utils.rs`
- Move: `crates/strider-analyze/src/opt/test_support.rs` → `crates/strider-analyze/tests/common/opt_test_support.rs`

For each move:

- [ ] **Step 1: Create the destination + copy contents**

```bash
mkdir -p crates/strider-ir/tests/common
git mv crates/strider-ir/src/test_utils.rs crates/strider-ir/tests/common/test_utils.rs
```

- [ ] **Step 2: Add `mod common;` to the crate's integration tests**

In `crates/strider-ir/tests/common/mod.rs` (new file):
```rust
pub mod test_utils;
pub mod graphmock;
```

In every existing `tests/*.rs` file that uses these helpers, add `mod common;` at the top.

- [ ] **Step 3: Remove from src/lib.rs**

In `crates/strider-ir/src/lib.rs`, remove:
```rust
#[cfg(any(feature = "test-utils", test))]
pub mod test_utils;
```

- [ ] **Step 4: Verify per-crate tests still pass**

```bash
cargo test -p strider-ir --no-fail-fast 2>&1 | grep "test result" | tail -10
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "refactor(strider-ir): move test_utils + graphmock to tests/common

~100 LOC of test scaffolding moves out of src/. The src/ tree now
contains only production code."
git push
```

Repeat for the other three files.

### Task D.3: Extract `TestGraph` to a dedicated `strider-ir-test-utils` crate

**Why a separate crate:** `TestGraph` is used by tests in *multiple* downstream crates (strider-analyze, strider). A `pub mod tests::common` in one crate isn't reachable from another crate's tests. The clean solution is a workspace dev-dep crate.

**Files:**
- Create: `crates/strider-ir-test-utils/Cargo.toml`
- Create: `crates/strider-ir-test-utils/src/lib.rs`
- Move: `crates/strider-ir/src/test_helpers.rs` → `crates/strider-ir-test-utils/src/lib.rs` (renamed module)
- Modify: `crates/strider-ir/Cargo.toml` — drop the `test-utils` feature.
- Modify: Every downstream crate's `Cargo.toml` — replace `strider-ir = { workspace = true, features = ["test-utils"] }` in dev-deps with `strider-ir-test-utils = { workspace = true }`.

- [ ] **Step 1: Create the new crate**

```bash
mkdir -p crates/strider-ir-test-utils/src
```

`crates/strider-ir-test-utils/Cargo.toml`:
```toml
[package]
name = "strider-ir-test-utils"
edition.workspace = true

[dependencies]
strider-ir = { workspace = true }
rsleigh.workspace = true

[lints]
workspace = true
```

`crates/strider-ir-test-utils/src/lib.rs`:
```rust
//! Test-support helpers for strider-ir. Used as a dev-dep by every
//! downstream crate that builds mock graphs in its integration tests.
//!
//! Was previously gated by the `test-utils` feature on strider-ir
//! itself, but production src/ should not contain test code.

// Paste contents of the old crates/strider-ir/src/test_helpers.rs here.
```

Add to workspace `Cargo.toml` `[workspace.dependencies]`:
```toml
strider-ir-test-utils = { path = "crates/strider-ir-test-utils" }
```

- [ ] **Step 2: Delete `test_helpers.rs` from strider-ir/src + drop the feature**

```bash
git rm crates/strider-ir/src/test_helpers.rs
```

In `crates/strider-ir/Cargo.toml`:
```toml
# Remove:
[features]
test-utils = []
```

In `crates/strider-ir/src/lib.rs`:
```rust
// Remove:
#[cfg(any(feature = "test-utils", test))]
pub mod test_helpers;
```

- [ ] **Step 3: Update every downstream crate's dev-deps**

```bash
grep -rn 'features = \["test-utils"\]' crates/*/Cargo.toml
```

For each match, replace:
```toml
# Before:
strider-ir = { workspace = true, features = ["test-utils"] }

# After:
strider-ir = { workspace = true }
strider-ir-test-utils = { workspace = true }
```

And in the test files themselves: `use strider_ir::test_helpers::TestGraph` → `use strider_ir_test_utils::TestGraph`.

- [ ] **Step 4: Build + verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 5: Commit + push**

```bash
git commit -m "feat(strider-ir-test-utils): extract TestGraph to a dedicated crate

strider-ir's src/ no longer contains test code. The 'test-utils'
feature is gone. Downstream tests dev-dep on strider-ir-test-utils
instead of feature-toggling strider-ir.

~300 LOC moves to its proper home; ~50 LOC of feature/cfg ceremony removed."
git push
```

---

## Theme E: Eject `dot` + `graphwalk` + `entity-utils` from strider-ir (~200 LOC reorganized, 1 day)

The user is right: these are generic utilities, not IR concerns. Absorbing them into `strider-ir` in Phase 1.2 was expedient but conceptually muddied the IR layer.

**Options per module:**
- **Make private** — `pub(crate) mod` instead of `pub mod`. External consumers stop reaching into the IR crate for renderers and walkers.
- **Re-extract as separate crates** — `strider-dot`, `strider-graphwalk`, `strider-entity-utils`. Restores the v1 layout.
- **Move into separate-but-justified homes** — `dot` could live in a `strider-viz` crate; `graphwalk` in `strider-graph-algos`.

The user's instinct is clear: get them OUT of the IR layer. The simplest path is **make them private** to strider-ir (just stop exporting them publicly). Code that genuinely needs DOT rendering or graph walking can use the IR's public API surface, not internal helpers.

### Task E.1: Make `dot` private to strider-ir

**Files:**
- Modify: `crates/strider-ir/src/lib.rs` — change `pub mod dot;` to `pub(crate) mod dot;`.

- [ ] **Step 1: Find every external `use strider_ir::dot::` import**

```bash
grep -rn "strider_ir::dot\|use strider_ir::dot" crates/ --include="*.rs" | grep -v "crates/strider-ir/"
```

- [ ] **Step 2: For each external user, decide:**

- If the consumer just renders a Graph to DOT — provide a `Graph::to_dot(&sleigh) -> String` method as the public API. The `dot` module stays internal.
- If the consumer uses lower-level dot primitives (DotStyle, DotEmitter) — those should also be private. The consumer should call `Graph::to_dot(&sleigh)` and get a string.

- [ ] **Step 3: Implement `Graph::to_dot(&self, sleigh: &Sleigh<R>) -> anyhow::Result<String>` as the public API**

This wraps the internal dot module's work in a single method. Replaces every existing `dot::GraphDot::new(...).as_dot()` invocation.

- [ ] **Step 4: Update consumers**

In `crates/strider/examples/strider.rs`, `crates/strider/tests/v3_baseline.rs`, etc., replace:
```rust
// Before:
let dot = dot::GraphDot::new(g.dot_dumper(&sleigh), dot::DotStyle::dark());
let s = dot.as_dot()?;

// After:
let s = g.to_dot(&sleigh)?;
```

- [ ] **Step 5: Privatize**

In `crates/strider-ir/src/lib.rs`:
```rust
// Before:
pub mod dot;

// After:
pub(crate) mod dot;
```

- [ ] **Step 6: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 7: Commit + push**

```bash
git commit -m "refactor(strider-ir): privatize dot module + add Graph::to_dot

dot is a rendering concern, not an IR concern. It stays private to
strider-ir; external consumers use Graph::to_dot(&sleigh) as the
public API. ~30 LOC removed from external API surface."
git push
```

### Task E.2: Make `graphwalk` private

Same pattern as E.1. Find external `use strider_ir::graphwalk::` imports. Either provide higher-level methods on `Graph` or leave the module private if no external use exists.

- [ ] Repeat the 7 steps from E.1 for `graphwalk`.

### Task E.3: Make `entity_utils` private

Same pattern. `entity_utils` is even more internal (cranelift-entity helpers); external use is unlikely.

- [ ] Repeat the 7 steps from E.1 for `entity_utils`.

### Task E.4: Document the boundary

Update `CLAUDE.md`'s strider-ir section to clarify: "strider-ir exposes only IR types, the validator, and `Graph::to_dot`. Internal modules (`dot`, `graphwalk`, `entity_utils`) are private."

- [ ] Commit + push.

---

## Theme F: Rename Non-Descriptive Identifiers (~50 LOC, 0.5 day)

### Task F.1: Rename `layer_a` / `layer_b` / `layer_c` to descriptive names

**Files:**
- Rename: `crates/strider-ir/src/validate/layer_a.rs` → `local_typing.rs`
- Rename: `crates/strider-ir/src/validate/layer_b.rs` → `use_list_consistency.rs`
- Rename: `crates/strider-ir/src/validate/layer_c.rs` → `graph_invariants.rs`

- [ ] **Step 1: Move + rename**

```bash
git mv crates/strider-ir/src/validate/layer_a.rs crates/strider-ir/src/validate/local_typing.rs
git mv crates/strider-ir/src/validate/layer_b.rs crates/strider-ir/src/validate/use_list_consistency.rs
git mv crates/strider-ir/src/validate/layer_c.rs crates/strider-ir/src/validate/graph_invariants.rs
```

- [ ] **Step 2: Update `validate/mod.rs`**

Replace `mod layer_a;` etc. with the new names.

- [ ] **Step 3: Rename the public fn names if they exist**

```bash
grep -rn "check_layer_a\|check_layer_b\|check_layer_c\|run_layer_a\|run_layer_b\|run_layer_c" crates/ --include="*.rs"
```

For each, rename:
- `check_layer_a` → `check_local_typing`
- `check_layer_b` → `check_use_list_consistency`
- `check_layer_c` → `check_graph_invariants` (or `check_asm_fingerprints` if that's its actual job)

- [ ] **Step 4: Update doc comments + CLAUDE.md**

Anywhere the v2 plan or comments say "Layer A / B / C", update to the descriptive names.

- [ ] **Step 5: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-ir --no-fail-fast 2>&1 | grep "test result" | tail -10
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 6: Commit + push**

```bash
git commit -m "refactor(strider-ir/validate): rename layer_{a,b,c} to descriptive names

local_typing / use_list_consistency / graph_invariants. The validator's
job is clearer in code. ~30 LOC of comments + identifier renames."
git push
```

### Task F.2: Survey + rename other cryptic identifiers

Likely cryptic names worth renaming (with justification):
- `Bfg` (in error messages, docs) — keep `BuiltFunctionGraph` everywhere; never abbreviate to `Bfg`.
- `Cfg` — keep `Cfg` (control-flow graph); it's a standard term, but document explicitly to disambiguate from cargo's `cfg!`.
- `Vn` — keep `Vn` (rsleigh's varnode type); standard in the domain.
- `Pat` — keep `Pat` (pattern); standard short.
- `IrStrider` vs `Strider` — both unclear. Rename `IrStrider` to `PerRegionDriver` (it's per-region driver state); rename `Strider` to `Orchestrator` (since `Strider` is the package name AND a struct name AND a Python class — confusion).

- [ ] **Step 1: Survey**

```bash
grep -rn "IrStrider\|Bfg\b\|pub struct Strider " crates/ --include="*.rs" | head -20
```

- [ ] **Step 2: Decide on renames**

Document each in the plan's Decisions section.

- [ ] **Step 3: Apply with `sed` per identifier**

```bash
grep -rln "IrStrider" crates/ --include="*.rs" | xargs sed -i 's/IrStrider/PerRegionDriver/g'
```

- [ ] **Step 4: Verify after each rename**

- [ ] **Step 5: Commit + push per rename**

---

## Theme G: Collapse `FunctionBuilder` + `Graph` + `BuiltFunctionGraph` Triple (~300 LOC, 1.5 days)

The triple exists because each represents a phase: builder is mutable, Graph is immutable arena, BuiltFunctionGraph is Graph + CC metadata. The user is right that this is too many types.

**v3 design choice:** Collapse to TWO types.
- `FunctionBuilder` stays (it does mutable SSA tracking; meaningfully different from the built phase).
- `Graph` and `BuiltFunctionGraph` merge into one type. The 5 CC metadata fields on BuiltFunctionGraph move INTO `Graph` as an optional analysis side-table; or BuiltFunctionGraph's wrapping shrinks to just the entry NodeId (since entry is the only real differentiator).

**Actually:** The simplest path — fold the BuiltFunctionGraph fields into Graph. Graph gains: `entry: Option<NodeId>`, `cc_metadata: Option<CcMetadata>`. After `FunctionBuilder::build()`, these are `Some(...)`. Before build, they're `None`.

`BuiltFunctionGraph` becomes a type alias or a thin newtype that asserts `entry.is_some()`.

### Task G.1: Move CC metadata fields into Graph

**Files:**
- Modify: `crates/strider-ir/src/function.rs` — fold fields.
- Modify: `crates/strider-ir/src/graph/mod.rs` — add `entry: Option<NodeId>` + CC fields.

- [ ] **Step 1: Read the current shape**

```bash
sed -n '50,110p' crates/strider-ir/src/function.rs
```

Identify the 5 CC fields + entry.

- [ ] **Step 2: Move fields into Graph**

```rust
// crates/strider-ir/src/graph/mod.rs

pub struct Graph {
    // ... existing fields ...

    /// Entry node of the function (`NodeKind::Entry`).
    /// Set by `FunctionBuilder::build()`; before then, None.
    pub(crate) entry: Option<NodeId>,

    /// Calling-convention metadata captured at build time.
    /// `None` before `build()`; `Some(...)` after.
    pub(crate) cc_metadata: Option<CcMetadata>,
}

#[derive(Clone, Debug)]
pub struct CcMetadata {
    pub call_clobbered: Vec<rsleigh::Vn>,
    pub callee_saved: Vec<rsleigh::Vn>,
    pub ret_val_regs: Vec<rsleigh::Vn>,
    pub ret_val_regs_float: Vec<rsleigh::Vn>,
    pub no_memory_clobber: bool,
}
```

- [ ] **Step 3: Reduce BuiltFunctionGraph to a thin wrapper / type alias**

```rust
// crates/strider-ir/src/function.rs

/// A `Graph` that has been finalized via `FunctionBuilder::build()`.
/// Guarantees that `entry.is_some()` and `cc_metadata.is_some()`.
#[derive(Clone, Debug)]
pub struct BuiltFunctionGraph {
    inner: Graph,
}

impl BuiltFunctionGraph {
    pub(crate) fn new(graph: Graph) -> Self {
        debug_assert!(graph.entry.is_some(), "BuiltFunctionGraph requires entry");
        debug_assert!(graph.cc_metadata.is_some(), "BuiltFunctionGraph requires cc_metadata");
        Self { inner: graph }
    }

    pub fn entry(&self) -> NodeId {
        self.inner.entry.expect("BuiltFunctionGraph invariant")
    }

    pub fn graph(&self) -> &Graph { &self.inner }
    pub fn graph_mut(&mut self) -> &mut Graph { &mut self.inner }

    pub fn cc_metadata(&self) -> &CcMetadata {
        self.inner.cc_metadata.as_ref().expect("BuiltFunctionGraph invariant")
    }
}
```

- [ ] **Step 4: Update FunctionBuilder::build()**

The build method sets `graph.entry = Some(entry_node)` and `graph.cc_metadata = Some(cc_meta)`, then wraps in `BuiltFunctionGraph::new(graph)`.

- [ ] **Step 5: Verify v3_baseline**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 6: Commit + push**

```bash
git commit -m "refactor(strider-ir): fold BuiltFunctionGraph fields into Graph

The CC metadata + entry NodeId now live on Graph as Option<...> fields.
BuiltFunctionGraph is a thin wrapper asserting Some(...). Eliminates
the duplicate storage that justified the three-type triple.

~150 LOC removed from BuiltFunctionGraph's surface."
git push
```

### Task G.2: Decide on the eventual fate of BuiltFunctionGraph

After G.1, `BuiltFunctionGraph` is a 30-line wrapper. Options:
- **Keep it** as a type-level guarantee that entry + cc_metadata are present.
- **Delete it** in favor of `Graph` directly; callers check `entry.is_some()` defensively.

Recommendation: **keep it**. The type-level guarantee saves every consumer from `expect(...)` calls. The wrapper costs ~30 LOC; the safety win is significant.

- [ ] Document in CLAUDE.md.

---

## Theme H: Unify Duplicate Types (~600 LOC, 2 days)

This theme absorbs the user's specific examples (BuiltCC + Parts + FunctionBuilderCC, VarPhi + ValuePhi) plus more.

### Task H.1: Delete `BuiltCallingConventionParts`

(As per the earlier audit's F4.)

- [ ] **Step 1: Find usages**

```bash
grep -rn "BuiltCallingConventionParts" crates/
```

- [ ] **Step 2: Inline at call sites**

`try_from_parts` becomes `try_new(field1, field2, ...)`. Or take the rich type directly.

- [ ] **Step 3: Delete the struct**

- [ ] **Step 4: Verify + commit + push**

### Task H.2: Decide on `FunctionBuilderCC`

(As per the earlier audit's F4 + the dep-direction tradeoff.)

Recommendation: **keep `FunctionBuilderCC`**. It serves the V6 layer-order architectural goal. Document the deliberate design.

- [ ] Update doc comments. Commit + push.

### Task H.3: Merge `VarPhi` + `ValuePhi` → `Phi(Option<Vn>)`

(As per the earlier plan's V4.1.)

- [ ] **Step 1: Edit the enum**

```rust
// Before:
pub enum NodeKind {
    VarPhi(rsleigh::Vn),
    ValuePhi,
    // ...
}

// After:
pub enum NodeKind {
    Phi(Option<rsleigh::Vn>),
    // ...
}
```

- [ ] **Step 2: Update every match arm**

```bash
grep -rln "NodeKind::VarPhi\|NodeKind::ValuePhi" crates/ --include="*.rs"
```

For each file, replace:
```rust
// Before:
NodeKind::VarPhi(vn) => { /* arm using vn */ }
NodeKind::ValuePhi => { /* arm without vn */ }

// After:
NodeKind::Phi(Some(vn)) => { /* arm using vn */ }
NodeKind::Phi(None) => { /* arm without vn */ }
```

OR (often cleaner):
```rust
NodeKind::Phi(vn_opt) => {
    // Common code; check vn_opt.is_some() if needed.
}
```

- [ ] **Step 3: Update pattern crate**

The macro emits `PyPhiPat` with a `for_vn(vn)` setter. Keep that interface; the internal mapping just changes.

- [ ] **Step 4: Update validator exemption list**

`Phi` replaces both `VarPhi` and `ValuePhi` in the fingerprint-exempt list.

- [ ] **Step 5: Update graph_dot renderer labels**

`"VarPhi(rax)"` → `"Phi(rax)"`; `"ValuePhi"` → `"Phi"`. v3_baseline will diverge on these labels.

- [ ] **Step 6: Regenerate v3_baseline**

```bash
INSTA_UPDATE=always cargo test -p strider --test v3_baseline
```

Review the diff — ONLY label changes should appear. If anything else changes, you've semantically broken a pass.

- [ ] **Step 7: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 8: Commit + push**

```bash
git commit -m "refactor(strider-ir): merge VarPhi + ValuePhi → Phi(Option<Vn>)

The two variants had identical structural roles. The optional Vn payload
distinguishes them in the few sites that care. ~60 LOC removed."
git push
```

### Task H.4: Unify Cast trio (`CastToInt` + `CastToBool` + `CastToFloat`)

(Per audit F12.)

Three variants for "one-input cast to target type". Could be one `Cast(target_type)` variant with `target_type: NodeOutputType`.

- [ ] **Step 1: Replace the enum variants**

```rust
// Before:
NodeKind::CastToInt,
NodeKind::CastToBool,
NodeKind::CastToFloat,

// After:
NodeKind::Cast,   // target_type is encoded in the node's output type
```

OR (more explicit):
```rust
NodeKind::Cast(CastTarget),

pub enum CastTarget {
    Bool,
    Int(IntWidth),
    Float(FloatWidth),
}
```

- [ ] **Step 2: Update every match arm**

- [ ] **Step 3: Lift-time-lower `CastToFloat` (per the earlier audit's F12)**

This step subsumes the earlier audit's separate finding. After the merge, CastToFloat is just `Cast` with a float target; pcode-lift can directly emit `IntBitsToFloat` / `FloatToFloat` / identity without going through a generic Cast.

- [ ] **Step 4: Verify v3_baseline (regenerate if label changes only)**

- [ ] **Step 5: Commit + push**

### Task H.5: Collapse the three `BinaryOpPat` builders → one

(Per audit F11.)

- [ ] **Step 1: Verify the three are structurally identical**

```bash
grep -A 30 "pub struct.*BinaryOpPat" crates/strider-py/src/pattern.rs
```

- [ ] **Step 2: Implement a single `BinaryOpPat` in `strider-pattern-macros`**

The macro emits one type with an `AnyBinaryOp` union payload.

- [ ] **Step 3: Migrate Python consumers**

```python
# Before:
pat.int_binary(IntBinaryOp.Add, lhs, rhs)
pat.bool_binary(BoolBinaryOp.And, lhs, rhs)

# After:
pat.binary_op(BinaryOp.Add, lhs, rhs)
pat.binary_op(BinaryOp.And, lhs, rhs)
```

- [ ] **Step 4: Verify + commit + push**

### Task H.6: Consider `NodeOutputType` + `NodeOutputKind` consolidation

Currently:
```rust
enum NodeOutputKind {
    Control,
    Memory,
    PhiToken,
    OutputType(NodeOutputType),
}

enum NodeOutputType {
    Bool, U8, U16, U32, U64, U80, U128, U256, U512, F32, F64, F80,
}
```

The wrapping is awkward but the distinction (data-typed vs control/memory) is real and load-bearing for the validator.

**Recommendation: keep as-is.** The wrapping IS the meaningful semantic boundary; flattening loses it.

- [ ] Document the choice in CLAUDE.md.

---

## Theme I: Collapse Orchestrator Sprawl (~700 LOC, 1.5 days)

The grep confirmed: 6 orchestrator-adjacent types + 4 entry points. v3 collapses to ~2 types + 1 entry point.

**Current state:**
- `strider::run(config)` — free function entry
- `Strider::new(arch, sleigh, cc)` — per-iteration state
- `Strider::run` (method, not the free fn) — runs the fixed-point loop
- `IrStrider` — per-region driver context
- `AnalyzeOptions` — input config bag
- `AnalyzeOutcome` — output bundle
- `RegionLiftHandles` — per-iteration snapshot
- `RegionIndex` — orchestrator's per-iteration index
- `RunConfig` — yet another input bag
- `OptimizerPipeline::run` vs `run_on_built` — two methods on Pipeline

**Target:**
- `orchestrator::Config` — input bag (replaces RunConfig, AnalyzeOptions)
- `orchestrator::run(config) -> Result<BuiltFunctionGraph>` — single entry point
- `orchestrator::Result` — output bundle if needed (replaces AnalyzeOutcome)
- `PerRegionDriver` (renamed from IrStrider) — per-region driver state, only used internally
- `OptimizerPipeline::run(graph, entry)` — one method

### Task I.1: Audit + design

- [ ] **Step 1: Catalog every orchestrator type's fields + responsibilities**

For each of the 6 types, write down:
- Fields
- Responsibilities (1-2 sentences)
- Consumers

- [ ] **Step 2: Identify field overlap**

E.g., `RunConfig` and `AnalyzeOptions` might both have `arch` + `cc` + `rom` fields. Merge.

- [ ] **Step 3: Design the unified shape**

```rust
// crates/strider-analyze/src/orchestrator/mod.rs

pub struct Config<'a, R: rsleigh::MemReader> {
    pub strider: &'a Strider,
    pub start_addr: u64,
    pub sleigh: &'a rsleigh::Sleigh<R>,
    pub rom: Option<&'a dyn ReadOnlyMemory>,
    pub fn_max_size: Option<usize>,
    pub allow_code_before_start_addr: bool,
    pub compact: bool,
    pub per_address_ccs: BTreeMap<u64, BuiltCallingConvention>,
}

pub fn run<R: rsleigh::MemReader>(config: Config<'_, R>) -> Result<BuiltFunctionGraph> {
    // The single entry point. Internal state lives in PerRegionDriver
    // (renamed from IrStrider), which is no longer pub.
}
```

`AnalyzeOptions` + `AnalyzeOutcome` + `RunConfig` collapse to `Config` + `BuiltFunctionGraph`.

### Task I.2: Implement the unified orchestrator API

- [ ] **Step 1: Add `Config` struct**

- [ ] **Step 2: Make `IrStrider` `pub(crate)` and rename to `PerRegionDriver`**

- [ ] **Step 3: Delete `AnalyzeOptions`, `AnalyzeOutcome`, `RunConfig`**

Update every caller.

- [ ] **Step 4: Verify + commit + push**

### Task I.3: Unify `OptimizerPipeline::run` + `run_on_built`

- [ ] **Step 1: Read both methods**

```bash
grep -A 30 "pub fn run\b\|pub fn run_on_built" crates/strider-analyze/src/opt/pipeline.rs
```

- [ ] **Step 2: Determine if they differ in semantics**

If they just differ in input type (`Graph` vs `BuiltFunctionGraph`), make one method take `&mut Graph` and wrap the BuiltFunctionGraph call to unwrap-then-pass.

- [ ] **Step 3: Delete the duplicate. Verify. Commit + push.**

---

## Theme J: Drop v1+v2 Dual Code + Phase-Leftover Code + Final Cleanup (~1400 LOC, 2.5 days)

### Task J.1: Collapse test helpers — drop `analyze_v1` + `analyze_v2` + `analyze_v2_with_iters`

(Per audit F19.)

After Theme B + Theme C retired the dual codebase, the v1/v2 distinction is gone. Keep one `analyze`.

- [ ] **Step 1: Find usages**

```bash
grep -rn "analyze_v1\|analyze_v2\|analyze_v2_with_iters" crates/ --include="*.rs"
```

- [ ] **Step 2: Replace each with `analyze`**

- [ ] **Step 3: Delete the three helpers**

- [ ] **Step 4: Verify + commit + push**

### Task J.2: Delete phase-leftover code

**Files:**
- `crates/strider-py/src/pattern_reference.rs` (Phase 4.0 hand-written reference)
- `crates/strider-py/src/pattern_macro_v2.rs` (Phase 4.1 byte-identity validator)

These existed for Phase 4 byte-identity verification. After Phase 4 closed, they're just clutter.

- [ ] **Step 1: Check whether the byte-identity test still uses them**

```bash
grep -n "pattern_reference\|pattern_macro_v2\|PyStackStorePatV2\|PyStackStorePatV3" crates/strider-py/tests/
```

If the test (`test_macro_emission.py`) still compares the two: keep one (the V3 / macro-emitted one) and delete the V2 hand-written reference. The test's value-prop was Phase 4 transition validation; the macro is proven now.

- [ ] **Step 2: Delete**

```bash
git rm crates/strider-py/src/pattern_reference.rs
git rm crates/strider-py/src/pattern_macro_v2.rs
git rm crates/strider-py/tests/python/test_macro_emission.py
```

(If the test is genuinely useful as ongoing protection: keep it but simplify to compare against a hand-stored .pyi golden.)

- [ ] **Step 3: Update `crates/strider-py/src/lib.rs`** to drop the modules.

- [ ] **Step 4: Verify + commit + push**

### Task J.3: Delete `ValidateOptions` + `validate_with_options`

(Per audit F7.)

- [ ] Per the steps in the earlier audit.

### Task J.4: Delete `strider-lift::target` re-export shim

(Per audit F8.)

- [ ] Per the steps in the earlier audit.

### Task J.5: Audit remaining `OptimizerRaw` + `Optimizer` + blanket impl trickery

```bash
grep -n "OptimizerRaw\|trait Optimizer\|impl<T: Optimizer> OptimizerRaw" crates/strider-analyze/src/opt/
```

The blanket impl + `Box<dyn OptimizerRaw>` pattern is heavy. If there are <10 concrete optimizers, an `enum Pass { ConstantFold, KnownBits, ... }` is simpler.

- [ ] **Step 1: Count optimizers**

- [ ] **Step 2: Decide**

If trait dispatch genuinely buys ergonomics (external passes, dyn-dispatch for plugins): keep. If just internal, fixed set: enum.

- [ ] **Step 3: If enum: refactor**

- [ ] **Step 4: Verify + commit + push**

### Task J.6: Audit and consolidate consolidated CC presets

```bash
grep -c "^pub fn\b" crates/target/src/calling_convention/mod.rs
```

12+ preset constructors. Many share structure (kernel/syscall variants differ from base in 1-3 fields). A `CallingConventionBuilder` (per Generalization Audit G5 in the v2 plan) was suggested but never implemented.

- [ ] **Step 1: Introduce `CallingConventionBuilder`** with `with_*` methods.

- [ ] **Step 2: Rewrite each derived preset** to start from a base + `.with_*(...)` calls.

- [ ] **Step 3: Verify v3_baseline + commit + push**

### Task J.7: Drop more dead code surfaced by previous phases

- [ ] Walk through `docs/superpowers/specs/2026-05-20-v3-audit.md` and check each finding F1-F21 has been addressed (or documented as deliberately skipped).

- [ ] Search for `"deletion-pending"`, `"Phase X follow-up"`, `"TODO"`, `"FIXME"` comments — clean up anything that's a real follow-up vs anything that's leftover noise.

- [ ] **Step 1-N: Per leftover, decide + clean + commit.**

---

## Theme L: Delete `crates/strider/` shim (~50 LOC + restructuring, 0.5 day)

`crates/strider/src/lib.rs` is a ~50-line re-export of `strider_analyze::*`. The crate exists primarily as a home for `tests/` and `examples/strider.rs`. Once the orchestrator lives in `strider-analyze`, those artifacts belong WITH the orchestrator, not in a satellite crate.

### Move integration tests

**Files:**
- Move: `crates/strider/tests/v3_baseline.rs` → `crates/strider-analyze/tests/v3_baseline.rs`
- Move: `crates/strider/tests/common/` → `crates/strider-analyze/tests/common/`
- Move: `crates/strider/tests/snapshots/v3_baseline__*.snap` → `crates/strider-analyze/tests/snapshots/`
- Move: any other v3-era integration test files

- [ ] **Step 1: Move files**

```bash
git mv crates/strider/tests/v3_baseline.rs crates/strider-analyze/tests/v3_baseline.rs
git mv crates/strider/tests/common crates/strider-analyze/tests/common
mkdir -p crates/strider-analyze/tests/snapshots
git mv crates/strider/tests/snapshots/v3_baseline__*.snap crates/strider-analyze/tests/snapshots/
```

- [ ] **Step 2: Update imports inside the moved files**

`use strider::X` → `use strider_analyze::X`. Should be mostly mechanical; the orchestrator types are accessible from both today via re-exports.

- [ ] **Step 3: Verify the moved baseline passes**

```bash
cargo test -p strider-analyze --test v3_baseline 2>&1 | tail -3
```

PASS (~220s).

- [ ] **Step 4: Commit + push**

```bash
git add crates/strider-analyze/
git commit -m "move v3 baseline + test helpers into strider-analyze"
git push
```

### Move the example

- [ ] **Step 5: Move**

```bash
git mv crates/strider/examples/strider.rs crates/strider-analyze/examples/orchestrator_demo.rs
```

(Renaming as part of the move — `strider.rs` was ambiguous; `orchestrator_demo.rs` describes what it is.)

- [ ] **Step 6: Update imports inside the example**

- [ ] **Step 7: Verify it runs**

```bash
cargo run -p strider-analyze --example orchestrator_demo
```

- [ ] **Step 8: Commit + push**

### Delete the shim crate

- [ ] **Step 9: Find any external `use strider::` import**

```bash
grep -rn "use strider::\|extern crate strider" crates/ --include="*.rs" | grep -v "crates/strider/\|crates/strider-py/\|crates/strider-analyze/"
```

For each match, update to `use strider_analyze::*`.

- [ ] **Step 10: Delete the crate**

```bash
git rm -r crates/strider/
```

Remove from workspace `Cargo.toml` `[workspace.dependencies]` and `[workspace] members` if listed.

- [ ] **Step 11: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider-analyze --test v3_baseline 2>&1 | tail -3
```

- [ ] **Step 12: Commit + push**

```bash
git add Cargo.toml
git rm -r crates/strider/
git commit -m "delete the strider crate (was a re-export shim of strider_analyze)"
git push
```

After this, `crates/` contains: `reader`, `strider-analyze`, `strider-ir`, `strider-ir-test-utils`, `strider-lift`, `strider-pattern-macros`, `strider-py`, `target`. 8 crates. The "strider" name persists only as the Python package's distribution name (configured in `strider-py/Cargo.toml`).

## Theme N: Crate-Prefix Consistency (0.5 day)

User-surfaced. Current workspace mixes prefix conventions:

**Prefixed** (5 crates): `strider-analyze`, `strider-ir`, `strider-lift`, `strider-pattern-macros`, `strider-py`.
**Unprefixed** (3 crates): `reader`, `strider`, `target`.

After Theme L deletes `crates/strider/`, the remaining inconsistency is `reader` and `target` (and `strider-ir-test-utils` if Theme D created it).

**Recommended:** every project crate gets the `strider-` prefix. Rename `reader` → `strider-reader` (matches the v2 plan's deferred rename — `binary` reflects "binary loader" better than `reader`), and `target` → `strider-target`.

The Python package name (`strider`) is unaffected — it's set in `strider-py`'s maturin config and is independent of Rust crate names.

### Task N.1: Rename `reader` → `strider-reader`

- [ ] **Step 1: Move the crate**

```bash
git mv crates/reader crates/strider-reader
```

- [ ] **Step 2: Update `crates/strider-reader/Cargo.toml`**

```toml
[package]
name = "strider-reader"
# ...
```

- [ ] **Step 3: Update workspace `Cargo.toml`**

In `[workspace.dependencies]`:
```toml
# Before:
reader = { path = "crates/reader" }

# After:
strider-reader = { path = "crates/strider-reader" }
```

- [ ] **Step 4: Update every downstream `Cargo.toml`**

```bash
grep -rln "^reader = \|reader.workspace" crates/*/Cargo.toml
```

For each: `reader.workspace = true` → `strider-reader.workspace = true`.

- [ ] **Step 5: Update every `use reader::` import**

```bash
grep -rln "use reader::\|reader::" crates/ --include="*.rs"
```

For each: `use reader::X` → `use strider_reader::X` (note underscore: cargo crate names use hyphens; Rust identifiers use underscores).

- [ ] **Step 6: Verify**

```bash
cargo build --workspace 2>&1 | tail -5
cargo test -p strider --test v3_baseline 2>&1 | tail -3   # before Theme L moves baseline
```

- [ ] **Step 7: Commit + push**

```bash
git commit -m "rename reader crate to strider-reader

Every project crate now carries the strider- prefix. The python package
distribution name (strider) is unaffected — it's configured in strider-py's
maturin metadata."
git push
```

### Task N.2: Rename `target` → `strider-target`

Same pattern as N.1.

- [ ] **Step 1: Move + rename Cargo.toml + update workspace + update downstreams + update imports**

Note: `target` is a tricky name to grep for (collides with Rust's `target/` build directory, the `target_arch` cfg, etc.). Use anchored grep:

```bash
grep -rEn '^use target::\|target::\b|^target = \{' crates/ --include="*.rs" --include="*.toml"
```

- [ ] **Step 2: For each match, edit to `strider_target` (Rust) or `strider-target` (Cargo.toml).**

- [ ] **Step 3: Verify**

- [ ] **Step 4: Commit + push**

```bash
git commit -m "rename target crate to strider-target

Avoids collision with widely-used 'target' name and the Rust ecosystem's
'target/' build directory. All project crates now share the strider-
prefix."
git push
```

### Task N.3: Sanity-check final crate layout

After N.1 + N.2 + Theme L (which deletes `crates/strider/`), the workspace contains:

- `strider-analyze`
- `strider-reader` (renamed from reader)
- `strider-ir`
- `strider-ir-test-utils` (created in Theme D)
- `strider-lift`
- `strider-pattern-macros`
- `strider-py`
- `strider-target` (renamed from target)

8 crates, all prefixed. Plus the external path-dep `rsleigh` (lives at `../rsleigh`, not part of this workspace).

- [ ] Update CLAUDE.md's crate listing.

## Theme K: Final Checkpoint + Tag (0.5 day)

### Task K.1: Full verification

- [ ] **Step 1: Per-crate**

```bash
for c in strider-ir strider-ir-test-utils strider-lift strider-analyze strider-py strider target reader strider-pattern-macros; do
    echo "=== $c ==="
    cargo test -p "$c" --no-fail-fast 2>&1 | grep "test result" | tail -3
done
```

- [ ] **Step 2: Baseline**

```bash
cargo test -p strider --test v3_baseline 2>&1 | tail -3
```

PASS (~220s).

### Task K.2: Dispatch the code-reviewer subagent

```bash
# Manually launch a subagent dispatch with subagent_type: pr-review-toolkit:code-reviewer
# Scope: review the cumulative v3 diff (v2-final..HEAD) as a coherent simplification PR.
```

### Task K.3: Update CLAUDE.md + final tag

- [ ] **Step 1: Rewrite the v2-era sections**

CLAUDE.md still references "9 v2 egg-based passes", "Salsa orchestrator", "PipelineV2", "v1_baseline + v2_baseline", "layer_a/b/c", etc. All become v3 language.

- [ ] **Step 2: Tag**

```bash
git tag -a v3-final -m "strider v3 simplification: drop salsa + egg, move test code out of src,
eject dot/graphwalk/entity-utils from IR layer, fold BuiltFunctionGraph into Graph,
collapse orchestrator sprawl, rename layer_a/b/c, unify duplicate types.

~8500-10000 LOC removed. Single optimizer, single baseline, single
analyze() helper. Cleaner crate boundaries (IR is just IR), readable
identifiers, fewer types per concept."
git push origin v3-final
```

### Task K.4: Update PR body

Append a "V3 simplification" section to the existing PR body.

---

## Decisions Appendix

| Theme | Decision | Rationale |
|---|---|---|
| A | v3_baseline = copy of v2 snapshots | Cheap to set up; allows independent contract anchoring. |
| B.1 | If imperative pipeline diverges from PipelineV2, regenerate v3_baseline ONCE | The 2 surfaced flip-blockers (Phase 8a, 8b) are documented pre-existing v1 issues. |
| D.3 | TestGraph moves to dedicated `strider-ir-test-utils` crate | Multiple downstream crates need it; tests/common/ in strider-ir isn't reachable cross-crate. |
| E | dot/graphwalk/entity-utils become `pub(crate)` (not re-extracted as separate crates) | Privatization is simpler than re-extraction; restores conceptual boundary without churn. |
| F.1 | Rename layer_a/b/c → local_typing/use_list_consistency/graph_invariants | User-readable. |
| F.2 | Rename `IrStrider` → `PerRegionDriver`; keep `Strider` (clarify it's the orchestrator) | Reduces struct/package/Python class name collision. |
| G | Fold BuiltFunctionGraph fields into Graph; BuiltFunctionGraph becomes thin wrapper | Triple → pair. Type-level guarantee preserved. |
| H.2 | Keep `FunctionBuilderCC` despite the duplication | V6 dep-direction architectural goal worth 50 LOC. |
| H.6 | Keep `NodeOutputType` + `NodeOutputKind` split | Wrapping IS the semantic boundary; flattening loses it. |
| J.5 | Decide `Optimizer`/`OptimizerRaw` trait vs enum based on optimizer count | <10 → enum; ≥10 or plugin support → keep traits. |

## Risk Register

| Risk | Mitigation |
|---|---|
| Theme B.1 (`analyze` flip) breaks v3_baseline on multiple fixtures | Regenerate once. Document fixture-specific diffs. |
| Theme D.3 dev-dep restructure breaks downstream test compilation | Each migration is per-crate; rollback by reverting one Cargo.toml. |
| Theme E privatization breaks an undocumented external consumer | grep + compile errors surface all of them. |
| Theme G fold breaks an external API consumer of BuiltFunctionGraph fields | The wrapper preserves all public methods; only field access patterns change. |
| Theme I orchestrator consolidation breaks strider-py | Theme C already migrated strider-py to direct `orchestrator::run`; further consolidation should affect only internal call paths. |
| Theme F renames produce a huge sed-diff that's hard to review | Commit one rename per commit. Each is independently revertable. |

## Out of Scope

- New features (V3 is pure simplification).
- The 4 remaining hand-written PyPat types (FunctionArg enum-dispatch + 3 binary-op required-construction). Already documented as deliberately hand-written.
- The 6 pre-existing strider-py test failures.
- Performance optimization beyond what theme C achieves (BfgContentCache restores Phase 8c's repeat-query 6.5× win).
- `IntConstWide` / `WideConstId` unification (audit F17 — left alone; unification touches too many sites for too small a win).
- `NodeOutputType` + `NodeOutputKind` flattening (semantically distinct boundaries; H.6 keeps them).

## Execution Notes

**Recommended:** Subagent-driven development per `superpowers:subagent-driven-development`. Per-theme dispatches with strict per-crate test verification. Hard 4-hour cap per subagent dispatch to prevent stalls.

**Per-theme rhythm:** Land each theme to `rewrite/strdier` (or a new `simplification/v3` branch). v3_baseline is the regression oracle at every commit. After each theme, request code review via `pr-review-toolkit:code-reviewer` against this plan + the v3_baseline contract.

**Failure recovery:** If v3_baseline regresses mid-theme, do NOT proceed. Use `superpowers:systematic-debugging` to isolate the divergence — bisect the commit, identify the failing pass or rename, fix or revert. v3_baseline is the source of truth; every diff is a bug until proven a deliberate improvement.

**Total commits expected:** 60-80 commits across 14 days. Push every commit immediately per the user's `feedback-push-each-step` preference.
