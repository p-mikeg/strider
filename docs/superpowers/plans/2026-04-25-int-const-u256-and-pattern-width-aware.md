# IntConst u128 + Width-Aware Pattern Const Queries — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Widen `NodeKind::IntConst`'s payload from `u64` to native Rust `u128`, drop the `Result` return type from `build_int_const`, propagate u128 arithmetic through `opt::constant_fold` and every consumer, fix the pattern crate's `int_const(...)` matcher so that querying `-50` (or any value) "just works" at any IntConst width by comparing modulo the node's declared width, and extend the analyzer's `vn_mask` to handle 16-byte (XMM0/q0) registers — the one real consumer of u128 IntConst today.

**Architecture:** Three layered changes.
1. **Storage widening (IR):** `IntConst(u64)` → `IntConst(u128)`. Add helpers on `NodeOutputType` for u128/i128 masking. `build_int_const` becomes infallible (`fn build_int_const(impl Into<u128>, NodeOutputType) -> NodeOutputId`) — masking happens inside.
2. **Arithmetic widening (opt):** `eval_int_binary` and friends widen to u128 arithmetic, supporting U64 and U128 uniformly. `KnownBits` masks become `u128`. `IntVar::get` returns `u128`. **No narrowing inside opt rules** — they compute at full u128 width.
3. **Width-aware pattern queries (pattern):** `int_const(v)` accepts any signed-or-unsigned integer literal (`impl Into<i128>`). At match time, both stored and query values are masked to the IntConst's declared width and compared. This fixes BUG-21 (negative constants on 32-bit archs) without per-arch defaults — the IntConst's own type drives the comparison.

**Why u128, not u256:** audited the codebase — only one path generates a wide IntConst today: the analyzer's `register_aliasing.rs` shift/mask constants for 16-byte SSE/NEON containers (XMM0 on x86_64, q0 on aarch64). Currently blocked by `vn_mask(16)` returning `UnsupportedRegSize` before reaching `build_int_const`. **No code path generates a 32-byte (U256) IntConst.** No Sleigh register on x86_64/aarch64/arm/mips32 is 32 bytes; AVX-2 YMM exists in hardware but Sleigh's x86-64 spec doesn't model YMM as varnodes flowing through this analyzer. `NodeOutputType::U256` stays as an enum variant; `build_int_const` will panic if anyone constructs one until a real consumer arrives. When that happens, the swap from `u128` → an external `ethnum::U256` is mechanical (signature widening + storage type swap).

**Tech Stack:** Rust 2024 edition, native `u128`/`i128` (no external bigint dep), `cranelift-entity`, existing workspace crates.

---

## Pre-flight context

### Current state (anchor commits on `review/analyzer-2026-04-25`)

- `BUG-2` (CFG fall-through), `BUG-3/10` (extend_if_needed Bool→AnyInt) fully fixed (commits `fa6a5c1`, `f0c2f04`).
- Partial BUG-8/9 (float subsystem): `ret_val_regs_float` plumbing + `read_reg_vn` truncate landed (`d2aa0ac`); soft-float archs (arm/mips32le/mips32be) now pass float arithmetic tests; hardware-FPU archs (x86/x64/aarch64) currently fail because `write_reg_vn` through 16-byte XMM0/q0 hits the U128 panic path. **This plan removes that panic.**
- `BUG-13` (AArch64 128-bit array-init constant): the same U128 panic path. **Resolved by this plan.**
- `BUG-21` (negative IntConst sign-extension differs across widths): the pattern-crate bug **resolved by Phase D**.
- Suite state: 311 pass, 130 ignored, 0 fail.

### Caller-impact estimate

Greps from current branch:

- `IntConst(` references: ~197 (most are match arms or test-fixture literals).
- `build_int_const(` calls: ~365.
- `.unwrap()` / `?` after `build_int_const`: ~44.

Mechanical cascade. Trickiest sites: opt's eval rules (need genuine u128 arithmetic) and the pattern crate's matcher.

### Why no u256-capable bigint crate

Rust ships `u128` and `i128` natively. Every method we need (`wrapping_add`, `wrapping_mul`, `checked_div`, `<<`, `>>`, `&`, `|`, `^`, signed shift via `i128`) exists in `core`. Zero new transitive deps. If U256 ever becomes a real requirement, the change to swap in `ethnum::U256` is local to `crates/ir/src/node/kind.rs` (storage type) and the helpers/eval functions (arithmetic surface).

---

## Resolved questions

| Q | Decision | Notes |
|---|----------|-------|
| Q1 | `IntVar::get -> Option<u128>` | Opt rules compute at u128 width with no narrowing. Sites that genuinely need u64 add explicit `u64::try_from(v).ok()` (returns `None` on overflow). Most sites stay in u128. |
| Q2a | `int_const(v: impl Into<i128>)` | Accepts every common signed/unsigned literal (`i32`, `i64`, `i128`, `u32`, `u64`). Values larger than `i128::MAX` (e.g. `u128::MAX`) require explicit `i128`-shaped construction or use `int_const_unsigned(v: u128)` if added later. |
| Q2b | Mask both stored and query values to the IntConst's declared width, compare equality | Negative inputs work because `(-50i64 as u64) & 0xffffffff == (-50i32 as u32 as u64)`. |
| Q2c | Skip `int_const_at_width(v, ty)` | Width-aware default suffices. |
| Q3 | Literal `0`, `1`, etc. silently widen via `Into<u128>` (RHS of rewrite rules) | No API change for callers writing `int_const(0)`. |
| Q4 | Sign-aware ops (Sdiv/Srem/SShr/Sless/SlessEqual/Sborrow/Scarry) route through `get_signed_int_i128` | Mirror the existing u64-path's `get_signed_int` pattern. KnownBits' u128 widening is a transparent type widening — bit-tracking semantics are agnostic of width as long as masks reflect each node's declared type, which existing code already does. |
| Q5 | Clean break for any external consumers (e.g. planned `strider-py`) | None exist yet. |

---

## File structure (high level)

