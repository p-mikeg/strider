# Round 8 / Ask 17 — Graph soundness vs real assembly

**Branch:** `review/ai2`.  Independent audit.

## A. Return-value flow

All 9 CC presets cross-checked against ABI specs (System V x86-64, x86 cdecl, AAPCS64, AAPCS, MIPS o32/n64, PowerPC SysV32 / ELFv1 / ELFv2).  Every `ret_val_regs` and `ret_val_regs_float` list correct.  No mismatches.

**Coverage gap:** No fixture exercises 128-bit `__int128` returns on x86_64 (RAX+RDX pair) or `long double` 80-bit returns on x86 (ST0).

## B. Clobber footprint

### B-1 (HIGH, conf 88): AArch64 `aarch64_aapcs64` lists x30 (LR) in `callee_saved_regs`

- **Where:** `crates/target/src/calling_convention/mod.rs:354-355`.
- **What's wrong:** AAPCS64 §6.1 lists r19-r28 plus r29 (FP) as callee-saved GPRs.  **r30 (LR/x30) is NOT callee-saved** — it is caller-saved by the spec.
- **Design tension:** Including x30 makes `InitialVar(x30)` propagate through call sites, enabling the indirect-branch resolver's `LinkRegister` arm.  But this suppresses a legitimate clobber output on x30 for every call.  A tail-call shim (`mov x30, x1; br x1`) would be mismodeled.
- **Fix:** Either (a) document the intentional deviation in a code comment, or (b) introduce a separate `link_register_preserved_by_convention` flag on the CC.  Same fix applies to ARM AAPCS (`lr` in `callee_saved_regs`) and PowerPC SysV/ELFv1/ELFv2 (`LR` in `callee_saved_regs`) — see B-2 / B-3.

### B-2 (HIGH, conf 82): PowerPC 32/ELFv1/ELFv2 list LR in `callee_saved_regs`

- **Where:** `crates/target/src/calling_convention/mod.rs:496-508, 532-545, 565-579`.
- **What's wrong:** PowerPC Processor Supplement §3.4 marks LR as volatile.  Same design tradeoff as B-1.

### B-3 (MED, conf 85): ARM AAPCS lists `lr` in `callee_saved_regs`

- **Where:** `crates/target/src/calling_convention/mod.rs` (arm_aapcs preset).
- **What's wrong:** AAPCS §5.1.1 specifies r4-r11 as callee-saved.  r14 (lr) is not.  Same tradeoff as B-1.

## C. Memory chain after a call

- **Normal calls (`no_memory_clobber=false`)**: `Call` advances memory chain; `LoadReadOnly` and `StackLoadForward` correctly stop at the Call's memory output.
- **All-preserving calls (`no_memory_clobber=true`)**: Memory chain unchanged across the call; forwarding works.  `test_per_address_ccs_honoured_in_both_pipeline_paths` covers this end-to-end.
- **Coverage gap:** No `__fentry__`-instrumented kernel binary fixture.  Required: x86_64 ELF object compiled with `gcc -pg -mfentry -O2`.

## D. CallOther implicit reads/writes

Cross-referenced against Intel SDM Vol.2, ARM ARM, GHIDRA Sleigh source.  Verified entries: CPUID, RDTSC, RDTSCP, RDMSR, WRMSR, RDFSBASE, RDGSBASE, WRFSBASE, WRGSBASE, SWAPGS, syscall (x86_64), swi (ARM), SMCCC (CallHyperVisor / CallSecureMonitor), RDPKRU, ARM DMB/ISB/DSB.

### D-1 (HIGH, conf 83): `mfence`/`sfence`/`lfence` missing from CallOther table

- **Where:** `crates/target/src/call_other_abi.rs::classify_arch_specific`.
- **What's wrong:** Memory fence instructions emit Sleigh user-ops not in the table.  Any binary using SFENCE/LFENCE/MFENCE fails to lift with `UnknownCallOtherError`.
- **Fix:**
  ```rust
  (crate::ArchPreset::X86 | crate::ArchPreset::X86_64,
   "mfence" | "sfence" | "lfence") => PURE_WITH_MEM_EDGE,
  ```

### D-2 (LOW, conf 75): MONITOR/MWAIT not covered

`cpuid_MONITOR_MWAIT_Features_info` models the CPUID leaf, not the actual instructions.  MONITOR/MWAIT in user-op form would fail.  Both should be `PURE_WITH_MEM_EDGE`.

### D-3 (LOW, conf 80): `sysret` classified as NoReturn

