# Unify `IntConst` behind a single interned `ConstId` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the dual `IntConst` payload (`IntPayload::Small(u64)` inline vs `IntPayload::Wide(WideConstId)` interned) with a single interned `ConstId`, so every integer constant has one representation and one SSoT.

**Architecture:** All integer constants are interned in `Function::const_interner` (`EntityInterner<ConstId, ConstValue>`), where `ConstValue { Bits(u128), Wide(Box<[u64]>) }`. Storage dedups **by value magnitude** (value ≤ u128 ⇒ `Bits`; wider ⇒ `Wide` boxed limbs); the constant's **width** lives on the node's output `ValueKind`, not in storage. Canonicalisation (mask-to-width + intern) concentrates in one SSoT, `Function::intern_int_const`. The node `NodeKind::IntConst(ConstId)` dedups structurally on `(kind, output_kinds)`, so `IntConst(v):I80` and `IntConst(v):I128` stay distinct nodes off one shared `ConstId`.

**Tech Stack:** Rust workspace; `cranelift-entity` (`entity_impl!`, `EntityInterner`); criterion benches (`strider-orchestrator/benches/scaling.rs`, `strider-opt/benches/pipeline.rs`).

## Global Constraints

- Branch `feature/const-id-unify` (already created from `develop`). Prompt the user before merging anywhere.
- Naming (verbatim): id `ConstId`; value type `ConstValue` with variants `Bits(u128)` and `Wide(Box<[u64]>)`; interner field `Function::const_interner`; intern method `intern_const`; read accessors `const_value` / `const_value_opt`; canonicalisation SSoT `Function::intern_int_const` / `intern_int_const_limbs`.
- `ConstValue` dedups **by value magnitude only** (≤ u128 ⇒ `Bits`, else `Wide`); width is carried by the node output `ValueKind`. A value that fits `u128` is ALWAYS `Bits`, for any declared type (I1..I512).
- Reads go through `IRViewer` accessors; never match `NodeKind::IntConst(..)`'s inner id to get a value.
- Measure-first gate: lift+optimize regression must be **≤ 3%** vs the `develop` baseline, or the unify does not merge (boxing + doc fixes may still merge; see Task 7).
- No `IntPayload` type after this change. No `WideConstStorage` / `WideConstId` / `wide_const*` identifiers after this change.
- Full workspace `cargo test` + `cargo clippy --workspace` + `pytest` must pass before any merge (gate on real exit codes, never `| tail`).

---

### Task 1: Capture the performance baseline

**Files:**
- Create: `docs/superpowers/plans/const-id-bench-baseline.md` (records numbers; git-tracked).

**Interfaces:**
- Produces: a saved criterion baseline named `before` for both benches, and a recorded summary file the final task (Task 7) compares against.

The branch tip currently differs from `develop` only by the spec + this plan (no code change), so benching the branch tip now measures `develop` behavior.

- [ ] **Step 1: Confirm no code delta vs develop**

Run: `git diff --stat develop -- crates/ | tail -1`
Expected: no lines under `crates/` (only `docs/` differs). If any `crates/` file differs, stop and investigate.

- [ ] **Step 2: Save the lift+optimize baseline**

Run: `cargo bench -p strider-orchestrator --bench scaling -- --save-baseline before`
Expected: criterion prints per-benchmark times and `Saving baseline "before"`.

- [ ] **Step 3: Save the optimizer baseline**

Run: `cargo bench -p strider-opt --bench pipeline -- --save-baseline before`
Expected: criterion prints times and `Saving baseline "before"`.

- [ ] **Step 4: Record the headline numbers**

Create `docs/superpowers/plans/const-id-bench-baseline.md` with the wall-clock means criterion reported for each benchmark id in both benches (copy the `time: [low mean high]` lines verbatim, one bullet per benchmark id). This file is the human-readable record Task 7 diffs against.

- [ ] **Step 5: Commit**

```bash
git add docs/superpowers/plans/const-id-bench-baseline.md
git commit -m "bench: record const-id-unify lift+opt baseline"
```

---

### Task 2: strider-ir core unify