- **`crates/ir/src/node/kind.rs`:** `IntConst(u64)` → `IntConst(u128)`.
- **`crates/ir/src/node/output_type.rs`:** add `bit_mask_u128()`, `get_unsigned_int_u128()`, `get_signed_int_i128()`.
- **`crates/ir/src/builder/nodes.rs`:** `build_int_const` signature change (drop Result, accept `impl Into<u128>`).
- **`crates/ir/src/builder/coerce.rs`:** `extend_if_needed`, `truncate_if_needed`, `convert_to_int_if_needed`, `get_as_int` widen to u128.
- **`crates/ir/src/error.rs`:** delete `IntConstWidthExceedsU64` variant.
- **`crates/ir/src/dot/label.rs`:** display U128 IntConst values in hex (already supports u64 display; widen the format).
- **`crates/opt/src/constant_fold/eval_int.rs`:** widen `eval_int_binary`, `eval_int_cmp`, helpers to u128/i128 arithmetic.
- **`crates/opt/src/constant_fold/rules.rs`:** rewrite-rule consumers (`int_const_with!` macro) widen.
- **`crates/opt/src/known_bits/`:** masks become u128.
- **`crates/pattern/src/var.rs`:** `IntVar::get` returns `Option<u128>`.
- **`crates/pattern/src/pat/ctor/wildcards.rs`:** `int_const(v: impl Into<i128>)` width-aware compare.
- **`crates/pattern/src/pat/ctor/consts.rs`:** `int_const_with_fn` widens.
- **`crates/pattern/src/macros.rs`:** `int_const_with!` macro widens its captured value type.
- **`crates/analyzer/src/utils.rs`:** extend `vn_mask` to handle 16-byte registers (returns `u128`).
- **All `build_int_const(...)?` callers across the workspace:** drop `?`, widen literal types where appropriate.
- **All `IntConst(c)` match-arm consumers:** `c` becomes `u128`; downstream callers either use u128 directly or call `c.try_into::<u64>().ok()` for in-range values.

---

## Phase A — Foundation: helpers on NodeOutputType

### Task 1: Add u128/i128 helpers on `NodeOutputType`

**Files:**
- Modify: `crates/ir/src/node/output_type.rs` (around line 130–170)
- Tests inline (`#[cfg(test)] mod tests { ... }` at the bottom of the same file)

- [ ] **Step 1: Read the existing helpers**

Open `crates/ir/src/node/output_type.rs:130-170` and confirm the existing `get_unsigned_int(self, val: u64) -> Option<u64>` and `get_signed_int(self, val: u64) -> Option<i64>` shape. The new helpers mirror these but at u128/i128 width.

- [ ] **Step 2: Add new methods**

Append after the existing `get_signed_int` method:

```rust
    /// Returns the all-ones bit mask for this integer type, as `u128`.
    /// `Bool` returns `1`; integer widths return their natural bit widths.
    /// `U256` returns `u128::MAX` as a best-effort sentinel — this method is
    /// not meaningful for U256 and the IntConst path panics for U256 today;
    /// callers that genuinely need U256 must be revisited when U256 support
    /// is added.  Float types return `0` (defensive — no caller should ask).
    #[must_use]
    pub fn bit_mask_u128(self) -> u128 {
        let bits = self.bit_width();
        if bits == 0 || !self.is_integer() {
            return 0;
        }
        if bits >= 128 {
            return u128::MAX;
        }
        (1u128 << bits) - 1
    }

    /// Masks `val` to this type's bit width.  For widths ≥ 128 returns `val`
    /// unchanged.  Companion to [`Self::get_unsigned_int`] but works at u128
    /// width.
    #[must_use]
    pub fn get_unsigned_int_u128(self, val: u128) -> Option<u128> {
        if !self.is_integer() {
            return None;
        }
        Some(val & self.bit_mask_u128())
    }

    /// Sign-extends `val` (treated as the type's bit-width-narrow representation)
    /// to a full 128-bit signed integer.  Companion to [`Self::get_signed_int`]
    /// but works at U128 width.
    ///
    /// For widths > 128 returns `None` — i128 cannot represent values wider
    /// than 128 bits as signed.  No current consumer hits this case
    /// (NodeOutputType::U256 is unreachable in IntConst land today).
    #[must_use]
    pub fn get_signed_int_i128(self, val: u128) -> Option<i128> {
        if !self.is_integer() {
            return None;
        }
        let bits = self.bit_width();
        if bits == 0 || bits > 128 {
            return None;
        }
        let masked = val & self.bit_mask_u128();
        if bits == 128 {
            return Some(masked as i128);
        }
        // Sign-extend: if the high bit at position bits-1 is set, OR in the
        // top (128-bits) bits to produce a negative i128.
        let sign_bit = 1u128 << (bits - 1);
        if (masked & sign_bit) != 0 {
            let high_extension = !((1u128 << bits) - 1);
            Some((masked | high_extension) as i128)
        } else {
            Some(masked as i128)
        }
    }
```

- [ ] **Step 3: Add unit tests inline**

In the existing `#[cfg(test)] mod tests { ... }` (or create one if absent):

