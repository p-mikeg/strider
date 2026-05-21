# Round 12 — Emphasis A axis 2: IR vs lifted-representation correctness

Branch: `review/ai6` · Trust model: strict (no prior-round reviews; no other round-12 reports read; rsleigh consulted directly).

## Verdict

**No HIGH findings.** 1 MED + 3 LOW findings. Every other opcode family verified consistent with rsleigh.

rsleigh authority: `/mnt/c/Users/mikeg/Documents/rsleigh/src/ffi.rs` (Opcode enum, lines 59-279) and `/mnt/c/Users/mikeg/Documents/rsleigh/src/core_types.rs`.

## Findings

### IRP-1 — `Subpiece` / `Extract` / `Insert` — CONST-space precondition not enforced
- **Severity:** MED
- **Where:** `crates/pcode-lift/src/value/cast.rs:32-59` (`handle_subpiece`), `:108-153` (`handle_extract`), `:156-221` (`handle_insert`)
- **rsleigh reference:** `../rsleigh/src/ffi.rs:249` (`Subpiece = 63`), `:270-271` (`Extract = 71`, `Insert = 70`)
- **What's wrong:** Sleigh's `Subpiece` encoding places the byte offset as `inputs[1].addr_off` where `inputs[1]` **must** be a CONST-space varnode. The lifter reads `insn.inputs[1].addr_off` directly without verifying `inputs[1].addr_space == VnSpace::CONST`. `handle_extract` similarly reads `lsb`/`len` from `inputs[1]`/`inputs[2]`, and `handle_insert` from `inputs[2]`/`inputs[3]`, all without CONST-space checks. A malformed pcode stream with a non-CONST varnode in those slots would have its address-offset (a pointer value) interpreted as a byte offset / bit count, producing a structurally valid but semantically wrong IR. For comparison, `handle_load`/`handle_store` correctly gate on `decode_space_id` which asserts CONST space (`crates/pcode-lift/src/lib.rs:140`).
- **Fix:** Add `if insn.inputs[1].addr_space != rsleigh::VnSpace::CONST { bail!(...) }` guards in each handler before reading `.addr_off` as a positional constant, matching `decode_space_id`'s pattern.
- **Regression test:** Lift a hand-crafted `Subpiece` insn with `inputs[1]` in REGISTER space; assert `lift()` returns `Err`, not a silently wrong IR.

### IRP-2 — `Piece` missing input-size-sum invariant check
- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/value/cast.rs:78-106` (`handle_piece`)
- **rsleigh reference:** `../rsleigh/src/ffi.rs:246` (`Piece = 62`)
- **What's wrong:** Sleigh's `Piece` requires `inputs[0].size + inputs[1].size == output.size`. The lifter builds the zero-extend + shift-left + OR shape but never validates this. If Sleigh emits a malformed `Piece` where parts don't sum to the declared output width, the IR silently zero-extends `hi` to `out_ty` and shifts by `lo_ty.bit_width()`, dropping or duplicating bits. `handle_subpiece` has an analogous bound check (`byte_offset < input_vn.size`); Piece lacks the equivalent.
- **Fix:** At the start of `handle_piece`, assert `insn.inputs[0].size + insn.inputs[1].size == out_vn.size` and `anyhow::bail!` if violated.
- **Regression test:** Lift a `Piece` where `hi.size=3, lo.size=2, out.size=4` (sum mismatch); assert error.

### IRP-3 — `PtrAdd` element-size CONST-space not verified
- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/value/cast.rs:223-243` (`handle_ptr_add`)
- **rsleigh reference:** `../rsleigh/src/ffi.rs:255` (`PtrAdd = 65`)
- **What's wrong:** `PtrAdd(base, index, elem_size)` — `elem_size` is always a CONST-space varnode per spec. The lifter reads `elem_size = insn.inputs[2].addr_off` without checking the space. Silent misclassification: a non-CONST varnode there would have its runtime address-offset interpreted as a compile-time element size, emitting `base + index * <garbage>`. Lower priority because GHIDRA always produces CONST here, but the contract is not enforced at this layer.
- **Fix:** Add `if insn.inputs[2].addr_space != rsleigh::VnSpace::CONST { bail!(...) }`.
- **Regression test:** Craft a `PtrAdd` with REGISTER-space inputs[2]; assert the lifter errors.

