# R1 — full-codebase correctness audit

## Executive summary

- **Files audited:** ~50 source files across `target`, `pcode-lift`,
  `strider`, `cfg`, `ir`, `opt`, `pattern`, `reader` — focusing on
  per-arch lowering, register aliasing, calling-convention threading,
  endianness propagation, constant-fold semantics, and the tier-2
  indirect-branch resolver.
- **Archs audited (all 15 supported):** x86, x86_64, aarch64 (LE),
  aarch64be, arm (LE), arm_be, arm_thumb, mips32le, mips32be,
  mips64le, mips64be, ppc32be, ppc32le, ppc64be, ppc64le.
- **Bugs found:** 2 critical, 0 major, 1 deferred (low-confidence).
  Both critical bugs **fixed in this round** under TDD.
- **Final test count:** pre-mission 2846 / 0 / 26 → post-mission
  **2854 / 0 / 26** (+8 new TDD tests).
- **Clippy:** `cargo clippy --workspace --all-targets -- -D warnings`
  remains clean.

## Methodology

### TDD discipline notes

Both fixes followed strict RED-GREEN-REFACTOR:

1. **R1-1 (constant-fold shift semantics).** Wrote 6 failing tests
   pinning Sleigh's `OpBehaviorIntLeft/IntRight/IntSright` semantics
   for shifts at-or-past the type's bit-width.  Watched 5 fail with
   exactly the predicted "loop back to the low bits" wrong values
   (1 case coincidentally passed pre-fix because `i32(-1).wrapping_shr(0)
   as u128 & 0xFFFFFFFF` = the all-ones mask Sleigh returns for
   sign-bit-set sshr-by-bit_width).  Wrote minimal fix in
   `eval_int_binary`.  Verified all 6 pass.  One commit (`1c2c9b0`).

2. **R1-2 (KnownBits shift propagation).** Wrote 2 failing tests
   pinning the knock-on bug in `KnownBits`.  Watched both fail with
   the predicted masked-shift-wraps result.  Wrote minimal fix.
   Verified both pass.  One commit (`60b46fe`).

No deviations.  No `--no-verify`.  No `panic!` / `unwrap` / `expect`
introduced in production code paths (test-only `unwrap`s match
the existing per-crate `#[cfg_attr(test, allow(...))]` policy).

### Verification per commit

Each commit ran:

* `cargo test --workspace` → 2854 / 0 / 26.
* `cargo clippy --workspace --all-targets -- -D warnings` → clean.

## Findings (by severity)

### Critical (wrong IR for some valid input)

#### R1-1 (FIXED) ConstantFold's `eval_int_binary` shift semantics diverge from Sleigh for shifts at-or-past bit-width

- **Location**: `crates/opt/src/constant_fold/eval_int.rs:23-44`
  (pre-fix); same range post-fix.
- **Symptom**: For an `IntBinaryOp::ShiftLeft` /
  `IntBinaryOp::ShiftRight` / `IntBinaryOp::SShiftRight` with both
  operands constant and shift `>= bit_width`, the constant-fold
  evaluator returned `value.wrapping_shl(r % bits)` /
  `value.wrapping_shr(r % bits)` — i.e. it masked the shift amount
  modulo the type's bit width.  Sleigh's runtime semantics
  (`OpBehaviorIntLeft::evaluateBinary`,
  `sleigh/src/opbehavior.cc:411`; matching `IntRight` at :432;
  `IntSright` at :454-460) instead return:
  - `0` for any `ShiftLeft` / `ShiftRight` with shift `>= bit_width`.
  - `signbit ? all_ones : 0` for `SShiftRight` with shift
    `>= bit_width`.

  The two diverge by the full output value any time a literal shift
  lands at-or-past the type width: `IntConst(1, U32) << IntConst(32, U32)`
  pre-fix folded to `1` (Sleigh: `0`); `IntConst(0xFFFFFFFF, U32) >>
  IntConst(32, U32)` pre-fix folded to `0xFFFFFFFF` (Sleigh: `0`).
- **Root cause**: The `shift` closure used `(s as u32) % bits`, which
  is the LLVM / hardware-mask convention (x86's `shl` masks the count
  to low 5/6 bits, ARM masks to low 5/6 bits, etc.).  Sleigh chose a
  different abstract semantic — "shift past width zeroes the value" —
  to model the architecture-independent truth that a fully-shifted-out
  value is zero.  Sleigh's choice means the constant-fold needs to
  match this semantic on the IR side, since the IR's
  `IntBinaryOp::ShiftLeft` is created directly from Sleigh's
  `INT_LEFT` p-code op (see
  `crates/pcode-lift/src/value/mod.rs:65`).
