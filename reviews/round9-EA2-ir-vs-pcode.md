# Round 9 — EA2: IR vs Lifted pcode Representation

**Branch:** `feature/ai`. Verified pcode→IR mapping for 65 instruction-IR pairs across all opcode families.

## Critical: Sleigh Nomenclature Inversion (verified correct)

`crates/pcode-lift/src/value/mod.rs:54-59` correctly handles the rsleigh naming reversal:

| rsleigh `Opcode` | Conventional meaning | IR mapping |
|---|---|---|
| `Int2Comp` (= 24, "Twos complement") | `-x` two's-complement negate | `IntUnaryOp::Neg` |
| `IntNeg` (= 25, "Logical/bitwise negation ~") | `~x` bitwise complement | `IntUnaryOp::BitNot` |

Documented in `op_kinds.rs:89-106` and at the dispatch site. Verified against `rsleigh/src/ffi.rs:134-139`.

## Findings

### F-1 (LOW, confidence 80) — `handle_float_sub` doc-comment overstates NaN bit-pattern exactness

**Where:** `crates/pcode-lift/src/value/float.rs:92-98`.

Doc says "the negation flips the sign bit on infinities and NaNs without changing their NaN-ness — so the bit-pattern result matches `FloatSub` exactly." For NaN inputs, "exact bit-pattern match" overstates IEEE 754 / C++ guarantees. GHIDRA's `opSub` and `opNeg` both route NaN through host `double` arithmetic; payload is implementation-defined. Semantic lowering is sound (NaN propagates); claim is too strong.

**Fix:** "so the semantics match `FloatSub` for all IEEE 754 values; NaN propagation preserved, though NaN payload for NaN inputs is host-FPU-dependent (same as GHIDRA's opSub)."

### F-2 (LOW, confidence 80) — `handle_float_not_equal` comment vs pcode reference docs

**Where:** `crates/pcode-lift/src/value/float.rs:109-114`.

Doc says `BoolNeg(FloatEqual) = true` on NaN "matching the correct `NotEqual` for NaN inputs." Pcode reference manual says `FLOAT_NOTEQUAL` returns false on NaN. The comment is correct per GHIDRA's actual `float.cc:opNotEqual` implementation (`val1 != val2` in C++, returns true for NaN), but a maintainer consulting pcode docs finds a contradiction.

**Fix:** Add note: "Note: pcode reference manual says `FLOAT_NOTEQUAL` returns false on NaN, but GHIDRA's actual `float.cc:opNotEqual` uses C++ `val1 != val2` (returns true for NaN). This lowering mirrors the implementation."

## Verified Pairs Coverage Table

| Opcode Family | Pairs | Status |
|---|---|---|
| Arithmetic: Add/Sub(lowered)/Mul/Div/Sdiv/Rem/Srem/Neg/BitNot | 9 | ✓ |
| Edge cases: INT_MIN+INT_MIN, 0+0, sub-byte operands | 3 | ✓ |
| Shifts: Left/Right(logical)/Sright(arith) | 3 | ✓ |
| Bit-ops: And/Or/Xor/Not | 4 | ✓ |
| Comparisons: Equal/Less/Sless/Carry/Scarry/Sborrow | 6 | ✓ |
| Comparison lowerings: NotEqual/LessEqual/SlessEqual | 3 | ✓ |
| Casts: ZeroExtend/SignExtend/Subpiece(→logical-shr+truncate) | 3 | ✓ |
| Popcount/Lzcount | 2 | ✓ |
| Float conversions: Int2Float/Float2Float/FloatTrunc | 3 | ✓ |
| Float arithmetic: Add/Mul/Div/Sub(lowered) | 4 | ✓ |
| Float unary: Neg/Abs/Sqrt/Ceil/Floor/Round | 6 | ✓ |
| Float comparisons: Equal/Less | 2 | ✓ |
| Float lowerings: NotEqual/LessEqual/NaN | 3 | ✓ |
| Memory: Load(VnSpace)/Store(VnSpace) | 2 | ✓ |
| Control: CondBranch(cond≠0)/Return/BoolOps | 3 | ✓ |
| Phis: VarPhi/MemPhi/ValuePhi | 3 | ✓ |
| Sub-reg aliasing: AL(LE)/AH(LE)/D0(LE)/S0(LE)/ST0(10-byte) | 5 | ✓ |
| **Total** | **65** | **CORRECT — 2 LOW comment issues** |

## Summary

No HIGH or MED issues. All 65 pairs semantically correct against GHIDRA pcode semantics. Two LOW doc-comment accuracy issues only.

Sources: GHIDRA pcode reference, `float.cc` source, rsleigh `ffi.rs` opcode table.
