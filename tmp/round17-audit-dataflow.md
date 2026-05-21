# Dataflow Unification Audit: Strider IR Analyses

## Executive Summary

Strider presently hosts **2.5 dataflow-style analyses** across 3 crates (opt, ir, strider) that propagate information over the IR graph via monotonic fixed-point computation and memoization. Today's infrastructure is hand-built per-analysis; a unified framework could extract ~300–500 LOC of shared scaffolding but risks over-engineering for the current workload. Only KnownBits is a full lattice analysis; SpExpr and asm-fingerprints use lattice-like joins but with specialized termination conditions.

**Recommendation:** Extract a generic `Lattice<L>` + `WorklistAnalysis<L>` skeleton; avoid a monolithic framework. Start with KnownBits migration to validate the abstraction; SpExpr and asm-fingerprints remain separate until a third similar analysis lands.

---

## Finding 1: Lattice Representation & Transfer Functions

### KnownBits (Worklist-Driven, Forward)
**File:** `crates/opt/src/known_bits/mod.rs` (lines 1–500+)

- **Lattice:** `Kb { ones: u64, zeros: u64 }` with bottom=`Kb::default()` (unknown) and partial join via `Kb::merge(&mut self, other) -> Result<bool>`.
- **Join:** bitwise OR of `ones` and `zeros` across inputs. Contradiction detection baked in.
- **Transfer:** Per-node `node_kind` match in `node_known_bits(ctx, node_id, known) -> Option<(NodeOutputId, Kb)>` dispatches on `IntConst`, `IntBinaryOp` (And/Or/Xor/Shift variants), `IntUnaryOp::BitNot`, `Truncate`, `Extend`, etc.
- **Storage:** `KnownBitsMap = SecondaryMap<NodeOutputId, Kb>` (dense indexed, O(1) lookup, migrated from HashSet at line 13).
- **Fixed-point:** Seeded with preorder + worklist `WorkSet` (pop → process → push consumers on change).
- **Soundness:** U64-bounded; U128/U256/float/bool fall through as "unknown" (line 26–31).

### SpExpr (Recursive Memo, Backward)
**File:** `crates/opt/src/sp_expr.rs` (lines 1–250)

