# Jump-table resolution via a non-mutating abstract evaluator

Date: 2026-06-22 (revised)
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

We do not need to collapse the graph. We only need to *evaluate* the dispatch
value for each concrete index. The only optimizations that move a value from a
concrete index to a concrete target are **ConstFold**, **LoadReadOnly**, and
**LoadForward** (plus seeding the index constant). The rest of the pipeline does
control/bit simplification that never contributes to "index `i` → address →
target".

## Design

Replace the clone-and-optimize core with a **read-only abstract evaluator**: no
`Function`/`Graph` clone, no `EditFunction`, no `default_pipeline`, no
`compact`. Build the dispatch value's cone once, topologically order it, and run
a flat per-index pass over that order.

### 1. Abstract value + evaluator (in `strider-opt`)

A jump-table address is either an absolute number (rodata) or stack-pointer
relative (on-stack table). The stack pointer is symbolic — never a constant —
so a pure-`u128` value can't represent a stack address. The abstract value is
therefore two-element:

```rust
enum Abs {
    Const(u128),                          // a concrete number (rodata addr, arithmetic)
    SpRel { base: ValueId, offset: i64 }, // sp_base + offset (on-stack addr)
}
```

The evaluator is a concrete struct (no trait — three node families, one
`match`), holding `{function, rom, alias_mode, endianness, sp_base, sp_memo,
map}`:

- `sp_base = function.initial_sp_value()` — the `InitialVar(stack_vn)` output,
  the canonical SP terminal (the same `ValueId` `decompose_sp`/`reaching_store`
  compare against).
- `map: FxHashMap<ValueId, Abs>` — results for the current index.

`eval_node(value) -> Option<Abs>` reads its inputs' results from `map` (never
recurses):

- `value == sp_base` → `SpRel { base: sp_base, offset: 0 }`.
- `IntConst` → `Const(int_const_u128(value))`.
- `IntBinaryOp(Add)` → combine: `(Const,Const)` via `eval_int_binary`;
  `(SpRel{b,o}, Const(c))` / `(Const(c), SpRel{b,o})` →
  `SpRel{b, o + sign_interpret(c)}`; `(SpRel,SpRel)` → `None`.
- every other `IntBinaryOp` / `IntUnaryOp` / `Truncate` / `Extend` / `Popcount`
  / `Lzcount` / `IntCmpOp` → require all inputs `Const` (an `SpRel` operand →
  `None`), compute via the existing pure `eval_int_*` helpers, return `Const`.
- `Load` → see §2.
- `Phi` → all-arms-agree: every value arm must resolve to the *same* `Abs`
  (`Const` by value, `SpRel` by `(base, offset)`), else `None`. Sound regardless
  of which arm control takes, because all arms are equal.
- anything else → `None`.

### 2. `Load` evaluation — ConstFold's two memory passes, read-only

`eval_load(load, value)` splits on the address's `Abs`:

- **`Const(c)` (LoadReadOnly).** `c` is an absolute address: `rom.read(c, buf)` →
  `endianness.read_uint` → mask to the load type → `Const`. `None` if no ROM or
  unmapped. Mirrors `load_readonly/mod.rs:138-173`, mutation stripped.
- **`SpRel{base, offset}` (LoadForward).** The index has already been folded
  into `offset` (a concrete `i64`), so call the existing
  **`SpAliasCfg::reaching_store(function, mem, base, offset, load_size)`**
  (`sp_expr/cfg.rs`) — the shared `MemPhi`-sound memory-SSA store lookup. It
  walks the load's memory token backward (purely structural, index-independent),
  decomposes each candidate *store's own* address (`sp + constant`,
  index-independent) and returns the covering store's `data`, `store_offset`,
  `size`. We then require `store_offset == offset` (exact anchor), read the
  store's `data` as a constant (`int_const_u128` — jump-table targets are always
  constants on the converged graph), and reshape from store width to load width
  (`Endianness`-aware, mirroring `LoadForward::narrow`). It honors
  `alias_mode`/call-clobbering exactly as the pass does (so the `Strict` and
  call-clobber tests behave identically).