```rust
    #[test]
    fn bit_mask_u128_widths() {
        assert_eq!(NodeOutputType::Bool.bit_mask_u128(), 0x1u128);
        assert_eq!(NodeOutputType::U8.bit_mask_u128(), 0xffu128);
        assert_eq!(NodeOutputType::U16.bit_mask_u128(), 0xffffu128);
        assert_eq!(NodeOutputType::U32.bit_mask_u128(), 0xffff_ffffu128);
        assert_eq!(NodeOutputType::U64.bit_mask_u128(), u64::MAX as u128);
        assert_eq!(NodeOutputType::U128.bit_mask_u128(), u128::MAX);
        // Float types return 0 (defensive — no caller should ask).
        assert_eq!(NodeOutputType::F32.bit_mask_u128(), 0);
        assert_eq!(NodeOutputType::F64.bit_mask_u128(), 0);
    }

    #[test]
    fn get_unsigned_int_u128_masks_to_width() {
        // 0x12345678 masked to U16 = 0x5678.
        assert_eq!(
            NodeOutputType::U16.get_unsigned_int_u128(0x12345678u128),
            Some(0x5678u128)
        );
        // 0x12345678 masked to U32 = 0x12345678.
        assert_eq!(
            NodeOutputType::U32.get_unsigned_int_u128(0x12345678u128),
            Some(0x12345678u128)
        );
        // U128 masking is identity.
        assert_eq!(
            NodeOutputType::U128.get_unsigned_int_u128(u128::MAX),
            Some(u128::MAX)
        );
        // Float types return None.
        assert_eq!(NodeOutputType::F32.get_unsigned_int_u128(0x12345678u128), None);
    }

    #[test]
    fn get_signed_int_i128_sign_extends_negative_at_narrow_widths() {
        // -50 stored at U32 width is 0xffff_ffce.  Sign-extending to i128
        // must produce -50.
        let neg50_at_u32 = 0xffff_ffceu128;
        assert_eq!(
            NodeOutputType::U32.get_signed_int_i128(neg50_at_u32),
            Some(-50i128)
        );
        // -50 stored at U8 width is 0xce.  Sign-extending must give -50.
        assert_eq!(
            NodeOutputType::U8.get_signed_int_i128(0xceu128),
            Some(-50i128)
        );
        // Positive 50 at U32 stays 50.
        assert_eq!(
            NodeOutputType::U32.get_signed_int_i128(50u128),
            Some(50i128)
        );
    }

    #[test]
    fn get_signed_int_i128_handles_full_u128_width() {
        // U128 with high bit set: read as negative i128.
        let neg1_at_u128 = u128::MAX;
        assert_eq!(
            NodeOutputType::U128.get_signed_int_i128(neg1_at_u128),
            Some(-1i128)
        );
        // U128 max-positive (high bit clear): stays positive when reinterpreted as i128.
        let max_pos = i128::MAX as u128;
        assert_eq!(
            NodeOutputType::U128.get_signed_int_i128(max_pos),
            Some(i128::MAX)
        );
    }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ir --lib node::output_type::tests::`
Expected: 4 new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/node/output_type.rs
git commit -m "feat(ir): add u128/i128 mask + sign-extension helpers on NodeOutputType

Three new methods that companion the existing u64-only helpers:
  - bit_mask_u128() returns the type's bit-width all-ones mask as u128
  - get_unsigned_int_u128(val) masks val to the type's width
  - get_signed_int_i128(val) sign-extends a width-narrow val to i128

Foundation for widening NodeKind::IntConst to u128.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase B — Storage widening: NodeKind::IntConst payload

### Task 2: Widen `IntConst` payload to `u128`

**Files:**
- Modify: `crates/ir/src/node/kind.rs:118` (the `IntConst(u64)` definition)
- Modify: `crates/ir/src/node/tests.rs` (test fixtures using `IntConst(42)` etc.)

- [ ] **Step 1: Change the variant**

In `crates/ir/src/node/kind.rs` line 118:

```rust
IntConst(u128),
```

- [ ] **Step 2: Build the workspace and let the compiler enumerate breakages**

Run: `cargo build --workspace 2>&1 | tee /tmp/build_after_task2.log | head -100`
Expected: many type errors. Tally them — most will be in `build_int_const`, opt eval rules, dot/label, and tests.

The errors are the to-do list for tasks 3+. Don't try to fix them all in this step.

- [ ] **Step 3: Quick-fix all the literals in tests/match arms with simple `u128` widening**

For every `NodeKind::IntConst(<integer-literal>)` site (search with `git grep 'IntConst(' -- ':!docs/'`), suffix integer literals with `u128`. Examples:

```rust
NodeKind::IntConst(42)          → NodeKind::IntConst(42u128)
NodeKind::IntConst(0)           → NodeKind::IntConst(0u128)
let bits = 0xdeadbeef_u64;
NodeKind::IntConst(bits)        → NodeKind::IntConst(u128::from(bits))
```

For match arms binding the inner value:

```rust
if let NodeKind::IntConst(v) = ... { /* v: u64 */ }
                              ↓
if let NodeKind::IntConst(v) = ... { /* v: u128 — narrow as needed */ }
```

Sites that downstream want `u64` add `.try_into::<u64>().ok()` — returns `None` if the u128 doesn't fit. Mark each such narrowing with a comment if the original code assumed 64-bit fit.

- [ ] **Step 4: Build incrementally**

Run: `cargo build --workspace 2>&1 | tail -20`
Iterate until the IR + pattern crates build. Opt/analyzer build errors are expected at this stage — they're addressed in Tasks 3+.

- [ ] **Step 5: Don't commit yet** — Task 3 is the natural commit point. This task's edits are a prerequisite.

---

### Task 3: Drop `Result` from `build_int_const`, accept `impl Into<u128>`

**Files:**
- Modify: `crates/ir/src/builder/nodes.rs:79-98` (`build_int_const` and `build_uint64_const`)
- Modify: `crates/ir/src/error.rs` (delete `IntConstWidthExceedsU64` variant)
- Modify: every `build_int_const(...)?` caller across the workspace (~365 sites)

- [ ] **Step 1: Replace `build_int_const`**

In `crates/ir/src/builder/nodes.rs` lines 75–98:

```rust
    /// Emits an integer constant node.
    ///
    /// `val` is masked to `output_type`'s bit width before storage.  Accepts
    /// any value convertible to `u128` — most callers pass a `u64` literal.
    ///
    /// # Panics
    ///
    /// Panics if `output_type` is not an integer type, or is `U256` (which
    /// is not yet representable in the u128 storage; no current consumer
    /// produces a U256 IntConst, see plan
    /// `2026-04-25-int-const-u256-and-pattern-width-aware.md`).
    pub fn build_int_const(
        &mut self,
        val: impl Into<u128>,
        output_type: NodeOutputType,
    ) -> NodeOutputId {
        assert!(
            output_type.is_integer(),
            "build_int_const called with non-integer type {output_type:?}"
        );
        assert!(
            !matches!(output_type, NodeOutputType::U256),
            "build_int_const(U256) not yet supported — IntConst storage is u128"
        );
        let val = val.into() & output_type.bit_mask_u128();
        self.build_single_output_pure(NodeKind::IntConst(val), [], output_type)
    }
```

Delete `build_uint64_const` entirely. Search for callers and replace `build_uint64_const(x)?` → `build_int_const(x, NodeOutputType::U64)`. Verify with `git grep build_uint64_const` after edits — should return nothing.

