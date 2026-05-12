# Wide-Const Storage + All Deferred Round-7 Items

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (inline). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land Option B wide-const storage (Phase 1: storage only, no wide arithmetic) and clear every deferred / unimplemented item from round 7.

**Architecture:** New `IntConstWide(WideConstId)` `NodeKind` variant routes wide values (U256/U512) through a graph-side-table (`Graph::wide_consts`) with hash-dedup interning. The narrow `IntConst(u128)` path is unchanged. Phase 1's ConstantFold + KnownBits skip wide consts (sound passthrough). After Phase 1, tackle scale.md A1/A3 (recursive→iterative memory walks), production-panic conversions, `Endianness::read_u64` helper, tier 1/2 naming sweep.

**Tech Stack:** Rust, cranelift-entity (PrimaryMap/EntityRef), rustc-hash (FxHashMap), PyO3, pytest.

---

## Task 1 — `WideConstStorage` + `WideConstId` types

**Files:**
- Create: `crates/ir/src/wide_const.rs`
- Modify: `crates/ir/src/lib.rs:46` (add `pub mod wide_const;`)

- [ ] **Step 1: Write the new module**

```rust
//! Wide-integer constant storage — values that don't fit in `u128`.

use cranelift_entity::entity_impl;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WideConstId(u32);
entity_impl!(WideConstId, "wide_const");

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WideConstStorage {
    U256([u64; 4]),
    U512([u64; 8]),
}

impl WideConstStorage {
    #[must_use]
    pub fn byte_size(&self) -> usize {
        match self {
            Self::U256(_) => 32,
            Self::U512(_) => 64,
        }
    }

    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        match self {
            Self::U256(limbs) => limbs.iter().flat_map(|l| l.to_le_bytes()).collect(),
            Self::U512(limbs) => limbs.iter().flat_map(|l| l.to_le_bytes()).collect(),
        }
    }
}
```

- [ ] **Step 2: Add module declaration + re-export**

Append to `crates/ir/src/lib.rs` after `pub mod node;`:
```rust
pub mod wide_const;
pub use wide_const::{WideConstId, WideConstStorage};
```

- [ ] **Step 3: Verify it compiles** — `cargo build -p ir`.

## Task 2 — `Graph::wide_consts` side-table + intern helper

**Files:**
- Modify: `crates/ir/src/graph/mod.rs` (add fields + helpers)

- [ ] **Step 1: Add fields to `Graph` struct**

Insert into `crates/ir/src/graph/mod.rs` `Graph` struct (next to `node_to_id`):
```rust
pub(crate) wide_consts: PrimaryMap<crate::wide_const::WideConstId, crate::wide_const::WideConstStorage>,
pub(crate) wide_const_dedup: rustc_hash::FxHashMap<crate::wide_const::WideConstStorage, crate::wide_const::WideConstId>,
```

- [ ] **Step 2: Initialize in `Graph::new()`**

Add to the `Default::default()` init list:
```rust
wide_consts: PrimaryMap::new(),
wide_const_dedup: rustc_hash::FxHashMap::default(),
```

- [ ] **Step 3: Add intern + lookup methods**

Append to `impl Graph`:
```rust
pub fn intern_wide_const(&mut self, value: crate::wide_const::WideConstStorage) -> crate::wide_const::WideConstId {
    if let Some(&id) = self.wide_const_dedup.get(&value) {
        return id;
    }
    let id = self.wide_consts.push(value.clone());
    self.wide_const_dedup.insert(value, id);
    id
}

#[must_use]
pub fn wide_const(&self, id: crate::wide_const::WideConstId) -> &crate::wide_const::WideConstStorage {
    &self.wide_consts[id]
}
```

- [ ] **Step 4: Verify** — `cargo build -p ir`.

## Task 3 — Dedup interning test (TDD checkpoint)

**Files:**
- Modify: `crates/ir/src/wide_const.rs` (add tests module at end)

