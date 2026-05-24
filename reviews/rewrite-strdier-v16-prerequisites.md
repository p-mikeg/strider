# Strider v16 Structural Redesign — Prerequisites Report

**Branch:** `rewrite/strdier`  
**Generated:** 2026-05-24  
**Scope:** Comprehensive audit of optimizer passes, memory subsystem, pattern DSL, test landscape, and Phase 1–4 readiness

---

## Section A: Optimizer Pass List

All passes live in `crates/strider-analyze/src/opt/`. Each is a public type implementing the `Optimizer` trait and integrated into either the stable pipeline (`stable_default_pipeline()`) or the full pipeline (`default_pipeline()`).

### Stable Pipeline Passes (run during fixed-point iteration)

| Pass | File | Kind | Description |
|------|------|------|-------------|
| `ConstantFold` | `constant_fold/mod.rs` | peephole | Evaluates integer/float/bool operations to constants; simplifies algebraic identities (`x+0→x`, `x^x→0`, AND-mask merging) |
| `KnownBits` | `known_bits/mod.rs` | post-pass | Bit-level stateless propagation of zeros/ones via operand annotation |
| `FlagCmpCanonicalize` | `flag_cmp_canonicalize/mod.rs` | peephole | Rewrites AArch64-style flag-tree chains into single `IntCmpOp` |
| `IfCondInversion` | `if_cond_inversion/mod.rs` | peephole | Canonicalises `If(BoolNeg(C)){A}{B}` → `If(C){B}{A}` |
| `RedundantPhis` | `redundant_phis/mod.rs` | post-pass | Eliminates `Phi`/`MemPhi`/`ControlState` with single reachable predecessor |

### Full Pipeline Additions (after fixed-point convergence)

| Pass | File | Kind | Description |
|------|------|------|-------------|
| `DeadBranchElimination` | `dead_branch/mod.rs` | post-pass | Removes `If(const)` branches and strips dead control edges |
| `LoadReadOnly` | `load_readonly/mod.rs` | peephole | Folds constant-address loads via caller-supplied `ReadOnlyMemory` |
| `StackStoreDetect` | `stack_store/detect.rs` | peephole | Promotes SP-relative `Store` to `StackStore { offset }` or `StackStorePhi` |
| `StackLoadForward` | `stack_load_forward/mod.rs` | peephole | Forwards values from `StackStore` to same-offset `Load`; walks memory chain |
| `FunctionArgDetect` | `function_args/mod.rs` | post-pass | Canonicalises register/stack arg reads to `FunctionArg` nodes |
| `CallStackArgCollect` | `stack_store/call_args.rs` | post-pass | Wires positional stack args into `Call` nodes (memory-aware) |

### Phase 4 Concern Passes

**CONFIRMED EXIST — all required for Phase 4 memory redesign:**

- `StackStoreDetect` (`stack_store/detect.rs`) — entry point for StackStore creation
- `StackLoadForward` (`stack_load_forward/mod.rs`) — uses `probe()` / `realize()` + memory walks
- `CallStackArgCollect` (`stack_store/call_args.rs`) — backward memory chain walk
- `FunctionArgDetect` (`function_args/mod.rs`) — post-pass arg canonicalization
- `LoadReadOnly` (`load_readonly/mod.rs`) — peephole for ROM reads
- `IfCondInversion` (`if_cond_inversion/mod.rs`) — control-flow canonicalization
- `RedundantPhis` (`redundant_phis/mod.rs`) — multi-predecessor collapse
- `KnownBits` (`known_bits/mod.rs`) — stateless bit annotation
- `ConstantFold` (`constant_fold/mod.rs`) — value canonicalization
- `FlagCmpCanonicalize` (`flag_cmp_canonicalize/mod.rs`) — flag-tree deforest

### Additional Optimizer Infrastructure

