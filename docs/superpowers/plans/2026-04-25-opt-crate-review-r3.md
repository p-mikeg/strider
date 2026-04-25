# opt Crate Review (round 3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the `opt` crate pass `cargo clippy --workspace --all-targets` cleanly, fix small correctness/consistency issues, and apply targeted simplification + readability improvements without changing pass semantics.

**Architecture:** Independent task batches. Each task is self-contained: a single concern, a single small commit. Tasks are ordered so the clippy-clean baseline (Task 1) lands first, after which subsequent refactors can be re-verified with `cargo clippy --workspace --all-targets` at every step. No task introduces new abstractions or pass behavior.

**Tech Stack:** Rust 2024 edition, workspace lints from `Cargo.toml` (deny `unwrap_used`/`expect_used`/`panic`/`unreachable`/`todo`), `cargo clippy --workspace --all-targets`, `cargo test --package opt`.

---

## Findings summary (review notes — not part of the plan tasks)

The review surfaced one **mandatory clippy gap**, a small set of **real and latent correctness issues**, and a curated set of **simplification + readability** opportunities.

### Narrow-type overflow / unmask audit

I traced every integer op in `eval_int_binary` and `eval_int_cmp` for U8/U16/U32 hazards. Three categories:

- **Already in plan as Task 2** — `Sdiv`/`Srem` `INT_MIN/-1` at narrow widths silently wrap to `INT_MIN` instead of skipping (the U64 case already skips, the narrow cases do not). Semantic-overflow concern.

- **Newly found, addressed by Tasks 2b / 2c below** — `eval_int_binary`'s unsigned `Div`, `Rem`, `ShiftRight` and `eval_int_cmp`'s `Equal`, `Less`, `LessEqual`, `Carry`, `Borrow` operate on raw `u64` values pulled from `IntConst(u64)` without re-masking to the type width. `make_int_const(val, ty)` (at `crates/ir/src/ops/consts.rs:60`) never masks `val`; the analyzer's lifter at `crates/analyzer/src/analyzer/vn_io.rs:19` passes `vn.addr.off` (a raw Sleigh u64 offset) straight through; and the constant-fold `Truncate(IntConst(v))` rule itself produces an unmasked narrower IntConst (rule 4 at `crates/opt/src/constant_fold/rules.rs:282-287` emits `int_const_with!([v] => v)` without `ty.get_unsigned_int(v)`). So unmasked U8/U16/U32 `IntConst` nodes can plausibly enter the constant-fold rules — and when they do, the listed unsigned ops give wraparound-wrong answers. Worked example for U8 `Div`: input `IntConst(0x1FF, U8)`, `IntConst(0x02, U8)` → today's eval returns `0x1FF / 2 = 0xFF` masked to U8 = `0xFF`; the masked-then-evaluated answer is `0xFF / 2 = 0x7F`. Same shape for `Rem`, `ShiftRight`, `Equal`, `Less`, `LessEqual`, `Carry`, `Borrow`.

- **Confirmed safe by masking commutativity, no fix required** — `Add`, `Sub`, `Mul`, `And`, `Or`, `Xor`, `ShiftLeft`: each does `wrapping_*` then final `ty.get_unsigned_int(raw)`, which commutes with input masking (proof: `(a + b) mod m == ((a mod m) + (b mod m)) mod m` for the relevant ops). `IntUnaryOp::Neg`/`Not`: same wrapping+final-mask pattern. The signed ops `Sdiv`, `Srem`, `SShiftRight`, `Sless`, `SlessEqual`, `Scarry`, `Sborrow`: all start with `ty.get_signed_int(l)` / `(r)` which masks before sign-extending, so the value is normalised regardless of input hygiene.

### False positives identified during review (and excluded from this plan):

- `load_readonly/mod.rs:51` calling `node_outputs_exact::<1>` on a `Load` node is correct: `Load`'s signature in `crates/ir/src/node_signature.rs:338` is `outputs: [INT_VAL]` (one output), not `[Memory, Value]` as CLAUDE.md's prose suggests.
- `dead_branch::try_eliminate_dead_branch` index-shift concern between `live_ctrl` and `dead_ctrl` is unfounded: `replace_all_uses` is **in-place rewiring** (see `crates/ir/src/ops/rewrite.rs:18`), not slot removal, so `dead_uses` indices stay valid across step 2.
- `redundant_phis::remove_phis` "single ctrl predecessor" branch is correct under hash-set deduplication: when a predecessor's NodeOutputId appears twice (same producer wired into two ControlState slots), both value slots must hold the same value by construction, so picking position 0 is safe.

The pedantic-tier clippy noise (cast widening, `const fn`, `unnecessary structure name repetition`) is **out of scope**: workspace lints don't enable pedantic, and the user's "pass all clippy warnings" requirement is interpreted as default-tier (`cargo clippy --workspace --all-targets` clean).

The `mem_chain_is_dirty` cycle-truncation choice (returns `false`/clean on cycle) and `eval_int_binary` Sdiv/Srem narrow-type wraparound are pre-existing design decisions; this plan only **documents** them, not changes them.

---

## Task 1: Clippy gap — integration-test allow attributes

**Files:**
- Modify: `crates/opt/tests/multi_pass.rs:1`
- Modify: `crates/opt/tests/pipeline_default.rs:1`
- Modify: `crates/opt/tests/pipeline_fixedpoint.rs:1`
- Modify: `crates/opt/tests/pipeline_validation.rs:1`
- Modify: `crates/opt/tests/pipeline_with_stack.rs:1`
- Modify: `crates/opt/tests/common/mod.rs:6`

**Why:** Workspace `[workspace.lints.clippy]` denies `unwrap_used`, `expect_used`, `panic`, `unreachable`, `todo`. The lib has `#![cfg_attr(test, allow(...))]` at `crates/opt/src/lib.rs:21-29`, which **only applies to in-lib unit tests** — each file under `tests/*.rs` is compiled as its own crate and inherits the workspace deny without the `cfg(test)` allow. Result: `cargo clippy --package opt --all-targets` currently fails with 37 `unwrap()` errors. Bench files already do the right thing (`#![allow(clippy::unwrap_used, clippy::panic)]` at `crates/opt/benches/*.rs:1`). This task mirrors the bench pattern in tests.