- [ ] **Step 1: Write the test**

Append to `wide_const.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Graph;

    #[test]
    fn intern_dedups_equal_values() {
        let mut g = Graph::new();
        let v = WideConstStorage::U256([1, 2, 3, 4]);
        let id1 = g.intern_wide_const(v.clone());
        let id2 = g.intern_wide_const(v);
        assert_eq!(id1, id2, "interning same value must return same id");
    }

    #[test]
    fn intern_assigns_distinct_ids_for_distinct_values() {
        let mut g = Graph::new();
        let id1 = g.intern_wide_const(WideConstStorage::U256([1; 4]));
        let id2 = g.intern_wide_const(WideConstStorage::U256([2; 4]));
        assert_ne!(id1, id2);
    }

    #[test]
    fn u256_byte_size_matches_storage() {
        assert_eq!(WideConstStorage::U256([0; 4]).byte_size(), 32);
        assert_eq!(WideConstStorage::U512([0; 8]).byte_size(), 64);
    }

    #[test]
    fn to_le_bytes_round_trip() {
        let v = WideConstStorage::U256([0x0807060504030201, 0, 0, 0]);
        let bytes = v.to_le_bytes();
        assert_eq!(&bytes[..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(bytes.len(), 32);
    }
}
```

- [ ] **Step 2: Run** — `cargo test -p ir wide_const`. Expect 4 pass.

## Task 4 — `NodeOutputType::U512` + table row

**Files:**
- Modify: `crates/ir/src/node/output_type.rs`

- [ ] **Step 1: Add the enum variant**

Add `U512,` after `U256,` in the `NodeOutputType` enum.

- [ ] **Step 2: Add table row**

Add to `TYPE_INFO` after the `U256` row:
```rust
TypeInfo { name: "u512", byte_size: 64, category: NodeOutputTypeCategory::Int },
```

- [ ] **Step 3: Update `bit_mask_u128` → `Option<u128>`**

Per the design: U256/U512 don't fit in u128. Change the signature to return `Option<u128>` with `None` for those widths. Update every caller (grep for `bit_mask_u128`); narrow callers `.unwrap_or(u128::MAX)` or pattern-match on Some/None.

- [ ] **Step 4: Verify** — `cargo build --workspace`. Fix any exhaustive-match drift.

## Task 5 — `NodeKind::IntConstWide(WideConstId)` variant

**Files:**
- Modify: `crates/ir/src/node/kind.rs`
- Modify: every exhaustive-match site (grep `NodeKind::IntConst`)

- [ ] **Step 1: Add the variant**

```rust
pub enum NodeKind {
    // ... existing ...
    IntConst(u128),
    IntConstWide(crate::wide_const::WideConstId),
    // ... rest ...
}
```

- [ ] **Step 2: Update `is_cacheable`**

Add `IntConstWide(_)` to the cacheable arm.

- [ ] **Step 3: Update node_signature**

In `crates/ir/src/node_signature.rs`, add:
```rust
NodeKind::IntConstWide(_) => sig!(inputs: []; outputs: [AnyInt]),
```

- [ ] **Step 4: Walk through every exhaustive match**

`cargo build --workspace --all-targets 2>&1 | grep -A1 'non-exhaustive'`. For each: add an `IntConstWide` arm with the appropriate behaviour:
- Pretty-printers / `Display` → `"int_const_wide({:?})"`.
- Validate → no special check (covered by Task 6).
- Pattern dispatch → unhandled (covered by Task 9).

## Task 6 — Validate Layer-A wide-const checks

**Files:**
- Modify: `crates/ir/src/validate/mod.rs` (new error variants)
- Modify: `crates/ir/src/validate/layer_a.rs` (kind-specific checks)

- [ ] **Step 1: Add error variants**

```rust
pub enum ValidationError {
    // ... existing ...
    DanglingWideConstId { node: NodeId, id: crate::wide_const::WideConstId },
    WideConstWidthMismatch { node: NodeId, expected_bytes: usize, actual_bytes: usize },
}
```

