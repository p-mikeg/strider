# Signed-style int type names, Bool→I1 collapse, and lifter-owned casts

## Goal

Simplify the IR type system and make the lifter the single owner of all
type-fixup (truncate / extend / bitcast), so node constructors become
strict and the resulting graph stays faithful to the lifted assembly.

Seven missions, verified for soundness before design:

1. Rename integer type variants `U8…U512` → `I8…I512` everywhere (enum,
   `Display` strings, dot labels, docs, Python docs).
2. Replace `TryFrom<u32> for NodeOutputType` with a named
   byte-size→type function.
3. Make the node-construction builders (`build_int_binary`, … ) strict:
   they error if inputs are not already the correct type. The **lifter**
   becomes the sole inserter of `Truncate` / `Extend` / bitcast fixups.
4. Treat `Bool` as a 1-bit integer (`I1`). Full collapse: remove the
   `Bool` category and the `BoolConst` / `BoolBinaryOp` / `BoolUnaryOp`
   node kinds; comparisons output `I1`; logical ops become integer ops
   at width 1. Removes `CastToBool` / `CastToInt`.
5. Remove `CastToFloat`; it is equivalent to `IntBitsToFloat` in lifter
   usage.
6. Pattern DSL keeps the ability to query "booleans" by constraining a
   value pattern's output **bit width** (== 1), not via a `Bool` type.
7. Fix the obsolete AArch64 scalar-FP soundness note in `vn_io.rs` and
   the ignored regression test.

## Verification findings (evidence)

### Mission 5 — `CastToFloat` ≡ `IntBitsToFloat`: CONFIRMED

Register variables always hold integer-typed values, so every
`CastToFloat` the lifter creates has an integer input. Empirically, for
`fadd s0,s1,s2` Sleigh emits `s0:4 = FloatAdd(s1:4, s2:4)`: operands are
read as 4-byte ints (`I32`) and cast to `F32` — a same-width int→float
bitcast = `IntBitsToFloat`. The float→different-precision-float arm of
`CastToFloat` (lowered to `FloatToFloat`) is **never fed by the lifter**.
Replacement is sound provided each float operand is bitcast to the float
type matching **its own** width (the refactor makes `build_float_cmp_op`
do this per-operand instead of inferring from lhs only).

### Mission 7 — AArch64 soundness note is WRONG: CONFIRMED

Lifting scalar-FP / SIMD writes and dumping pcode shows Sleigh emits the
upper-bits zeroing as **explicit, separate pcode ops** on both arches:

```
AArch64  fmov s0, w0  →  s0:4 = Copy(w0:4)
                         register(0x5004):4 = Copy(#0)   ← explicit
                         register(0x5008):8 = Copy(#0)      zeroing of
                         register(0x5010):8 = Copy(#0)      the V/Z reg
                         register(0x5018):8 = Copy(#0)
x86_64   movss xmm0,xmm1 (SSE)  →  XMM0_Da:4 = Copy(XMM1_Da:4)   (preserve — correct)
         vmovaps xmm0,xmm1(VEX) →  XMM0:16=Copy(..); ZMM0:64=IntZext(XMM0)  (zero — correct)
         movsd xmm0,[rax]       →  XMM0_Qa=load; XMM0_Qb:8=Copy(#0)         (zero hi — correct)
```

The lifter processes the zeroing ops as ordinary sub-register writes, so
the IR already reflects the ISA semantics. `write_reg_vn` preserving bits
*within the scalar-write op* is correct precisely because the zeroing
arrives as its own op. No `ArchPreset` threading or per-container policy
is needed. **Action:** delete the SOUNDNESS NOTE; replace it with a
comment explaining *why* it is sound (Sleigh's explicit upper-zeroing
ops). Convert the ignored `aarch64_scalar_fp_write_zeroes_upper_bits_of_simd_container`
test into a positive test that lifts `fmov s0,w0` and asserts the IR
zeroes the upper container bytes.

### Mission 4 — the soundness wrinkle (and how the design handles it)

`CastToBool` means **`value != 0`** (`get_as_bool` folds a constant int
as `val != 0`), **not** "truncate to the low bit". So int→bool must lower
to the `!= 0` comparison, which already produces a 1-bit value
(`BoolNeg(IntEqual(x,0))` today → `BitNot(IntEqual(x,0))` at `I1`). A bare
`Truncate` to `I1` (low bit) would be unsound for a condition register
holding e.g. `2`. The bool→int direction *is* a clean `ZeroExtend`
(true=1, false=0). `IntUnaryOp::BitNot` folds with masking to the type
width (`ty.get_unsigned_int(!v)`), so logical-NOT at `I1` is sound
(`~0 & 1 = 1`, `~1 & 1 = 0`).

## Design

### Type model (`crates/strider-ir/src/node/output_type.rs`)

`NodeOutputType` becomes:

```
I1, I8, I16, I32, I64, I80, I128, I256, I512, F32, F64, F80
```

- `I1` replaces `Bool` and is an **integer** (category `Int`). There is
  no `Bool` category; `NodeOutputTypeCategory` becomes `{ Int, Float }`.
- `bit_width` becomes a first-class column in `TYPE_INFO` (it can no
  longer be `byte_size * 8`, because `I1` has `byte_size 1` but
  `bit_width 1`). `byte_size(I1) = 1`, `bit_width(I1) = 1`.
- `bit_mask_u128(I1)` = `(1<<1)-1 = 1` falls out naturally; the `Bool`
  special-case is removed.