### IRP-4 — `Scarry` narrow-width constant-fold uses unchecked i128 add (latent)
- **Severity:** LOW
- **Where:** `crates/opt/src/constant_fold/eval_int.rs:183-186` (`eval_int_cmp`, `Scarry` arm, `bits < 128` branch)
- **rsleigh reference:** `../rsleigh/src/ffi.rs:128-129` (`IntScarry = 22`)
- **What's wrong:** For `bits < 128`, the code computes `let result = sl + sr;` on i128 and compares to min/max. For `bits=64`, sums stay within i128. For a hypothetical future type with `bits=127`, the sum can reach ±2^128 and overflow. Currently the IR caps at U128 and the `bits >= 128` branch correctly uses `wrapping_add` to guard that case, so there's no exploitable overflow today. The asymmetry creates a latent trap if a new sub-128-bit wide type is added without updating this arm.
- **Fix:** Replace `let result = sl + sr; result < min || result > max` with `sl.checked_add(sr).map_or(true, |r| r < min || r > max)`.
- **Regression test:** Pin `Scarry` constant-fold on U64 with `i64::MIN` and `-1`; assert the fold returns `false`.

## Categories verified consistent

✓ **Add / Mul** — `IntBinaryOp::{Add,Mul}` lifted via `process_int_binary_op`; constant-fold uses `wrapping_*` masked to type width. Matches `OpBehaviorIntAdd::evaluateBinary`. (`../rsleigh/src/ffi.rs:118-121`)

✓ **Sub (lowered)** — `IntSub(a,b)` → `Add(a, Neg(b))` at `pcode-lift/src/value/arithmetic.rs:151-180`. Bit-exact for `a=INT_MIN, b=INT_MIN` (both equal `0`). (`../rsleigh/src/ffi.rs:123`)

✓ **Div / Sdiv / Rem / Srem** — constant-fold handles div-by-zero (returns `None`) and `INT_MIN / -1` (returns `None`). (`../rsleigh/src/ffi.rs:163-170`)

✓ **Shifts (Shl / Shr / Sar)** — constant-fold returns 0 for shift-count ≥ bit-width matching `OpBehaviorIntLeft::evaluateBinary`. `SShiftRight` fills with sign bit. (`../rsleigh/src/ffi.rs:149-152`)