- [ ] **Step 2: Delete the error variant**

In `crates/ir/src/error.rs`, find and delete the `IntConstWidthExceedsU64` enum variant (and any `#[error("...")]` attribute attached to it). Run `cargo build -p ir 2>&1 | grep error:` — the compiler will surface anywhere the variant is named.

- [ ] **Step 3: Drop `?` from every `build_int_const` call**

Run: `git grep -l 'build_int_const(' -- 'crates/'` to enumerate the files. For each file, the simplest sed pass:

```bash
sed -i 's|\.build_int_const(\([^)]*\))?|\.build_int_const(\1)|g' <file>
```

Multi-line expressions need manual editing. After the sed pass, run `cargo build --workspace 2>&1 | grep -E '^(error|warning):'` and fix residuals.

Common patterns to fix manually:

```rust
// Before:
let c = self.builder.build_int_const(0, ty)?;

// After:
let c = self.builder.build_int_const(0u64, ty);
```

(Add `u64` suffix to bare integer literals so the `Into<u128>` resolution is unambiguous.)

- [ ] **Step 4: Build the workspace**

Run: `cargo build --workspace 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 5: Run tests to confirm no behavioural regression**

Run: `cargo test --workspace 2>&1 | grep -E 'test result|FAILED' | head -30`
Expected: every `test result: ok`. The masking inside `build_int_const` is semantically equivalent to the old behavior for in-range values.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(ir): widen IntConst payload to u128, drop Result from build_int_const

NodeKind::IntConst now stores u128 (was u64).  build_int_const takes
impl Into<u128>, masks to the type's bit width, returns NodeOutputId
directly — no Result.  Removes the IntConstWidthExceedsU64 error
variant (no longer reachable) and the build_uint64_const wrapper.

Cascades through ~365 caller sites: drops .unwrap() and ? from each.
NodeKind::IntConst(N) literals in test fixtures get a u128 suffix.

U256 IntConst remains a panic — no current consumer produces one.
When that changes, the swap to ethnum::U256 storage is local to
NodeKind::IntConst's payload and the helpers/eval functions.

Resolves the U128 panic in the analyzer's write_reg_vn path
(BUG-13 / part of BUG-9 in analyzer-known-issues).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Widen `extend_if_needed`/`truncate_if_needed`/`get_as_int` for u128

**Files:**
- Modify: `crates/ir/src/builder/coerce.rs`

- [ ] **Step 1: Inspect current `get_as_int`**

```bash
grep -n "fn get_as_int" crates/ir/src/builder/coerce.rs
```

Current likely returns `Option<(u64, i64)>` (unsigned/signed pair).

- [ ] **Step 2: Widen its return type**

Change `get_as_int` to return `Option<(u128, i128)>`. Inside, use the new `get_unsigned_int_u128` / `get_signed_int_i128` helpers instead of u64/i64.

- [ ] **Step 3: Update `extend_if_needed`'s constant-folding branch**

In `extend_if_needed`, the early-return on `get_as_int` was building `IntConst(unsigned_val as u64, ...)` — now build `IntConst(unsigned_val, ...)` (u128 value passes through unchanged since `build_int_const` accepts u128).

For the sign-extend branch: `IntConst(signed_val as u64, ...)` was reinterpreting the i64 bits as u64. Equivalent now: `IntConst(signed_val as u128, ty)` — relying on Rust's `i128 as u128` reinterpreting bits, plus the inner `bit_mask_u128` masking to the target width.

- [ ] **Step 4: Build + test**

Run: `cargo test -p ir --lib`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/builder/coerce.rs
git commit -m "feat(ir): widen extend_if_needed/get_as_int to u128/i128 storage

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase C — Arithmetic widening: opt/constant_fold + known_bits

### Task 5: Widen `eval_int_binary` and `eval_int_cmp` to u128

**Files:**
- Modify: `crates/opt/src/constant_fold/eval_int.rs`
- Modify: `crates/opt/src/constant_fold/tests.rs` (existing test calls + new wider-width tests)

- [ ] **Step 1: Read the current `eval_int_binary`**

Open `crates/opt/src/constant_fold/eval_int.rs:11-100`. Note: `fn eval_int_binary(op: IntBinaryOp, l: u64, r: u64, ty: NodeOutputType) -> Option<u64>`.

- [ ] **Step 2: Replace the signature and body**

```rust
pub(super) fn eval_int_binary(
    op: IntBinaryOp,
    l: u128,
    r: u128,
    ty: NodeOutputType,
) -> Option<u128> {
    let mask = ty.bit_mask_u128();
    let l = l & mask;
    let r = r & mask;
    let result = match op {
        IntBinaryOp::Add  => l.wrapping_add(r),
        IntBinaryOp::Sub  => l.wrapping_sub(r),
        IntBinaryOp::Mul  => l.wrapping_mul(r),
        IntBinaryOp::Div  => {
            if r == 0 { return None; }
            l / r
        }
        IntBinaryOp::Rem  => {
            if r == 0 { return None; }
            l % r
        }
        IntBinaryOp::Sdiv => {
            if r == 0 { return None; }
            let l_signed = ty.get_signed_int_i128(l)?;
            let r_signed = ty.get_signed_int_i128(r)?;
            // Two's-complement overflow: i128::MIN / -1 is undefined in Rust;
            // skip and let the fold fail (returns None).
            if l_signed == i128::MIN && r_signed == -1i128 {
                return None;
            }
            (l_signed.wrapping_div(r_signed)) as u128 & mask
        }
        IntBinaryOp::Srem => {
            if r == 0 { return None; }
            let l_signed = ty.get_signed_int_i128(l)?;
            let r_signed = ty.get_signed_int_i128(r)?;
            if l_signed == i128::MIN && r_signed == -1i128 {
                return None;
            }
            (l_signed.wrapping_rem(r_signed)) as u128 & mask
        }
        IntBinaryOp::And  => l & r,
        IntBinaryOp::Or   => l | r,
        IntBinaryOp::Xor  => l ^ r,
        IntBinaryOp::ShiftLeft  => {
            let bits = ty.bit_width() as u32;
            if bits == 0 { return Some(0); }
            let shift_count = (r as u32) % bits;
            l.wrapping_shl(shift_count) & mask
        }
        IntBinaryOp::ShiftRight => {
            let bits = ty.bit_width() as u32;
            if bits == 0 { return Some(0); }
            let shift_count = (r as u32) % bits;
            l.wrapping_shr(shift_count)
        }
        IntBinaryOp::SShiftRight => {
            let bits = ty.bit_width() as u32;
            if bits == 0 { return Some(0); }
            let shift_count = (r as u32) % bits;
            let l_signed = ty.get_signed_int_i128(l)?;
            l_signed.wrapping_shr(shift_count) as u128 & mask
        }
    };
    Some(result & mask)
}
```

(If `IntBinaryOp` has variants this snippet doesn't cover, fill them in following the same pattern. The actual variant list is in `crates/ir/src/node/kind.rs`.)

- [ ] **Step 3: Widen `eval_int_cmp`**

Similar treatment: `l: u64, r: u64` → `l: u128, r: u128`. Signed comparisons go through `get_signed_int_i128`. Run `grep -n 'eval_int_cmp\|fn eval_' crates/opt/src/constant_fold/eval_int.rs` to enumerate all helpers.

- [ ] **Step 4: Update existing unit tests**

Tests previously calling `eval_int_binary(op, 5u64, 3u64, U64)` now pass `5u128, 3u128`. Asserts comparing `result == Some(8u64)` become `Some(8u128)`.

- [ ] **Step 5: Add U128 unit tests**

Append to `crates/opt/src/constant_fold/tests.rs`:

```rust
    #[test]
    fn eval_int_binary_handles_u128_overflow_correctly() {
        // u128::MAX + 1 wraps at U128 width to 0.
        let got = eval_int_binary(IntBinaryOp::Add, u128::MAX, 1u128, NodeOutputType::U128);
        assert_eq!(got, Some(0));
    }

    #[test]
    fn eval_int_binary_sdiv_at_u128_width() {
        // (-50) / 5 = -10 at U128 width.
        let neg50 = (-50i128) as u128;
        let got = eval_int_binary(IntBinaryOp::Sdiv, neg50, 5u128, NodeOutputType::U128);
        let neg10 = (-10i128) as u128;
        assert_eq!(got, Some(neg10));
    }

    #[test]
    fn eval_int_binary_shift_count_modular_at_u128_width() {
        // ShiftLeft by 128 at U128 wraps modular: shift_count % 128 == 0.
        let got = eval_int_binary(
            IntBinaryOp::ShiftLeft,
            0xdeadbeef_u128,
            128u128,
            NodeOutputType::U128,
        );
        assert_eq!(got, Some(0xdeadbeef_u128));
    }

    #[test]
    fn eval_int_cmp_signed_negative_at_u32_width() {
        // -1 < 0 (signed comparison at U32) — even though as u32 it's 0xffff_ffff > 0.
        let neg1_at_u32 = 0xffff_ffffu128;
        let got = eval_int_cmp(IntCmpOp::Sless, neg1_at_u32, 0u128, NodeOutputType::U32);
        assert_eq!(got, Ok(true));
    }
