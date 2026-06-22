# Unify `IntConst` behind a single interned `ConstId` — Design

**Date:** 2026-06-23
**Branch:** `feature/const-id-unify` (from `develop`)
**Status:** design — pending user approval before planning

## Goal

Collapse the dual `IntConst` representation (`IntPayload::Small(u64)` inline vs
`IntPayload::Wide(WideConstId)` interned) into a **single interned `ConstId`**,
so a constant value has exactly one representation and one SSoT. Rename the
interner value type `WideConstStorage → ConstValue` and the id
`WideConstId → ConstId`.

## Motivation (from the read-only spike)

- **The representation invariant is subtle and mis-documented.** Construction
  canonicalises **by value** (`Function::create_node_attributed`,
  `crates/strider-ir/src/function/data.rs:830`): a value that fits `u64` is
  `Small`, else `Wide` — and a `Wide` whose value fits `u64` is *demoted* back
  to `Small`. But the doc on the type itself (`crates/strider-ir/src/node/kind.rs:5`)
  says the opposite — "keyed on the constant's TYPE (I1..I64 ⇒ Small)". A
  maintainer reading the type definition is actively misled. This contradiction
  *is* the maintainability complaint, made concrete.
- **The dual path leaks into ~19 non-test consumer sites** across
  strider-pattern, strider-lift, strider-opt (census). Unifying reduces this to
  ~6–8 sites, all routed through one `intern_const`.
- **The split buys no node-size savings today.** Measured: `NodeKind = 24B`,
  pinned by `InitialVar(Vn)` (`Vn = 16B`). `IntConst(IntPayload)` is also 16B
  but not the sole driver — collapsing `IntPayload` 16B → a 4B `ConstId` leaves
  `NodeKind` at 24B. **This change is a simplicity/SSoT play, not a memory
  play.**
- **`WideConstStorage` is oversized at 80B** (sized to its inline
  `I512([u64;8])` variant). Boxing values >128 bits shrinks the interner slot to
  ~24B — a standalone win that also removes the "80B per interned const"
  objection to unifying.

## Non-goals / out of scope

- Shrinking `NodeKind` (it stays 24B — `Vn` dominates).
- Any change to `FloatConst` (separate, unaffected).
- Reworking the structural dedup cache (`IrCacheable` / `NodeCache`) beyond what
  the new key type requires.

## Design

### Data model

- `crates/strider-ir/src/node/kind.rs`: `NodeKind::IntConst(IntPayload)` →
  `NodeKind::IntConst(ConstId)`. **Delete `enum IntPayload`.**
- `crates/strider-ir/src/wide_const.rs` → renamed concept:
  - `WideConstId` → `ConstId` (still `entity_impl!`, 4B).
  - `WideConstStorage` → `ConstValue`:
    ```rust
    pub enum ConstValue {
        /// I1..I128 — value held inline (low `bit_width` bits significant).
        Bits(u128),
        /// I256/I512 — little-endian limbs, boxed. limbs[0] = low 64 bits.
        Wide(Box<[u64]>),
    }
    ```
    ~24B/slot. Derives `Clone, Eq, Hash` (required by `EntityInterner`).
- `Function`: `wide_const_interner: EntityInterner<WideConstId, WideConstStorage>`
  → `const_interner: EntityInterner<ConstId, ConstValue>`. Accessors
  `wide_const` / `wide_const_opt` / `intern_wide_const` →
  `const_value` / `const_value_opt` / `intern_const`.

### Dedup soundness — the load-bearing invariant

Storage dedups **by value only**: the value `42` interns to one `ConstId`
regardless of type. `IntConst(42):I80` and `IntConst(42):I128` remain **distinct
nodes** because the structural dedup cache keys on `(kind, inputs,
output_kinds)` and the output `ValueKind`s differ. This is sound and is verified
today by the cache contract; the redesign relies on it wholly, so it gets:

- A dedicated test: interning `42` once, building `IntConst(42):I80` and
  `IntConst(42):I128`, asserting **one** `ConstId` but **two** `NodeId`s, and
  that each reads back masked to its own width.
- A validation rule (`crates/strider-ir/src/validate/graph_invariants.rs`): an
  `IntConst`'s interned value must fit its declared output width (replaces the
  current "storage variant byte_size matches output type" check). For
  `Bits(v)`, `v & !ty.bit_mask_u128() == 0`; for `Wide(limbs)`, the limb count
  matches the declared width's limbs and high bits beyond the width are zero.