✓ **Bit-ops** — `IntNeg` (Sleigh = bitwise NOT) → `IntUnaryOp::BitNot`; `Int2Comp` (two's complement) → `IntUnaryOp::Neg`. Name inversion documented at `crates/ir/src/ops/op_kinds.rs:89-107`. (`../rsleigh/src/ffi.rs:136-139`)

✓ **Comparisons (Equal / Less / Sless)** — width passed is operand width (`inputs[0].size`), not output width. Correct per Sleigh. (`../rsleigh/src/ffi.rs:95-115`)

✓ **Lowered comparisons (Le / Sle / Ne)** — `IntLessEqual(a,b)` → `BoolNeg(IntLess(b,a))`; `IntSlessEqual(a,b)` → `BoolNeg(IntSless(b,a))`; `IntNotEqual(a,b)` → `BoolNeg(IntEqual(a,b))`. Bit-exact for all signed/unsigned patterns. (`arithmetic.rs:97-138`)

✓ **Carry / Scarry / Sborrow** — `IntCarry` uses `wrapping_add(r) > max` for narrow, `wrapping_add(r) < l` for U128. `Sborrow` uses `wrapping_sub`. All correct except the IRP-4 latent. (`../rsleigh/src/ffi.rs:126-133`)

✓ **Casts (Truncate / ZeroExtend / SignExtend)** — `Subpiece` lowers to right-shift + truncate. `IntZext` → `Extend(ZeroExtend)`, `IntSext` → `Extend(SignExtend)`. `extend_if_needed` inserts `CastToInt` for non-integer inputs before extending. (`../rsleigh/src/ffi.rs:113-116`)

✓ **Float arith (Add / Mul / Div)** — auto-cast via `cast_to_float_if_needed`. `FloatSub(a,b)` → `FloatAdd(a, FloatNeg(b))` — IEEE 754: `a-b == a+(-b)` for all finite values; NaN/inf sign-flip preserved. (`float.rs:92-107`)

✓ **Float comparisons (Equal / Less)** — `FloatNotEqual(a,b)` → `BoolNeg(FloatEqual(a,b))`: NaN-safe because `FloatEqual` returns false for NaN. `FloatLessEqual(a,b)` → `Or(FloatLess(a,b), FloatEqual(a,b))`: NaN-safe. `FloatNan(x)` → `BoolNeg(FloatEqual(x,x))`: IEEE 754 reflexivity. (`float.rs:78-140`)

✓ **Float-integer conversions** — `FloatInt2Float` → `IntToFloat`; `FloatFloat2Float` → `FloatToFloat`; `FloatTrunc` → `FloatToInt` (truncates toward zero). `FloatTrunc` (inter-domain conversion) is NOT `FloatUnaryOp::Trunc` (round-toward-zero in-precision). (`float.rs:142-185`, `../rsleigh/src/ffi.rs:224-228`)

✓ **Bitcasts (IntBitsToFloat / FloatBitsToInt)** — raw bit reinterpretation. `build_int_bits_to_float` folds `IntConst` → `FloatConst` and vice versa. F80 excluded from immediate fold because `FloatConst`'s u64 payload cannot represent 80-bit. (`crates/ir/src/builder/nodes.rs:419-478`)

✓ **Memory (Load / Store with VnSpace)** — `Load(space)` carries target space; lifter reads via `decode_space_id` which decodes CONST-encoded space pointer via `VnSpace::by_id`. Store advances memory token. Byte-order delegated to Sleigh's pcode output. (`mem_load.rs:10-19`, `nodes.rs:694-758`)

✓ **Control: If** — pcode `CondBranch` dispatches when `inputs[1] != 0`. IR `If` requires `Bool`; `handle_cond_branch` inserts `convert_to_bool_if_needed` (either pass-through or `CastToBool`), semantically equivalent. (`control.rs:188-212`)

✓ **Control: IndirectBranch placeholder** — `[ctrl, mem, target_value]` no outputs. Memory slot anchored. (`nodes.rs:540-568`, `node_signature.rs:344`)

✓ **Control: Call / Return** — `Call [ctrl, mem, target, ...args]` → `[ctrl, mem, ...clobbered]`; `Return` discards pcode's fabricated input (popped LR/RA) and reads real ABI return regs. (`control.rs:222-225`, `nodes.rs:501-536`)

✓ **Phis (VarPhi / MemPhi / ValuePhi / StackStorePhi)** — arity enforced by Layer C. StackStorePhi fixed-arity 3 with per-predecessor offsets in `Graph::stack_phi_offsets`. (`node_signature.rs:314-352`)

✓ **Wide constants (U256 / U512)** — `build_int_const` rejects `U256`/`U512`; `build_int_const_wide` requires matching `WideConstStorage`. Interning deduplicates. (`builder/nodes.rs:108-141`, `wide_const.rs`)

✓ **Sub-register aliasing — x86 AL/AH/EAX/RAX** — LE shift formula `8 * (reg.addr_off - container.addr_off)`. AL: shift=0 → truncate. AH: shift=8 → shift-right 8 then truncate (bits 8-15 of RAX). EAX: shift=0 → truncate to 4 bytes. Matches Intel SDM register-overlap. (`vn_io.rs:190-203`, tests `:404-434`)

✓ **Sub-register aliasing — AArch64 V0/D0/S0** — V0 (q0, 16 bytes) is container. D0 = offset 0 size 8 (shift 0); S0 = offset 0 size 4 (shift 0); upper-8-bytes at offset 8 size 8 (shift 64). Write-side: positioned mask isolates target position. Pin test at `vn_io.rs:469-527`.

✓ **Sub-register aliasing — x87 ST* 80-bit** — `vn_mask` returns `(1u128 << 80) - 1` for 10-byte regs. `NodeOutputType::U80` / `F80` both 10-byte. Float ops insert `CastToFloat`. (`vn_io.rs:43-44`, `output_type.rs:274-284`)

✓ **Sub-register aliasing — x86 segment registers** — 2-byte varnodes in REGISTER space; no overlap with other tracked regs in typical sla specs; pass through normal `read_reg_vn`.

## Files reviewed

- `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/value/{arithmetic,boolean,cast,float,integer,mem_load,misc_value,mod}.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pcode-lift/src/{lib,vn_io}.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/ir/src/{builder/nodes.rs,node/output_type.rs,ops/op_kinds.rs,node_signature.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/constant_fold/eval_int.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/insn/control.rs`

Cross-referenced against `/mnt/c/Users/mikeg/Documents/rsleigh/src/ffi.rs` (Opcode enum) and `/mnt/c/Users/mikeg/Documents/rsleigh/src/core_types.rs` (varnode space encoding).
