# Deep code audit — `strider-lift` (CFG → IR)

Date: 2026-06-14
Scope: `crates/strider-lift/src/lift/*` (Lifter engine, FunctionLifter
per-CFG driver, dispatch, value families, control/call/vn_io/pcode_util).
Read-only audit. Verified against actual code + adjacent crates
(`strider-ir` builders, `strider-cfg` query/region_builder).

## Summary

The lifter is in genuinely good shape. The pcode→IR semantics are
faithful for the opcodes modelled, the lift-time canonicalisations
(IntSub→Add(Neg), the three negated cmp lowerings, FloatSub, FLOAT_NAN,
Float<=→Or) are each individually sound under their documented argument,
and the structural defensive guards (`ensure_const_space`,
`require_equal_input_widths`, Piece/Extract/Insert bounds, Subpiece range)
are real and correct. Findings are mostly edge-case hardening and a few
small simplifications; no HIGH-severity correctness defect was confirmed.

Severity counts: HIGH 0 · MED 4 · LOW 5

---

## MED-1 — `handle_load` ignores the input-0 space-id width / sign of the address operand; but more importantly the LOAD value byte-size flows through `int_for_byte_size`, which rejects 3/5/6/7/12-byte loads with a hard lift error

- Dimension: SOUNDNESS / EDGE CASES
- Severity: MED
- Confidence: HIGH (for the failure mode), MED (for real-world frequency)
- Location: `lift/memory.rs:10-20`, `lift/vn_io.rs:75-90`,
  `crates/strider-ir/src/node/value_type.rs:324-336`.

What & why: every value-producing handler that sizes an output via
`ValueType::int_for_byte_size(out_vn.size)` (Load, Store data read,
Copy through a RAM vn, arithmetic, cmp, etc.) hard-errors for any byte
size not in {1,2,4,8,10,16,32,64}. Real Sleigh specs *do* emit
odd-width varnodes: x86 `fld`/`fbld` 10-byte (handled — I80), but also
6-byte far-pointer loads (`lds`/`les`), 12-byte, and some PPC/ARM
multi-reg shapes. Today these abort the whole-function lift rather than
degrading. This is a `strider-ir` limitation surfaced through every
`strider-lift` handler, so the lifter can't fix it alone, but it is the
single most likely real-binary lift failure in this crate's call graph.

Proposed fix: not strider-lift-local. Either (a) widen
`int_for_byte_size` to round odd sizes up to the next supported width
with an explicit truncate, or (b) document the unsupported-width set as a
known gap. At minimum the lifter should attach the machine address /
opcode to the error so a failed lift names the offending instruction
(currently the error is just "unsupported node output size: 3 bytes"
with no asm context).

Missing edge-case test: `load_six_byte_far_pointer_errors_cleanly` /
`subpiece_of_twelve_byte_vn` — assert the behaviour (whatever it is
decided to be) is pinned, with an asm-attributed message.

---

## MED-2 — `process_int_binary_op` silently truncates a wider-than-output operand for signed ops, corrupting sign-extension order

- Dimension: SOUNDNESS (code vs pcode)
- Severity: MED
- Confidence: MED
- Location: `lift/arithmetic.rs:148-186`.

What & why: the comment claims the coercion handles operands *narrower*
than the output by sign/zero-extending. But `extend_if_needed`
(builder_ext.rs:112-117) silently **truncates** when the operand is
*wider* than `out_ty`. For `Sdiv`/`Srem` the path is
`extend_if_needed(lhs, out_ty, SignExtend)`; if `lhs.size > out_vn.size`
this truncates the value *before* the signed division, dropping the high
bits with no sign awareness. Pcode contractually emits equal widths for
arithmetic, so this is only reachable from a malformed/exotic spec — but
unlike the lowered forms (`handle_int_sub`) this dispatched path has **no**
`require_equal_input_widths` guard, so the mismatch is silently mis-lifted
into a wrong signed result rather than surfaced. The asymmetry is the
real issue: the lowered forms guard, the table-driven form doesn't.

Proposed fix: either add a width-equality guard for the
signedness-sensitive table ops (`Sdiv`/`Srem`/`SShiftRight`), or document
that a wider operand is truncate-to-output (and confirm that is the pcode
semantics). A guard is cheaper and matches the rest of the file's
"surface real spec bugs loud" stance.

Missing edge-case test:
`sdiv_with_wider_lhs_does_not_silently_truncate` — feed an `IntSdiv`
with `lhs.size=8, out=4` and assert it either errors or sign-handles
correctly (not a bare truncate).

---

## MED-3 — `handle_subpiece` performs the right-shift at the *input* width and will hard-error for any input wider than 16 bytes with a nonzero offset

