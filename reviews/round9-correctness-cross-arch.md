# Round 9 — Ask-8 R5: Cross-arch correctness pass

**Branch:** `review/ai3`. Each axis verified against all 15 supported arch presets.

## Critical

### C-1 — `indirect_branch.rs` test uses `Builder::with_endianness` instead of `Builder::for_arch`

**Confidence:** 95.

**Where:** `crates/strider/tests/indirect_branch.rs:91`.

```rust
let cfg = cfg::Builder::with_endianness(sleigh, addr, cfg_opts, sleigh_arch.endianness)
    .build()
```

`Builder::with_endianness` initialises `preset = ArchPreset::X86_64` unconditionally (`crates/cfg/src/cfg/builder/mod.rs:107-118`). For every non-x86_64 arch tested by `assert_no_unresolved_indirect_branch`, any `CallOther` encountered will be classified under the x86_64 dispatch table.

Concrete impact: ARM `swi` would be dispatched as `(X86_64, "swi")` (empty-ABI stub) instead of `(Arm, "swi")` (`r7/r0..r6` ABI). AArch64 `CallHyperVisor`/`CallSecureMonitor` would return `None` from the x86_64 table. Production path in `orchestrator.rs:837` correctly uses `Builder::for_arch`.

The same issue exists in `indirect_branch_lift_placeholder.rs:46` but that test is x86_64-only by construction.

**Fix:** Replace with `cfg::Builder::for_arch(&sleigh_arch, sleigh, addr, cfg_opts).build()`.

## Important

### I-1 — AArch64 AAPCS64 test coverage gap

**Confidence:** 82.

**Where:** `crates/target/src/calling_convention/tests.rs:678` (ARM test) and missing AArch64 parallel.

`link_register_vn_resolves_to_callee_saved_lr` pins ARM (`lr` in `callee_saved_regs`). No parallel for AArch64 (`x30` in `callee_saved_regs`). A future edit dropping `x30` from `aarch64_aapcs64`'s `callee_saved_regs` while keeping `link_register_reg_name = Some("x30")` would pass all tests. Test coverage gap, not current correctness bug.

**Fix:** Add parallel test asserting `x30` present in `aarch64_aapcs64`'s `callee_saved_regs` and equals `link_register_vn()`.

### I-2 — `cfg::Builder::with_endianness` normalised in `known_targets.rs` tests

**Confidence:** 83.

**Where:** `crates/cfg/tests/known_targets.rs:30, 71, 104, 143, 158, 203`.

All `Builder::with_endianness(...)` calls silently set `preset = X86_64`. Current test bodies use synthetic x86_64 byte sequences (`jmp rax`), so the wrong preset doesn't fire. But future tests adding a real-arch synthetic sequence would silently use the wrong preset.

**Fix:** Change all calls to `Builder::for_arch(&arch, sleigh, base, opts)` for test hygiene.

## Verified Correct Axes

- **CallOther dispatch cross-arch**: `(Arm|ArmBe|ArmThumb, "swi")`, `(X86_64, "syscall")`, `(Aarch64|Aarch64Be, "CallHyperVisor"|"CallSecureMonitor")`, `(X86|X86_64, "mfence"|"sfence"|"lfence")`, `(X86|X86_64, "rdmsr"|"readfsbase"|...)` — all match published ABIs.
- **Calling convention presets**: x86_64 SysV (6 callee-saved + ret_stack_pop=8), x86 cdecl, AArch64 AAPCS64 (12 callee-saved including x30), ARM AAPCS (9 callee-saved), MIPS O32 (11 callee-saved + 16-byte shadow), MIPS N64 (no shadow), PPC32 SysV, PPC64 ELFv1 (48-byte linkage), PPC64 ELFv2 (32-byte linkage). All correct against published ABIs.
- **Endianness handling**: `calculate_reg_shift_from_container` LE and BE arms verified. `StackLoadForward::Narrow` BE path emits `Truncate(ShiftRight(data, (store_size - load_size) * 8))` — correct.
- **Lift-time canonicalisations**: All 8 canonicalisations dispatch is opcode-keyed, no arch parameter. Identical behaviour on every arch.
- **Indirect-branch resolver link-register**: All 15 CC presets correctly declare `link_register_reg_name`. `lr_vn` propagates correctly through `LoopState::new`.
- **`Builder::for_arch` propagation in production**: `orchestrator.rs:837` uses `for_arch` correctly. Bug confined to test file (C-1).
- **MIPS branch-delay slot**: Sleigh MIPS specs inline delay-slot pcode via `delayslot(1)`; transparent to `RegionBuilder`. Confirmed by un-skipped MIPS32 integration tests.
- **7 ignored tests in `indirect_branch.rs`**: all 7 ignore reasons accurately describe known classifier gaps (aarch64-be `Or(SP,K)`, mips64 GOT-indirect, ppc shape uncharacterised).

## Summary

- **1 CRITICAL** (C-1): test misuses `Builder::with_endianness` for non-x86_64 arches.
- **2 IMPORTANT** (I-1, I-2): test coverage / hygiene gaps.

All 15 arches verified across 8 axes. No production correctness bugs found.