**Why `reaching_store` works without graph mutation.** It never reads the load's
address node. The load offset is supplied by us (folded from the seeded index in
the `SpRel` domain); the store offsets are decomposed from their own addresses,
which are `sp + constant` with no index term. eval stays read-only; the only
index-dependent input is the `offset` argument. This is the asymmetry the
clone+pipeline got via `ConstantFold` rewriting the address — here the rewrite
is replaced by folding the index into `offset`.

### 3. Cone construction + per-index driver

Build the cone as backward reachability from the dispatch value over **value
edges only** (`value_type_opt(input).is_some()` — the same edge set
`find_index_candidates` already walks; the memory token is *not* followed). The
store's data is reached at eval time via `reaching_store` + `int_const_u128`, not
through the cone — so no memory-edge traversal and no recursion are needed.
Topologically order the cone (postorder over inputs, producers before
consumers); the order is index-independent, so compute it **once** per
`classify_table_dispatch` call.

```text
order = topo(value_cone(dispatch_value))   // once per classify call
for i in lo..=hi:
    map = { idx_value: Const(i) }
    for val in order:
        if map.contains(val) { continue }      // skip the seed
        if let Some(a) = eval_node(val) { map.insert(val, a) }
    targets.push(u64::try_from(map[dispatch_value].as_const()?)?)   // any miss / SpRel ⇒ reject
```

Cost: O(cone × range) per candidate, versus today's O(pipeline × graph × range).

### 4. Soundness semantics

Preserve the existing **never-over-approximate** contract (range analysis gives a
sound upper bound; enumeration treats `lo..=hi` as the complete target set):

- Any value that fails to resolve (`None`, or a non-`Const` dispatch result) ⇒
  the whole candidate is rejected (`enumerate_targets` returns `None`), identical
  to today when a wrong candidate fails to fold.
- **Cycles** (loop-carried phi): a back-edge input is absent from `map` when its
  consumer is evaluated ⇒ `None` ⇒ reject. The flat RPO pass needs no explicit
  cycle guard.
- **Value-`Phi`**: all-arms-agree (§1).

### 5. Clone deletion

- Remove `Clone` from `Function`'s derive (`crates/strider-ir/src/function/data.rs:96`).
- Delete the manual `Clone` impl on the generic `strider_graph::Graph<N, V, C>`
  (`crates/strider-graph/src/graph.rs`).
- Delete both clone sites + the `compact`/remap + the per-index `EditFunction` /
  `replace_value` / `default_pipeline` machinery in `table.rs`.
- Compiler-enforced — exploration confirms `table.rs` was the only consumer.

### 6. Testing (TDD)

- The existing cross-arch `table_tests.rs` suite (x86 / x64 / aarch64 / arm /
  thumb / mips32 / ppc32; mips64 stays a known gap; includes the SP-rooted
  stack-table and alias-mode cases) is the behavioral gate — the new evaluator
  must keep every test green.
- Add unit tests for the evaluator's arithmetic + fail-closed paths; rely on
  `table_tests.rs` for the load/forward/phi/reshape end-to-end paths (isolated
  graph fixtures for those cost more than the lifted-binary coverage already
  provides).
- Clone removal is a compile-time gate.
- Gate on the full workspace: `cargo test --workspace` + `cargo clippy
  --workspace` + `pytest` before requesting merge.

## Out of scope

- A GP/GOT-rooted (PIC) table base — a third symbolic base; stays the existing
  mips64 gap, unchanged.
- Non-constant stack-table store data (computed-then-spilled targets) — jump
  targets are constants; `int_const_u128` on the store data suffices.
- Other optimization passes' participation in jump-table resolution.
- Range analysis / candidate detection (`find_index_candidates`), unchanged.