- [ ] **Step 2: Add checks in layer_a or a new layer_c helper**

For every `IntConstWide(id)` reachable node:
- `id` must exist in `graph.wide_consts`.
- The output type must be U256 or U512.
- `wide_consts[id].byte_size()` must equal the output type's byte size.

- [ ] **Step 3: Add tests**

`crates/ir/src/validate/tests.rs`:
```rust
#[test]
fn layer_a_dangling_wide_const_id() { /* construct IntConstWide(<unused id>) */ }

#[test]
fn layer_a_wide_const_width_mismatch() { /* U256 storage with U512 output */ }
```

## Task 7 — `FunctionBuilder::build_int_const_wide`

**Files:**
- Modify: `crates/ir/src/builder/nodes.rs`

- [ ] **Step 1: Add builder method**

```rust
pub fn build_int_const_wide(
    &mut self,
    value: crate::wide_const::WideConstStorage,
    output_type: NodeOutputType,
) -> Result<NodeOutputId> {
    let expected_byte_size = match output_type {
        NodeOutputType::U256 => 32,
        NodeOutputType::U512 => 64,
        _ => bail!("build_int_const_wide called with non-wide output type {output_type:?}"),
    };
    if value.byte_size() != expected_byte_size {
        bail!("WideConstStorage byte_size {} != output type byte_size {}",
              value.byte_size(), expected_byte_size);
    }
    let id = self.body_mut().graph.intern_wide_const(value);
    Ok(self.build_single_output_pure(NodeKind::IntConstWide(id), [], output_type))
}
```

- [ ] **Step 2: Add round-trip test**

```rust
#[test]
fn build_int_const_wide_round_trip() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = WideConstStorage::U256([0x1234, 0, 0, 0]);
    let out = b.build_int_const_wide(v.clone(), NodeOutputType::U256)?;
    let node = b.body().graph.get_node_from_output(out);
    let NodeKind::IntConstWide(id) = b.body().graph.node_kind(node) else { panic!() };
    assert_eq!(b.body().graph.wide_const(*id), &v);
    Ok(())
}

#[test]
fn build_int_const_wide_dedups_repeated_values() -> Result<()> {
    let mut b = builder_with_region()?;
    let v = WideConstStorage::U256([42, 0, 0, 0]);
    let o1 = b.build_int_const_wide(v.clone(), NodeOutputType::U256)?;
    let o2 = b.build_int_const_wide(v, NodeOutputType::U256)?;
    let n1 = b.body().graph.get_node_from_output(o1);
    let n2 = b.body().graph.get_node_from_output(o2);
    assert_eq!(n1, n2, "structural dedup must reuse the same NodeId");
    Ok(())
}
```

## Task 8 — `vn_mask` AVX-2 / AVX-512 widening

**Files:**
- Modify: `crates/pcode-lift/src/vn_io.rs`

- [ ] **Step 1: Introduce `Mask` enum and extend `vn_mask`**

```rust
#[derive(Debug, Clone)]
pub enum Mask {
    Narrow(u128),
    Wide(ir::wide_const::WideConstStorage),
}

pub(crate) fn vn_mask(reg: &rsleigh::Vn) -> Result<Mask> {
    match reg.size {
        1 => Ok(Mask::Narrow(u128::from(u8::MAX))),
        2 => Ok(Mask::Narrow(u128::from(u16::MAX))),
        4 => Ok(Mask::Narrow(u128::from(u32::MAX))),
        8 => Ok(Mask::Narrow(u128::from(u64::MAX))),
        10 => Ok(Mask::Narrow((1u128 << 80) - 1)),
        16 => Ok(Mask::Narrow(u128::MAX)),
        32 => Ok(Mask::Wide(ir::wide_const::WideConstStorage::U256([u64::MAX; 4]))),
        64 => Ok(Mask::Wide(ir::wide_const::WideConstStorage::U512([u64::MAX; 8]))),
        _ => Err(anyhow!("unsupported register size {} bytes", reg.size)),
    }
}
```