```

- [ ] **Step 6: Test**

Run: `cargo test -p opt 2>&1 | grep -E 'test result|FAILED' | head`
Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add crates/opt/src/constant_fold/eval_int.rs crates/opt/src/constant_fold/tests.rs
git commit -m "feat(opt): widen eval_int_binary/cmp to u128/i128 arithmetic

Constant folding now handles U128 IntConst values uniformly with U64
and below.  Signed ops route through get_signed_int_i128 for proper
sign-extension; shift counts use modular reduction by ty.bit_width().

Adds 4 new unit tests covering U128 overflow wrap, signed division,
modular shift behaviour, and signed comparison at narrow widths.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: Widen `KnownBits` masks to u128

**Files:**
- Modify: `crates/opt/src/known_bits/` (every file using u64 masks)

- [ ] **Step 1: Audit current widths**

```bash
grep -rn ": u64\|as u64\|u64::" crates/opt/src/known_bits/ | head -30
```

The pass tracks per-output-edge known-zero/known-one bit masks. Widen to `u128`.

- [ ] **Step 2: Widen the bit-tracking type**

Change the internal mask type from `u64` to `u128`. The merge/intersect/union logic stays bit-for-bit equivalent — just on a wider integer.

KnownBits' contract: at any node, `known_zeros & known_ones == 0` (no bit can be both known-zero and known-one), and the meaningful range is `0..ty.bit_width()` of the node's output type. With u128 storage, bits above `ty.bit_width()` are simply unused — same as u64 storage was for U32 inputs. **No semantic change at U8/U16/U32/U64; new coverage at U128.**

- [ ] **Step 3: Test**

Run: `cargo test -p opt 2>&1 | grep -E 'test result|FAILED' | head`
Expected: pass. The widening should be transparent for U8–U64 inputs.

- [ ] **Step 4: Add a U128 KnownBits test**

In `crates/opt/src/known_bits/tests.rs` (or wherever the tests live), add one focused test:

```rust
#[test]
fn known_bits_at_u128_width_matches_constant_fold() {
    // (a | 0x_FFFF_FFFF_0000_0000_FFFF_FFFF_0000_0000) when a is unknown:
    // the known-ones mask should be exactly that constant; the unknown bits
    // are the complement.  This pins that KnownBits' u128 storage is correct.
    todo!("see existing tests for patterns; mirror at U128 width");
}
```

(If the existing test infrastructure doesn't cleanly accommodate U128 fixtures, skip this test — the U64 tests + the eval_int U128 tests jointly cover the contract. Document the skip in the commit message.)

- [ ] **Step 5: Commit**

```bash
git add crates/opt/src/known_bits/
git commit -m "feat(opt): widen KnownBits internal masks to u128

Bit-tracking semantics unchanged for U8-U64; U128 inputs now tracked
correctly (was previously truncated to u64 silently).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: Widen rewrite-rule helpers (`int_const_with!` macro)

**Files:**
- Modify: `crates/pattern/src/macros.rs` (the `int_const_with!` macro definition)
- Modify: `crates/opt/src/constant_fold/rules.rs` (callers, if needed)

- [ ] **Step 1: Inspect the current macro**

