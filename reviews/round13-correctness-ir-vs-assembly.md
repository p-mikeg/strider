# Round 13 — Emphasis A axis 3: lifted IR vs real assembly

Branch: `review/ai7` · Per-arch ABI verification against published specs (System V x86_64, AAPCS / AAPCS64, MIPS o32 / n64, PPC ELFv1 / v2, Intel SDM Vol. 2B, ARM ARM, SMCCC, Linux kernel ABI).

## Verdict

**No HIGH findings.** 1 LOW (sound over-approximation on `mwait`/`mwaitx` aliasing).  1 verified-open tracked gap (AArch64-BE `Or(SP,K)` indirect-resolution).  All other axes pass.

## Findings

### IRA13-1 — `mwait`/`mwaitx` `implicit_reads` is sound but over-approximates on x86_64
- **Severity:** LOW (sound over-approximation)
- **Where:** `crates/target/src/call_other_abi.rs:225-230`
- **What:** `mwait` and `mwaitx` apply identically to both `X86` and `X86_64` with `implicit_reads: ["EAX", "ECX"]`.  On x86_64 the hardware reads only bits 31:0 (EAX/ECX fields of RAX/RCX).  Sleigh resolves "EAX" via `find_largest_fitting_register` to the containing `RAX` register, so the emitted implicit read becomes `InitialVar(RAX)` — strictly stronger than the actual constraint.  Sound (over-approximates), but pattern queries that ask "does mwait read RAX bits 32:63?" will incorrectly answer yes.
- **Fix (optional):** Split into separate `X86_64` and `X86` entries with explicit register-name resolution narrower than 64-bit on the x86_64 side.  Lower-priority cosmetic / pattern-precision improvement.

### IRA13-2 — `indirect_branch_resolved_aarch64be` `#[ignore]` is real (not stale)
- **Severity:** documented known gap (not a regression)
- **Where:** `crates/strider/tests/indirect_branch.rs:215-218`
- **What:** The ignore reason ("aarch64-be: stack-array dispatch unresolved — lifter emits `Or(SP,K)` instead of `Add(SP,K)` and wraps stored labels in `Truncate`; resolver matches `Add(SP,K)+raw-IntConst` only") accurately describes the current code state.  The classifier has an `IntBinaryOp::And` arm for ARM Thumb-interworking but no `IntBinaryOp::Or` arm.  Round 12's IRA-2 claim that "fix already landed, just re-enable the test" was wrong.  Round 13 1B confirmed this.
- **Fix:** Extend `classify_stack_array`'s SP-offset recognition to accept both `Add(SP, K)` and `Or(SP, K)` (the bitwise OR is semantically equivalent to addition when K is a power-of-two-aligned offset, which is what AArch64-BE Sleigh emits).  Then remove the `#[ignore]`.

## Categories verified consistent

### Axis 1 — Return-value flow

| Preset | ret int | ret float | Spec | Verdict |
|---|---|---|---|---|
| `x86_64_systemv` | `RAX, RDX` | `XMM0, XMM1` | SysV AMD64 §3.2.3 | ✓ |
| `arm_aapcs` | `r0, r1` | `d0, d1` | AAPCS §5.4 | ✓ |
| `aarch64_aapcs64` | `x0, x1` | `q0, q1` | AAPCS64 §6.4 | ✓ |
| `x86_cdecl` | `EAX, EDX` | `ST0, XMM0` | i386 SysV §3.9 | ✓ |
| `mips_o32` | `v0, v1` | `f0, f2` | MIPS O32 §3.5 (f2 not f1 for double) | ✓ |
| `powerpc64_elf_v1`/v2 | `r3, r4` | `f1, f2` | Power ELFv1 §3.2.3 / ELFv2 §2.2.2 | ✓ |

### Axis 2 — Clobber footprint

✓ **Positive case** (caller-saved IS clobbered): x86_64 caller-saved set = ALL GPRs minus `callee_saved_regs` minus SP.  `Call` nodes emit `Control + Memory + per-CC clobber slots`.  Pinned by `tests/per_address_cc.rs:88-119`.

✓ **Negative case** (callee-saved NOT clobbered): `tests/per_address_cc.rs:31-83` verifies `x86_64_all_preserving` override eliminates clobbers entirely.

### Axis 3 — Memory chain after Call

✓ `CallingConvention::no_memory_clobber` is `true` only for `x86_64_all_preserving`; `false` for every standard preset.  When true, `build_call_with_cc` skips the Memory output → `LoadReadOnly` / `StackLoadForward` can forward.  Normal calls correctly break the chain.

### Axis 4 — CallOther implicit reads/writes

24 entries verified against Intel SDM / ARM ARM / Linux kernel ABI:

