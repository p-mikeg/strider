# Round 9 — EA3: IR vs Real Assembly

**Branch:** `feature/ai`. Verification axes: return-value flow, clobber footprint, memory-chain across calls, CallOther implicit reads/writes, indirect-branch resolution shapes, ignored-test fix paths, lift-time canonicalisations.

## Critical

### CRITICAL-1: `sysret` classified `NoReturn` — incorrect; it returns to user-mode at RCX

**Confidence:** 88.

**Where:** `crates/target/src/call_other_abi.rs:259`.

```rust
"sysret" => NO_RETURN,
```

Intel SDM Vol. 2B: SYSRET returns from ring-0 to ring-3 by restoring `RIP` from `RCX` and `RFLAGS` from `R11`. Control reaches the instruction and resumes the user program. Linux `arch/x86/entry/entry_64.S` ends `system_call_fastpath` with `swapgs; sysretq`.

Classifying as `NoReturn` causes `cfg::region_builder` to terminate the region with `RegionTerminator::NoReturn` and `build_call_other_terminal` to emit a dangling-output CallOther. For kernel syscall-entry code containing `SYSRET`, the entire return-to-userspace control path is dropped from the CFG.

**Fix:** Reclassify as `PURE_WITH_MEM_EDGE` (writes RIP and RFLAGS, swaps GS base). If Sleigh models it as a control-transfer with no pcode outputs, may need to fold into a `Return`-like shape instead.

### CRITICAL-2: `x86_64_all_preserving` `ret_stack_pop=0` — correct but underdocumented

**Confidence:** 80.

**Where:** `crates/target/src/calling_convention/mod.rs:321-343`.

`__fentry__`/`mcount` use a normal `call`/`ret` pair. The `call` pushes a return address (-8 RSP) and `ret` pops it (+8 RSP) for a net zero SP delta. `ret_stack_pop = 0` is correct because it models the caller's RSP shift after the call returns; `call` itself is modeled by the architecture. A future maintainer could mistakenly change to 8 thinking it matches `x86_64_systemv`.

**Fix:** Add explanatory comment explaining why `ret_stack_pop: 0` is correct (net zero SP delta on caller after call/ret).

## Important

### IMP-1: AArch64-BE `Or(SP,K)` ignore reason — fix path has two distinct components

**Confidence:** 85.

**Where:** `crates/strider/tests/indirect_branch.rs:213` and `crates/opt/src/indirect_branch_resolve/stack_array.rs`.