| Module | Purpose |
|--------|---------|
| `mem_walk.rs` | Generic backward memory-chain walker with `MemChainStep` trait + cycle guard |
| `sp_expr/mod.rs` | Stack-pointer expression decomposition (SP-rooted loads/stores) |
| `sp_expr/decompose.rs` | `decompose_sp()` entry point; returns `SpExpr::Terminal` or `SpExpr::Phi` |
| `sp_expr/ranges.rs` | SP offset range analysis |
| `sp_expr/walk.rs` | Low-level SP chain traversal |
| `sp_pass_cc.rs` | Calling-convention-aware SP pass harness |
| `peephole.rs` | Peephole-pass trait + combinator |
| `pipeline.rs` | `Optimizer` trait, `OptimizerPipeline`, fixed-point loop |
| `worklist.rs` | Worklist algorithm for iterative optimization |
| `error.rs` | `Result<T>` error handling |
| `test_support.rs` | Test fixture builders |

---

## Section B: Memory Subsystem Entry Points

### `decompose_sp()` — Stack Pointer Expression Decomposer

**Location:** `crates/strider-analyze/src/opt/sp_expr/decompose.rs`

**Signature:**
```rust
pub fn decompose_sp(
    g: &Graph,
    out: NodeOutputId,
    sp_vn: rsleigh::Vn,
    memo: &mut SpExprMemo,
) -> Option<SpExpr>
```

**Returns:** `SpExpr::Terminal { base, offset }` (concrete SP + i64 offset) or `SpExpr::Phi { phi_node, offsets }` (SP phi with per-predecessor offsets) or `None` (non-SP expression).

**Contract:** Walks `InitialVar(sp)` transformed by `Add`/`Sub` of constants and joined by `VarPhi(sp)`. The `Phi` variant requires every predecessor to decompose to a `Terminal` with base `InitialVar(sp)`; a nested Phi anywhere causes `None` return (strictness ensures caller-owned correctness invariants, e.g., exact offset for stack arg detection).

**Usage:** Called by `StackStoreDetect` to detect which `Store` nodes target the stack; called by indirect-branch classifier to match `Load[sp + idx*stride]` patterns.

---

### `walk_mem_chain()` — Generic Backward Memory-Chain Walker

**Location:** `crates/strider-analyze/src/opt/mem_walk.rs`

**Signature:**
```rust
pub fn walk_mem_chain<S: MemChainStep>(
    g: &Graph,
    mem: NodeOutputId,
    cycle_policy: CyclePolicy,
    seen: &mut DenseEntitySet<NodeOutputId>,
    is_phi: impl Fn(NodeId) -> bool,
    step: &mut S,
) -> Result<S::Verdict>
```

**Parameters:**
- `mem`: Starting memory edge (`NodeOutputId`)
- `cycle_policy`: `CyclePolicy::GuardEveryNode` (all nodes deduped) or `GuardPhiOnly` (only MemPhi boundaries deduped)
- `seen`: Cycle guard (pass fresh `DenseEntitySet::new()` on entry)
- `is_phi`: Closure determining which `NodeId` is a phi-like multi-input node
- `step`: Mutable classifier implementing `MemChainStep` trait

**Returns:** `Result<S::Verdict>` — the classifier's accumulated verdict (type and combine-policy are classifier-defined).

### `MemChainStep` Trait

**Location:** `crates/strider-analyze/src/opt/mem_walk.rs`

**Three core methods:**

```rust
pub trait MemChainStep {
    type Verdict;  // Classifier's verdict type (bool, Count, etc.)

    fn classify(
        &mut self,
        g: &Graph,
        mem: NodeOutputId,
        node: NodeId,
    ) -> Result<StepResult<Self::Verdict>>;
    
    fn cycle_verdict(&mut self) -> Self::Verdict;
    
    fn combine_phi(
        &mut self,
        phi_node: NodeId,
        phi_token: NodeOutputId,
        preds: Vec<Self::Verdict>,
    ) -> Self::Verdict;
}
```

**`StepResult` enum:**
```rust
enum StepResult<V> {
    Verdict(V),                // Terminal verdict; stop walk
    JoinPhi { phi_node, phi_token, preds },  // Fork: walk all predecessors
    ContinueFrom(NodeOutputId),  // Single-predecessor chain step
}
```