This is the atomic core change: deleting `IntPayload` ripples through every site that matches `NodeKind::IntConst`. Consumer crates (lift/pattern/opt/py) will not compile until Tasks 3–6; that is expected — gate this task on `cargo test -p strider-ir` only.

**Files:**
- Rename + rewrite: `crates/strider-ir/src/wide_const.rs` → `crates/strider-ir/src/const_value.rs`
- Modify: `crates/strider-ir/src/lib.rs` (module + re-exports)
- Modify: `crates/strider-ir/src/node/kind.rs` (delete `IntPayload`; `IntConst(ConstId)`)
- Modify: `crates/strider-ir/src/function/data.rs` (interner field, accessors, `intern_int_const`, `create_node_attributed`, `gc_consts`)
- Modify: `crates/strider-ir/src/builder/builder_ext.rs` (`build_int_const`, `build_int_const_limbs`)
- Modify: `crates/strider-ir/src/viewer.rs` (`int_const_u128`, `int_const_wide_le_bytes`)
- Modify: `crates/strider-ir/src/validate/graph_invariants.rs` + `validate/mod.rs` (error variants + check)
- Modify: `crates/strider-ir/src/function/dot/label.rs`, `crates/strider-ir/src/function/dot/raw.rs`
- Modify tests: `crates/strider-ir/src/{const_value.rs,node/tests.rs,builder/tests.rs,validate/tests.rs,graph/tests.rs,walk/cast/tests.rs,function/dot/tests.rs}`

**Interfaces:**
- Produces:
  - `pub struct ConstId` (entity ref) in `const_value.rs`.
  - `pub enum ConstValue { Bits(u128), Wide(Box<[u64]>) }` with `fn fits_u128(&self) -> Option<u128>`, `fn to_le_bytes(&self, byte_size: usize) -> Vec<u8>`.
  - `Function::intern_int_const(&mut self, value: u128, ty: ValueType) -> ConstId`
  - `Function::intern_int_const_limbs(&mut self, limbs: &[u64], ty: ValueType) -> ConstId`
  - `Function::const_value(&self, ConstId) -> &ConstValue`, `const_value_opt(&self, ConstId) -> Option<&ConstValue>`, `intern_const(&mut self, ConstValue) -> ConstId`
  - `NodeKind::IntConst(ConstId)`
  - `IRBuilderExt::build_int_const(&mut self, val: impl Into<u128>, ty) -> Result<ValueId>` (now covers I1..I512), `build_int_const_limbs(&mut self, limbs: &[u64], ty) -> Result<ValueId>`
- Consumes: `EntityInterner` (`intern`/`get`/index), `ValueType::{bit_mask_u128, byte_size, is_integer, is_wide_int}`.

- [ ] **Step 1: Write the failing dedup-invariant test**

Add to `crates/strider-ir/src/node/tests.rs`:

```rust
#[test]
fn same_value_distinct_width_shares_const_id_distinct_node() {
    use crate::node::{NodeKind, ValueType};
    use crate::{IRBuilderExt, IRViewer};
    let mut f = crate::Function::default();
    // build_int_const interns by value; I80 and I128 both hold 42.
    let v80 = f.build_int_const(42u128, ValueType::I80).unwrap();
    let v128 = f.build_int_const(42u128, ValueType::I128).unwrap();
    let n80 = f.producer(v80);
    let n128 = f.producer(v128);
    // One interned ConstId (same value) ...
    let (NodeKind::IntConst(id80), NodeKind::IntConst(id128)) =
        (*f.node_kind(n80), *f.node_kind(n128))
    else {
        panic!("expected IntConst nodes")
    };
    assert_eq!(id80, id128, "equal value must share one ConstId");
    // ... but two distinct nodes (output type differs).
    assert_ne!(n80, n128, "different declared widths must be distinct nodes");
    assert_eq!(f.int_const_u128(v80), Some(42));
    assert_eq!(f.int_const_u128(v128), Some(42));
}
```

- [ ] **Step 2: Run it — expect a COMPILE failure**