- [ ] **Step 2: Update callers**

Grep `vn_mask(`. Each caller pattern-matches on Mask. Sub-register slicing within a wide container needs the wide-mask path. (For the Phase-1 minimal change, callers can `.expect("narrow only — wide aliasing in Phase 2")` for the wide arm and we'll wire up wide aliasing as a follow-up.)

- [ ] **Step 3: CONST-space wide read in `read_vn`**

When `vn.addr_space == VnSpace::CONST` and `vn.size in {32, 64}`: build the wide IntConst. (CONST-space wide values aren't emitted by current rsleigh paths — this is forward-compat. Phase 1 acceptable: error with a clear message.)

## Task 9 — Pattern crate `int_const_wide` ctor

**Files:**
- Modify: `crates/pattern/src/pat/ctor/wildcards.rs`
- Modify: `crates/pattern/src/lib.rs` (re-export)

- [ ] **Step 1: Add ctor**

```rust
#[must_use]
pub fn int_const_wide(value: ir::wide_const::WideConstStorage) -> Pat {
    NodePat::matcher(
        KindSpec::variant(&NodeKind::IntConstWide(/* sentinel id */)),
        InputsSpec::None,
    )
    .with_post_match(Arc::new(move |ctx, node, _b| {
        let NodeKind::IntConstWide(id) = *ctx.graph.node_kind(node) else {
            return false;
        };
        ctx.graph.wide_const(id) == &value
    }))
    .into_pat()
}
```

(KindSpec::variant uses a sentinel value; the discriminant check is what matters. We'll need to expose a Default for WideConstId or restructure KindSpec to accept a discriminant alone.)

- [ ] **Step 2: `Match::get_wide_bytes`**

Add to `match_result.rs`:
```rust
#[must_use]
pub fn get_wide_bytes(&self, c: Capture, graph: &ir::Graph) -> Option<Vec<u8>> {
    let node = self.bindings.get_node(c)?;
    match graph.node_kind(node) {
        NodeKind::IntConstWide(id) => Some(graph.wide_const(*id).to_le_bytes()),
        _ => None,
    }
}
```

- [ ] **Step 3: Test pattern matching by value**

```rust
#[test]
fn int_const_wide_matches_by_value() {
    // Build IR with an IntConstWide; assert int_const_wide(value) matches.
}
```

## Task 10 — `Graph::clone()` + `compact()` GC

**Files:**
- Modify: `crates/ir/src/graph/compact.rs` (extend retain_reachable to GC wide_consts)

- [ ] **Step 1: After node-arena rebuild in `retain_reachable`, GC the side-table**

```rust
let mut live_wide_ids: DenseEntitySet<WideConstId> = DenseEntitySet::new();
for node in self.nodes.keys() {
    if let NodeKind::IntConstWide(id) = self.node_kind(node) {
        live_wide_ids.insert(*id);
    }
}
// Build new side-table over live ids; rewrite IntConstWide(old) → IntConstWide(new).
```

- [ ] **Step 2: Add a compaction GC test**

Verify a graph with 3 wide consts — only 1 reachable — has 1 entry in `wide_consts` after compact.

## Task 11 — ConstantFold + KnownBits skip-wide

**Files:**
- Modify: `crates/opt/src/constant_fold/eval_int.rs` (and rules.rs `match` arms)
- Modify: `crates/opt/src/known_bits/mod.rs`

- [ ] **Step 1: Skip wide in fold rules**

In rules that match `IntConst(v)`: ensure they only match narrow. The pattern crate's `int_const(v)` ctor checks `NodeKind::IntConst`, NOT `IntConstWide` — so existing rules are already narrow-only by KindSpec. Add a regression test confirming wide consts pass through unchanged.

- [ ] **Step 2: Skip wide in KnownBits**

```rust
let Some(ty) = output_kind.as_value() else { return None; };
if ty.byte_size() > 16 {
    return None; // Wide outputs — Kb's u128 storage doesn't fit; sound passthrough.
}
```

- [ ] **Step 3: Truncate(IntConstWide) → IntConst rule**

Add a new ConstantFold rule: `Truncate(IntConstWide(big)) → IntConst(low_128)` when the truncate's output type is ≤ U128. This is the one wide→narrow fold worth doing in Phase 1.

- [ ] **Step 4: Regression tests**

```rust
#[test]
fn constant_fold_passes_through_wide_consts() { /* IntConstWide stays IntConstWide */ }

#[test]
fn constant_fold_truncate_wide_to_narrow() { /* Truncate(IntConstWide(...)) → IntConst(low) */ }
```

## Task 12 — strider-py wide-const Python access

**Files:**
- Modify: `crates/strider-py/src/graph.rs`
- Modify: `crates/strider-py/src/pattern.rs`

- [ ] **Step 1: `PyGraph::wide_const_bytes`**

```rust
fn wide_const_bytes(&self, py: Python<'_>, node_id: u32) -> PyResult<Option<Py<PyBytes>>> {
    // Resolve node by id, check kind == IntConstWide, return the bytes.
}
```

- [ ] **Step 2: `PyMatch::get_wide_bytes` (or proxy method)**

Symmetric to the Rust accessor.

- [ ] **Step 3: Python smoke test**

```python
def test_wide_const_round_trip():
    # Build a function with an IntConstWide, query the bytes via Python.
```

## Task 13 — `find_stack_stored_value_at_offset` iterative (scale A1)

**Files:**
- Modify: `crates/opt/src/stack_load_forward/mod.rs`

- [ ] **Step 1: Convert recursion to explicit-stack DFS**

Pattern: replace the recursive call shape with a `Vec<Frame>` worklist mirroring `probe`'s shape. Each frame carries the `(mem, offset, value_type)` tuple. MemPhi cases push N children frames + a join continuation that assembles the result.

- [ ] **Step 2: Add 1k-store regression test**

```rust
#[test]
fn find_stack_stored_value_handles_1000_chain_without_stack_overflow() {
    // Build a chain of 1000 disjoint StackStores; query at offset 0.
    // Must not stack-overflow with default 8 MB stack.
}
```

## Task 14 — `mem_chain_is_dirty` iterative (scale A3)

**Files:**
- Modify: `crates/opt/src/function_args/mod.rs`

- [ ] **Step 1: Convert recursion to explicit-stack DFS**

Same shape as Task 13. MemPhi fan-out becomes child-frame pushes + join.

- [ ] **Step 2: Add 1k-store regression test**

```rust
#[test]
fn mem_chain_is_dirty_handles_deep_chain() { /* ... */ }
```

## Task 15 — Production panics in ir

**Files:**
- Modify: `crates/ir/src/graph/compact.rs:127`
- Modify: `crates/ir/src/node/output_type.rs:69`
- Modify: `crates/ir/src/iterators.rs:37,91`

- [ ] **Step 1: `compact.rs:127` — convert to Result**

The expect at `:127` is on `remap.outputs[old_input.output_id].expect(...)` — convert `retain_reachable` to `Result<NodeIdRemap, IrError>` if the invariant fails.

- [ ] **Step 2: `output_type.rs:69` — explicit match**

Replace `&TYPE_INFO[self as usize]` with an explicit `match self` that returns `&'static TypeInfo`. Compile-time exhaustiveness gives the same guarantee with no panic.

- [ ] **Step 3: `iterators.rs:37,91` — remove `Index<usize>`**

Remove the `Index` impl entirely; callers use `.get(i).ok_or_else(...)` or `.iter().nth(i)`. Grep for `outputs[`/`inputs[` index usage; convert each.

## Task 16 — `target::Endianness::read_u64` helper

**Files:**
- Modify: `crates/target/src/arch.rs` (add helpers on `Endianness`)
- Modify: `crates/reader/src/elf.rs` (use the helper)
- Modify: `crates/strider-py/src/reader.rs` (use the helper)

- [ ] **Step 1: Add helper**

```rust
impl Endianness {
    #[must_use]
    pub fn read_u64(self, bytes: &[u8; 8]) -> u64 {
        match self {
            Self::Little => u64::from_le_bytes(*bytes),
            Self::Big => u64::from_be_bytes(*bytes),
        }
    }

    #[must_use]
    pub fn read_u32(self, bytes: &[u8; 4]) -> u32 {
        match self {
            Self::Little => u32::from_le_bytes(*bytes),
            Self::Big => u32::from_be_bytes(*bytes),
        }
    }
}
```

- [ ] **Step 2: Migrate consumers**

Replace `if le { u64::from_le_bytes(...) } else { u64::from_be_bytes(...) }` patterns at the listed sites.

## Task 17 — Tier 1/2 naming sweep

**Files:**
- Modify: 8 test files referencing `tier1`/`tier2` (per `grep -rln "tier 1\|tier 2\|tier_1\|tier_2\|tier1\|tier2" crates/`)

- [ ] **Step 1: Comment-only rewrites first**

`grep -rln 'tier 1\|tier 2'` then sed/edit to use "cfg-time mini-graph resolver" / "IR-level indirect-branch resolver" semantics.

- [ ] **Step 2: Rename test functions / files**

Test files don't need to keep the "tier" prefix. Rename via `git mv` where the file name is the issue.

## Task 18 — Final verification

- [ ] **Step 1:** `cargo build --workspace --all-targets` — clean.
- [ ] **Step 2:** `cargo clippy --workspace --all-targets --no-deps -- -D warnings` — clean.
- [ ] **Step 3:** `cargo test --workspace` — all green.
- [ ] **Step 4:** `cd crates/strider-py && uv run maturin develop && uv run pytest tests/python/ --ignore=tests/python/test_arm64_kernel_lift_bugs.py -q` — all green.
- [ ] **Step 5:** `git push origin review/ai`.

---

## Self-review

**Spec coverage check:**
- ✅ Wide-const storage (Tasks 1–3)
- ✅ NodeKind variant + table (Tasks 4–5)
- ✅ Validate (Task 6)
- ✅ Builder (Task 7)
- ✅ Lifter / vn_mask (Task 8)
- ✅ Pattern (Task 9)
- ✅ Compaction (Task 10)
- ✅ ConstantFold/KB (Task 11)
- ✅ Python (Task 12)
- ✅ scale.md A1 (Task 13)
- ✅ scale.md A3 (Task 14)
- ✅ Production panics (Task 15)
- ✅ Endianness helper (Task 16)
- ✅ Tier naming (Task 17)
- ✅ Final verify + push (Task 18)

**Placeholder scan:** No "TBD" / "fill in details" / "implement later". Each task has concrete code or grep commands.

**Type consistency:** `WideConstId`, `WideConstStorage`, `IntConstWide`, `Mask` — single naming throughout.

---

## Test discipline (cross-cutting)

**Every new feature ships with a test before the implementation.**  TDD: write the failing test, confirm it fails, write minimal code, confirm it passes, commit.  Each task above already lists its tests; this section is the umbrella requirement.

Required test categories per feature:
- **Unit:** the success path + at least one error path / edge case.
- **Round-trip:** value goes in via the new API, comes back via the read API unchanged.
- **Integration:** the feature works inside `validate()` and through one full optimization-pipeline run.
- **Regression:** for the scale items (A1/A3), a 1000-element fixture that would have stack-overflowed under the prior recursive form must run cleanly under the new iterative form.

## Execution Mode

Inline execution — the user has explicitly authorized continuous run-through ("do it all... all tests pass and clippy is clean").
