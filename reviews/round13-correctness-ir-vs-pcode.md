# Round 13 — Emphasis A axis 2: IR vs lifted-representation correctness

Branch: `review/ai7` · rsleigh authority: `/mnt/c/Users/mikeg/Documents/rsleigh/src/ffi.rs`.

## Verdict

**No findings at confidence ≥ 80.** All IR `NodeKind` families verified consistent with rsleigh opcode semantics.

## Categories verified consistent

✓ **Add / Sub (lowered) / Mul / Div / Mod**
- `IntAdd` / `IntMul` / `IntDiv` / `IntSdiv` / `IntRem` / `IntSrem` map to the matching `IntBinaryOp` variants.
- `IntSub(a,b) → Add(a, Neg(b))`; width-consistency guard enforced before lowering.

✓ **Shifts**
- `IntLeft` / `IntRight` (logical) / `IntSright` (arithmetic) map to `ShiftLeft` / `ShiftRight` / `SShiftRight`.  Shift-mask semantics architecture-defined and unconstrained.

✓ **Bit-ops**
- `IntAnd` / `IntOr` / `IntXor` → `IntBinaryOp::And/Or/Xor`.
- `IntNeg (=25, bitwise)` → `IntUnaryOp::BitNot` (~x).
- `Int2Comp (=24, two's complement)` → `IntUnaryOp::Neg` (-x).  Naming inversion documented at every call site.

✓ **Comparisons + lowered Le / Sle / Ne**
- `IntEqual` / `IntLess` / `IntSless` / `IntCarry` / `IntScarry` / `IntSborrow` → matching `IntCmpOp` variants (output type always Bool).
- `IntNotEqual(a,b)` → `BoolNeg(IntEqual(a,b))`.
- `IntLessEqual(a,b)` → `BoolNeg(IntLess(b,a))` (operand swap; `a≤b ↔ ¬(b<a)`).
- `IntSlessEqual(a,b)` → `BoolNeg(IntSless(b,a))`.

✓ **Casts**
- `IntZext` / `IntSext` → `Extend(ZeroExtend / SignExtend)`.
- `Subpiece(value, byte_offset, out_size)` → logical right-shift by `byte_offset*8`, truncate.  CONST-space + range guards present.
- `FloatInt2Float` / `FloatFloat2Float` / `FloatTrunc` → `IntToFloat` / `FloatToFloat` / `FloatToInt` (toward zero).
- `Cast` → identity copy.

✓ **Float arithmetic**
- `FloatAdd` / `FloatMul` / `FloatDiv` → matching `FloatBinaryOp` variants.
- `FloatSub(a,b) → FloatAdd(a, FloatNeg(b))`.  IEEE 754: sign-bit flip exact on NaN/inf.
- `FloatNeg` / `FloatAbs` / `FloatSqrt` / `FloatCeil` / `FloatFloor` / `FloatRound` → `FloatUnaryOp` variants.
- `FloatEqual` / `FloatLess` → `FloatCmpOp::Equal/Less`.
- `FloatNotEqual(a,b) → BoolNeg(FloatEqual(a,b))` — NaN-aware (Equal false on NaN → !Equal true).
- `FloatLessEqual(a,b) → Or(FloatLess(a,b), FloatEqual(a,b))` — NaN-aware (both arms false on NaN; the alternative `BoolNeg(Less(b,a))` would incorrectly return true).
- `FloatNan(x) → BoolNeg(FloatEqual(x,x))` — NaN ≠ NaN.

✓ **Memory (Load / Store)**
- `Load`: `inputs[0]` CONST-space space-id decoded via `decode_space_id` → `unsafe VnSpace::by_id`.  Width from `out_vn.size`.  IR sig `[MEM, ADDR] → [INT_VAL]`.
- `Store`: handled in strider (advances memory chain).  IR sig `[MEM, ADDR, DATA] → [MEM]`.
- Memory-chain ordering threaded through `cur_region_memory` in program order.

✓ **Control flow**
- `CondBranch` cond from `inputs[1]`, coerced to Bool via `convert_to_bool_if_needed`.
- `BranchIndirect` link-register case routes to `handle_return`.
- `Call` target from `inputs[0]` (code-space const for direct, register for indirect).
- `CallOther` user-op id from CONST `inputs[0]`; classified via `target::call_other_abi::classify`; unknown names raise `UnknownCallOtherError`.
- `MultiEqual` raises typed error (decompiler-internal, not emitted by `rsleigh::lift_one`).
- `Return` discards pcode's fabricated popped-LR input; uses CC `ret_val_regs` for IR value inputs.
- `IndirectBranch` placeholder anchors `[CTRL, MEM, TARGET]` so resolver can rewrite in place.

✓ **Phis**
- `VarPhi(Vn)` / `ValuePhi`: `[phi_token, val_0, val_1, …]`, output `AnyValue` (relaxed from `AnyInt` because flag-register phis are Bool-typed in real binaries).
- `MemPhi`: `[phi_token, mem_0, mem_1, …]`, output `Memory`.
- `StackStorePhi { space }`: fixed `[phi_token, memory, data]`, output `Memory`.  Per-branch offsets in `Graph::stack_phi_offsets`.
- `StackStore { space, offset }`: `[memory, base, data]`, output `Memory`.

✓ **Wide constants**
- `Graph::make_int_const` rejects `U256`/`U512` with typed error directing to `build_int_const_wide`.
- `build_int_const_wide` validates `WideConstStorage::byte_size()` matches declared output type; interns via `Graph::intern_wide_const` (dedup).
- `IntConst` masked to declared type's bit width.

✓ **Sub-register aliasing**
- x86 AL/AH/EAX/RAX: container `rax` (8B); shift formula correct LE for sub-register offsets.
- AArch64 V0/D0/S0/upper-8 of V0: container `q0` (16B); positioned-mask correctly isolates target bits.
- x87 ST* (10B U80): `vn_mask(10) = (1u128 << 80) - 1`.
- Wide-container guard (>16B): typed error for sub-register aliasing within ymm/zmm; direct full-container reads/writes still work via `container_reg == *reg` early-out.
- Defensive shift bound (round-12 EC-3): runtime `Err` for `shift_value >= container_bits` in both read and write paths.

✓ **Subpiece / Extract / Insert / PtrAdd / PtrSub / Piece guards**
- CONST-space guards via `ensure_const_space` (cast.rs:23-37) on Subpiece byte_offset, Extract lsb/bit_count, Insert lsb/bit_count, PtrAdd elem_size.
- Piece size-sum check `hi.size + lo.size == out.size` enforced.
- Subpiece range check `byte_offset < input.size` enforced.
- PtrSub lowers to `Add(base, Neg(index))`.  PtrAdd lowers to `Add(base, Mul(index, elem_size))`.

## Files reviewed

- `pcode-lift/src/value/{mod,arithmetic,integer,cast,float,boolean,mem_load}.rs`
- `pcode-lift/src/vn_io.rs`
- `strider/src/strider/insn/{mod,control}.rs`
- `ir/src/node/{kind,output_type}.rs`, `ir/src/ops/{op_kinds,consts}.rs`, `ir/src/node_signature.rs`, `ir/src/wide_const.rs`
- `ir/src/builder/{nodes,coerce}.rs`
- Cross-referenced against `/mnt/c/Users/mikeg/Documents/rsleigh/src/ffi.rs` (Opcode authority).