- Dimension: SOUNDNESS / EDGE CASES
- Severity: MED
- Confidence: HIGH
- Location: `lift/cast.rs:92-141`.

What & why: the shift constant and the `ShiftRight` are built at
`int_for_byte_size(input_vn.size)`. For a YMM (32-byte) / ZMM (64-byte)
SUBPIECE with a nonzero byte_offset — e.g. extracting lane 1 of a YMM —
this resolves to `I256`/`I512`, and `build_int_const` rejects
I256/I512 (per builder_ext doc: "use build_int_const_wide for those").
So a perfectly legal AVX SUBPIECE aborts the lift. The byte_offset==0
fast path (pure truncate) escapes this, so simple low-lane extracts work;
non-zero-offset wide extracts do not. The `debug_assert!` at line 120-124
reasons only about the `<128` case and gives a false sense of coverage.

Proposed fix: route the shift-constant through the wide-const path
(mirroring `build_all_ones` in arithmetic.rs) for I256/I512 input widths,
or document SUBPIECE-of->16-byte-with-offset as unsupported and error
with an asm-attributed message.

Missing edge-case test: `subpiece_ymm_high_lane` — SUBPIECE with
input.size=32, byte_offset=16, out=16; assert defined behaviour.

---

## MED-4 — `decode_space_id`'s `unsafe { VnSpace::by_id }` trusts a CONST tag as proof of a valid space pointer; the safety comment conflates the tag check with the safety precondition

- Dimension: SOUNDNESS (code vs itself / FFI)
- Severity: MED
- Confidence: MED
- Location: `lift/pcode_util.rs:77-87`.

What & why: `decode_space_id` calls `ensure_const_space` then
`unsafe { rsleigh::VnSpace::by_id(space_id_vn) }`. The comment states the
CONST check "is a structural sanity gate, not the safety condition
itself" and that safety holds "because the pcode comes from
`Sleigh::lift_one`". That is the correct reasoning — but it means the
function is `pub` (re-exported) and `unsafe`-internally while accepting an
arbitrary `&rsleigh::Insn`. A caller (test util, fuzzer, future pcode
synthesiser) that constructs an `Insn` with a CONST input-0 whose
`addr_off` is not a real `AddrSpace` pointer passes the gate and hits UB.
The CONST gate provably does **not** establish the safety precondition.

Proposed fix: make `decode_space_id` (and the unsafe call) `pub(crate)`
rather than `pub` if no external caller needs it (verify — `nth_input_or_err`
is the only intended public export); and have `VnSpace::by_id` itself
return `Option`/`Result` upstream so the LOAD/STORE space decode is fully
safe. At minimum, tighten the doc to state the precondition is
*caller-guaranteed validity of the space pointer*, not the CONST tag.

Missing edge-case test: n/a (would require constructing UB); instead a
doc-test pinning that only `Sleigh::lift_one`-sourced insns are valid
inputs.

---

## LOW-1 — `handle_cond_branch` reads the cond at `nth_input_or_err(insn, 1)` and `read_vn` (not `read_input`); inconsistent and one extra bounds-read

- Dimension: RUNTIME / readability
- Severity: LOW
- Confidence: HIGH
- Location: `lift/control.rs:180`.

What & why: `self.read_vn(nth_input_or_err(insn, 1)?)` open-codes what
`self.read_input(insn, 1)` does in one call (vn_io.rs:46-53). Every other
handler uses `read_input`. Cosmetic; no soundness impact.

Proposed fix: `let cond_raw = self.read_input(insn, 1)?;`.

---

## LOW-2 — `handle_piece` does a redundant double int-conversion of hi/lo

- Dimension: RUNTIME / simplification
- Severity: LOW
- Confidence: HIGH
- Location: `lift/cast.rs:183-197`.

What & why: `hi` is converted to `hi_ty` (natural width) at line 187 and
then immediately re-converted to `out_ty` at line 196 (same for `lo`).
The intermediate `hi_int`/`lo_int` at natural width are never used except
as the source of the second conversion. The first conversion is dead —
`convert_to_int_if_needed(hi, out_ty)` directly is equivalent because
`hi` read from a register is already integer-typed and
`convert_to_int_if_needed` handles width directly. (`lo_bits` correctly
derives from the *varnode* byte size, not the SSA type, so that part must
stay.)

Proposed fix: drop the `hi_ty`/`lo_ty`/`hi_int`/`lo_int` intermediates;
convert `hi`/`lo` straight to `out_ty`.

---

## LOW-3 — Three "all-ones / one" I1 constants are open-coded as `build_int_const(u128::MAX, I1)` across boolean.rs / arithmetic.rs / float.rs