Two separate gaps: (a) `flatten_add_tree` only handles `IntBinaryOp::Add`, not `Or`. AArch64-BE Sleigh emits `Or(sp, K)` (sound when sp's upper bits are zero). (b) `find_stack_stored_value_at_offset` doesn't peel `Truncate` wrappers around stored labels.

**Fix A:** Add `IntBinaryOp::Or` to `flatten_add_tree`. **Fix B:** Apply `Truncate(IntConst)` peeling in `classify_stack_array`.

### IMP-2: MIPS64 PIC GOT-indirect — fix path is incompletely specified

**Confidence:** 85.

**Where:** `crates/strider/tests/indirect_branch.rs:238-245`.

GOT-indirect `Add(Load[gp+off], const)` shape — `LoadReadOnly` fails because `gp` is runtime-initialized GOT base. Fix requires either (1) relocation-applied ROM (similar to `apply_elf_relocations_autoload`) or (2) a new "GOT-indirect" classifier arm.

**Suggest:** Update ignore reason: "MIPS64 PIC GOT-indirect — `LoadReadOnly` needs a relocation-applied ROM to fold `Load[gp+off]`; test fixture uses raw `ElfFileMemReader` that does not apply relocations."

### IMP-3: PPC32/PPC64 ignored tests — actual cause likely `Or(r1,K)` same as AArch64-BE

**Confidence:** 82.

**Where:** `crates/strider/tests/indirect_branch.rs:248-265`.

Likely shape: `Or` for indexed-load address computation (same as AArch64-BE) or extra Truncate/Extend nodes for 32/64-bit pointer manipulation. PPC `bctr` with table-based dispatch is highly likely a stack-array pattern at `-O0`.

**Suggest:** Update ignore reasons to point at likely cause (Or-shape, stack-array shape).

### IMP-4: `handle_int_sub` neg width mismatch (theoretical)

**Confidence:** 81.

**Where:** `crates/pcode-lift/src/value/arithmetic.rs:151-163`.

`neg_rhs` built at `out_ty` (output type), but `rhs` was read at `insn.inputs[1].size`. Theoretical mismatch — Sleigh always emits `IntSub` with `input_size == output_size` so unlikely to fire in practice.

**Fix (defensive):** Use `insn.inputs[0].size.try_into()?` for the `Neg` width.

### IMP-5: ARM-BE VFP register aliasing drops float chain

**Confidence:** 83.

**Where:** `crates/strider/tests/floats.rs:15-43` (ignored).

Documented gap accurate but ignore reason text is imprecise. ARM-BE register file in Sleigh reverses LE containment order. `find_largest_fitting_register` for `s0` finds no container that strictly contains it via `s <= reg_start && e >= reg_end`. Fix path: extend `find_largest_fitting_register` with BE-aware containment logic.

### IMP-6: `swapgs` comment misleading

**Confidence:** 82.

**Where:** `crates/target/src/call_other_abi.rs:308-313`.

Comment says "No general-reg or RAM effect on its own". Technically correct for GPRs/RAM but misleading: `swapgs` swaps `IA32_GS_BASE` ↔ `IA32_KERNEL_GS_BASE` MSRs, which DOES affect virtual base address used by `%gs:`-relative loads. The `PURE_WITH_MEM_EDGE` classification is correct, but comment should explicitly state the reordering hazard.

### IMP-7: PPC32/64 `ret_val_regs_float: &["f1", "f2"]` over-approximates

**Confidence:** 80.

**Where:** `crates/target/src/calling_convention/mod.rs:501-519` (and PPC64 ELFv1/v2).

PPC32 SysV: `float` and `double` returns in `f1` only; `f2` only used for 128-bit `long double`. Listing `["f1", "f2"]` over-approximates and adds uninitialized `InitialVar(f2)` slots for `double`-returning functions. Pattern queries on `ret().ret_val(1, ...)` would match `f2` even for `double` returns.

**Fix:** Reduce to `["f1"]` for scalar floats; document `f2` as long-double-only edge case if needed.

## Verification Axis Coverage

| Arch | Return | Clobber | Mem | CallOther | Indirect | Canon |
|------|--------|---------|-----|-----------|----------|-------|
| x86 | ✓ | ✓ | ✓ | ✓ | N/A | ✓ |
| x86_64 | ✓ | ✓ | ✓ | ✓ (sysret CRITICAL-1) | ✓ | ✓ |
| AArch64 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| AArch64-BE | ✓ | ✓ | ✓ | ✓ | BLOCKED (IMP-1) | ✓ |
| ARM/ArmBe/Thumb | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ (BE float chain IMP-5) |
| MIPS32 | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| MIPS64 | ✓ | ✓ | ✓ | ✓ | BLOCKED (IMP-2) | ✓ |
| PPC32/64 | ✓ (IMP-7) | ✓ | ✓ | ✓ | BLOCKED (IMP-3) | ✓ |

## Canonicalisation Verification (8/8)

All 8 canonicalisations bit-exact: `IntSub→Add+Neg`, `IntLessEqual→¬Less(swap)`, `IntSlessEqual→¬Sless(swap)`, `IntNotEqual→¬Equal`, `FloatSub→FloatAdd+Neg`, `FloatNotEqual→¬FloatEqual` (NaN-safe), `FloatLessEqual→Or(Less, Equal)` (NaN-safe), `FLOAT_NAN(x)→¬FloatEqual(x,x)`.

## CallOther Implicit Channel Verification (20+ entries)

Verified against Intel SDM Vol. 2 / ARM ARM: cpuid, rdtsc/rdtscp, rdmsr/wrmsr, swapgs, rd*fsbase/wr*fsbase, mfence/sfence/lfence, syscall, swi (ARM Linux), sysret (**WRONG** CRITICAL-1), CallHyperVisor/CallSecureMonitor (SMCCC), DataMemoryBarrier/DSB/ISB, DC_CVAC, LOCK/UNLOCK, rdpkru_u32, software_interrupt, in/out, SoftwareBreakpoint/UndefinedInstructionException.

## Summary

| # | Severity | Finding | File:Line | Conf |
|---|----------|---------|-----------|------|
| C-1 | Critical | `sysret` classified NoReturn | `call_other_abi.rs:259` | 88 |
| C-2 | Med | `x86_64_all_preserving` ret_stack_pop=0 underdocumented | `calling_convention/mod.rs:335` | 80 |
| I-1 | Important | AArch64-BE Or(SP,K)+Truncate fix path split | `indirect_branch.rs:213` | 85 |
| I-2 | Important | MIPS64 PIC GOT-indirect needs reloc-ROM | `indirect_branch.rs:238` | 85 |
| I-3 | Important | PPC32/64 likely Or(r1,K) shape | `indirect_branch.rs:248` | 82 |
| I-4 | Important | `handle_int_sub` neg width mismatch (theoretical) | `arithmetic.rs:155` | 81 |
| I-5 | Important | ARM-BE VFP aliasing drops float chain | `floats.rs:15` | 83 |
| I-6 | Med | `swapgs` comment misleading | `call_other_abi.rs:308` | 82 |
| I-7 | Med | PPC32/64 `ret_val_regs_float` over-approximates | `calling_convention/mod.rs:511` | 80 |
