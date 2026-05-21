# Round 12 — Per-crate README diffs

Concrete edits derived from `round12-3A-doc-verify.md`, `round12-1C-opt.md`, `round12-1F-strider-py-aux.md`, and `round12-2B-naming.md`.

## Root `README.md`

### R-1 — Remove `pattern::float_is_nan` from Rust alias list (line 231)

**Current:**

> 2. **Lift-time canonicalisation aliases.** ... Use the alias constructors (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`, `pattern::float_is_nan`) ...

**Why stale:** `grep -rn "pub fn float_is_nan" crates/pattern/src/` returns 0 hits (verified 3A claim 14). The constructor exists only in the Python binding (`crates/strider-py/src/pattern.rs:1060`).

**Proposed:**

> Use the alias constructors (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`); to test for NaN, compose `bool_neg(float_eq(x, x))` directly (or use Python's `pattern.float_is_nan` which expands to the same shape).

## `crates/opt/README.md`

### R-2 — Remove `AnchorAddr` from `indirect_branch_resolve` public types (line 46)

**Current (1C HIGH finding):**

```
- `IndirectBranchResolve` (`indirect_branch_resolve/`) — producer-shape
  classifier for `BranchIndirect` placeholders. Exposes `classify_anchor`,
  `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp`,
  `apply_link_register`, `apply_tail_call`, plus the result types
  `AnchorAddr`, `AnchorCallingContext`, `ResolvedTargets`,
  `find_placeholder_return_for_anchor`.
```

**Why stale:** `grep -rn AnchorAddr crates/opt` finds only this README mention. The type was deleted by W9 S1.1 cleanup.

**Proposed:** Remove the `AnchorAddr,` token from the list.

```diff
-    `AnchorAddr`, `AnchorCallingContext`, `ResolvedTargets`,
+    `AnchorCallingContext`, `ResolvedTargets`,
```

### R-3 — Rename "tier-2" to "indirect-branch" (line 66)

**Current (2B):**

> The strider **tier-2** fixed-point splits passes into stable vs destructive.

**Proposed:**

> The strider **indirect-branch** fixed-point splits passes into stable vs destructive.

## `crates/cfg/README.md`

### R-4 — "tier-2" rename (line 45)

**Current:**

> the strider orchestrator's **tier-2** fixed-point loop rewrites ...

**Proposed:**

> the strider orchestrator's **indirect-branch** fixed-point loop rewrites ...

### R-5 — "tier-2" rename (line 82)

**Current:**

> `Cfg::sleigh` is reused across the strider **tier-2** fixed-point loop.

**Proposed:**

> `Cfg::sleigh` is reused across the strider **indirect-branch** fixed-point loop.

## `crates/strider-py/README.md`

### R-6 — `float_is_nan` description fix (line 203)

**Current (1F F-1):**

> `float_is_nan` is registered but raises `PatternError` until the IR gains a `FloatIsNan` node kind.

**Why stale:** The implementation at `crates/strider-py/src/pattern.rs:1059-1063` is:

```rust
#[pyfunction]
pub fn float_is_nan(operand: PatLike<'_>) -> PyResult<PyPat> {
    let op = operand.into_pat()?;
    Ok(PyPat::from_pat(pattern::float_ne(op.clone(), op)))
}
```

— a fully functional, non-raising pattern that desugars to `BoolNeg(FloatEqual(x, x))`.

**Proposed:**

> `pattern.float_is_nan(x)` matches the lifter's lowered FLOAT_NAN shape (`BoolNeg(FloatEqual(x, x))`) as well as any explicit `x != x` in the source. CLAUDE.md's pattern-crate section documents the lowering at the IR level.

### R-7 — `float_is_nan` v1-gap description fix (line 310)

**Current (1F F-1):**

> `float_is_nan` constructor (no `FloatIsNan` IR node yet).

**Why stale:** Same reason as R-6.

**Proposed:** Remove the bullet entirely. The constructor is part of v1 and works.

## `crates/pcode-lift/README.md`

### R-8 — Partial-write IR shape description (line 69-72)

**Current (1B LOW observation):**

> describes the partial-write IR shape as `Insert { lsb, len }` / `Extract { lsb, len }`.

**Why stale:** Actual implementation at `crates/pcode-lift/src/vn_io.rs:339-381` emits `Or` over `And`-masks plus `ShiftLeft`/`ShiftRight` constants — not `Insert`/`Extract` nodes. The IR shape is correct; only the prose is drifted.

**Proposed:** Rewrite the paragraph to describe the actual `Or(And(container, !pos_mask), Shl(Extend(reg), shift))` pattern emitted by `write_reg_vn`.

## `crates/cfg/src/cfg/builder/region_builder.rs` (source comment, not README)

### R-9 — Stale design-intent comment (line 358-364)

**Current (1B LOW):**

> ("fall back to TailCall to the in-range target") that the actual code path no longer implements (it relies on `add_region`'s relaxed empty-Branch invariant instead).

**Proposed:** Replace with the current behaviour description:

> "Single-OOB path: pop the trailing `CondBranch` insn and emit `Branch` to the in-range successor. The empty-region case (single-insn CondBranch with one-side OOB) is accepted by `add_region`'s relaxed empty-Branch invariant."

## Crates with no README drift

- `crates/ir/README.md` — verified accurate (1A audit cross-checked).
- `crates/pattern/README.md` — verified accurate (1D audit; `ethnum` declared-but-unused is a Cargo.toml hygiene item, not README).
- `crates/strider/README.md` — verified accurate (1E audit).
- `crates/target/README.md` — verified accurate (1E audit cross-checked against rsleigh SLA tables).
- `crates/reader/README.md` — verified accurate (1E audit).
- `crates/dot/README.md`, `crates/graphwalk/README.md`, `crates/entity-utils/README.md` — verified accurate (1F).

## Summary

| Edit | File | Line | Severity |
|------|------|------|----------|
| R-1 | `README.md` (root) | 231 | LOW |
| R-2 | `crates/opt/README.md` | 46 | HIGH (named non-existent type `AnchorAddr`) |
| R-3 | `crates/opt/README.md` | 66 | LOW |
| R-4 | `crates/cfg/README.md` | 45 | LOW |
| R-5 | `crates/cfg/README.md` | 82 | LOW |
| R-6 | `crates/strider-py/README.md` | 203 | HIGH (misadvertises working feature) |
| R-7 | `crates/strider-py/README.md` | 310 | HIGH (misadvertises v1 gap) |
| R-8 | `crates/pcode-lift/README.md` | 69-72 | LOW |
| R-9 | `crates/cfg/src/cfg/builder/region_builder.rs` (source) | 358-364 | LOW |

**Total:** 9 edits across 5 README files + 1 source comment. Three are HIGH (R-2 / R-6 / R-7) because they actively mislead users; the rest are cosmetic.
