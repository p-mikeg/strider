# Jump-table resolution via a non-mutating abstract evaluator

Date: 2026-06-22
Branch: `feature/jump-table-abstract-eval` (off `develop`)

## Problem

The indirect jump-table classifier resolves a table by *constant-folding the
dispatch value once per index*. Today it does this by graph duplication
(`crates/strider-opt/src/post_opt/indirect_branch_resolve/table.rs`):

1. Clone the whole `Function`, `compact()` it once (`table.rs:101-103`).
2. For each candidate index value, in range `lo..=hi`: clone the compacted
   function again (`fold_dispatch_to_const`, `table.rs:258`), substitute the
   index with `IntConst(i)` via `EditFunction::replace_value`, run the full
   `default_pipeline()` on the clone, then read the now-constant dispatch input
   off the `IndirectBranch` node.

This is sound but heavy: two clones plus a full optimizer pipeline run per index
(up to `MAX_TABLE_ENTRIES = 4096`), and it forces `Function`/`Graph` to be
`Clone`. The only whole-`Function` clones in the entire workspace are these two
sites — nothing else needs `Clone`.

We do not actually need to collapse the graph. We only need to *evaluate* the
dispatch value for each concrete index. The only optimizations that move a value
from a concrete index to a concrete target are **ConstFold**, **LoadReadOnly**,
and **LoadForward** (plus seeding the index constant). The rest of the pipeline
(KnownBits, DeadBranchElimination, FlagCmp, RegionCollapse, …) does control/bit
simplification that never contributes to "index `i` → address → target".

## Design

Replace the clone-and-optimize core with a **read-only abstract evaluator**: no
`Function`/`Graph` clone, no `EditFunction`, no `default_pipeline`, no
`compact`. Walk the dispatch value's cone once and evaluate it under each
concrete index.

### 1. `ValueEvaluator` trait (in `strider-opt`)

```rust
trait ValueEvaluator {
    fn evaluate(
        &self,
        ctx: &EvalCtx<'_>,
        value: ValueId,
        map: &FxHashMap<ValueId, u128>,
    ) -> Option<u128>;
}
```

- `map` is read-only to the evaluators; the **driver** owns insertion.
- `EvalCtx<'_>` carries `&Function`, `Option<&dyn ReadOnlyMemory>` (ROM),
  `AliasMode`, and `Endianness`.
- The driver tries impls in fixed order — **ConstFold → LoadReadOnly →
  LoadForward** — and the first `Some` wins.

### 2. The three implementations

**ConstFold.** Reads operand values out of `map`, then calls the existing pure
cores: `constant_fold/eval_int.rs::eval_int_binary` / `eval_int_cmp`, plus the
unary / `Truncate` / `Extend` / `Popcount` / `Lzcount` evaluators. It must cover
**every integer node kind ConstFold currently folds** — completeness is verified
during TDD against the existing test suite. Also owns the `Phi` rule (below). An
`IntConst` node evaluates to its own value (no inputs).

**LoadReadOnly.** Address from `map` → `rom.read(addr, buf)` →
`endianness.read_uint(buf)` → mask to the load's type. Returns `None` when no
ROM is configured. Mirrors `load_readonly/mod.rs:138-173`, with the graph
mutation stripped.

**LoadForward.** `find_nearest_clobber(&Function, mem)` (already read-only over
`&Function`) → dominating `Store` → read `map[store_data]`, reshaping on the
`u128` (truncate / shift) when store width ≠ load width. A `MemPhi` / `Call` /
`InitialMemory` boundary, or a non-exact-overlap store, yields `None`. Honors
`AliasMode` exactly as the pass does today.

**Why the store's value is already available.** The cone is built over *all*
value inputs **including the memory token**, not just data edges. A `Load` takes
`[memory, address]`; the memory input's producer is the upstream memory op, so
walking it backward traverses the whole memory chain, every `Store` in it, and
each store's `data` value. Therefore `store_data ≺ store ≺ load` in topological
order, and the stored value is evaluated *before* the load that consumes it — in
the same single pass. `find_nearest_clobber` can only ever return a store that
is already in the cone and already in `map`. No recursion and no second cone are
needed.

### 3. Cone construction + per-index driver

Build the cone as backward reachability from the dispatch value over all value
inputs (memory token included; control edges excluded), then topologically order
it (postorder-over-inputs, producers before consumers). The cone and its order
are **fixed across all `i`** — compute them **once per candidate**.

```text
cone_rpo = topo_order(backward_value_cone(dispatch_value))   // once per candidate
for i in lo..=hi:
    map = { idx_value: i }
    for val in cone_rpo:                 // producers before consumers
        if let Some(v) = evaluate_any(ctx, val, &map) { map.insert(val, v); }
    targets.push(u64::try_from(*map.get(&dispatch_value)?)?)   // any miss ⇒ reject candidate
```

Cost: O(cone × range) per candidate, versus today's O(pipeline × graph × range).

### 4. Soundness semantics

Preserve the existing **never-over-approximate** contract (range analysis gives a
sound upper bound; enumeration treats `lo..=hi` as the complete target set):

- Any value that fails to collapse ⇒ `None` ⇒ the whole candidate is rejected
  (`enumerate_targets` returns `None`), identical to today's behavior when a
  wrong candidate fails to fold.
- **Cycles** (loop-carried phi): inputs stay unresolved ⇒ `None` ⇒ reject. A
  single RPO pass is sufficient; there is no fixpoint loop.
- **Value-`Phi`**: evaluate every arm; if all arms collapse to the *same*
  constant, return it, else `None`. Sound regardless of which arm control would
  take, because they are all equal. Arm values are in the cone (they are value
  inputs of the phi) and therefore ordered before it.

### 5. Clone deletion

- Remove `Clone` from `Function`'s derive (`crates/strider-ir/src/function/data.rs:96`,
  `#[derive(Default, Clone)]` → `#[derive(Default)]`).
- Delete the manual `Clone` impl on the generic `strider_graph::Graph<N, V, C>`
  (`crates/strider-graph/src/graph.rs`), now unused.
- Delete both clone sites and the now-pointless `compact` in `table.rs`, plus
  the per-index `EditFunction` / `replace_value` / `default_pipeline` machinery
  there.
- Clone removal is enforced by the compiler — if anything else relied on it, the
  build breaks (exploration confirms nothing does).

### 6. Testing (TDD)

- The existing cross-arch `table_tests.rs` suite (x86 / x64 / aarch64 / arm /
  thumb / mips32 / ppc32; mips64 stays a known pre-existing gap) is the
  behavioral gate — the new evaluator must keep every test green.
- Add unit tests for the evaluator per node-kind family (binary, unary,
  truncate/extend, popcount/lzcount, cmp), for LoadReadOnly (rodata read,
  no-ROM), for LoadForward (exact store→load, width-reshape, boundary ⇒ `None`),
  and for the `Phi` all-arms-agree and fail-closed paths.
- Clone removal is a compile-time gate (the derive and impl simply go away).
- Gate on the full workspace: `cargo test --workspace` + `cargo clippy
  --workspace` + `pytest` before requesting merge.

## Out of scope

- Other optimization passes' participation in jump-table resolution — explicitly
  only ConstFold / LoadReadOnly / LoadForward + the index seed.
- The mips64 PIC/GOT `gp` gap (pre-existing, unchanged).
- Any change to range analysis / candidate detection (`find_index_candidates`),
  which stays as-is and continues to feed `(value, lo, hi)` candidates.