### Construction funnel

`create_node_attributed` (`function/data.rs`): mask the value to the declared
integer output width, then `intern_const` it to a `ConstId`. **The by-value
Small↔Wide demotion branch is deleted** — masking + interning is the whole
canonicalisation. Width-masking applies to both `Bits` (mask the `u128`) and
`Wide` (zero the limbs above the declared width).

### Builder API

`crates/strider-ir/src/builder/builder_ext.rs`: collapse `build_int_const`
(small) + `build_int_const_wide` (wide) into:

- `build_int_const(&mut self, value: u128, ty: ValueType) -> Result<ValueId>` —
  covers I1..I128. Masks `value` to `ty`, interns, builds.
- `build_int_const_limbs(&mut self, limbs: &[u64], ty: ValueType) -> Result<ValueId>`
  — covers I256/I512. (Lift `cast.rs`/`arithmetic.rs` all-ones / shift-const
  sites use this.)

### Read path

- `int_const_u128` (`viewer.rs`): the two `Small`/`Wide` match arms collapse to
  one — read `const_value_opt(id)`, return the value masked to declared width
  for `Bits` (and `I80`/`I128`), `None` for `Wide` (I256/I512, doesn't fit
  `u128`). `int_const_i128` / `int_const_i64` / `int_const_val` / `bool_const_val`
  are unchanged (they delegate).
- `int_const_wide_le_bytes`: one arm — read `ConstValue` and serialise to the
  declared byte width (`Bits` zero-extended to width, `Wide` limbs).

### Consumer migration (~19 → ~6–8 sites)

- **strider-lift** `lift/cast.rs:91`, `lift/arithmetic.rs:142`: build via
  `build_int_const` / `build_int_const_limbs` instead of constructing
  `WideConstStorage` variants directly.
- **strider-pattern** `template/mod.rs:213–256`: the 4-way I80/I128/I256/I512
  construction collapses to "value ≤ u128 → `Bits`, else `Wide(box)`" via the
  builder. `typed/consts.rs:52,246` discriminant computations become trivial
  (single variant); `typed/consts.rs:254,324` `Small`-only filters route through
  `int_const_val`.
- **strider-opt** `pipeline.rs:727` (test) assertion updated.
- **dot** `function/dot/label.rs`, `function/dot/raw.rs`: read via accessor /
  `ConstValue`.
- **strider-py** `node.rs`, `function.rs`: already use
  `int_const_wide_le_bytes` — unaffected beyond the rename.

## Measurement gate (measure-first)

Benches already exist: `crates/strider-orchestrator/benches/scaling.rs` (full
lift+optimize) and `crates/strider-opt/benches/pipeline.rs` (optimizer).

1. On `develop`: `cargo bench -p strider-orchestrator --bench scaling -- --save-baseline before`
   and the equivalent for `strider-opt/pipeline`.
2. Implement on `feature/const-id-unify`.
3. Re-run with `--baseline before`.
4. **Gate: lift+optimize regression ≤ 3%.** Over 3% → stop; either retain a
   small-value inline fast path (keeping the unified *API* but an enum payload
   internally) or abandon the unify and keep only the boxing + doc-fix wins. The
   measured numbers are recorded in the PR description regardless of outcome.

## Testing

- TDD per task. Existing const tests adapt: `builder/tests.rs`, `node/tests.rs`,
  `validate/tests.rs`, `wide_const.rs` tests (rename + the value-only dedup
  semantics — note `intern` no longer distinguishes `I80(42)` from `I128(42)`;
  that distinction moves to node output type, covered by the new node-level
  dedup test).
- New tests: value-only dedup invariant (one `ConstId`, two `NodeId`s by type);
  the validation rule (a too-wide value for its declared type is rejected);
  round-trip `build_int_const(v, I128)` / `build_int_const_limbs(.., I256)` →
  `int_const_u128` / `int_const_wide_le_bytes`.
- Full workspace `cargo test` + `clippy` + `pytest` before any merge.
- Code review on correctness, focused on the dedup-by-node-type soundness and
  the construction/validation masking.

## Risks & rollback

- **Hot-path indirection** (every const read/construct now interns) — the gated
  measurement is the mitigation; abandon-path defined above.
- **Dedup regression** if the value-only invariant is mishandled — covered by
  the dedicated test + validation rule.
- Rollback is clean: the work is isolated on `feature/const-id-unify`; nothing
  merges until benches pass the gate and review is clean. Prompt the user before
  merging anywhere.