- **Lattice:** `SpExpr { Terminal { base, offset }, Phi { phi_node, offsets[] } }` with `Unknown` implicitly represented by `None` on failed decomposition.
- **Join:** Phi offsets are collected per predecessor; mismatch (predecessor doesn't decompose to Terminal or decomposes to nested Phi) returns `None` (line 12–16).
- **Transfer:** `decompose_sp(graph, output, sp_vn, memo, visiting) -> Option<SpExpr>` recursively walks Add/Sub chains backward, checking against `InitialVar(sp)` roots.
- **Storage:** `SpExprMemo = FxHashMap<NodeOutputId, Option<SpExpr>>` (line 18) + recursion guard `visiting: DenseEntitySet<NodeId>` (implicit in call signature).
- **Fixed-point:** Not explicitly loop-driven; instead memoized on repeated decomposition calls (used by `StackStoreDetect`, `StackLoadForward`, stack-argument classification).
- **Termination:** Memo hits after first decomposition of each output; recursion guard prevents cycles.

### Asm-Fingerprints (Set-Valued Side-Table)
**File:** `crates/ir/src/graph/store.rs` (lines 151–200)

- **Lattice:** `asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>` where the join is **set union** (line 179–201).
- **Join:** `extend_asm_fingerprint(&mut self, node_id, contributors: &[u64])` unions contributors in sorted order; O(n log n) but amortized by the "mostly-appending" fast path (line 186–198).
- **Transfer:** At lift time (`FunctionBuilder::set_lift_addr(Some(addr))`) every `create_node` unions `addr` into the node's fingerprint side-table (CLAUDE.md line 65). At rewrite time, `OptimizationResult::after_replace` absorbs old producer's fingerprint into new producer via `extend_asm_fingerprint_from` (pipeline.rs line 54).
- **Storage:** Dense entity map; default empty `Vec` is the "no contributors" sentinel.
- **Fixed-point:** Monotonic (superset-only invariant); no explicit iteration—passes grow fingerprints incrementally.

---

## Finding 2: Shared Scaffolding Across Analyses

### WorkSet Pattern
**File:** `crates/opt/src/worklist.rs`

```rust
pub struct WorkSet {
    queued: DenseEntitySet<NodeId>,     // O(1) duplicate-prevent bitset
    queue: VecDeque<NodeId>,
}
impl WorkSet {
    pub fn seeded(it) -> Self { ... }  // Preorder + custom seed
    pub fn push(&mut self, n) { ... }  // Re-enqueue if not pending
    pub fn pop(&mut self) -> Option<NodeId> { ... }
}
```

Both **KnownBits** and **ConstantFold** (and potentially asm-fingerprints) use this same worklist. SpExpr avoids it by being memoization-only.

**Shared structure:**
- Preorder seeding (via `WorkSet::seeded` or `WorkSet::seeded_kind`)
- Worklist-driven iteration with consumer re-enqueue on change
- `DenseEntitySet<NodeId>` for O(1) duplicate prevention

**Savings if unified:** ~30 LOC (worklist struct + seeding methods); currently duplicated nowhere in codebase (WorkSet is shared), but the *pattern* of "seed + drain-and-re-enqueue" could be parameterized.

### Memoization Pattern (SpExpr)
**File:** `crates/opt/src/sp_expr.rs`

```rust
let memo: FxHashMap<NodeOutputId, Option<SpExpr>> = ...;
let visiting: DenseEntitySet<NodeId> = DenseEntitySet::new();
decompose_sp(graph, output, sp_vn, &mut memo, &mut visiting);
```

SpExpr uses a hand-rolled memo + recursion guard for backward analysis. KnownBits does not memoize across calls (it's a single fixed-point pass); asm-fingerprints doesn't memoize (it's a side-table accumulation).

**Shared structure if extracted:**
```rust
pub struct MemoTable<K: Entity, V> {
    table: SecondaryMap<K, Option<V>>,
    visiting: DenseEntitySet<NodeId>,
}
impl<K, V> MemoTable<K, V> {
    pub fn get_or_compute<F>(&mut self, key: K, f: F) -> Option<V> where F: FnOnce() -> Option<V> { ... }
}
```

**Savings:** ~20 LOC if a second backward-recursive analysis lands; not yet justified.

### Per-NodeKind Transfer Functions
**Pattern in KnownBits:**
```rust
match kind {
    NodeKind::IntConst(v) => { Kb::from_const(v, ty) }
    NodeKind::IntBinaryOp(op) => {
        match op {
            And => { ... }, Or => { ... }, Xor => { ... }, ShiftLeft => { ... }, ...
        }
    }
    NodeKind::IntUnaryOp(BitNot) => { ... }
    NodeKind::Truncate => { ... }
    NodeKind::Extend(op) => { ... }
    ...
}
```

**Pattern in SpExpr** (implicit via `decompose_sp` match on producer kind):
```rust
match ctx.node_kind(node) {
    NodeKind::IntBinaryOp(Add | Sub) => { /* walk operands */ }
    NodeKind::IntConst(_) => { /* terminal */ }
    NodeKind::InitialVar(vn) if vn == sp_vn => { /* root */ }
    ...
}
```

**Pattern in asm-fingerprint propagation:**
```rust
// At lift time: every create_node unions addr
// At rewrite: every replace absorbs old → new fingerprint
```

No unified dispatch table exists; each pass reimplements the "what does this node do?" match. A `Transfer<L: Lattice>` trait parameterized per analysis could unify, but today only KnownBits has a full per-kind match.

**Savings:** ~50 LOC if a second truly divergent per-kind transfer lands (SpExpr is too specialized to generalize as-is).

---

## Finding 3: Backward vs Forward Analysis

**KnownBits:** Forward (seed entry, flow data forward via uses).
**SpExpr:** Backward (seed a leaf output, flow data backward via producer chain).
**Asm-fingerprints:** Orthogonal (accumulates during both forward lift and forward rewrite).

A unified framework could parameterize `Direction::Forward | Backward`, but the control flow is different enough that unifying them would require abstracting the traversal order (preorder + worklist vs. recursion + memo), which loses clarity.

**Risk:** Premature abstraction. Only unify if a third backward-only analysis lands.

---

## Finding 4: Per-Output vs Per-Node Side-Tables

- **KnownBitsMap:** `SecondaryMap<NodeOutputId, Kb>` (per-output; multiple outputs per node can have different Kb).
- **Asm-fingerprints:** `SecondaryMap<NodeId, Vec<u64>>` (per-node; every node has one fingerprint regardless of outputs).
- **SpExpr memo:** `FxHashMap<NodeOutputId, Option<SpExpr>>` (per-output).

A generic `AnalysisResult<K, L>` where K ∈ {NodeId, NodeOutputId} and L is the lattice could unify storage. Today they each hardcode their key type.

**Savings:** ~10 LOC in storage abstraction; marginal value unless both key types are used heavily.

---

## Finding 5: Shape Recognition as Dataflow (Indirect-Branch Classification)

**File:** `crates/opt/src/indirect_branch_resolve/classify.rs`

`classify_anchor_with_rom_and_sp(ctx, anchor, lr_vn, rom, sp, known_bits) -> Option<ResolvedTargets>`

This is **not** a lattice analysis but a shape classifier that pattern-matches an anchor's producer tree:
- `IntConst(addr)` → `Single(addr)`
- `InitialVar(lr)` → `LinkRegister` (if lr_vn is set)
- `ValuePhi(constants)` → `Multiple(unique_constants)`
- `Load(jump_table_shape)` with known bounds → `Multiple(table_entries[])`
- `Load(stack_array_shape)` → `Multiple(stack_array_targets[])`

Each arm is a **sound unambiguous shape**; the result is cached nowhere (recomputed per anchor). If framed as a dataflow analysis:

```
Lattice L = { Unknown, LinkRegister, Single(u64), Multiple(Set<u64>) }
Transfer(node_kind, inputs_L) -> Option<L>
Join (multiple paths to same node) = union of target sets (over-approximate)
```

This is **not** currently pushed into a lattice framework because:
1. Each arm has different side-conditions (rom reads, control-flow dominance checks, KB bounds).
2. Results aren't reused across iterations (each call recomputes).
3. The shape match is inherently one-shot (not iterative).

**Verdict:** Could be expressed as dataflow *in principle*, but doing so would add 50+ LOC of abstraction for no real reuse today.

---

## Finding 6: Convergence & Fixed-Point Detection

**KnownBits:**
```rust
pub struct OptimizerPipeline { ... }
impl OptimizerPipeline {
    pub fn run(&mut self, ctx: &mut RewriteCtx<'_>) -> Result<()> {
        loop {
            let mut any_changed = false;
            for pass in &self.passes {
                let result = pass.optimize(ctx)?;
                any_changed |= result.changed();
            }
            if !any_changed { break; }
        }
    }
}
```

The pipeline iterates passes until all return `NoChange`. KnownBits is a single pass (called once per iteration); it internally uses a worklist to reach fixed point in a single `optimize` call.

**SpExpr:** Memo-driven (inherently convergent after the first call to each output).

**Asm-fingerprints:** Monotonic (superset-only guarantee); no explicit fixed-point loop at the side-table level, just pass-level accumulation.

**Shared pattern:** None explicitly; the pipeline's outer loop is already generic (`OptimizationResult` + loop-on-changed). A generic `FixedPointDriver<L: Lattice, P: Pass<L>>` could be extracted but today all three analyses plug into the existing infrastructure.

**Savings:** 0 LOC (already abstracted at the pipeline level).

---

## Finding 7: Bottom & Top Elements

- **KnownBits:** Top = `Kb::default()` (no known bits; `ones=0, zeros=0`). Bottom = all bits known (e.g., `Kb::from_const(5, U32)` → `ones=5, zeros=0xFFFFFFFA`). **Join = bitwise OR** (adds information).
- **Asm-fingerprints:** Top = empty `Vec<u64>` (no contributors). Bottom = every possible address (infeasible; the fingerprint grows monotonically). **Join = set union**.
- **SpExpr:** Implicit top = `None` (decomposition failed; treat as "unknown shape"). No true bottom.

All three use **at-least-as-good-as** ordering (join adds information); KnownBits and fingerprints are explicitly lattices; SpExpr is a poset (partial join via memo, not algebraic).

**Lattice trait:** Could standardize `.top() -> L`, `.bottom() -> L`, `.join(&mut self, other) -> Result<bool>`. KnownBits already has `merge`; fingerprints use `extend_*`; SpExpr has none.

**Savings:** ~15 LOC for a trait + 3 impls; mostly a naming/documentation exercise.

---

## Finding 8: Pattern-Based Shape Recognition

Beyond direct-branch classification (Finding 5), shape recognition appears in:

1. **ConstantFold:** `eval_int`, `eval_float` match on node kind and operand patterns (line ~100 in constant_fold/mod.rs).
2. **KnownBits:** Some transfer functions check sub-patterns (e.g., `ShiftLeft` inspects the shift operand's Kb to decide if shift amount is fully known).
3. **SpExpr:** Entire decomposition is shape-matching (Add/Sub/Const chains).

These are not unified; each pass reimplements its pattern match. A **Pattern trait** could be abstracted, but it would layer on top of the existing pattern crate's AST-style matching, not beneath it.

**Verdict:** Not a low-hanging unification; shape matching is already fairly modular per pass.

---

## Finding 9: Per-Region vs Per-Function Scope

**KnownBits:** Per-function (global analysis over the entire IR).
**SpExpr:** Per-function (decomposition walks the entire graph backward from a seed output).
**Asm-fingerprints:** Per-function.
**Variable tracking** (FunctionBuilder::vars, crates/ir/src/builder/vars.rs): Per-region + per-function (regions have per-var PHI nodes; the builder maintains a region-scoped `Vn → NodeOutputId` map that changes as regions are processed).

No analysis today is explicitly per-region; all work on the completed global IR. Variable tracking is per-region *during* IR construction, not during analysis.

**Verdict:** Not a unification opportunity; different concerns (IR construction vs. IR analysis).

---

## Finding 10: Recursion Guard & Cycle Detection

**SpExpr:** Uses `visiting: DenseEntitySet<NodeId>` to detect backward-walk cycles. Not reused elsewhere.

**KnownBits & asm-fingerprints:** No cycles because the IR is a DAG (no data-flow loops; phis are resolved via the join operator, not by revisiting).

**Verdict:** Recursion guard is SpExpr-specific; no generalization needed yet.

---

## Proposed Minimal Framework

If a unification were pursued, the skeleton would be:

```rust
// crates/opt/src/dataflow/mod.rs

pub trait Lattice: Clone + Default {
    type Error;
    /// Try to merge `other` into self, returning Ok(true) if changed.
    fn join(&mut self, other: Self) -> Result<bool, Self::Error>;
}

pub trait DataflowAnalysis {
    type L: Lattice;
    type Key: Entity;  // NodeId or NodeOutputId
    
    fn transfer(&self, ctx: RewriteCtxView<'_>, node: NodeId) -> Result<Option<(Self::Key, Self::L)>>;
    fn seed(&self, ctx: RewriteCtxView<'_>) -> Box<dyn Iterator<Item = Self::Key>>;
}

pub struct WorklistAnalyzer<A: DataflowAnalysis> {
    analysis: A,
    state: SecondaryMap<A::Key, A::L>,
    worklist: WorkSet,
}

impl<A: DataflowAnalysis> WorklistAnalyzer<A> {
    pub fn run(&mut self, ctx: &mut RewriteCtx<'_>) -> Result<SecondaryMap<A::Key, A::L>> {
        // Seed + worklist loop...
    }
}
```

**Refactor effort:**
- KnownBits migration: ~100 LOC (replace `node_known_bits` match with `impl DataflowAnalysis`, wire into `WorklistAnalyzer`).
- SpExpr: Not suitable (recursion-based, not worklist-based); leave as-is.
- Asm-fingerprints: Per-node side-table, not an explicit analysis pass; leave as-is.

**NET LOC delta:** +200 LOC (framework boilerplate) − 50 LOC (KnownBits deduplication) = **+150 LOC net** for one migrated pass. Not worth it unless a second similar pass lands.

---

## Risk Assessment

### Foot-Guns If Framework Extracted

1. **Over-generalization:** The framework's `transfer` signature would need to handle both forward and backward analyses; adding a `Direction` parameter multiplies complexity. Start with forward-only; add backward later if needed.

2. **Lattice contract violations:** A generic `Lattice` trait cannot prevent callers from violating the join-idempotence or commutativity contracts. KnownBits's `merge` contradiction detector is hard-won; a sloppy `impl Lattice` for a future analysis could silently break soundness. **Mitigation:** Require all impls to document and test fixed-point convergence on a canonical test suite.

3. **Per-key abstraction:** Mixing `SecondaryMap<NodeId, _>` and `SecondaryMap<NodeOutputId, _>` in one trait requires careful handling of entity-type parameters. Today's hand-rolled code is clear about which it uses. **Mitigation:** Define separate `NodeAnalysis` and `OutputAnalysis` subtypes if both are needed; don't force one trait to fit both.

4. **Debugging:** A generic framework hides per-analysis quirks (e.g., KnownBits's U64-width bound, SpExpr's memo invalidation rules). Logging and introspection become harder. **Mitigation:** Keep per-analysis `Optimizer` wrapper impls; framework is just plumbing.

---

## Honest Assessment

**Unification ROI:**

| Analysis | LOC | Reuse? | Worth Extracting? |
|----------|-----|--------|-------------------|
| KnownBits | 500 | Worklist pattern only | Only if 2nd similar pass lands |
| SpExpr | 200 | Memo + recursion guard (unique) | No |
| Asm-fingerprints | 100 | Set-union join (generic) | No (orthogonal to KnownBits) |

**Verdict:** Today's 2.5 analyses are **not enough to justify a framework**. The shared infrastructure (WorkSet, SecondaryMap) is already unified at the platform level. Further unification buys convenience naming (`.join()` vs `.merge()`, `.transfer()` vs `node_known_bits()`) but adds ~150 LOC overhead with no measurable speedup or memory savings.

**Recommended approach:** Extract utilities lazily. If a third forward-worklist analysis surfaces (e.g., "may-be-zero" tracking, pointer-alignment inference), then extract a `WorklistAnalysis<L: Lattice>` framework and migrate KnownBits + the new pass together. SpExpr and asm-fingerprints stay hand-rolled because they don't fit the mold.

---

**File references:** KnownBits (crates/opt/src/known_bits/mod.rs:1–500), SpExpr (crates/opt/src/sp_expr.rs:1–250), Asm-fingerprints (crates/ir/src/graph/store.rs:151–201), Worklist (crates/opt/src/worklist.rs), Pipeline (crates/opt/src/pipeline.rs:1–100), Indirect-branch classify (crates/opt/src/indirect_branch_resolve/classify.rs:1–100).