- `get_unsigned_int(I1, v)` returns `Some(v & 1)` (it is now an integer).
- `to_natural_int_type`: `I1→I1`, `F32→I32`, `F64→I64`, `F80→I80`,
  `Ix→Ix`.
- `is_bool()` is retained as sugar meaning `self == I1` (bit width 1),
  used by the pattern width filter; `is_integer()` now includes `I1`.
- Display names: `"i1","i8",…,"i512","f32","f64","f80"`.

`WideConstStorage::U256/U512` (a separate enum) is renamed to `I256/I512`
for consistency with `NodeOutputType`.

### Mission 2 — byte-size→type function

Replace `impl TryFrom<u32> for NodeOutputType` with a named constructor
`NodeOutputType::int_for_byte_size(n: u32) -> Result<Self>`:
`1→I8, 2→I16, 4→I32, 8→I64, 10→I80, 16→I128, 32→I256, 64→I512`
(byte size 1 maps to `I8`, never `I1`; `I1` is produced only by
comparisons). Sits alongside the existing `float_for_byte_size`. All
`.try_into()? / try_from` call sites switch to `int_for_byte_size`.

### Node kinds (`crates/strider-ir/src/node/kind.rs`)

Removed: `BoolConst`, `BoolBinaryOp`, `BoolUnaryOp`, `CastToBool`,
`CastToInt`, `CastToFloat`.

Mappings applied by the lifter / opt:

| Old | New |
|-----|-----|
| `BoolConst(b)` | `IntConst(b as u128)` typed `I1` |
| `BoolBinaryOp::{And,Or,Xor}` | `IntBinaryOp::{And,Or,Xor}` typed `I1` |
| `BoolUnaryOp::Neg` (logical not) | `IntUnaryOp::BitNot` typed `I1` |
| `IntCmpOp` / `FloatCmpOp` output | `I1` (was `Bool`) |
| `CastToInt(bool→int)` | `Extend(ZeroExtend)` |
| `CastToBool(int→bool)` | `IntNotEqual(x,0)` = `BitNot(IntEqual(x,0))` → `I1` |
| `CastToFloat(int→float)` | `IntBitsToFloat` (same width) |

`If` requires an `I1` condition. The lifter guarantees this: a condition
already produced by a comparison is `I1` (no node added); a wider integer
condition is lowered to `x != 0` (produces `I1`).

### Mission 3 — strict builders, lifter owns fixups

The five coercion helpers stay as **public `FunctionBuilder` methods the
lifter calls explicitly**, but the eleven `build_*` constructors stop
calling them implicitly and instead `require_*` the correct input type
(error otherwise). After the Bool/float-cast collapse the helpers
simplify:

- `convert_to_int_if_needed` → truncate-then-zero-extend only (the
  non-integer `CastToInt` branch is gone — everything is an integer).
- `convert_to_bool_if_needed` → `ensure_i1`: identity if already `I1`,
  else `x != 0`.
- `cast_to_float_if_needed` → `IntBitsToFloat` at the matching width.

The lifter already calls these helpers at ~25 sites; the refactor adds
explicit calls wherever it previously relied on the builder's implicit
coercion (the int/bool/float binary/unary/cmp construction sites in
`value/arithmetic.rs`, `value/float.rs`, `value/boolean.rs`,
`value/integer.rs`, and the `If`-condition site in
`strider-analyze/.../insn/control.rs`).

### Mission 6 — pattern DSL boolean queries by width

- Remove pattern ctors `bool_binary` / `bool_and/or/xor` / `bool_not` /
  `bool_const`; comparisons and int ops cover their role. Keep
  `int_cmp` / `float_cmp` (now produce `I1`).
- Remove `cast_to_bool` / `cast_to_int` / `cast_to_float` pattern ctors
  and the `CAST_TO_BOOL/INT/FLOAT` bits from `CastMask`
  (`walk/cast/mod.rs`).
- Generalize the existing `bit_width` post-match filter (today only on
  `LoadPat`/`StorePat`, `pattern/pat/builders/memory.rs`) into an
  output-bit-width constraint usable on value patterns, so "query a
  boolean" = constrain output width to `1`.
- Python mirror: types already cross the boundary as bit-width ints (no
  `NodeOutputType` enum mirror exists), so the Python change is removing
  the bool/cast builders + `PyCastMask` arms + doc-string updates.

### Mission 7 — comment + test

Rewrite the `vn_io.rs:288` comment to state the sound rationale; convert
the ignored test into a positive assertion.

## Soundness summary (per the user's directive)

Every transformation preserves assembly semantics:
- int→bool uses `!= 0`, not low-bit truncate.
- bool→int uses `ZeroExtend` (0/1 preserved).
- logical-NOT → `BitNot` at `I1` (masking verified).
- float casts → same-width `IntBitsToFloat` (verified register reads are
  same-width ints).
- FP upper-bits zeroing is already emitted by Sleigh; lifter honours it.
- strict builders error rather than silently coerce, so any missed
  lifter fixup surfaces as a hard failure (and is caught by the
  always-on validator) rather than wrong IR.

## Testing strategy

TDD per phase. Each phase keeps `cargo build --workspace`,
`cargo clippy --workspace -D warnings`, and the per-crate test suites
green before commit+push. Snapshot tests (`*.snap`) regenerated where
type-name / node-histogram output legitimately changes. The Python
`pytest` suite (805 tests) runs after the pattern/py phase. A
cross-arch shape test continues to assert the lifted IR matches expected
node histograms (regression guard for the lifter fixup changes).

## Out of scope

- Adding new pattern features beyond the output-width filter.
- The AVX-512 wide-container (>16 byte) sub-register read limitation
  (separate, pre-existing, documented).