- **Failing tests that pin it**:
  - `eval_int_binary_shl_at_bit_width_returns_zero_u32` —
    `crates/opt/src/constant_fold/tests.rs:1737`.
  - `eval_int_binary_shl_at_bit_width_returns_zero_u64` — :1750.
  - `eval_int_binary_shl_above_bit_width_returns_zero_u32` — :1763.
  - `eval_int_binary_shr_at_bit_width_returns_zero_u32` — :1777.
  - `eval_int_binary_sshr_at_bit_width_negative_returns_all_ones_u32`
    — :1791 (passed pre-fix coincidentally; pinned for completeness).
  - `eval_int_binary_sshr_at_bit_width_positive_returns_zero_u32` —
    :1804.
- **Fix commit**: `1c2c9b0` ("opt: fix INT_LEFT/RIGHT/SRIGHT fold for
  shifts >= bit-width").
- **Confidence**: **High**.  Sleigh's semantic is documented in
  source (`opbehavior.cc:411`) and the divergence produces wrong
  IntConsts on every supported arch — none of the 15 archs override
  the Sleigh INT_LEFT semantic.  TDD pinned the exact wrong values
  pre-fix and the corrected values post-fix.
- **Archs affected**: ALL 15.  Sleigh's INT_LEFT / INT_RIGHT /
  INT_SRIGHT semantic is arch-independent at the IR level.  Real
  occurrence frequency is low — most lifters mask the shift amount
  *before* emitting INT_LEFT (so `r` is always `< bits` at runtime)
  — but the bug is reachable whenever:
  1. A lifter directly emits `INT_LEFT(value, IntConst(K))` with
     `K >= bits` (rare but legal).
  2. KnownBits propagates a `rhs_kb.ones >= bits` (which it now does
     correctly post-R1-2), and the constant-fold sees a derived
     IntConst at-or-past the bit width.
  3. A future arch adds an instruction whose Sleigh spec emits
     unmasked INT_LEFT.

#### R1-2 (FIXED) KnownBits shift propagation masks the shift amount instead of zeroing for shifts at-or-past bit-width

- **Location**: `crates/opt/src/known_bits/mod.rs:113-164` (pre-fix);
  same range post-fix.
- **Symptom**: For `IntBinaryOp::ShiftLeft` / `IntBinaryOp::ShiftRight`
  whose `rhs` is fully known (`Kb` resolved to a single value), the
  pre-fix arms computed `let shift = (rhs_kb.ones & (bit_width - 1))
  as u32;` and reported the shifted Kb pair using that masked shift.
  For `1u8 << 8`, this produced `Kb { ones: 1, zeros: 0xFE }` (= the
  literal `1`) when the Sleigh-correct answer is `Kb { ones: 0,
  zeros: 0xFF }` (= the literal `0`).  Phase 2 of `KnownBits` then
  replaced the chain with `IntConst(1)` instead of `IntConst(0)`,
  silently introducing a wrong constant into the IR.
- **Root cause**: Same conceptual mismatch as R1-1, but reaching the
  IR through a different door — the KnownBits pass produces the
  wrong constant *first* (Phase 1's masked-shift), then phase 2
  rewrites the graph with that wrong value.  Even after R1-1 made
  ConstantFold consistent with Sleigh, the masked-shift in
  KnownBits would still introduce wrong IntConsts at every full-width
  shift the propagator encountered.
- **Failing tests that pin it**:
  - `known_bits_shl_at_bit_width_folds_to_zero_u8` —
    `crates/opt/src/known_bits/tests.rs:373`.
  - `known_bits_shr_at_bit_width_folds_to_zero_u32` — :395.
- **Fix commit**: `60b46fe` ("opt: fix KnownBits shift propagation
  for shifts >= bit-width").
- **Confidence**: **High**.  Same Sleigh source (`opbehavior.cc:411`)
  as R1-1.  TDD pinned the exact wrong constant the masked-shift
  produces and the post-fix correct constant.
- **Archs affected**: ALL 15, same as R1-1.  Frequency depends on
  whether a lifter emits a literal shift at-or-past bit-width.  Both
  fixes are needed to close the door on this class of bug; KnownBits
  alone (without the fix) feeds a wrong IntConst into ConstantFold
  *even when ConstantFold's own arm is correct* — the wrong const
  arrives via `replace_all_uses`.

### Major / Minor

(None found in this round.)

### Found but not fixed

#### F-R1-A `extract_idx_and_stride` ShiftLeft bound check uses `>= 64` instead of `>= bit_width`

- **Location**: `crates/opt/src/indirect_branch_resolve/stack_array.rs:418-428`.
- **Symptom**: The stack-array classifier's `extract_idx_and_stride`
  helper recognises `ShiftLeft(idx, IntConst(s))` as the
  shift-equivalent of `Mul(idx, 1<<s)` for a power-of-two stride.
  The bound check `if s_u128 >= 64 { return None }` rejects
  pathologically large shifts but accepts shifts that are `< 64` and
  `>= bit_width` of the IR type.  For a U8 idx `<< 8`, Sleigh
  evaluates this to `0` (degenerate "no jump table"), but our impl
  computes `stride = 1u64.checked_shl(8) = 256` and proceeds to
  enumerate as if the table had a real stride.
- **Why I didn't fix**: I have no fixture from any of the 15
  supported archs that produces a `ShiftLeft(idx, IntConst(K))` with
  `K >= bit_width(idx)`.  Real toolchains emit `idx << small_const`
  where `small_const < bit_width`.  The hypothesis is that an
  adversarial lifter or a bug in a future Sleigh spec could surface
  this, but absent a concrete reproducer I'd be writing a synthetic
  test for an out-of-band condition.
- **Suggested remediation**: Change the bound check from `s_u128 >=
  64` to `s_u128 >= bit_width(idx_ty)`, where `idx_ty` is the IR
  type of the captured `idx_var`.  Pattern-DSL exposes the input
  type via `m.get_int(...)`'s match-context; the fix is one extra
  type lookup.
- **Confidence**: **Low** that this has any observable consequence
  today.  **High** that the discrepancy with Sleigh exists in
  principle.  R1-1 + R1-2 close the same class of bug for the
  optimizer's interior loops; this last hole is in a niche
  classifier that's already gated by a 4096-entry enumeration cap.

## Confidence calibration

### Where I was confident and right

- **R1-1 + R1-2** (Sleigh shift semantic divergence): the Sleigh
  source (`opbehavior.cc`) is unambiguous.  Pre-fix tests produced
  the exact wrong values predicted from reading the masked-shift
  formula (`1u8 << 8 → 1`, `0xFFu32 >> 32 → 0xFF`).  Post-fix tests
  pass.  Full workspace went 2846 → 2854 (+8) with no other tests
  flipping.  These bugs are class-related: KnownBits feeds
  ConstantFold via replace_all_uses, so fixing only one would leave
  the wrong-IntConst introduction path open.

### Where I was wrong and learned

- I initially scoped R1 to "look for endianness propagation bugs and
  arch-specific layouts."  Read register-aliasing, jump-table
  reading, stack_load_forward, function_args' BE-aware register
  fallback — all clean, well-tested.  The bug I actually found was
  arch-INDEPENDENT: a Sleigh-vs-LLVM-style semantic choice in a
  shared evaluator.  The lesson: per-arch correctness audits should
  also re-check the **arch-independent code paths Sleigh uses**, not
  just the per-arch register layouts.
- I burned 15 minutes investigating whether x86_64's W-register
  zero-extension semantics were correctly modeled in our
  register-aliasing logic.  Reading the Sleigh spec
  (`AARCH64instructions.sinc:104-113`) showed Sleigh emits
  `Rd_GPR64xsp = zext(tmp_1)` directly — the upper-zeroing is
  modeled at the spec level, not at our aliasing layer.  Our
  `write_reg_vn` for the container register is correct.  Same
  pattern for x86_64 EAX → RAX (`ia.sinc:1499-1505`).  Both
  initially looked suspect to me; both are sound.

### Open questions for round 2 / 3 / 4 / 5

1. **F-R1-A** (stack_array bit-width check): is there an adversarial
   fixture worth constructing, or is the 4096-entry enumeration cap
   a sufficient backstop?  R5 (test gaps) territory.

2. **`apply_link_register` / `apply_tail_call` SP-adjust on x86 /
   x86_64**: `FunctionBuilder::build_call` emits an
   `Add(sp, ret_stack_pop)` to model the caller-visible SP shift
   across a call.  `apply_tail_call` doesn't (because the very next
   thing is a Return that doesn't read SP).  Verified by reading the
   code; flagged as "checked clean" rather than "found-not-fixed."
   R3 (duplication / abstraction) territory if a refactor unifies
   the two builders.

3. **The `IntBinaryOp(And)` arm in `classify_anchor_with_rom_and_sp`
   short-circuits to stack_array only**: a rodata-jump-table whose
   load is wrapped in `And(load, mask)` (uncommon shape) would not
   be classified.  Couldn't construct a fixture from any of the 15
   archs; flagged as low-confidence concern.  R2 (pattern dogfood)
   territory if the jump_table rewrite under R2 needs to widen the
   And-anchor handling.

4. **Decoupled SShiftRight semantics for narrow types**: the
   pre-existing test
   `eval_int_binary_sshr_at_bit_width_negative_returns_all_ones_u32`
   passes on the unfixed code by coincidence (specifically because
   `i32(-1).wrapping_shr(0) as u128 & 0xFFFFFFFF` happens to equal
   the all-ones mask).  Other negative U32 inputs (`i32::MIN`,
   `-2`) would have failed pre-fix.  The post-fix `if sl < 0
   { mask } else { 0 }` is correct for every negative.  Worth a
   fuzz/property-test in R5 to systematically exercise the
   SShiftRight at-or-past-bit-width arm across every (sign, shift,
   width) cell.