Run: `cargo test -p strider-ir --lib same_value_distinct_width 2>&1 | tail -20`
Expected: does not compile (`ConstId`, the new `build_int_const` width support, etc. don't exist yet). That is the failing state; proceed to implement.

- [ ] **Step 3: Rewrite `wide_const.rs` → `const_value.rs`**

`git mv crates/strider-ir/src/wide_const.rs crates/strider-ir/src/const_value.rs`, then replace its non-test body with:

```rust
//! Interned integer-constant values. Every `NodeKind::IntConst(ConstId)`
//! references one entry in `crate::Function::const_interner`.
//!
//! Storage dedups by VALUE MAGNITUDE: a value that fits `u128` is `Bits`
//! (covers I1..I512 whose value ≤ u128); a value that needs more than 128
//! bits is `Wide` (boxed little-endian limbs, I256/I512). The constant's
//! WIDTH is carried by the node's output `ValueKind`, never by this storage,
//! so `IntConst(42):I80` and `IntConst(42):I128` share one `ConstId` and are
//! distinguished only at the node level (different output kind ⇒ different
//! dedup-cache key). Read values through `crate::IRViewer` accessors.

use cranelift_entity::entity_impl;

/// Dense id of an interned integer-constant value
/// (`crate::Function::const_interner`). Opaque; resolve via
/// `crate::Function::const_value`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstId(u32);
entity_impl!(ConstId, "const");

/// The interned value of an integer constant.
///
/// `Bits` holds any value ≤ 128 bits inline. `Wide` boxes the little-endian
/// limbs of a value that exceeds 128 bits (`limbs[0]` low, `limbs[N-1]` high);
/// only I256 (4 limbs) / I512 (8 limbs) reach it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstValue {
    /// Value ≤ 128 bits, held inline.
    Bits(u128),
    /// Value > 128 bits, boxed little-endian limbs.
    Wide(Box<[u64]>),
}

impl ConstValue {
    /// The value as `u128` if it fits (always for `Bits`; for `Wide` only
    /// when every limb above the low two is zero), else `None`.
    pub fn fits_u128(&self) -> Option<u128> {
        match self {
            Self::Bits(v) => Some(*v),
            Self::Wide(limbs) => {
                if limbs.iter().skip(2).all(|&l| l == 0) {
                    let lo = u128::from(*limbs.first().unwrap_or(&0));
                    let hi = u128::from(limbs.get(1).copied().unwrap_or(0));
                    Some((hi << 64) | lo)
                } else {
                    None
                }
            }
        }
    }

    /// Little-endian bytes zero-extended / truncated to `byte_size`.
    pub fn to_le_bytes(&self, byte_size: usize) -> Vec<u8> {
        let mut out = vec![0u8; byte_size];
        match self {
            Self::Bits(v) => {
                let b = v.to_le_bytes();
                let n = byte_size.min(b.len());
                out[..n].copy_from_slice(&b[..n]);
            }
            Self::Wide(limbs) => {
                let mut i = 0;
                for limb in limbs.iter() {
                    for byte in limb.to_le_bytes() {
                        if i >= byte_size {
                            break;
                        }
                        out[i] = byte;
                        i += 1;
                    }
                }
            }
        }
        out
    }
}
```

Replace the `#[cfg(test)] mod tests` block with tests covering: `intern_const` dedups equal values (one `Bits`, one `Wide`); distinct values get distinct ids; `fits_u128` (`Bits(v) → Some(v)`; `Wide([1,0,0,0]) → Some(1)`; `Wide([0,0,1,0]) → None`); `to_le_bytes` for a `Bits` at byte_size 10/16 and a `Wide([..]; 4)` at byte_size 32. (Mirror the structure of the old `wide_const.rs` tests; the construction helper is `Function::intern_const`.)

- [ ] **Step 4: Run const_value tests to verify they pass**

Run: `cargo test -p strider-ir --lib const_value 2>&1 | tail -15`
Expected: the `const_value` module tests pass (this module compiles independently of the node-kind change).

- [ ] **Step 5: Update `lib.rs` module + exports**

In `crates/strider-ir/src/lib.rs`: change `pub mod wide_const;` → `pub mod const_value;`. Remove `IntPayload` from the `node` re-export list (line ~73). Add `pub use const_value::{ConstId, ConstValue};` near the other public re-exports.

- [ ] **Step 6: Change the node kind**

In `crates/strider-ir/src/node/kind.rs`: delete the `IntPayload` enum (lines ~3–16) and its doc. Change the variant to `IntConst(crate::const_value::ConstId)`. Update the `IntConst` doc comment to: "An integer constant; the value is interned in `Function::const_interner`, read via `IRViewer::int_const_u128`." Leave `is_cacheable` / `is_const` (they already match `IntConst(..)`).

- [ ] **Step 7: Interner field + accessors + canonicalisation SSoT**

In `crates/strider-ir/src/function/data.rs`:
- Rename field `wide_const_interner` → `const_interner`, retype to `EntityInterner<crate::const_value::ConstId, crate::const_value::ConstValue>`. Update its doc to describe interning ALL integer constants.
- Rename `intern_wide_const` → `intern_const`, `wide_const` → `const_value`, `wide_const_opt` → `const_value_opt` (retyped to `ConstId`/`ConstValue`).
- Add the canonicalisation SSoT:

```rust
/// Interns the integer value `value`, masked to `ty`'s width, returning its
/// `ConstId`. The single canonicalisation point for ≤ u128 constants: equal
/// (masked) values share one id regardless of declared type.
pub fn intern_int_const(
    &mut self,
    value: u128,
    ty: crate::node::ValueType,
) -> crate::const_value::ConstId {
    let masked = value & ty.bit_mask_u128();
    self.const_interner
        .intern(crate::const_value::ConstValue::Bits(masked))
}

/// Interns a > 64-bit-limbed integer value (I256/I512), canonicalising to
/// `Bits` when the limbs fit `u128`. `limbs` is little-endian.
pub fn intern_int_const_limbs(
    &mut self,
    limbs: &[u64],
    _ty: crate::node::ValueType,
) -> crate::const_value::ConstId {
    let cv = crate::const_value::ConstValue::Wide(limbs.to_vec().into_boxed_slice());
    let canon = match cv.fits_u128() {
        Some(v) => crate::const_value::ConstValue::Bits(v),
        None => cv,
    };
    self.const_interner.intern(canon)
}
```

- In `create_node_attributed` (lines ~819–873): **delete** the `IntConst(IntPayload::Small/Wide)` canonicalisation match (lines ~850–867); constants now arrive pre-canonical (their `ConstId` was minted by `intern_int_const*`). Keep the `output_kinds` collection and the contributor/fingerprint loop. The `kind` passes through unchanged.

- [ ] **Step 8: `gc_consts` (compact path)**

Rename `gc_wide_consts` → `gc_consts` (lines ~1033–1080). Change the scan to collect ALL constant nodes:

```rust
fn gc_consts(&mut self) -> bool {
    use crate::const_value::ConstId;
    use crate::node::NodeKind;

    let mut live_old_ids: Vec<ConstId> = Vec::new();
    let mut const_nodes: Vec<NodeId> = Vec::new();
    for node in self.graph.all_node_ids() {
        if let NodeKind::IntConst(id) = *self.graph.node_kind(node) {
            const_nodes.push(node);
            live_old_ids.push(id);
        }
    }
    if live_old_ids.is_empty() {
        self.const_interner = Default::default();
        return false;
    }
    let mut new_interner: entity_utils::EntityInterner<
        ConstId,
        crate::const_value::ConstValue,
    > = entity_utils::EntityInterner::default();
    let mut old_to_new: FxHashMap<ConstId, ConstId> = FxHashMap::default();
    for old_id in live_old_ids {
        if old_to_new.contains_key(&old_id) {
            continue;
        }
        let value = self.const_interner[old_id].clone();
        let new_id = new_interner.intern(value);
        old_to_new.insert(old_id, new_id);
    }
    self.const_interner = new_interner;
    for node in const_nodes {
        if let NodeKind::IntConst(id) = self.graph.node_kind_mut(node)
            && let Some(&new_id) = old_to_new.get(id)
        {
            *id = new_id;
        }
    }
    true
}
```

Update the caller (the `compact` body near line ~1006) to call `gc_consts` and adjust its comment (now GCs all constants, not just wide ones).

- [ ] **Step 9: Builder**

In `crates/strider-ir/src/builder/builder_ext.rs`, replace `build_int_const` (lines ~202–250) and `build_int_const_wide` (lines ~253–290) with:

```rust
/// Builds an integer constant of `output_type` from a ≤ 128-bit value.
/// The value is masked to the type's width and interned; equal (value,
/// width) constants dedup. Covers I1..I512 (any value that fits `u128`).
fn build_int_const(&mut self, val: impl Into<u128>, output_type: ValueType) -> Result<ValueId> {
    if !output_type.is_integer() {
        return Err(anyhow!("build_int_const called with non-integer type {output_type:?}"));
    }
    let id = self.function_mut().intern_int_const(val.into(), output_type);
    Ok(self.build_single_output_pure(NodeKind::IntConst(id), [], output_type))
}

/// Builds a wide integer constant (I256/I512) from little-endian limbs.
/// Canonicalises to the inline form when the limbs fit `u128`.
fn build_int_const_limbs(&mut self, limbs: &[u64], output_type: ValueType) -> Result<ValueId> {
    if !output_type.is_wide_int() {
        return Err(anyhow!(
            "build_int_const_limbs called with non-wide output type {output_type:?}; \
             use build_int_const for ≤ I128"
        ));
    }
    let id = self.function_mut().intern_int_const_limbs(limbs, output_type);
    Ok(self.build_single_output_pure(NodeKind::IntConst(id), [], output_type))
}
```

- [ ] **Step 10: Read accessors**

In `crates/strider-ir/src/viewer.rs`, rewrite `int_const_u128` (the body, lines ~135–161) to a single interner read:

```rust
fn int_const_u128(&self, value: ValueId) -> Option<u128> {
    let ty = self.value_kind(value).as_value()?;
    if !ty.is_integer() {
        return None;
    }
    let NodeKind::IntConst(id) = *self.kind_of_value(value) else {
        return None;
    };
    let v = self.function().const_value_opt(id)?.fits_u128()?;
    Some(v & ty.bit_mask_u128())
}
```

Rewrite `int_const_wide_le_bytes` (lines ~205–224) to:

```rust
fn int_const_wide_le_bytes(&self, node: crate::node::NodeId) -> Option<Vec<u8>> {
    let [out] = self.node_outputs_exact::<1>(node).ok()?;
    let ty = self.value_kind(out).as_value()?;
    if !ty.is_wide_int() {
        return None;
    }
    let NodeKind::IntConst(id) = *self.node_kind(node) else {
        return None;
    };
    Some(self.function().const_value(id).to_le_bytes(ty.byte_size()))
}
```

(`int_const_i128` / `int_const_i64` / `int_const_val` / `bool_const_val` are unchanged — they delegate to `int_const_u128`.)

- [ ] **Step 11: Validation**

In `crates/strider-ir/src/validate/mod.rs`: rename error variants `DanglingWideConstId` → `DanglingConstId`, `WideConstWidthMismatch` → `ConstWidthMismatch` (adjust their fields/messages to reference `ConstId` / "value exceeds declared width").

In `crates/strider-ir/src/validate/graph_invariants.rs`, rewrite `check_graph_invariants_wide_consts` → `check_graph_invariants_consts`:

```rust
pub(super) fn check_graph_invariants_consts(
    function: &crate::Function,
    errs: &mut Vec<ValidationError>,
) {
    use crate::node::NodeKind;
    for node in function.reachable_node_ids() {
        let NodeKind::IntConst(id) = *function.node_kind(node) else {
            continue;
        };
        let Some(value) = function.const_value_opt(id) else {
            errs.push(ValidationError::DanglingConstId { node, id });
            continue;
        };
        let [out] = match function.node_outputs_exact::<1>(node) {
            Ok(o) => o,
            Err(_) => continue, // arity reported elsewhere
        };
        let Some(ty) = function.value_kind(out).as_value() else {
            continue;
        };
        // Every bit above the declared width must be zero (canonical masking).
        let too_wide = match value {
            crate::const_value::ConstValue::Bits(v) => v & !ty.bit_mask_u128() != 0,
            crate::const_value::ConstValue::Wide(limbs) => {
                limbs.len() * 64 > ty.bit_width() as usize
                    && limbs.iter().enumerate().any(|(i, &l)| {
                        (i + 1) * 64 > ty.bit_width() as usize && l != 0
                    })
            }
        };
        if too_wide {
            errs.push(ValidationError::ConstWidthMismatch { node, id });
        }
    }
}
```

Match the exact iteration helper the sibling checks in this file use (e.g. `reachable_node_ids` / the existing reachable iterator); keep the call site in `validate/mod.rs` renamed accordingly.

- [ ] **Step 12: Dot rendering**

In `crates/strider-ir/src/function/dot/label.rs` and `dot/raw.rs`: replace any `IntPayload::Small/Wide` match with reading the value via `const_value_opt(id)` (label) and via the accessor `int_const_u128` / `int_const_wide_le_bytes` where a value string is needed. For raw dump, print `IntConst(#<id>)` plus the `ConstValue` debug.

- [ ] **Step 13: Fix strider-ir's own tests**

Update every strider-ir test that names `IntPayload`, `WideConstStorage`, `WideConstId`, `wide_const`, `intern_wide_const`, `build_int_const_wide`:
- `IntPayload::Small(v)` literal kinds → `IntConst` built via `build_int_const(v, ty)` then read kind, or compare via `int_const_u128`.
- `build_int_const_wide(WideConstStorage::I256(arr), I256)` → `build_int_const_limbs(&arr, I256)`.
- `intern_wide_const` → `intern_const` / `intern_int_const*`.
Search: `rg -n 'IntPayload|WideConst|wide_const|build_int_const_wide' crates/strider-ir/src` and fix each. Do not weaken assertions — preserve what each test proves.

- [ ] **Step 14: Run the dedup test + full strider-ir suite**

Run: `cargo test -p strider-ir 2>&1 | tail -15`
Expected: all pass, including `same_value_distinct_width_shares_const_id_distinct_node`. Then `cargo clippy -p strider-ir 2>&1 | grep -E "warning|error" || echo clean`.

- [ ] **Step 15: Commit**

```bash
git add crates/strider-ir
git commit -m "refactor(ir): unify IntConst behind a single interned ConstId"
```

---

### Task 3: strider-lift consumer

**Files:**
- Modify: `crates/strider-lift/src/lift/cast.rs` (~line 91, `build_shift_const`)
- Modify: `crates/strider-lift/src/lift/arithmetic.rs` (~line 142, `build_all_ones`)
- Modify tests: `crates/strider-lift/src/lift/handler_tests.rs`

**Interfaces:**
- Consumes: `IRBuilderExt::build_int_const(u128, ty)`, `build_int_const_limbs(&[u64], ty)` from Task 2.

- [ ] **Step 1: Replace `WideConstStorage` construction in `cast.rs`**

The `match ty { I80 => WideConstStorage::I80(..), I128 => .., I256 => .., I512 => .. }` (lines ~91–96) feeding `build_int_const_wide` collapses. For I80/I128 use `build_int_const(value_u128, ty)`; for I256/I512 use `build_int_const_limbs(&limbs, ty)`. Show the new `build_shift_const` body in the edit (read the value/limbs the old arms produced and route by width: `if ty.byte_size() <= 16 { build_int_const(v, ty) } else { build_int_const_limbs(&limbs, ty) }`).

- [ ] **Step 2: Replace `WideConstStorage` construction in `arithmetic.rs`**

`build_all_ones` (lines ~142–147): I80/I128 → `build_int_const((1u128 << bits) - 1 or u128::MAX masked, ty)`; I256/I512 → `build_int_const_limbs(&[u64::MAX; N], ty)`.

- [ ] **Step 3: Fix lift tests naming**

`rg -n 'WideConst|wide_const|IntPayload|build_int_const_wide' crates/strider-lift/src` → update each to the new builder calls / accessor reads.

- [ ] **Step 4: Run lift suite**

Run: `cargo test -p strider-lift 2>&1 | tail -10`
Expected: all pass. Then `cargo clippy -p strider-lift 2>&1 | grep -E "warning|error" || echo clean`.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-lift
git commit -m "refactor(lift): build constants via unified ConstId builder"
```

---

### Task 4: strider-pattern consumer

**Files:**
- Modify: `crates/strider-pattern/src/template/mod.rs` (~lines 205–256, the `FnIntConst` instantiator)
- Modify: `crates/strider-pattern/src/typed/consts.rs` (~lines 52, 205, 246, 254, 317, 324, 361, 398)
- Modify: `crates/strider-pattern/src/matcher/builder.rs`, `matcher/graph.rs`, `template/builder.rs` (if they name the removed types — verify by grep)

**Interfaces:**
- Consumes: `build_int_const` / `build_int_const_limbs`; `IRViewer::int_const_val` / `int_const_u128`.

- [ ] **Step 1: Collapse the `FnIntConst` instantiator (`template/mod.rs`)**

The 4-way I80/I128/I256/I512 `intern_wide_const(WideConstStorage::..)` block (lines ~213–244) plus the `Small` arm (line ~256) collapse to: build via the function's interner — `if ty.byte_size() <= 16 { build via intern_int_const(value_u128, ty) } else { intern_int_const_limbs(&limbs, ty) }` — then `NodeKind::IntConst(id)`. Show the full new instantiator arm in the edit. (It already holds `builder.function_mut()`.)

- [ ] **Step 2: Simplify the matcher predicates (`typed/consts.rs`)**

- Discriminant exemplars (lines ~52, 205, 246, 317, 361) that built `NodeKind::IntConst(IntPayload::Small(0))` only to take a `std::mem::discriminant(..)`: replace the discriminant dance with a direct `matches!(k, NodeKind::IntConst(_))` structural check (no `ConstId` value needed). The variant is now payload-uniform, so "is this an int constant?" is one `matches!`.
- `Small`-only value filters (lines ~254, 324): replace `matches!(k, NodeKind::IntConst(IntPayload::Small(v)) if set.contains(&u128::from(*v)))` with a value read through the viewer — these matchers run with a `Function` in scope; use `int_const_u128(value).is_some_and(|v| set.contains(&v))`. If the matcher has only the `NodeKind` (no value id) at that point, keep a `matches!(k, NodeKind::IntConst(_))` structural gate and move the value check to where the `ValueId`/`Function` is available (mirror how the sibling value-predicate matchers in this file already read values).
- Bool exemplars (lines ~205, 398): `NodeKind::IntConst(..)` exemplar built via the interner path, or compared structurally.

- [ ] **Step 3: Fix pattern tests + any stragglers**

`rg -n 'IntPayload|WideConst|wide_const' crates/strider-pattern/src` → update each.

- [ ] **Step 4: Run pattern suite**

Run: `cargo test -p strider-pattern 2>&1 | tail -10`
Expected: all pass. Then `cargo clippy -p strider-pattern 2>&1 | grep -E "warning|error" || echo clean`.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-pattern
git commit -m "refactor(pattern): match/build constants via unified ConstId"
```

---

### Task 5: strider-opt consumer

**Files:**
- Modify: `crates/strider-opt/src/pipeline.rs` (~line 727 test assertion)
- Modify tests across `crates/strider-opt/src/opt/**/tests.rs` and `post_opt/**/tests.rs` that name the removed types.

**Interfaces:**
- Consumes: `IRViewer::int_const_u128` / `int_const_val`; `build_int_const`.

- [ ] **Step 1: Fix the pipeline test assertion**

`pipeline.rs:727` `matches!(kind, NodeKind::IntConst(IntPayload::Small(0x42)))` → read the value: assert `f.int_const_val(value) == Some(0x42)` (use the `ValueId` the test already has; if only the kind is in scope, fetch the node's output value first).

- [ ] **Step 2: Fix remaining opt tests**

`rg -n 'IntPayload|WideConst|wide_const|build_int_const_wide' crates/strider-opt/src` → update each to accessor reads / the new builder.

- [ ] **Step 3: Run opt suite**

Run: `cargo test -p strider-opt 2>&1 | tail -10`
Expected: all pass. Then `cargo clippy -p strider-opt 2>&1 | grep -E "warning|error" || echo clean`.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-opt
git commit -m "refactor(opt): read constants via accessors under unified ConstId"
```

---

### Task 6: strider-py, remaining crates, workspace + pytest green

**Files:**
- Modify: `crates/strider-py/src/node.rs` (~line 211), `crates/strider-py/src/function.rs` (~line 326)
- Any other crate flagged by a workspace-wide grep (orchestrator, reader, cfg) — verify.

**Interfaces:**
- Consumes: `IRViewer::int_const_wide_le_bytes` (rename-only impact; already used via accessor).

- [ ] **Step 1: Update py naming**

`rg -n 'WideConst|wide_const|IntPayload' crates/strider-py/src` → the py `wide_const_bytes` methods already call `int_const_wide_le_bytes`, so only identifier renames (if any) are needed. Keep the Python-facing method names (`wide_const_bytes`, `const_int`) unchanged — they are public API.

- [ ] **Step 2: Workspace-wide straggler sweep**

Run: `rg -n 'IntPayload|WideConstStorage|WideConstId|wide_const_interner|intern_wide_const|build_int_const_wide' crates/`
Expected: ZERO matches outside renamed identifiers. Fix any remaining (e.g. orchestrator/cfg/reader).

- [ ] **Step 3: Full workspace build + test**

Run: `cargo test --workspace 2>&1 | grep -E "test result: FAILED|error\[" ; echo "exit: ${PIPESTATUS[0]}"`
Expected: no FAILED/error lines; exit 0. (Gate on the real exit code, not a tail.)

- [ ] **Step 4: Clippy**

Run: `cargo clippy --workspace 2>&1 | grep -E "warning:|error" ; echo "exit: ${PIPESTATUS[0]}"`
Expected: clean, exit 0.

- [ ] **Step 5: Rebuild wheel + pytest**

Run: `cd crates/strider-py && uv run maturin develop && uv run pytest -q 2>&1 | tail -3 ; echo "exit: ${PIPESTATUS[0]}"`
Expected: 870 passed, exit 0.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(py): unified ConstId rename through to Python bindings"
```

---

### Task 7: Measurement gate + decision

**Files:**
- Modify: `docs/superpowers/plans/const-id-bench-baseline.md` (append the `after` comparison + verdict).

**Interfaces:**
- Consumes: the `before` criterion baseline from Task 1.

- [ ] **Step 1: Re-run lift+optimize bench against baseline**

Run: `cargo bench -p strider-orchestrator --bench scaling -- --baseline before`
Expected: criterion prints `change: [..] (p = ..)` per benchmark id and labels each `No change` / `Improved` / `Regressed`.

- [ ] **Step 2: Re-run optimizer bench against baseline**

Run: `cargo bench -p strider-opt --bench pipeline -- --baseline before`
Expected: per-id change report.

- [ ] **Step 3: Record + verdict**

Append to `docs/superpowers/plans/const-id-bench-baseline.md`: the `after` change percentages per benchmark id, and the verdict line:
- If every lift+optimize id is within **+3%** (or improved): `VERDICT: PASS — within gate`.
- If any regresses beyond +3%: `VERDICT: FAIL — <id> regressed N%`. Do NOT proceed to merge; surface to the user with the abandon/fallback options from the spec (retain an internal inline fast path, or keep only the boxing + doc fixes).

- [ ] **Step 4: Commit**

```bash
git add docs/superpowers/plans/const-id-bench-baseline.md
git commit -m "bench: record const-id-unify after-baseline comparison + verdict"
```

---

## Final review

After Task 7 passes the gate, run the whole-branch code review (superpowers:requesting-code-review) focused on: the value-only dedup soundness (`IntConst(v):I80` vs `:I128`), the canonicalisation SSoT (`intern_int_const*` — every construction site routes through it, nothing builds a raw non-canonical `ConstId`), the `gc_consts` rewrite (now covers all constants), and the validation rule. Then **prompt the user before merging** to `develop`.