```bash
grep -A 30 'macro_rules! int_const_with' crates/pattern/src/macros.rs
```

The macro captures `IntVar` values and lets the user write `int_const_with!([l, r] => l + r)` where the captured values are integer-typed. Today they're u64. Widen to u128.

- [ ] **Step 2: Replace the macro body**

Adapt to the actual macro shape; the key change is the type binding inside the closure: `let $var: u128 = b.get_int($var)?;` (was `let $var: u64`). Most rule bodies use only `+`, `-`, `&`, `|`, `^`, `<<`, `>>`, `wrapping_add`, etc., all of which exist on u128 with identical signatures, so the rule bodies usually need no change.

- [ ] **Step 3: Update opt rules that explicitly type-narrow**

Compile errors will enumerate them. Most are `c1 + c2` (works on u128 unchanged). A few may have `c1 as u32` or similar — those need to compute at u128 then narrow only at the boundary if truly needed.

- [ ] **Step 4: Test**

Run: `cargo test -p pattern -p opt 2>&1 | grep -E 'test result|FAILED' | head`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/pattern/src/macros.rs crates/opt/src/constant_fold/rules.rs
git commit -m "feat(pattern,opt): widen int_const_with! macro captures to u128

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase D — Width-aware pattern const queries (BUG-21 fix)

### Task 8: Widen `IntVar::get` to return `u128`

**Files:**
- Modify: `crates/pattern/src/var.rs` (or wherever `IntVar` is defined)
- Modify: `crates/pattern/src/matcher/match_result.rs:45` (`fn get_int`)

- [ ] **Step 1: Change return type**

In `crates/pattern/src/matcher/match_result.rs:45`:

```rust
pub fn get_int(&self, iv: IntVar) -> Option<u128> {
    self.int_bindings.get(&iv).copied()
}
```

(Adjust field name/type per the actual struct.)

The internal storage was `HashMap<IntVar, u64>` — change to `HashMap<IntVar, u128>`. Every site that stores an IntVar binding now stores `u128`; the constant evaluator already returns u128 after Task 5.

- [ ] **Step 2: Update consumers**

`b.get_int(c)` callers in opt/rewrite rules now receive `Option<u128>`. Most uses (`if let Some(v) = b.get_int(c) { ... }`) work directly on u128. Sites that genuinely need a u64 add `u64::try_from(c).ok()` or document the truncation.

- [ ] **Step 3: Build + test**

Run: `cargo test -p pattern -p opt 2>&1 | grep -E 'test result|FAILED'`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(pattern): IntVar::get returns Option<u128>

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 9: Width-aware `int_const(v)` — accepts any signed/unsigned literal

**Files:**
- Modify: `crates/pattern/src/pat/ctor/wildcards.rs:35-39` (the `int_const` constructor)
- Modify: `crates/pattern/src/pat/node_pat.rs` (add the width-aware `KindSpec` variant)

- [ ] **Step 1: Replace the `int_const` constructor**

```rust
/// Matches an `IntConst` node whose stored value, when masked to the node's
/// declared width, equals `v`'s representation at that same width.
///
/// `v` accepts any signed or unsigned integer literal (`i32`, `i64`, `i128`,
/// `u32`, `u64`).  Negative values are sign-extended to the IntConst's width
/// before comparison, so `int_const(-50)` matches both
/// `IntConst(0xffff_ffce, U32)` and `IntConst(0xffff_ffff_ffff_ffce, U64)` —
/// no per-arch default needed.
///
/// Values larger than `i128::MAX` (e.g. `u128::MAX` as raw bits) require
/// passing an `i128` constructed via `as i128` — the `Into<i128>` conversion
/// reinterprets the high bit as sign.
///
/// In build position (RHS of a rewrite rule), constructs an `IntConst(v
/// masked to the root's output type)` node.
#[must_use]
pub fn int_const(v: impl Into<i128>) -> Pat {
    let v_signed: i128 = v.into();
    NodePat::matcher(
        KindSpec::IntConstWidthAware(v_signed),
        InputsSpec::None,
    )
    .with_build_int_const(v_signed)
    .into_pat()
}
```

The change requires adding two pieces:

1. **`KindSpec::IntConstWidthAware(i128)` variant** in `crates/pattern/src/pat/node_pat.rs`. Matcher impl: "match if the node is `IntConst(stored, ty)`, and `(stored & ty.bit_mask_u128()) == ((v as u128) & ty.bit_mask_u128())`."
2. **`with_build_int_const` builder method** that records the i128 to use when constructing an IntConst on the RHS.

- [ ] **Step 2: Add the matcher logic**

In `crates/pattern/src/pat/node_pat.rs`, in the `KindSpec::matches(...)` impl, add:

```rust
KindSpec::IntConstWidthAware(query_signed) => {
    let NodeKind::IntConst(stored) = node_kind else { return false; };
    let mask = node_output_type.bit_mask_u128();
    let stored_masked = stored & mask;
    let query_masked = (*query_signed as u128) & mask;
    stored_masked == query_masked
}
```

(Adapt variable names to the actual surrounding code.)

- [ ] **Step 3: Add the build-position logic**

When `int_const(-50)` is on the RHS of a rewrite rule and the rule fires at a U32 root, the constructed node must be `IntConst((-50i32 as u32) as u128, U32)`. Inside `NodePat::build`:

```rust
let ty = root_output_type;
let val = (query_signed as u128) & ty.bit_mask_u128();
fg.builder().build_int_const(val, ty)
```

- [ ] **Step 4: Add unit tests**

Create `crates/pattern/tests/matching/int_const_width_aware.rs`:

```rust
//! Width-aware int_const matching: -50 matches IntConst at any IntConst's
//! declared width without explicit pinning.

use ir::node::NodeOutputType;
use pattern::{Matcher, int_const};

mod support;
use support::graph::FunctionGraphBuilder;

#[test]
fn negative_int_const_matches_at_u32_width() {
    let mut fb = FunctionGraphBuilder::new();
    let neg50_u32 = fb.int_const(0xffff_ffceu64, NodeOutputType::U32);
    let g = fb.ret_val(neg50_u32);
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(-50));
    assert!(!hits.is_empty(), "expected to match -50 at U32 width");
}