- Dimension: simplification / generalize
- Severity: LOW
- Confidence: HIGH
- Location: `lift/boolean.rs:49`, `lift/arithmetic.rs:261-263`,
  `lift/float.rs:119-121`.

What & why: the canonical "logical NOT" constant `IntConst(1):I1` is
materialised three ways with the same `u128::MAX`-masked-to-1-bit trick
and a comment re-explaining it each time. There is a
`build_boolean_const(true)` helper (builder_ext.rs:175) that produces
exactly `IntConst(1):I1` and would dedup identically. Using it removes
three copies of the "all ones I1 is 1" rationale.

Proposed fix: replace the three sites with
`self.builder.build_boolean_const(true)` (or a small
`build_i1_not(value)` helper if the xor pattern recurs).

---

## LOW-4 — `find_all_unique_vns` is O(insns × vns) into a hash set then collected unordered — fine, but the comment claims ordering is owned downstream while the set iteration order is nondeterministic across runs

- Dimension: RUNTIME / determinism
- Severity: LOW
- Confidence: HIGH (FxHashSet iteration order is deterministic-per-seed
  but not stable across insertion sets; `FunctionBuilder::new` re-sorts,
  so this is genuinely fine)
- Location: `lift/mod.rs:136-148`.

What & why: verified non-issue — `FunctionBuilder::new` re-sorts by
`(space, offset, size)` so `VarId` numbering is stable regardless of the
set's iteration order. Flagging only to record that the determinism
claim was checked and holds; the `Vec::with_capacity` micro-opt
(`all_vns.len()` is unknown up front) is not worth it. No action needed
beyond possibly trimming the duplicated comment (the ordering note
appears twice, lines 132-135 and 145-147).

Proposed fix: delete the duplicated ordering comment block.

---

## LOW-5 — `handle_return` ignores its `_insn` entirely; the discard of pcode RETURN's input is correct but only the `Return` opcode carries that input — `BranchIndirect` (link-register) routes here with a *different* input shape

- Dimension: SOUNDNESS (verified OK) / EDGE CASES
- Severity: LOW
- Confidence: HIGH
- Location: `lift/control.rs:207, 216-219`; `dispatch.rs:207`.

What & why: `Return | BranchIndirect` share `handle_return`, which emits
a CC `Return` from the resolved ret-val registers and discards `_insn`.
Verified sound: a `LinkRegister`-resolved `BranchIndirect` gets
`RegionTerminator::Return` in the cfg (region_builder.rs:433), so it is
*not* a special terminator and flows here; discarding its dispatch input
is correct because the real return values come from the CC. The only gap
is the absence of a test pinning that the `bx lr` → `handle_return` path
emits the CC ret-vals and *not* an `IndirectBranch` placeholder.

Proposed fix: none to code. Add the missing test below.

Missing edge-case test: `aarch64_bx_lr_lifts_to_cc_return_not_indirect`
— lift an AArch64 `ret`/`bx lr`, assert the terminator node is `Return`
with the CC ret-val inputs and that `unresolved_branches` is empty.

---

## Things checked and found sound (no finding)

- IntSub / FloatSub → Add(Neg): wrap semantics preserve bit pattern. OK.
- IntLessEqual/SlessEqual/NotEqual → Xor(cmp,1):I1 with the documented
  operand swap (`a<=b ≡ ¬(b<a)`). Swap polarity verified correct. OK.
- FLOAT_NAN → Xor(FloatEqual(x,x),1) and FloatLessEqual → Or(Less,Equal):
  NaN-correct under IEEE 754 (both Less and Equal false on NaN). OK.
- `process_int_cmp_op` mixed-width: extends narrower operand to max width
  with correct signedness; guards Carry/Scarry/Sborrow to equal-width
  (extending a width-relative flag op would corrupt it). Correct.
- CBRANCH polarity: `build_if(cond, true_block, false_block)` with
  `true_block` = region containing cfg `true_target` = the cond!=0
  destination. Matches pcode. OK.
- Sub-register write preserves bits outside the slice; the upper-zeroing
  ISAs emit their own explicit pcode zeroing ops. (This logic lives in
  strider-ir, but the lift-time dispatch is correct.) OK.
- Subpiece/Extract/Insert/Piece bounds checks and u128-domain masks are
  correct for the I80/I128 cases (high-bit masks computed in u128). OK.
- `ensure_const_space` applied to every literal-bearing slot (Subpiece
  offset, Extract/Insert lsb/bitcount, SegmentOp/CallOther id, LOAD/STORE
  space). Thorough. OK.
- PtrAdd/PtrSub/MultiEqual fail-closed rather than guessing. Correct
  per the documented rsleigh contract.
