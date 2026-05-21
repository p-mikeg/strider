# Round 13 — Ask-8 pass 5: cross-architecture consistency audit

Branch: `review/ai7`.

## Verdict

**No findings.  All 9 areas consistent across all 15 SleighArch presets and 16 `per_arch_test!` variants.**

## Categories verified consistent

✓ **Area 1: CC preset coverage**

| Arch family | Userland | Linux kernel | Linux syscall |
|---|---|---|---|
| x86 | `x86_cdecl` | `x86_linux_kernel` | `x86_linux_syscall` |
| x86_64 | `x86_64_systemv` + `x86_64_all_preserving` | `x86_64_linux_kernel` | `x86_64_linux_syscall` |
| AArch64 LE/BE | `aarch64_aapcs64` | `aarch64_linux_kernel` | `aarch64_linux_syscall` |
| ARM LE/BE/Thumb | `arm_aapcs` | `arm_linux_kernel` | `arm_linux_syscall` |
| MIPS32 LE/BE | `mips_o32` | `mips_linux_kernel_o32` | `mips_linux_syscall_o32` |
| MIPS64 LE/BE | `mips_n64` | `mips_linux_kernel_n64` | `mips_linux_syscall_n64` |
| PPC32 LE/BE | `powerpc_sysv32` | — | — |
| PPC64 LE/BE | `powerpc64_elf_v1` / `powerpc64_elf_v2` | — | — |

PPC has no kernel/syscall (consistent with CLAUDE.md docs — not a gap).

✓ **Area 2: SleighArch endianness** — every BE variant uses the `_BE` SLA spec; every LE uses the LE spec.  AArch64 BE → `SLA_SPEC_AARCH64BE`+`Big`; ARM BE → `SLA_SPEC_ARM8_BE`+`Big`; PPC64 uses `SLA_SPEC_PPC_64_ISA_ALTIVEC_BE`/`_LE` (not the stripped spec).

✓ **Area 3: CallOther tabulation** — `swi` arch-specific for ARM family (Linux SVC) vs X86/X86_64 (empty-ABI stub).  `syscall` X86_64-only.  `sysret`/`swapgs` X86/X86_64-only (R12 CA-2 enforced).  `CallHyperVisor`/`CallSecureMonitor` AArch64/AArch64Be-only.  Structural invariant pinned by `arch_independent_call_entries_have_empty_register_channels` test.

✓ **Area 4: LR-as-callee-saved** — every link-register CC lists LR in `callee_saved_regs`:

| Preset | LR | Sleigh name |
|---|---|---|
| `aarch64_aapcs64` (LE+BE) | `x30` | `x30` |
| `arm_aapcs` (LE+BE+Thumb) | `lr` | `lr` |
| `mips_o32` (LE+BE), `mips_n64` (LE+BE) | `ra` | `ra` |
| `powerpc_sysv32` (LE+BE), `powerpc64_elf_v1` (BE), `powerpc64_elf_v2` (LE) | `LR` | `LR` |

`try_from_parts` enforces `link_register_vn ⊆ callee_saved_regs`.  `link_register_vn_resolves_to_callee_saved_lr` pin test covers all 13 link-register presets.  Linux syscall presets correctly set `link_register_reg_name: None`.

✓ **Area 5: `Builder::for_arch` migration** — `Builder::new` and `Builder::with_endianness` fully deleted (R12 S1.4 W5c).  Three remaining mentions are comments explaining preference for `for_arch`.  Orchestrator at `orchestrator.rs:958` uses `Builder::for_arch(&opts.strider.arch, ...)` exclusively.

✓ **Area 6: `per_arch_test!` coverage** — macro generates 16 arch variants: X86, X86Kernel, X64, Aarch64, Aarch64Be, Arm, ArmBe, ArmThumb, Mips32le, Mips32be, Mips64le, Mips64be, Ppc32be, Ppc32le, Ppc64be, Ppc64le.  All 15 SleighArch presets represented (X86Kernel reuses X86 SleighArch with kernel CC).

✓ **Area 7: `apply_elf_relocations` arch coverage** — RELATIVE/IRELATIVE: x86_64 (8B), I386 (4B), AArch64 (8B), ARM (4B), PowerPc64 (8B), PowerPc (4B), Mips/Mips64 (4B per R_MIPS_REL32 documented).  GLOB_DAT/JUMP_SLOT: same arches; MIPS64 correctly 8B for slots vs MIPS32's 4B.

✓ **Area 8: sub-register aliasing per arch** — `calculate_reg_shift_from_container` branches on `target::Endianness`: LE = `8*(reg.off-container.off)`; BE = `8*(container.size-reg.size-(reg.off-container.off))`.  `shift_formula_tests` covers 4-byte LE/BE and 8-byte BE containers.  Wide-container guard (>16B) rejects YMM/ZMM sub-register aliasing.

✓ **Area 9: `StackLoadForward` endianness** — `from_convention` threads `arch.endianness()`.  `realize::Narrow`: LE = `Truncate(data)`; BE = `Truncate(ShiftRight(data, (store_size-load_size)*8))`.  All synthetic nodes use `create_node_attributed(..., &[load])` for asm-fingerprint coverage.

## Summary

Cross-arch coverage is uniform: 15 SleighArch presets × 9 areas verified.  No drift between BE/LE variants, no missing CC tier (PPC kernel/syscall absent is documented and consistent), no migration leftover.  Round-12 W5c's `Builder::new` deletion landed cleanly.