#[test]
fn negative_int_const_matches_at_u64_width() {
    let mut fb = FunctionGraphBuilder::new();
    let neg50_u64 = fb.int_const(0xffff_ffff_ffff_ffceu64, NodeOutputType::U64);
    let g = fb.ret_val(neg50_u64);
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(-50));
    assert!(!hits.is_empty(), "expected to match -50 at U64 width");
}

#[test]
fn negative_int_const_matches_at_u128_width() {
    let mut fb = FunctionGraphBuilder::new();
    let neg50_at_u128 = (-50i128) as u128;
    let neg50 = fb.int_const(neg50_at_u128, NodeOutputType::U128);
    let g = fb.ret_val(neg50);
    let m = Matcher::new(&g);
    let hits = m.find_all(&int_const(-50));
    assert!(!hits.is_empty());
}

#[test]
fn positive_int_const_matches_unchanged() {
    let mut fb = FunctionGraphBuilder::new();
    let fifty = fb.int_const(50u64, NodeOutputType::U32);
    let g = fb.ret_val(fifty);
    let m = Matcher::new(&g);
    assert!(!m.find_all(&int_const(50)).is_empty());
    assert!(m.find_all(&int_const(-50)).is_empty()); // Different value.
}
```

(Use the existing `crates/pattern/tests/matching/support/graph.rs` helpers. Update `FunctionGraphBuilder::int_const` if needed to accept `impl Into<u128>` matching the new `build_int_const` signature.)

- [ ] **Step 5: Run the new tests**

Run: `cargo test -p pattern --test matching int_const_width_aware`
Expected: 4 tests pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(pattern): width-aware int_const(v) — accepts any signed/unsigned literal

The int_const constructor now takes impl Into<i128> and matches at the
IntConst node's declared width.  int_const(-50) finds 0xffff_ffce in
a U32 IntConst, 0xffff_ffff_ffff_ffce in a U64 IntConst, and -50 at
U128 — without per-arch defaults.

Resolves BUG-21 from analyzer-known-issues: the pattern crate's
test_if_returns_const::{arm,mips32le,mips32be} cases (which expected
IntConst(-50)) now match cleanly across all width-narrow archs.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 10: Un-ignore `if_returns_const` cases on 32-bit archs

**Files:**
- Modify: `crates/analyzer/tests/patterns.rs:25-29` (the BUG-21 ignore block)

- [ ] **Step 1: Remove the ignore block for now-passing archs**

```rust
// Before:
per_arch_test!("patterns", "if_returns_const", if_const_pattern_finds_two_consts, ignore = {
    Arm:      "BUG-21: 32-bit IntConst(-50) sign-extension differs from u32/u64 expectations",
    Mips32le: "BUG-21: 32-bit IntConst(-50) sign-extension differs from u32/u64 expectations",
    Mips32be: "BUG-21: 32-bit IntConst(-50) sign-extension differs from u32/u64 expectations",
});

// After:
// if_returns_const: BUG-21 (width-aware int_const matching) is fixed; this is
// the regression coverage.
per_arch_test!("patterns", "if_returns_const", if_const_pattern_finds_two_consts);
```

The test body `if_const_pattern_finds_two_consts` may also need its assertion simplified — previously it checked for `0xffff_ffff_ffff_ffce` AND `0xffff_ffce` separately. With width-aware matching, a single `int_const(-50)` query suffices.

- [ ] **Step 2: Run**

Run: `cargo test -p analyzer --test patterns test_if_returns_const`
Expected: 6 tests pass (was 3 pass + 3 ignored).

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/patterns.rs
git commit -m "test(analyzer): un-ignore if_returns_const on 32-bit archs (BUG-21 fixed)

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase E — Analyzer hookup: extend vn_mask, restore 16-byte float ret regs

### Task 11: Extend `vn_mask` to support 16-byte registers

**Files:**
- Modify: `crates/analyzer/src/utils.rs` (the `vn_mask` function)
- Modify: `crates/analyzer/src/error.rs` (`UnsupportedRegSize` may need to widen its accepted range — or stay as-is and just stop being reachable for size 16)

- [ ] **Step 1: Replace `vn_mask`**

```rust
/// Returns a bitmask that covers all bits for a varnode's width in bytes.
///
/// Supported sizes are 1, 2, 4, 8, and 16 bytes — wider sub-register writes
/// through 16-byte SIMD container registers (XMM0/q0) need a u128-wide mask.
pub fn vn_mask(reg: &rsleigh::Vn) -> Result<u128> {
    match reg.size {
        1 => Ok(u128::from(u8::MAX)),
        2 => Ok(u128::from(u16::MAX)),
        4 => Ok(u128::from(u32::MAX)),
        8 => Ok(u128::from(u64::MAX)),
        16 => Ok(u128::MAX),
        _ => Err(ErrorKind::UnsupportedRegSize(reg.size).into()),
    }
}
```

(Return type changes from `u64` to `u128`. Callers in `register_aliasing.rs` already pass the result to `build_int_const`, which now accepts u128. Verify callers don't `as u64` the result.)

- [ ] **Step 2: Update existing unit tests**

In `crates/analyzer/src/utils.rs::tests`:

```rust
    #[test]
    fn mask_covers_only_the_declared_width() -> Result<()> {
        assert_eq!(vn_mask(&reg(1))?, u128::from(u8::MAX));
        assert_eq!(vn_mask(&reg(2))?, u128::from(u16::MAX));
        assert_eq!(vn_mask(&reg(4))?, u128::from(u32::MAX));
        assert_eq!(vn_mask(&reg(8))?, u128::from(u64::MAX));
        assert_eq!(vn_mask(&reg(16))?, u128::MAX);
        Ok(())
    }