Defensible for kernel analysis (kernel's local control flow ends at SYSRET) but loses the well-defined RCX→RIP / R11→RFLAGS register effects.  Documented design choice.

### D-4 (LOW, conf <80): `in`/`out` port I/O

Whether DX is implicit-read or pcode-explicit depends on Sleigh `.sla` content.  Verification gap (no Sleigh spec file accessible at audit time).

## E. Indirect-branch resolution

- **LinkRegister shape**: classifier matches `InitialVar(vn) if Some(vn) == link_register_vn` at `classify.rs:149`.  Verified via system tests `test_fib_recursive` on aarch64/arm.  No structural test asserting `ResolvedTargets::LinkRegister` at ≥3 sites.
- **Jump table**: `test_orchestrator_resolves_jump_table_x86` covers `switch.elf::dispatch_value` end-to-end with ≥4 case bodies.  No AArch64/ARM jump-table fixture.
- **Stack-array dispatch**: `test_run_resolves_indirect_branch_x86` exercises `indirect_branch.c::switch_void_p` on x86.  No multi-arch coverage.
- **Tail call (`Single(K)` outside function range)**: `apply_tail_call` path exercised via `abi.c::tail_caller`.  No test pins the IR shape (`Call` without following `Return`).

## F. Lift-time canonicalisations

All 8 verified in `crates/pcode-lift/src/value/{arithmetic,float}.rs`:

1. `IntSub(a,b) → Add(a, Neg(b))` — `arithmetic.rs::handle_int_sub`.
2. `IntLessEqual(a,b) → BoolNeg(IntLess(b,a))` — `arithmetic.rs::handle_int_less_equal`.
3. `IntSlessEqual(a,b) → BoolNeg(IntSless(b,a))` — `arithmetic.rs::handle_int_sless_equal`.
4. `IntNotEqual(a,b) → BoolNeg(IntEqual(a,b))` — `arithmetic.rs::handle_int_not_equal`.
5. `FloatSub(a,b) → FloatAdd(a, FloatNeg(b))` — `float.rs::handle_float_sub`.  IEEE 754 sound.
6. `FloatNotEqual(a,b) → BoolNeg(FloatEqual(a,b))` — IEEE 754 sound on NaN.
7. `FloatLessEqual(a,b) → Or(FloatLess(a,b), FloatEqual(a,b))` — NaN-aware.
8. `FLOAT_NAN(x) → BoolNeg(FloatEqual(x,x))` — IEEE 754: NaN ≠ NaN.

**Sleigh nomenclature inversion** in `value/mod.rs:58-59`:
- `Opcode::Int2Comp → IntUnaryOp::Neg` (two's complement negate)
- `Opcode::IntNeg → IntUnaryOp::BitNot` (bitwise NOT)

Correctly implemented per GHIDRA's reversed naming.

**Coverage gap:** No test lifts a real machine instruction (e.g. `sub rax, rbx`) and asserts the resulting IR is `Add(InitialVar(RAX), Neg(InitialVar(RBX)))`.  Existing `count_int_binop` matches via `pat.sub` (which itself is a lowered alias) — confirms shape is present but not its origin.

## Required regression tests

- **RT-1** (B-1): Lift AArch64 binary with LR-overwriting tail-call shim; assert post-call x30 is NOT `InitialVar(x30)`.
- **RT-2** (D-1): Unit test `fence_ops_classify_as_pure_with_mem_edge` for mfence/sfence/lfence on X86 and X86_64.
- **RT-3** (D-1): Fixture-based `mfence` lift in `fixtures/cases/builtins.c`.
- **RT-4** (F): Lift `arithmetic.elf::sub_expr` and assert `Add(_, Neg(_))` with corresponding `InitialVar` operands.

## Coverage gaps requiring new fixtures

1. `__fentry__`-instrumented kernel binary (C section).
2. AArch64 jump-table fixture (E section).
3. 128-bit return value fixture (`__int128` on x86_64; `long double` on x86) (A section).
4. Callee-saved register flow-through test (asserts same `NodeOutputId` before and after a call) (B section).

## Summary

| # | Severity | Confidence | Title |
|---|---|---|---|
| B-1 | HIGH | 88 | x30 in `aarch64_aapcs64::callee_saved_regs` contradicts AAPCS64 §6.1 |
| B-2 | HIGH | 82 | LR in PPC32/64 `callee_saved_regs` contradicts ELF v1/v2 spec |
| B-3 | MED | 85 | lr in `arm_aapcs::callee_saved_regs` contradicts AAPCS §5.1.1 |
| D-1 | HIGH | 83 | mfence/sfence/lfence missing from CallOther table |
| D-2 | LOW | 75 | MONITOR/MWAIT not covered |
| D-3 | LOW | 80 | `sysret` NoReturn loses register effects |
| A | gap | — | No 128-bit return fixture |
| C | gap | — | No `__fentry__` fixture |
| E | gap | — | No AArch64 jump-table fixture |
| F | gap | — | No fixture-based canonicalisation shape test |