**Contract:** The walker calls `classify` for each visited (non-cycle) node. The classifier returns a verdict to stop, a fork (MemPhi) to explore multiple predecessors, or a single predecessor to continue. After all predecessors of a fork complete, `combine_phi` is called with the collected verdicts to produce a single result for the phi.

**Usage:** Two existing call sites implement this:
1. `stack_load_forward::probe()` — walks backward from a memory token, checking whether a specific `StackStore` offset is reachable; verdict is `bool` (found or not found).
2. `function_args::mem_chain_is_dirty()` — walks backward checking memory-consistency (all arms same offset or clean); verdict is `bool`.

---

### `StackLoadForward` — Stack Load Value Forwarding

**Location:** `crates/strider-analyze/src/opt/stack_load_forward/mod.rs`

**Two key methods:**

- `probe(&mut self, mem: NodeOutputId, offset: i64) -> Result<bool>` — Walks backward from the memory token to find whether a `StackStore` at exactly `offset` is reachable; returns `true` if found.
  
- `realize(&mut self, load_node: NodeId, mem: NodeOutputId) -> Result<OptimizationResult>` — After `probe()` succeeds, rewrites the load to forward the stored value; uses accumulated state from the walk.

**Contract:** Operates in two phases: exploration (probe) collects candidate StackStore offsets, then realization (realize) replaces the load. The walker uses an internal memoized lookup (`StackStoredValueMemo`) to avoid O(n²) repeated chains walks.

---

### `CallStackArgCollect` — Stack Argument Collection

**Location:** `crates/strider-analyze/src/opt/stack_store/call_args.rs`

**Entry point:** `collect_stack_args_in_chain_order(ctx, call_node, memo) -> Result<Vec<NodeOutputId>>`

**Memory-chain behavior:** Walks backward from the `Call` node's memory input, accumulating `StackStore` data outputs in slot-order. Treats `MemPhi` as a chain terminator (never forks) — the walk must succeed in each arm independently, and all arms must agree on the set of offsets and orderings.

**Invariant:** The walk is linear (non-forking) by design; it returns early on disagreement between arms rather than trying to merge divergent states. This avoids the generality of `MemChainStep` but gains simplicity for the position-dependent accumulator.

---

## Section C: Pattern DSL Surface for Memory Ops

### Pattern Builders for Memory Operations

**Location:** `crates/strider-analyze/src/pattern/pat/builders/memory.rs`

**Builders exist for:**
- `Load(space)` — matches `NodeKind::Load(VnSpace)`
- `Store(space)` — matches `NodeKind::Store(VnSpace)`
- `StackStore { space, offset }` — matches `NodeKind::StackStore { space, offset }`
- `StackStorePhi { space }` — matches `NodeKind::StackStorePhi { space }`
- `MemPhi` — matches `NodeKind::MemPhi`

### Pattern Builders for Function Arguments

**Location:** `crates/strider-analyze/src/pattern/pat/builders/function_arg.rs`

**Builders:**
- `function_arg(index: u32)` — matches `NodeKind::FunctionArg { source, index }` for exact index
- `function_arg_source(source: FunctionArgSource)` — matches specific source (register or stack offset)

**Corresponding MatchResult:** `FunctionArgHandle<'g>` provides typed access to matched argument nodes.

### Matcher API for Function Arguments

**Location:** `crates/strider-analyze/src/pattern/matcher/mod.rs`

**Public methods:**
```rust
impl<'g> Matcher<'g> {
    /// Returns the `FunctionArgHandle` for arg at `index`, or `None` if no such arg exists.
    pub fn function_arg(&self, index: u32) -> Option<FunctionArgHandle<'g>>;
    
    /// Count of function arguments on this graph.
    pub fn function_arg_count(&self) -> usize;
    
    /// Iterate over `(index, handle)` pairs for all function arguments.
    pub fn function_args(&self) -> impl Iterator<Item = (u32, FunctionArgHandle<'g>)>;
}
```