```

Also update `narrower_mask_is_subset_of_wider_mask` to add the 16-byte mask. And update `unsupported_sizes_return_unsupported_reg_size_error` to drop `16` from the bad-sizes list (16 is now supported).

- [ ] **Step 3: Run**

Run: `cargo test -p analyzer --lib utils::tests::`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/analyzer/src/utils.rs
git commit -m "feat(analyzer): extend vn_mask to support 16-byte SIMD container registers

Returns u128 (was u64).  Required for the analyzer's write_reg_vn path
on 16-byte SSE/NEON containers (XMM0 on x86_64, q0 on aarch64), which
generates U128-wide mask constants.  Now that build_int_const accepts
u128, vn_mask can handle 16-byte registers without panicking.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

### Task 12: Restore ABI-correct float ret regs (q0/q1 for aarch64)

**Files:**
- Modify: `crates/target/src/calling_convention.rs:148-156` (aarch64_aapcs64's `ret_val_regs_float`)

- [ ] **Step 1: Switch from d0/d1 back to q0/q1**

```rust
            // AArch64 SIMD return regs (16-byte vector; contain s0/d0/q0
            // sub-registers).  Now that vn_mask + build_int_const support
            // U128, the ABI-correct q0/q1 (16-byte) is preferred over d0/d1
            // (which was a workaround for the U128 panic).
            ret_val_regs_float: &["q0", "q1"],
```

- [ ] **Step 2: Run analyzer test suite**

Run: `cargo test -p analyzer 2>&1 | grep 'test result' | head`
Expected: still all pass — the ABI change should be transparent for soft-float archs (which still pass via library calls) and unblock hardware-FPU paths.

- [ ] **Step 3: Try un-ignoring some of the float tests on aarch64**

Edit `crates/analyzer/tests/floats.rs` — for ONE test (e.g. `test_f32_arith`), remove the `Aarch64` entry from its `ignore = { ... }` block. Run the test:

```bash
cargo test -p analyzer --test floats test_f32_arith::aarch64
```

If it passes, remove `Aarch64` entries from every applicable `floats.rs` ignore block. If a test fails for a *different* reason (e.g. ConstantFold orphans the chain — already-known issue), restore the ignore with an updated reason and document it.

This task's success depends on whether the upstream ConstantFold orphan-bug is already gone post-IntConst-widening (it might be — the bug was triggered by the U128 panic interaction). Try and report.

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs crates/analyzer/tests/floats.rs
git commit -m "feat(target): restore ABI-correct q0/q1 float ret regs on aarch64

With u128 IntConst storage in place, the analyzer's write_reg_vn path
through 16-byte q0/q1 no longer panics.  Switching from d0/d1 (8-byte
workaround) back to q0/q1 (ABI spec) un-blocks float arithmetic tests
on aarch64.

Un-ignores N float tests on aarch64 where the U128 panic was the
gating issue; tests that fail for other reasons (e.g. ConstantFold
orphan-bug) keep their ignore with an updated reason.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Phase F — Verification

### Task 13: Final clippy + test sweep + known-issues update

- [ ] **Step 1: Workspace clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -10`
Expected: clean. The pre-existing `unwrap_used` errors on `build_int_const(...).unwrap()` go away naturally (no Result, no `.unwrap()`).

- [ ] **Step 2: Workspace test sweep**

Run: `cargo test --workspace 2>&1 | grep -E 'test result' | head -30`
Expected: every result line is `ok`. Tally pass/fail/ignore counts — this plan should land at higher pass count than the current 311.

- [ ] **Step 3: Update `2026-04-25-analyzer-known-issues.md`**

Mark these bugs as **FIXED** with their commit SHAs:
- BUG-13 (AArch64 128-bit array-init constant) — fixed by the U128 IntConst extension.
- BUG-21 (negative IntConst sign-extension across widths) — fixed by width-aware `int_const`.
- The U128-panic part of BUG-9 (float subsystem on hardware-FPU archs).

Note: BUG-8 (`FloatBinaryOp not lowered`) and the ConstantFold orphan-chain residue may still be present on hardware-FPU archs — that's separate work tracked in the doc.

- [ ] **Step 4: Final commit**

```bash
git add docs/superpowers/plans/2026-04-25-analyzer-known-issues.md
git commit -m "docs: mark BUG-13 / BUG-21 / U128 panic fixed by u128 IntConst extension

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Self-review

- **Spec coverage:**
  - "Extend IntConst to u128 with support for u64+u128 in opt constant folding": Phase A (helpers), Phase B (storage), Phase C (eval/known_bits/macros). ✓
  - "Drop Result in build_int_const": Task 3. ✓
  - "Pattern const should query on any type it specifies; -50 bug on 32-bit archs": Phase D. ✓
  - "Keep simple for the user": `int_const(-50)` works at any width via `impl Into<i128>` + width-aware compare. ✓
  - "Real consumer for u128": Phase E extends `vn_mask` for 16-byte SIMD registers and restores ABI-correct float ret regs (q0/q1) on aarch64. ✓
  - "Add tests": Tasks 1, 5, 6, 9 each add unit tests; Tasks 10 and 12 un-ignore integration tests as regression coverage. ✓

- **Placeholder scan:** No `TBD`/`fill in details`. The Task 5 step 2 says "(If `IntBinaryOp` has variants this snippet doesn't cover, fill them in following the same pattern.)" — that's a real instruction (read the actual variant list before applying), not a placeholder. Same for Task 6 step 4 (skip-if-awkward documented).

- **Type consistency:** `u128`/`i128` from std consistent throughout. `bit_mask_u128` (Task 1) used in Tasks 3, 5, 7, 9, 11. `get_signed_int_i128` (Task 1) used in Tasks 4, 5. `int_const(impl Into<i128>)` (Task 9) consistent with Q2a's signature.

- **Forward path to U256:** if a real consumer arrives, the change to swap u128 → ethnum::U256 is local: `NodeKind::IntConst`'s payload type, the helpers in `output_type.rs`, the eval functions, and `vn_mask`. The pattern API stays as `Into<i128>` until 256-bit literal queries are needed (rare).

---

## Execution handoff

Plan saved to `docs/superpowers/plans/2026-04-25-int-const-u256-and-pattern-width-aware.md`.

(Filename retained for ergonomics — it's referenced in TodoWrite and prior context. The actual plan now uses u128, with a clear forward path to u256 documented.)

Two execution options:

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks. Suits Tasks 3 (cascade through ~365 callers) and 9–10 (pattern API surface) particularly well.
2. **Inline Execution** — execute tasks in this session via executing-plans, batched checkpoints.

**Approval gate:** does this plan have your buy-in? Confirm to proceed; if anything needs revision, point it out and I'll iterate before any code is written.