| Entry | Verification |
|---|---|
| `monitor` (x86_64/x86) | SDM Vol.2B §4-39: addr-reg + ECX + EDX | ✓ |
| `monitorx` | AMD64 Vol.3 MONITORX: same as MONITOR | ✓ |
| `mwait`/`mwaitx` | SDM Vol.2B §4-44: EAX hints + ECX extensions | ✓ (with IRA13-1 sound over-approximation note) |
| `syscall` x86_64 | Linux x86_64 syscall ABI (RAX/RDI/RSI/RDX/R10/R8/R9 → RAX, RCX, R11 clobbered) | ✓ |
| `swi` ARM family | Linux arm SVC ABI (r7=syscall#, r0-r6=args, r0=ret) | ✓ |
| `swi` x86/x86_64 | empty ABI stub | ✓ (sound; INT vector determines meaning) |
| `CallHyperVisor`/`CallSecureMonitor` AArch64 | SMCCC §6.1: x0-x7 in, x0-x3 out | ✓ |
| `rdtsc`/`rdtscp` | SDM Vol.2B: EDX:EAX(:ECX) | ✓ |
| `rdpkru_u32` | SDM Vol.2B RDPKRU: ECX=0 input, EAX+EDX output | ✓ |
| `rdmsr` PURE | Sleigh emits `tmp:8 = rdmsr(ECX)` (ECX pcode-explicit) | ✓ |
| `wrmsr` PURE_WITH_MEM_EDGE | TSC/FSBASE mem-edge effects | ✓ |
| `readfsbase`/`readgsbase` PURE | destination explicit | ✓ |
| `writefsbase`/`writegsbase` PURE_WITH_MEM_EDGE | segment-base load-effects | ✓ |
| `swapgs` PURE_WITH_MEM_EDGE | SDM Vol.2B SWAPGS: no GPR; mem edge for GS-relative loads | ✓ |
| `sysret` NoReturn | kernel→user mode; no return to kernel caller | ✓ |
| `lfence`/`mfence`/`sfence` PURE_WITH_MEM_EDGE | ordering primitives | ✓ |
| `cpuid` family PURE | Sleigh tmpptr + downstream Loads | ✓ |
| `DataMemoryBarrier` / `DataSynchronizationBarrier` / `InstructionSynchronizationBarrier` PURE_WITH_MEM_EDGE | ARM ARM §B2.3 | ✓ |
| `LOCK`/`UNLOCK` PURE_WITH_MEM_EDGE | bracket markers | ✓ |

### Axis 5 — Indirect-branch resolution

✓ **Link-register** (`bx lr` / `pop {pc}`): `StackLoadForward` simplifies push-lr/pop-pc to `InitialVar(lr)`; classifier's `InitialVar(vn) if Some(vn)==link_register_vn` arm fires.

✓ **Jump-table**: `classify_jump_table` handles `Load(Add(IntConst(base), Mul(idx, IntConst(stride))))`.

✓ **Stack-array** (`gcc -O0` computed-goto): pinned across x86, x86_64, AArch64, ARM (all endians), MIPS32.

✓ **Tail-call / `Truncate(IntConst)` / `Extend(IntConst)`**: ConstantFold rules 4-6 pre-fold to `IntConst`; the IntConst classifier arm then fires.

✗ **AArch64-BE Or(SP,K) shape**: open gap (IRA13-2 above).

### Axis 6 — Lift-time canonicalisations bit-exact

All 8 verified against IEEE 754 / two's-complement boundaries:

1. `IntSub(a,b) → Add(a, Neg(b))` — `INT_MIN_32 - 0 = INT_MIN_32` (no wrap); `0 - INT_MIN_32 = INT_MIN_32` (wrap, sound). ✓
2. `IntLessEqual(a,b) → BoolNeg(IntLess(b,a))` — unsigned boundary 0 vs UINT_MAX. ✓
3. `IntSlessEqual(a,b) → BoolNeg(IntSless(b,a))` — INT_MIN signed boundary. ✓
4. `IntNotEqual(a,b) → BoolNeg(IntEqual(a,b))` — trivial. ✓
5. `FloatSub(a,b) → FloatAdd(a, FloatNeg(b))` — IEEE 754 §6.3.  Verified NaN (sign-bit flip), signed-zero (`0.0 - (-0.0) = +0.0`). ✓
6. `FloatNotEqual(a,b) → BoolNeg(FloatEqual(a,b))` — IEEE 754: `Equal(NaN, x) = false` → `BoolNeg = true` matches `NaN != x`. ✓
7. `FloatLessEqual(a,b) → Or(FloatLess(a,b), FloatEqual(a,b))` — NaN-aware (both arms false → false); infinity & signed-zero verified. ✓
8. `FloatNan(x) → BoolNeg(FloatEqual(x,x))` — IEEE 754 NaN ≠ NaN. ✓

## Summary

| # | Finding | Severity |
|---|---|---|
| IRA13-1 | `mwait`/`mwaitx` `implicit_reads` over-approximates on x86_64 (aliasing EAX→RAX) | LOW (sound) |
| IRA13-2 | AArch64-BE `Or(SP,K)` stack-array dispatch — `#[ignore]` is real, not stale | known tracked gap |

No critical correctness bugs.  All ABI return-value, clobber-footprint, memory-chain models consistent with published specs.  All 8 lift-time canonicalisations bit-exact.  24+ CallOther entries spot-checked against Intel SDM / ARM ARM / Linux kernel ABI.