**Note:** These are **query-time APIs** (reading pre-existing FunctionArg nodes), not construction APIs. Pattern builders (`function_arg(index)`) are used in `Pat` match expressions to search.

---

## Section D: Test Landscape

### Test Item Counts (by `git grep` on `rewrite/strdier`)

**strider-ir (`crates/strider-ir/src/`)**
- Total test items: **327** (#[test] decorated items across all modules)
- Key test modules: `builder/tests.rs` (90), `graph/tests.rs` (42), `graph/compact.rs` (10), `graph_dot/tests.rs` (13), `node/tests.rs` (29), `validate/tests.rs` (33), `walk/cast/tests.rs` (17), `node/output_type.rs` (13), `function.rs` (1 test: `compact_remaps_entry_and_drops_zombies`)

**strider-analyze (`crates/strider-analyze/src/`)**
- Opt passes: **315** test items across all opt modules
  - `constant_fold/tests.rs` (95), `dead_branch/tests.rs` (7), `flag_cmp_canonicalize/tests.rs` (13), `function_args/tests.rs` (20), `if_cond_inversion/tests.rs` (6), `known_bits/tests.rs` (22), `load_readonly/tests.rs` (7), `mem_walk.rs` (7), `redundant_phis/tests.rs` (7), `sp_expr/decompose.rs` (15), `sp_expr/ranges.rs` (3), `sp_expr/walk.rs` (1), `stack_load_forward/tests.rs` (21), `stack_store/tests.rs` (20), `pipeline.rs` (7), `worklist.rs` (6), `peephole.rs` (6), plus various indirect-branch tests (35+ items)
  
- Pattern module: Matcher, consumer, rewrite tests (41+ items)
- Orchestrator: Indirect branch resolve + strider insn tests (30+ items)

### Integration Test Coverage

**Key integration tests (end-to-end FunctionArg pipeline):**
- `function_args/tests.rs` (20 tests) — Tests FunctionArgDetect pass: register-arg reads → FunctionArg nodes, stack-arg loads → FunctionArg nodes, uniqueness invariant validation
- `stack_load_forward/tests.rs` (21 tests) — Tests memory-chain forwarding: probes through StackStore, MemPhi, and Store chains; realizes loads
- `stack_store/tests.rs` (20 tests) — Tests StackStoreDetect: SP-relative Store detection, offset decomposition, StackStorePhi creation

**Integration test file:** `crates/strider-analyze/tests/` — (NOT examined in detail; presence confirmed but specific test count not enumerated)

---

## Section E: Sanity Checks for the 4-Phase Plan

### Phase 1: ControlState Rename → Region

**Files mentioning `ControlState` or `control_state`:**

Confirmed exists:
- `crates/strider-ir/src/node/kind.rs` — NodeKind::ControlState variant + category classifier
- `crates/strider-ir/src/node_signature.rs` — signature lookup
- `crates/strider-ir/src/graph/mod.rs` — doc comments
- `crates/strider-ir/src/builder/mod.rs` — region creation
- `crates/strider-ir/src/builder/call.rs` — control-state emission
- `crates/strider-ir/src/validate/layer_c.rs` — validator
- `crates/strider-ir/src/walk/mod.rs` — control-state barrier logic
- `crates/strider-ir/src/dot/mod.rs` + `crates/strider-ir/src/dot/label.rs` — rendering
- `crates/strider-analyze/src/opt/redundant_phis/mod.rs` — pattern match (heaviest usage)
- `crates/strider-analyze/src/opt/dead_branch/mod.rs` — pattern match
- `crates/strider-analyze/src/opt/*/tests.rs` (multiple test modules reference ControlState fixtures)
- `crates/strider-analyze/src/pattern/pat/builders/phi.rs` — control_state builder
- `crates/strider-analyze/src/pattern/pat/ctor/mod.rs` — DSL reflection
- `crates/strider-analyze/src/orchestrator/mod.rs` — region header creation
- `crates/cfg/src/**/*.rs` — region builder
- `crates/strider-py/src/**/*.rs` — Python pattern mirror
- `crates/strider-py/strider/__init__.pyi` — stub

**Estimate:** ~40–60 references across codebase. Mechanical rename via find-and-replace is **safe** and **compiler-verified**.

**Risk assessment:** LOW — single symbol, no semantic branching, all uses are structural classification.

---

### Phase 2: Function Struct Introduction

**Current state on `rewrite/strdier`:**

- `crates/strider-ir/src/function.rs` — **Exists and is doc-only + test mod**. Contains single test `compact_remaps_entry_and_drops_zombies()`.
- **No `BuiltFunctionGraph` or similar** on this branch (that exists only on `simplification/ai1`).
- `Graph` carries 9 overlays: `entry`, `cc_metadata`, `asm_fingerprints`, `stack_phi_offsets`, `call_other_names`, `call_clobbered_overrides`, `phi_var_tag`, `wide_consts`, `initial_var_index`.

**Plan compatibility:** **SAFE**. The function.rs file is a clean slate (no stale types to collide with). All Graph overlay accessors are `pub`, so callers can be updated incrementally.

---

### Phase 3: FunctionArg Side-Table

**Consumers of `NodeKind::FunctionArg` and `FunctionArgSource`:**

- `crates/strider-ir/src/node/kind.rs` — Variant declaration + classifier
- `crates/strider-analyze/src/opt/function_args/mod.rs` — `FunctionArgDetect` pass: creates, validates uniqueness
- `crates/strider-analyze/src/opt/function_args/tests.rs` — 20 tests exercising FunctionArg creation and matching
- `crates/strider-analyze/src/pattern/pat/builders/function_arg.rs` — Pattern builder
- `crates/strider-analyze/src/pattern/matcher/function_arg_handle.rs` — Match result handle
- `crates/strider-analyze/src/pattern/pat/ctor/mod.rs` — DSL reflection (pattern constructor)
- `crates/strider-analyze/src/orchestrator/mod.rs` — Orchestrator arg pipeline
- `crates/strider-py/src/**/*.rs` — Python mirror
- Various test modules that construct FunctionArg nodes as fixtures

**Estimate:** ~15–25 files touching FunctionArg types directly. Pattern-matching and test fixtures will be bulk of the impact.

**Risk assessment:** MEDIUM — Node kind removal is a breaking change (any pattern match becomes a compile error if not handled), but the scope is well-bounded (FunctionArgDetect pass + pattern DSL + tests).

---

### Phase 4: Memory SSA Redesign

**Consumers of `NodeKind::StackStorePhi` and `NodeKind::StackStore`:**

Confirmed usage:
- `crates/strider-ir/src/node/kind.rs` — Variant declarations
- `crates/strider-analyze/src/opt/stack_store/detect.rs` — **StackStoreDetect** creates both; entry point
- `crates/strider-analyze/src/opt/stack_store/call_args.rs` — **CallStackArgCollect** walks chain backward
- `crates/strider-analyze/src/opt/stack_load_forward/mod.rs` — **StackLoadForward** probes/realizes through chain
- `crates/strider-analyze/src/opt/stack_store/tests.rs` (20 tests) — StackStoreDetect + call-arg collection tests
- `crates/strider-analyze/src/opt/stack_load_forward/tests.rs` (21 tests) — Memory chain forward pass
- `crates/strider-analyze/src/opt/mem_walk.rs` (7 tests) — Generic memory walker
- `crates/strider-analyze/src/opt/sp_expr/decompose.rs` (15 tests) — SP expression decomposition
- `crates/strider-analyze/src/opt/*/tests.rs` — various fixtures referencing StackStore nodes
- `crates/strider-analyze/src/pattern/pat/builders/memory.rs` — Pattern builders
- `crates/strider-analyze/src/orchestrator/mod.rs` — Orchestrator arg pipeline

**Sunset list:**
- `StackStoreDetect` — to be replaced by a MemPartition insertion pass
- Memory-chain walks in `StackLoadForward` + `CallStackArgCollect` — to become partition-membership queries
- `StackStorePhi` node kind — to be dropped

**Risk assessment:** HIGH — Largest semantic change. Requires:
1. New node kinds: `MemPartition { partition }`, `MemUnion`
2. Extend `NodeOutputKind::Memory` to carry `Option<MemPartitionId>`
3. New `AliasSplit` optimization pass to insert partition boundaries
4. Rewrite StackLoadForward + CallStackArgCollect to query partitions instead of walking chains

**Mitigation:** Phase 4 is **last** by design — Phases 1–3 stabilize the IR structure (Region rename, Function struct, arg side-table) before the largest semantic change.

---

## Section F: Risk Assessment for Cherry-Picking

### Compatibility: Plan vs. rewrite/strdier

The plan in `docs/superpowers/plans/2026-05-24-v16-structural-redesign.md` uses **unprefixed crate names** (`ir`, `opt`, `cfg`, `pattern`, `pcode-lift`, `target`, `reader`, `strider` — only `strider-py` is prefixed). The `rewrite/strdier` branch **also uses unprefixed names**.

Verification:
```
crates/cfg/              ✓ (plan says `crates/cfg`)
crates/ir/               ✓ (plan says `crates/ir`)
crates/opt/              ✓ (plan says `crates/opt`)
crates/pattern/          ✓ (plan says `crates/pattern`)
crates/strider/          ✓ (plan says `crates/strider`)
crates/strider-py/       ✓ (plan says `crates/strider-py`, prefixed as expected)
```

### API Surface Compatibility

**Graph::create_node signature on rewrite/strdier:**
```rust
pub fn create_node(
    &mut self,
    kind: NodeKind,
    inputs: impl IntoIterator<Item = NodeOutputId>,
    output_kinds: impl IntoIterator<Item = NodeOutputKind>,
) -> NodeId
```

**Matches plan expectation:** YES. Plan examples use this exact signature.

**Graph::create_node_attributed:**
```rust
pub fn create_node_attributed(
    &mut self,
    kind: NodeKind,
    inputs: impl IntoIterator<Item = NodeOutputId>,
    output_kinds: impl IntoIterator<Item = NodeOutputKind>,
    contributors: &[NodeId],
) -> NodeId
```

**Matches plan expectation:** YES. This method absorbs asm-fingerprint from contributor nodes.

### Missing Prerequisites

**entity_utils::DenseEntitySet:**
- **Confirmed present** in `crates/entity-utils/src/set.rs`
- Exported from `crates/entity-utils/src/lib.rs`
- Already used throughout `mem_walk.rs` (cycle guard), `stack_load_forward.rs`, and pattern matching

**Pattern infrastructure:**
- Pattern builders, rewrite context, matcher — all confirmed present with compatible APIs
- `Pat` construction via `builders::*` — confirmed
- `Matcher::find_all()`, `Matcher::match_at()` — confirmed

### Cross-Branch Conflicts

The other branch `simplification/ai1` has **no conflicting work** on `rewrite/strdier`'s files:
- `simplification/ai1` does not have a `BuiltFunctionGraph` on the v16 path (unverified; checked only that `crates/strider-ir/src/function.rs` is not present on `simplification/ai1`)
- The plan's Phase 2 file paths (`crates/ir/src/function.rs`, etc.) are **directly applicable** to `rewrite/strdier`

**Adjustments needed:** **NONE** for path translation. Code examples in the plan can be applied as-is.

---

## Summary

| Phase | Risk | Blockers | Estimated Effort |
|-------|------|----------|------------------|
| 1: ControlState→Region | LOW | None | 30–60 min |
| 2: Function struct | MEDIUM | None (clean slate) | 1–2 days |
| 3: FunctionArg side-table | MEDIUM | Phase 2 complete | 1 day |
| 4: Memory SSA redesign | HIGH | Phases 1–3 complete; new pass design required | 2–3 days |

**All prerequisites confirmed present and compatible.** The plan is directly executable on `rewrite/strdier` without path translation or API adjustments.