- [ ] **Step 1: Run baseline clippy to confirm the gap**

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-review-r3
cargo clippy --package opt --all-targets 2>&1 | grep -E "^error" | grep -v "previous error" | wc -l
```

Expected: prints a number > 0 (currently 37 + 5 "could not compile" = 42 errors).

- [ ] **Step 2: Add allow attributes to each integration test file**

Insert this block as the first non-doc-comment line of every `crates/opt/tests/*.rs` file (5 files: `multi_pass.rs`, `pipeline_default.rs`, `pipeline_fixedpoint.rs`, `pipeline_validation.rs`, `pipeline_with_stack.rs`):

```rust
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
```

Place it **after** the `//!` module-level doc comment and **before** any `mod` / `use` line. For files starting with `//! ...\n\nmod common;`, the order is: module doc, blank line, allow block, blank line, `mod common;`.

For `crates/opt/tests/common/mod.rs`, extend the existing block at line 6-7 from:

```rust
#![allow(dead_code)] // Helpers are reused across files; rustc can't see all uses.
#![allow(unused_imports)] // Re-exports and helpers may not all be used in every test file.
```

to:

```rust
#![allow(dead_code)] // Helpers are reused across files; rustc can't see all uses.
#![allow(unused_imports)] // Re-exports and helpers may not all be used in every test file.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
```

- [ ] **Step 3: Verify clippy is clean for opt at default tier**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -10
```

Expected: `Finished` with no `error:` lines.

- [ ] **Step 4: Verify the rest of the workspace still clean**

```bash
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | grep -E "^(error|warning)" | head -20
```

Expected: no opt-crate errors. (Other crates may have pre-existing warnings; do not touch them.)

- [ ] **Step 5: Verify tests still pass**

```bash
cargo test --package opt 2>&1 | tail -3
```

Expected: `test result: ok.`

- [ ] **Step 6: Commit**

```bash
git add crates/opt/tests/
git commit -m "fix(opt): allow unwrap/panic/expect in integration tests

Mirrors lib.rs's cfg_attr(test) allow block and the bench pattern.
Without this, workspace deny lints make cargo clippy --all-targets
fail on every tests/*.rs file."
```

---

## Task 2: Fix `eval_int_binary` Sdiv/Srem narrow-type overflow inconsistency

**Files:**
- Modify: `crates/opt/src/constant_fold/eval_int.rs:39-49`
- Modify: `crates/opt/src/constant_fold/eval_int.rs:56-63`
- Test: `crates/opt/src/constant_fold/tests.rs` (new test in existing tests module)

**Why:** Today's `Sdiv` arm at `eval_int.rs:39-49` rejects `i64::MIN / -1` only — but the IR also supports U8/U16/U32, where `i32::MIN / -1`, `i16::MIN / -1`, `i8::MIN / -1` are similarly the **only** signed overflows. After widening to `i64`, those divisions succeed mathematically but `get_unsigned_int(...)` masks them back to wraparound, silently producing the same value as the input (e.g. `Sdiv(i32::MIN, -1) → i32::MIN`). For consistency with the U64 case, **return None** (skip the rewrite) on the narrow signed-overflow case. `Srem` has the same shape: `Srem(INT_MIN, -1) == 0` mathematically but only the U64 case is currently undefined.

This is a low-impact correctness fix: real binaries rarely hit `INT_MIN/-1`, but the inconsistency is a foot-gun and was previously raised as a review concern.

- [ ] **Step 1: Write failing tests**

Add to `crates/opt/src/constant_fold/tests.rs` (after the last existing `#[test]`):

```rust
#[test]
fn sdiv_narrow_int_min_neg_one_skips() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // i32::MIN as u32, then masked to u64. Same shape as the u64 case
    // already guarded explicitly; should also return None.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x8000_0000, 0xFFFF_FFFF, NodeOutputType::U32),
        None,
        "Sdiv(i32::MIN, -1) on U32 must skip — signed overflow"
    );
    // i16::MIN, -1 on U16.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x8000, 0xFFFF, NodeOutputType::U16),
        None,
        "Sdiv(i16::MIN, -1) on U16 must skip — signed overflow"
    );
    // i8::MIN, -1 on U8.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Sdiv, 0x80, 0xFF, NodeOutputType::U8),
        None,
        "Sdiv(i8::MIN, -1) on U8 must skip — signed overflow"
    );
}

#[test]
fn srem_narrow_int_min_neg_one_skips() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // Same INT_MIN/-1 case for Srem on every narrow signed type.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x8000_0000, 0xFFFF_FFFF, NodeOutputType::U32),
        None,
        "Srem(i32::MIN, -1) on U32 must skip"
    );
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x8000, 0xFFFF, NodeOutputType::U16),
        None,
    );
    assert_eq!(
        eval_int_binary(IntBinaryOp::Srem, 0x80, 0xFF, NodeOutputType::U8),
        None,
    );
}
```

The `eval_int_binary` symbol is `pub(super)` today; access it from sibling test module via `crate::constant_fold::eval_int::eval_int_binary`. If that path resolution fails because `eval_int` is private, change `eval_int_binary` visibility to `pub(crate)` for the duration of the test, or add a re-export in `crates/opt/src/constant_fold/mod.rs`:

```rust
#[cfg(test)]
pub(crate) use eval_int::eval_int_binary;
```

- [ ] **Step 2: Run the failing tests**

```bash
cargo test --package opt --lib constant_fold::tests::sdiv_narrow_int_min_neg_one_skips 2>&1 | tail -3
cargo test --package opt --lib constant_fold::tests::srem_narrow_int_min_neg_one_skips 2>&1 | tail -3
```

Expected: both fail with assertion mismatch (today's code returns `Some(input)` on these).

- [ ] **Step 3: Add a per-type signed-overflow guard**

Replace the `Sdiv` arm at `crates/opt/src/constant_fold/eval_int.rs:39-49` from:

```rust
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            if sl == i64::MIN && sr == -1 {
                return None;
            } // overflow
            (sl / sr) as u64
        }
```

to:

```rust
        IntBinaryOp::Sdiv => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            // Signed overflow: INT_MIN / -1 is undefined for every signed
            // integer width. The narrow-type case looks "well-defined" at
            // i64 width (e.g. -i32::MIN as i64 = 2^31 fits), but masking
            // back to the type silently wraps to INT_MIN, which is not the
            // mathematical result. Skip rather than emit a wraparound.
            let bits = ty.bit_width() as u32;
            let int_min: i64 = -(1i64 << (bits - 1));
            if sl == int_min && sr == -1 {
                return None;
            }
            (sl / sr) as u64
        }
```

Replace the `Srem` arm at `crates/opt/src/constant_fold/eval_int.rs:56-63` from:

```rust
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            (sl % sr) as u64
        }
```

to:

```rust
        IntBinaryOp::Srem => {
            let sl = ty.get_signed_int(l)?;
            let sr = ty.get_signed_int(r)?;
            if sr == 0 {
                return None;
            }
            // Signed-overflow guard: INT_MIN % -1 is mathematically 0 but
            // hardware idiv raises #DE; treat it as undefined and skip,
            // matching the Sdiv case.
            let bits = ty.bit_width() as u32;
            let int_min: i64 = -(1i64 << (bits - 1));
            if sl == int_min && sr == -1 {
                return None;
            }
            (sl % sr) as u64
        }
```

Note: `ty.bit_width()` for `U64` returns 64; `1i64 << 63` overflows on cast. Guard or use the safer form `i64::MIN >> (64 - bits)` — for `U64`, `i64::MIN >> 0 == i64::MIN`. Choose this form:

```rust
            let bits = ty.bit_width() as u32;
            let int_min: i64 = i64::MIN >> (64 - bits);
            if sl == int_min && sr == -1 {
                return None;
            }
```

(For `U64` `bits=64`, `64-64=0`, `i64::MIN >> 0 == i64::MIN`. ✓ For `U32`, `64-32=32`, `i64::MIN >> 32 == -2^31 == i32::MIN as i64`. ✓ Etc.)

- [ ] **Step 4: Verify tests pass**

```bash
cargo test --package opt --lib constant_fold:: 2>&1 | tail -5
```

Expected: `test result: ok.` for the new tests + all existing constant-fold tests remain green.

- [ ] **Step 5: Verify clippy + full opt suite**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

Expected: clippy clean; all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/opt/src/constant_fold/
git commit -m "fix(opt): skip Sdiv/Srem on signed overflow at every width

Sdiv/Srem at U8/U16/U32 with INT_MIN / -1 previously emitted a
wraparound value (the result silently re-aliased to INT_MIN after
masking) instead of skipping the rewrite. Now consistent with the
U64 guard."
```

---

## Task 2b: Mask unmasked-input vulnerabilities in `eval_int_binary` and `eval_int_cmp`

**Files:**
- Modify: `crates/opt/src/constant_fold/eval_int.rs:11-17` (function entry of `eval_int_binary`)
- Modify: `crates/opt/src/constant_fold/eval_int.rs:69` (function entry of `eval_int_cmp`)
- Test: `crates/opt/src/constant_fold/tests.rs` (new tests covering Div/Rem/Shr/Carry/Borrow/Equal/Less with deliberately-unmasked u64 inputs)

**Why:** `eval_int_binary` and `eval_int_cmp` receive raw `u64` values pulled from `IntConst(u64)` nodes. The IR layer's `make_int_const(val, ty)` does NOT mask, so an `IntConst(0x1FF, U8)` is constructible, and the analyzer's `vn_io.rs:19` lifter passes raw `vn.addr.off` to `build_int_const` which can produce one. Today's evaluator gives wrong answers for unsigned `Div`, `Rem`, `ShiftRight`, `Equal`, `Less`, `LessEqual`, `Carry`, `Borrow` when their inputs hold high bits beyond the type width. Add/Sub/Mul/And/Or/Xor/ShiftLeft and the signed ops are immune by construction (see findings summary). The fix is two lines per function: mask both inputs at entry. Idempotent on already-masked inputs.

- [ ] **Step 1: Write failing tests**

Add to `crates/opt/src/constant_fold/tests.rs` (module `tests`, alongside the Sdiv/Srem tests added in Task 2):

```rust
#[test]
fn eval_int_binary_unsigned_div_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // U8 Div with l carrying high garbage bits beyond U8.
    // Masked: 0xFF / 2 = 0x7F. Unmasked-eval: 0x1FF / 2 = 0xFF (wrong).
    assert_eq!(
        eval_int_binary(IntBinaryOp::Div, 0x1FF, 2, NodeOutputType::U8),
        Some(0x7F),
        "Div must mask inputs to U8 before division"
    );
}

#[test]
fn eval_int_binary_unsigned_rem_unmasked_u16() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // Masked: 0xFFFF % 0x10 = 0x0F. Unmasked-eval: 0x1FFFF % 0x10 = 0x0F.
    // Pick a divisor that distinguishes: 0xFFFF % 7 = 1, 0x1FFFF % 7 = 5.
    assert_eq!(
        eval_int_binary(IntBinaryOp::Rem, 0x1FFFF, 7, NodeOutputType::U16),
        Some(1),
        "Rem must mask inputs to U16 before remainder"
    );
}

#[test]
fn eval_int_binary_unsigned_shr_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_binary;
    use ir::IntBinaryOp;
    use ir::node::NodeOutputType;

    // Masked: 0xFF >> 1 = 0x7F. Unmasked-eval: 0x1FF >> 1 = 0xFF, masked = 0xFF.
    assert_eq!(
        eval_int_binary(IntBinaryOp::ShiftRight, 0x1FF, 1, NodeOutputType::U8),
        Some(0x7F),
        "ShiftRight must mask the input to U8 before shifting"
    );
}

#[test]
fn eval_int_cmp_equal_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0xFF == 0xFF → true. Unmasked-eval: 0x1FF != 0xFF → false.
    assert_eq!(
        eval_int_cmp(IntCmpOp::Equal, 0x1FF, 0xFF, NodeOutputType::U8).unwrap(),
        true,
        "Equal must mask both sides to U8 before comparing"
    );
}

#[test]
fn eval_int_cmp_less_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0x00 < 0x01 → true. Unmasked-eval: 0x100 < 0x01 → false.
    assert_eq!(
        eval_int_cmp(IntCmpOp::Less, 0x100, 0x01, NodeOutputType::U8).unwrap(),
        true,
        "Less must mask both sides to U8 before comparing"
    );
}

#[test]
fn eval_int_cmp_carry_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0x00 + 0x00 → no carry. Unmasked-eval: 0x100 + 0 = 0x100 > 0xFF → false-carry.
    assert_eq!(
        eval_int_cmp(IntCmpOp::Carry, 0x100, 0, NodeOutputType::U8).unwrap(),
        false,
        "Carry must mask both sides before checking overflow"
    );
}

#[test]
fn eval_int_cmp_borrow_unmasked_u8() {
    use crate::constant_fold::eval_int::eval_int_cmp;
    use ir::IntCmpOp;
    use ir::node::NodeOutputType;

    // Masked: 0x00 < 0x01 → true. Unmasked-eval: 0x100 < 0x01 → false.
    assert_eq!(
        eval_int_cmp(IntCmpOp::Borrow, 0x100, 0x01, NodeOutputType::U8).unwrap(),
        true,
        "Borrow must mask both sides to U8 before comparing"
    );
}
```

(`eval_int_cmp` returns `Result<bool, _>`; the `.unwrap()` is allowed in unit tests by `lib.rs`'s `cfg_attr(test, allow(...))` block. The cross-module visibility for `eval_int_binary` / `eval_int_cmp` is the same as in Task 2 — re-use the same `pub(crate)` bump or `#[cfg(test)] pub(crate) use` re-export.)

- [ ] **Step 2: Run failing tests**

```bash
cargo test --package opt --lib constant_fold::tests::eval_int_binary_unsigned 2>&1 | tail -3
cargo test --package opt --lib constant_fold::tests::eval_int_cmp_ 2>&1 | tail -3
```

Expected: each test fails with assertion mismatch (today's eval returns the unmasked-arithmetic answer).

- [ ] **Step 3: Mask both inputs at entry of `eval_int_binary`**

In `crates/opt/src/constant_fold/eval_int.rs:11-17`, replace:

```rust
pub(super) fn eval_int_binary(
    op: IntBinaryOp,
    l: u64,
    r: u64,
    ty: NodeOutputType,
) -> Option<u64> {
    let bits = ty.bit_width() as u64;
```

with:

```rust
pub(super) fn eval_int_binary(
    op: IntBinaryOp,
    l: u64,
    r: u64,
    ty: NodeOutputType,
) -> Option<u64> {
    // Defensive: IntConst(u64) values are not guaranteed to be masked to the
    // declared type's width — `make_int_const` stores the raw u64, and the
    // analyzer's vn_io lifter feeds raw Sleigh `VnAddr.off` values through.
    // Operations safe under masking-commutativity (Add, Sub, Mul, And, Or,
    // Xor, ShiftLeft) would still produce the right answer because the final
    // `ty.get_unsigned_int(raw)` cancels any high bits, but Div, Rem, and
    // ShiftRight are NOT commutative with masking and would give wrong
    // results. Mask once at entry; the `?` skips evaluation entirely for
    // U128/U256 (consistent with the existing per-arm fallthroughs).
    let l = ty.get_unsigned_int(l)?;
    let r = ty.get_unsigned_int(r)?;
    let bits = ty.bit_width() as u64;
```

- [ ] **Step 4: Mask both inputs at entry of `eval_int_cmp`**

In `crates/opt/src/constant_fold/eval_int.rs:69` (the function declaration), replace:

```rust
pub(super) fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    Ok(match op {
```

with:

```rust
pub(super) fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    // See `eval_int_binary` — mask both inputs to `ty` at entry. The
    // unsigned comparisons (Equal, Less, LessEqual, Carry, Borrow) operate
    // on raw u64s and would otherwise return wrong answers for U8/U16/U32
    // IntConsts that carry high bits beyond the type width. The signed
    // arms (`Sless`, `Scarry`, …) re-mask via `get_signed_int` so the
    // double-mask is idempotent for them.
    let l = ty
        .get_unsigned_int(l)
        .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty))?;
    let r = ty
        .get_unsigned_int(r)
        .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty))?;

    Ok(match op {
```

(After Task 3 lands, the `signed` / `unsigned_max` closures already use `ty.get_unsigned_int(...)` / `get_signed_int(...)` and remain idempotent; this entry-mask is a defensive cheap guard for the unsigned arms.)

- [ ] **Step 5: Verify tests pass**

```bash
cargo test --package opt --lib constant_fold:: 2>&1 | tail -5
cargo test --package opt 2>&1 | tail -3
```

Expected: all 7 new tests + every existing test green.

- [ ] **Step 6: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 7: Commit**

```bash
git add crates/opt/src/constant_fold/
git commit -m "fix(opt): mask IntConst inputs at eval entry

eval_int_binary and eval_int_cmp received raw u64 values pulled
from IntConst nodes without re-masking to the declared type. For
U8/U16/U32 inputs that carry high bits beyond the type width
(constructible from raw Sleigh VnAddr offsets), Div, Rem,
ShiftRight, Equal, Less, LessEqual, Carry, and Borrow returned
arithmetically wrong answers. Mask once at entry — idempotent on
already-masked inputs and on the signed arms that re-mask via
get_signed_int."
```

---

## Task 2c: Mask the `Truncate(IntConst(v))` rewrite output

**Files:**
- Modify: `crates/opt/src/constant_fold/rules.rs:280-287`
- Test: `crates/opt/src/constant_fold/tests.rs` (one new test)

**Why:** Rule 4 in `build_const_eval_rules` produces an `IntConst(v)` typed at the truncate's output width but stores the *raw* wider value:

```rust
truncate(any_int_const(v)),
int_const_with!([v] => v),
```

So a `Truncate(IntConst(0xFFFF, U16)) → IntConst(0xFFFF, U8)` (typed-narrow but value-wide). Even after Task 2b masks at eval-time, this rule plants unmasked IntConsts directly into the IR — every other consumer (KnownBits, sp_expr's `int_const_signed`, downstream rules) handles them correctly only because each individual consumer re-masks. But the IR-layer invariant "an IntConst's stored value fits its declared type" is silently broken here. The fix is one extra capture and one mask call.

- [ ] **Step 1: Write failing test**

Add to `crates/opt/src/constant_fold/tests.rs`:

```rust
#[test]
fn truncate_int_const_emits_masked_value() -> crate::Result<()> {
    use ir::node::{NodeKind, NodeOutputType};
    use ir::FunctionBuilder;

    let mut b = FunctionBuilder::new_raw(vec![], &[], &[], &[], None, 0)?;
    let region = b.create_region()?;
    b.set_entry_region(region)?;
    b.set_region(region);
    let wide = b.build_int_const(0xFFFF, NodeOutputType::U16).unwrap();
    let narrow = b.build_truncate(wide, NodeOutputType::U8)?;
    b.build_return(Some(narrow), &[])?;
    let mut fg = b.build()?;

    let opt = crate::ConstantFold;
    use crate::Optimizer;
    opt.optimize(&mut fg)?;

    // Walk the graph: the Return's value-input must now be an
    // IntConst(0x00FF) — the LOW byte of 0xFFFF, masked to U8.
    let return_node = fg
        .preorder()
        .find(|&n| matches!(*fg.graph.node_kind(n), NodeKind::Return))
        .unwrap();
    let ret_inputs = fg.graph.node_inputs(return_node);
    // Return inputs: [ctrl, mem, value0?]. Find the int-const value.
    for inp in ret_inputs.into_iter() {
        let producer = fg.graph.get_node_from_output(inp);
        if let NodeKind::IntConst(v) = *fg.graph.node_kind(producer) {
            // The narrow IntConst's stored value must be masked to U8.
            assert_eq!(
                v & 0xFF,
                v,
                "Truncate of IntConst must store the masked value, got 0x{:X}",
                v
            );
            assert_eq!(v, 0xFF, "Expected low byte 0xFF");
            return Ok(());
        }
    }
    panic!("no IntConst producer found in Return inputs");
}
```

- [ ] **Step 2: Run the failing test**

```bash
cargo test --package opt --lib constant_fold::tests::truncate_int_const_emits_masked_value 2>&1 | tail -10
```

Expected: panic at the `assert_eq!(v, 0xFF)` (today's value is `0xFFFF`).

- [ ] **Step 3: Add the mask to the rule**

In `crates/opt/src/constant_fold/rules.rs:280-287`, replace:

```rust
        // 4. Truncate(IntConst(v)) => int_const(v, ty)
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                truncate(any_int_const(v)),
                int_const_with!([v] => v),
            ))
        },
```

with:

```rust
        // 4. Truncate(IntConst(v)) => int_const(v masked to ty, ty)
        //    The wider IntConst's raw value is *not* automatically masked
        //    to the truncate's output width — `make_int_const` stores raw
        //    u64s. Mask explicitly here so we don't plant an unmasked
        //    narrow IntConst into the IR. Skip when ty is U128/U256 (the
        //    truncate output is always narrower than U64 in practice, but
        //    the skip costs nothing and is consistent with other rules).
        {
            let v = IntVar::new();
            boxed_rule(rewrite_rule(
                truncate(any_int_const(v)),
                int_const_with!([v, ty] =>
                    ty.get_unsigned_int(v).ok_or_else(pattern::Error::skip)?
                ),
            ))
        },
```

- [ ] **Step 4: Verify the test passes**

```bash
cargo test --package opt --lib constant_fold::tests::truncate_int_const_emits_masked_value 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

Expected: all green.

- [ ] **Step 5: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 6: Commit**

```bash
git add crates/opt/src/constant_fold/
git commit -m "fix(opt): mask Truncate(IntConst(v)) output to the narrow type

Previously the rewrite stored the wider raw value in a narrow-typed
IntConst — silently breaking the invariant that an IntConst's value
fits its declared type. Mask explicitly via ty.get_unsigned_int(v)."
```

---

## Task 3: DRY `eval_int_cmp` repeated `.ok_or(ExpectedIntegerType)?` lines

**Files:**
- Modify: `crates/opt/src/constant_fold/eval_int.rs:69-127`

**Note:** Run this task **after** Task 2b. Task 2b adds entry-mask `.ok_or_else(...)?` calls; this task collapses the in-body repetitions. The two tasks compose cleanly because Task 2b's masks are idempotent on already-masked l/r.

**Why:** Eight occurrences of `.ok_or(ErrorKind::ExpectedIntegerType(ty))?` (lines 76, 78, 82, 85, 91, 102, 105, 116, 119 — counting both U64 widening pairs and Carry/Borrow widenings) bury the actual comparison expressions. A small local helper closure makes the comparison logic readable.

- [ ] **Step 1: Refactor with a local helper closure**

Replace `crates/opt/src/constant_fold/eval_int.rs:69-127` with:

```rust
/// Evaluates a comparison on two constant integer values.
pub(super) fn eval_int_cmp(op: IntCmpOp, l: u64, r: u64, ty: NodeOutputType) -> Result<bool> {
    let signed = |v: u64| -> Result<i64> {
        ty.get_signed_int(v)
            .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty).into())
    };
    let unsigned_max = || -> Result<u64> {
        ty.get_unsigned_int(u64::MAX)
            .ok_or_else(|| ErrorKind::ExpectedIntegerType(ty).into())
    };
    let bits = ty.bit_width() as u32;
    let signed_min_max = || -> (i128, i128) {
        let min = -(1i128 << (bits - 1));
        let max = (1i128 << (bits - 1)) - 1;
        (min, max)
    };

    Ok(match op {
        IntCmpOp::Equal => l == r,
        IntCmpOp::Less => l < r,
        IntCmpOp::LessEqual => l <= r,
        IntCmpOp::Sless => signed(l)? < signed(r)?,
        IntCmpOp::SlessEqual => signed(l)? <= signed(r)?,
        IntCmpOp::Carry => {
            // Unsigned add overflow: l + r > type's max unsigned value.
            (l as u128 + r as u128) > unsigned_max()? as u128
        }
        IntCmpOp::Borrow => l < r,
        IntCmpOp::Scarry => {
            let (min, max) = signed_min_max();
            let result = signed(l)? as i128 + signed(r)? as i128;
            result < min || result > max
        }
        IntCmpOp::Sborrow => {
            let (min, max) = signed_min_max();
            let result = signed(l)? as i128 - signed(r)? as i128;
            result < min || result > max
        }
    })
}
```

The behavior is identical (`ok_or_else` produces the same `ErrorKind::ExpectedIntegerType(ty)` error); only the syntax shrinks from ~58 lines to ~30. The two helper closures are independent so neither captures the other; the `bits`/`signed_min_max` closure is also local-pure. Rust's borrow checker accepts this since `ty` is `Copy`.

- [ ] **Step 2: Verify tests still pass**

```bash
cargo test --package opt --lib constant_fold:: 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

Expected: `test result: ok.`

- [ ] **Step 3: Verify clippy still clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/constant_fold/eval_int.rs
git commit -m "refactor(opt): DRY eval_int_cmp with signed/unsigned closures

Eight identical .ok_or(ErrorKind::ExpectedIntegerType(ty))? chains
collapsed into two closures plus a single signed_min_max helper.
Behavior unchanged."
```

---

## Task 4: Merge double-loop in `redundant_phis::remove_phis`

**Files:**
- Modify: `crates/opt/src/redundant_phis/mod.rs:60-115`

**Why:** The current code at lines 69-83 walks `ctrl_inputs` twice — once to build `reachable_ctrl: FxHashSet<NodeOutputId>`, once to build `live_values: FxHashSet<NodeOutputId>` — using the same `reachable.contains(...)` predicate. One loop suffices. The simplification preserves the existing dedup semantics (both sets are still `FxHashSet`, both still keyed on the same NodeOutputIds), and the existing logic at lines 85-115 (the `(Some, None)` matches on iterators) is untouched.

- [ ] **Step 1: Replace the two-loop block with one**

Replace `crates/opt/src/redundant_phis/mod.rs:69-83` from:

```rust
            let reachable_ctrl: FxHashSet<NodeOutputId> = ctrl_inputs
                .into_iter()
                .filter(|ctrl_in| reachable.contains(&function.graph.output_definition(*ctrl_in).0))
                .collect();

            // Values from live predecessors only: positionally, inputs[j + 1]
            // is the value on predecessor ctrl_inputs[j].
            let live_values: FxHashSet<NodeOutputId> = ctrl_inputs
                .into_iter()
                .enumerate()
                .filter(|&(_j, ctrl_in)| {
                    reachable.contains(&function.graph.output_definition(ctrl_in).0)
                })
                .map(|(j, _ctrl_in)| inputs[j + 1])
                .collect();
```

to:

```rust
            // Single pass: gather both the deduplicated reachable ctrl edges
            // and their corresponding values (inputs[j + 1]) for live
            // predecessors only.
            let mut reachable_ctrl: FxHashSet<NodeOutputId> = FxHashSet::default();
            let mut live_values: FxHashSet<NodeOutputId> = FxHashSet::default();
            for (j, ctrl_in) in ctrl_inputs.into_iter().enumerate() {
                if reachable.contains(&function.graph.output_definition(ctrl_in).0) {
                    reachable_ctrl.insert(ctrl_in);
                    live_values.insert(inputs[j + 1]);
                }
            }
```

- [ ] **Step 2: Verify tests pass**

```bash
cargo test --package opt redundant_phis 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

Expected: all green.

- [ ] **Step 3: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/redundant_phis/mod.rs
git commit -m "refactor(opt): single-pass live-ctrl + live-values gather

Merges two separate filter+collect loops over ctrl_inputs into one,
preserving the existing FxHashSet dedup semantics on both sets."
```

---

## Task 5: Drop the `cfg_reachable` re-collect in `RedundantPhis::optimize`

**Files:**
- Modify: `crates/opt/src/redundant_phis/mod.rs:43-47`
- Modify: `crates/opt/src/redundant_phis/mod.rs:161-167`

**Why:** `ir::walk::cfg_reachable` returns `std::collections::HashSet<NodeId>` (default hasher; see `crates/ir/src/walk.rs:22`). The optimizer immediately re-hashes it into an `FxHashSet<NodeId>` at lines 163-166 just to pass it to `remove_phis`, which only ever uses `.contains(...)`. Drop the re-hash; pass the `HashSet` directly. The `remove_phis` signature changes from `&FxHashSet<NodeId>` to `&std::collections::HashSet<NodeId>`. (We could alternatively change `cfg_reachable` to return `FxHashSet`, but that's an `ir`-crate API change beyond this review's scope.)

- [ ] **Step 1: Change the signature of `remove_phis`**

In `crates/opt/src/redundant_phis/mod.rs:43-47`, replace:

```rust
fn remove_phis(
    function: &mut ir::BuiltFunctionGraph,
    node_id: NodeId,
    reachable: &FxHashSet<NodeId>,
) -> Result<OptimizationResult> {
```

with:

```rust
fn remove_phis(
    function: &mut ir::BuiltFunctionGraph,
    node_id: NodeId,
    reachable: &std::collections::HashSet<NodeId>,
) -> Result<OptimizationResult> {
```

- [ ] **Step 2: Drop the re-collect at the call site**

In `crates/opt/src/redundant_phis/mod.rs:163-166`, replace:

```rust
        let reachable: FxHashSet<NodeId> =
            ir::walk::cfg_reachable(&function.graph, function.entry)
                .into_iter()
                .collect();
```

with:

```rust
        let reachable = ir::walk::cfg_reachable(&function.graph, function.entry);
```

The remaining `&reachable` argument at line 182 type-checks against the new `HashSet<NodeId>` signature.

- [ ] **Step 3: Verify tests pass**

```bash
cargo test --package opt redundant_phis 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

- [ ] **Step 4: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/redundant_phis/mod.rs
git commit -m "perf(opt): drop FxHashSet re-collect of cfg_reachable result

cfg_reachable already returns HashSet<NodeId>; the FxHashSet
re-hash was a wasted O(n) pass with no benefit (only .contains()
is ever called)."
```

---

## Task 6: Drop unnecessary `Vec` allocation in `stack_load_forward::probe`

**Files:**
- Modify: `crates/opt/src/stack_load_forward/mod.rs:187-210`

**Why:** Line 197's `let inputs_vec: Vec<NodeOutputId> = fg.graph.node_inputs(node).into_iter().collect();` allocates a `Vec` only to slice into it. `node_inputs` returns a borrowed slice; binding directly removes the allocation and clarifies intent.

- [ ] **Step 1: Replace the body of the `MemPhi` arm**

Replace `crates/opt/src/stack_load_forward/mod.rs:187-207` from:

```rust
        NodeKind::MemPhi => {
            // Cycle guard: loop-header MemPhis feed their own region
            // indirectly.  Guard only at MemPhi boundaries — other memory
            // nodes walk backward to strictly earlier producers and cannot
            // cycle on their own, and guarding them would prevent sibling
            // branches from re-reaching a shared upstream node.
            if !visited.insert(mem) {
                return None;
            }
            // MemPhi inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
            let inputs_vec: Vec<NodeOutputId> = fg.graph.node_inputs(node).into_iter().collect();
            if inputs_vec.len() < 2 {
                return None;
            }
            let phi_token = inputs_vec[0];
            let mut preds: Vec<ResolveShape> = Vec::with_capacity(inputs_vec.len() - 1);
            for pred_mem in &inputs_vec[1..] {
                preds.push(probe(fg, *pred_mem, offset, load_size, load_ty, visited)?);
            }
            Some(ResolveShape::Phi { phi_token, preds })
        }
```

to:

```rust
        NodeKind::MemPhi => {
            // Cycle guard: loop-header MemPhis feed their own region
            // indirectly.  Guard only at MemPhi boundaries — other memory
            // nodes walk backward to strictly earlier producers and cannot
            // cycle on their own, and guarding them would prevent sibling
            // branches from re-reaching a shared upstream node.
            if !visited.insert(mem) {
                return None;
            }
            // MemPhi inputs: [phi_token, mem_pred_0, mem_pred_1, ...].
            let inputs = fg.graph.node_inputs(node);
            if inputs.len() < 2 {
                return None;
            }
            let phi_token = inputs[0];
            // Snapshot pred mem-tokens before recursing — `probe` only borrows
            // `fg.graph` immutably, but extracting the values up front keeps
            // the hot loop free of repeated `node_inputs` calls.
            let pred_mems: Vec<NodeOutputId> = inputs.into_iter().skip(1).collect();
            let mut preds: Vec<ResolveShape> = Vec::with_capacity(pred_mems.len());
            for pred_mem in pred_mems {
                preds.push(probe(fg, pred_mem, offset, load_size, load_ty, visited)?);
            }
            Some(ResolveShape::Phi { phi_token, preds })
        }
```

The crucial subtlety: `inputs` borrows `fg.graph`; the recursive `probe` call also takes `fg: &BuiltFunctionGraph`. Whether the borrow checker accepts iteration over `inputs` directly depends on the exact return type of `node_inputs`. If it returns `&[NodeOutputId]`, iterating it while passing `fg` immutably to `probe` should work (overlapping shared borrows). If it returns an iterator type that mutably borrows graph internals (unlikely but possible), the snapshot via `pred_mems` Vec is required. The above keeps the snapshot for safety; if compilation succeeds without `pred_mems`, simplify in a follow-up. Keep the form that compiles.

- [ ] **Step 2: Verify tests pass**

```bash
cargo test --package opt stack_load_forward 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

- [ ] **Step 3: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/stack_load_forward/mod.rs
git commit -m "refactor(opt): tighten MemPhi probe — drop redundant Vec collect

inputs_vec was allocated only to read len/index/slice. Use the
slice directly; pre-collect only the per-pred mem tokens needed
to feed the recursive probe call without holding the input borrow."
```

---

## Task 7: Tighten `realize`'s phi-dedup check

**Files:**
- Modify: `crates/opt/src/stack_load_forward/mod.rs:260-279`

**Why:** Line 268's `resolved.iter().all(|v| *v == resolved[0])` indexes `resolved[0]` after the iterator already returned `true` for `len == 0` (vacuous all). That returns `Ok(resolved[0])` which would panic on the empty Vec — but `probe` rejects empty MemPhi (`inputs.len() < 2`), so it's unreachable in practice. Replace with `resolved.windows(2).all(|w| w[0] == w[1])` and pull `resolved[0]` only after explicit non-emptiness — clearer intent, no hidden panic surface.

- [ ] **Step 1: Replace the dedup branch**

Replace `crates/opt/src/stack_load_forward/mod.rs:260-279` from:

```rust
        ResolveShape::Phi { phi_token, preds } => {
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(preds.len());
            for p in preds {
                resolved.push(realize(fg, p, load_ty, endianness)?);
            }
            // Dedup: if all per-predecessor results coincide, skip the
            // ValuePhi — returning the common value keeps the graph
            // smaller and exposes it to later passes more cleanly.
            if resolved.iter().all(|v| *v == resolved[0]) {
                return Ok(resolved[0]);
            }
```

to:

```rust
        ResolveShape::Phi { phi_token, preds } => {
            let mut resolved: Vec<NodeOutputId> = Vec::with_capacity(preds.len());
            for p in preds {
                resolved.push(realize(fg, p, load_ty, endianness)?);
            }
            // Dedup: if all per-predecessor results coincide, skip the
            // ValuePhi — returning the common value keeps the graph
            // smaller and exposes it to later passes more cleanly.
            // `windows(2).all` is vacuously true for len < 2, but `probe`
            // already rejects MemPhi with fewer than 2 mem predecessors,
            // so `resolved.first()` is the actual emptiness guard here.
            if let Some(&first) = resolved.first() {
                if resolved.windows(2).all(|w| w[0] == w[1]) {
                    return Ok(first);
                }
            }
```

- [ ] **Step 2: Verify tests pass**

```bash
cargo test --package opt stack_load_forward 2>&1 | tail -3
```

- [ ] **Step 3: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/stack_load_forward/mod.rs
git commit -m "refactor(opt): make phi dedup check empty-safe

Replaces resolved.iter().all(|v| *v == resolved[0]) — which would
panic on empty input were probe ever to allow it — with the
explicit first()-then-windows(2) form."
```

---

## Task 8: Rename `call_sp_adjust` to `chain_anchor_offset` in `call_args.rs`

**Files:**
- Modify: `crates/opt/src/stack_store/call_args.rs:38-104`

**Why:** The variable `call_sp_adjust` (lines 49, 77, 89, 90) holds the byte offset of the *first store seen on the chain*, not the call-time SP value. The mismatch makes the code harder to read and was flagged in review as misleading. Renaming to `chain_anchor_offset` aligns name with usage.

- [ ] **Step 1: Rename the variable and update its doc-comment**

In `crates/opt/src/stack_store/call_args.rs`, rename `call_sp_adjust` → `chain_anchor_offset` everywhere within `collect_stack_args_in_chain_order` (lines 49, 77, 79, 89, 96).

Update the doc-comment block at lines 25-27 from:

```rust
/// The first store on the chain anchors `call_sp_adjust` (the SP value at
/// the call site).  Whether it is *itself* the first arg depends on the
/// architecture:
```

to:

```rust
/// The first store on the chain anchors `chain_anchor_offset` (the byte
/// offset of that first store, used as the relative origin for subsequent
/// arg-slot expectations).  Whether the anchor store is *itself* the first
/// arg depends on the architecture:
```

- [ ] **Step 2: Verify tests pass**

```bash
cargo test --package opt stack_store 2>&1 | tail -3
cargo test --package opt 2>&1 | tail -3
```

- [ ] **Step 3: Verify clippy clean**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/stack_store/call_args.rs
git commit -m "refactor(opt): rename call_sp_adjust to chain_anchor_offset

The variable holds the offset of the first store seen on the chain,
not a call-time SP value — the old name was misleading."
```

---

## Task 9: Fix the `(k * 4) as u64` sign-cast in `stack_store::tests`

**Files:**
- Modify: `crates/opt/src/stack_store/tests.rs:328`

**Why:** `let off = b.build_int_const((k * 4) as u64, NodeOutputType::U32).unwrap();` — `k` is declared `i32` in the surrounding loop, so `k * 4` is `i32`. The cast `i32 → u64` triggers `clippy::cast_sign_loss`. The test's intent is to encode a signed offset as a u64 with `U32` mask semantics (sign-extend then mask). Replace with `u64::from((k * 4) as u32)` to make the i32→u32 wraparound explicit (matches the U32 mask intent) and silences the lint cleanly.

This is a tests-only change but lives outside `tests/` (it's a unit test inside the lib's test module, where the workspace lints don't auto-allow `cast_sign_loss` even at default tier in some toolchain versions). It also slightly clarifies the test's intent.

- [ ] **Step 1: Replace the cast**

Replace `crates/opt/src/stack_store/tests.rs:328` from:

```rust
        let off = b.build_int_const((k * 4) as u64, NodeOutputType::U32).unwrap();
```

to:

```rust
        let off = b.build_int_const(u64::from((k * 4) as u32), NodeOutputType::U32).unwrap();
```

- [ ] **Step 2: Verify tests pass**

```bash
cargo test --package opt --lib stack_store:: 2>&1 | tail -3
```

- [ ] **Step 3: Verify clippy clean (default tier)**

```bash
cargo clippy --package opt --all-targets -- -D warnings 2>&1 | tail -3
```

- [ ] **Step 4: Commit**

```bash
git add crates/opt/src/stack_store/tests.rs
git commit -m "test(opt): make i32→u64 cast explicit via u32 wrap

Encodes the U32 mask intent without triggering cast_sign_loss."
```

---

## Task 10: Final verification

**Files:**
- (no edits)

**Why:** Confirms every prior task survived together — each task should be independently safe, but a final guard against ordering bugs is cheap.

- [ ] **Step 1: Full clippy + workspace test sweep**

```bash
cd /home/mike/Desktop/strider/.worktrees/opt-review-r3
cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tee /tmp/clippy.log | tail -20
cargo test --workspace 2>&1 | tee /tmp/test.log | tail -10
```

Expected:
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0 (no errors anywhere; pre-existing warnings in non-opt crates may remain — only the opt crate is in scope).
- `cargo test --workspace` reports `test result: ok.` for every target.

- [ ] **Step 2: Run benches build (compile-only)**

```bash
cargo bench --package opt --no-run 2>&1 | tail -3
```

Expected: `Finished` with no errors.

- [ ] **Step 3: If anything failed, reopen the offending task; otherwise this plan is done**

```bash
git log --oneline -10
```

Expected: 9 commits on top of the baseline (`8c2400b`), one per task above.

---

## Self-review checklist

- [x] **Spec coverage:** Every line of the user request is covered:
  - "Review opt crate for correctness" → Tasks 2 (Sdiv/Srem narrow overflow), 2b (unmasked-input Div/Rem/Shr/Equal/Less/Carry/Borrow), 2c (Truncate-rule masking), plus the explicitly-documented non-bugs in the findings summary.
  - "Code that can be simplified" → Tasks 3 (eval_int_cmp DRY), 4 (redundant_phis double-loop), 6 (probe Vec drop), 7 (realize dedup).
  - "More readable" → Task 8 (rename `call_sp_adjust`).
  - "Pass all clippy warnings" → Tasks 1 (integration-test allows) + 9 (sign-cast tests fix).
- [x] **No placeholders:** every step has concrete code or commands.
- [x] **Type consistency:** `remove_phis` signature change in Task 5 matches the new call-site type in Task 5; the `eval_int_binary` / `eval_int_cmp` test access path in Tasks 2 / 2b is documented.
- [x] **Bite-sized tasks:** each task is one concern, one commit, ~5 minutes of work.
- [x] **Task ordering preserves correctness:** Task 2 lands the narrow-Sdiv/Srem skip first; Task 2b's entry-mask is idempotent on top; Task 3's DRY refactor is mechanical; Tasks 4–9 are independent.

## Out of scope (explicitly NOT in this plan)

- **Pedantic clippy** (cast widening, `const fn`, unnecessary structure-name repetition) — workspace doesn't enable pedantic; user requested "clippy warnings" which this plan resolves at default tier.
- **Pipeline `OptimizationResult` API redesign** — the boilerplate `OptimizationResult::Changed/NoChange` repetition is real but rewriting the API surface is a behavior risk for every pass and out of scope for a review-driven cleanup.
- **`mem_chain_is_dirty` cycle handling** — current design choice (`return false` on cycle) is documented in source; tightening to a more pessimistic cycle verdict could create false-negative shadow misses across siblings and warrants its own design discussion.
- **Splitting large modules** — `function_args/mod.rs` (440 lines) and `stack_load_forward/mod.rs` (284 lines) are at the high end but each has one clear responsibility; splitting is a separate refactor with its own risk surface.
- **Doctest activation for `LoadReadOnly`** — the `///` block uses ` ```ignore ` (line 24); making it executable requires a fake `ReadOnlyMemory` and is doc work, not review work.
- **Pushing input-masking into the IR layer** — the cleanest way to eliminate unmasked `IntConst` nodes once and for all is to make `make_int_const`/`build_int_const` mask the value to `ty` at construction. That's an `ir`-crate API change with workspace-wide ripple (every existing `IntConst(some_u64, narrow_ty)` call site needs auditing) and belongs in a separate review of the `ir` crate. The defensive entry-masking in Tasks 2b/2c is the localised fix that closes the bug for the `opt` crate without touching `ir`'s contract.
