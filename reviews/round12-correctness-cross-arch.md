# Round 12 — Ask 8 pass 5: cross-architecture consistency audit

Branch: `review/ai6` · Trust model: strict (no prior-round reviews; no other round-12 reports read).

Arches in scope: x86, x86_64, ARM, ARM-BE, ARM-Thumb, AArch64, AArch64-BE, MIPS32-LE/BE, MIPS64-LE/BE, PPC32-LE/BE, PPC64-LE/BE.

## Findings

### CA-1 — Missing PowerPC linux_kernel and linux_syscall CC presets
- **Severity:** HIGH
- **Where:** `crates/target/src/calling_convention/mod.rs:781-951`
- **Inconsistent across:** Ppc32Be, Ppc32Le, Ppc64Be, Ppc64Le
- **Behaviour drift:** Every other supported arch has three CC tiers — userland + linux_kernel + linux_syscall. PowerPC (all 4 variants) has only userland presets (`powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`). CLAUDE.md explicitly documents Linux-kernel-internal and Linux-syscall presets for every arch family. A caller analysing a PowerPC kernel binary has no documented preset and must either use the wrong userland CC or hand-construct a `BuiltCallingConvention`. The Linux PPC syscall ABI uses `r0` for the syscall number, `r3-r8` for args, and `r3` for return — which differs from the userland CC in ways that matter for pattern queries.
- **Fix:** Add `powerpc_linux_kernel()`, `powerpc64_linux_kernel()`, `powerpc_linux_syscall()` (syscall number `r0`, args `r3-r8`, return `r3`, `callee_saved_regs: &[]`, `ret_stack_pop: 0`, `syscall_number_reg_name: Some("r0")`), and `powerpc64_linux_syscall()` mirroring the MIPS layout at lines 832-950.
- **Regression test:** Add rows to `cases()` in `crates/target/src/calling_convention/tests.rs` mirroring existing syscall-preset rows at lines 234-294. Verify `syscall_number_reg_name = Some("r0")` and disjointness invariants pass.

### CA-2 — x86-only ops `sysret` and `swapgs` live in `classify_arch_independent`
- **Severity:** MED
- **Where:** `crates/target/src/call_other_abi.rs:257` (`sysret`), `:313` (`swapgs`)
- **Inconsistent across:** All non-x86 arches
- **Behaviour drift:** `classify(ArmBe, "sysret")` returns `Some(NoReturn)`; `classify(Mips64Le, "swapgs")` returns `Some(Call(PURE_WITH_MEM_EDGE))`. Both ops are x86/x86_64-specific (the `swapgs` inline comment at line 306 even says "x86 SWAPGS"), yet they live in the arch-independent table. In practice no non-x86 Sleigh spec emits these user-op names, so misclassification is unreachable today. But a future Sleigh spec coincidentally naming a non-x86 instruction "sysret" would be silently classified `NoReturn`, truncating the CFG. The arch-independent invariant ("Call entries here MUST have empty implicit_reads/writes") is satisfied — but masks the conceptual error.
- **Fix:** Move both into `classify_arch_specific` matching `X86 | X86_64`. The `lfence`/`mfence`/`sfence`/`rdtsc`/`rdmsr`/`wrmsr`/`readfsbase`/`writefsbase` entries (lines 134-181) already follow this pattern. `sysret` keeps `NO_RETURN`; `swapgs` keeps `PURE_WITH_MEM_EDGE`.
- **Regression test:** `assert_eq!(classify(ArchPreset::Arm, "sysret"), None)` and `assert_eq!(classify(ArchPreset::Aarch64, "swapgs"), None)`.

### CA-3 — Surviving deprecated `Builder::new` in pipeline test
- **Severity:** LOW
- **Where:** `crates/strider/src/strider/pipeline.rs:620`
- **Inconsistent across:** Not an arch bug today (test fixture is x86_64-only and the LE+X86_64 default is correct), but a copy-paste footgun for future non-x86 test fixtures.
- **Behaviour drift:** `cfg::Builder::new(sleigh, 0x1000, …)` inside a `#[cfg(test)]` block annotated `#[allow(deprecated)]`. Keeping the deprecated call in any form means Clippy allows future copies without a warning.
- **Fix:** Replace with `cfg::Builder::for_arch(&arch, sleigh, 0x1000, …)` — `arch` is already in scope at line 609.

## Categories verified consistent

✓ **SleighArch endianness** — every BE variant uses the correct BE SLA; LE variants use LE specs. `target/src/arch.rs:180-352`.

✓ **LR-as-callee-saved tradeoff** — AArch64/AArch64-BE: `x30` ∈ `callee_saved_regs` + `link_register_reg_name = Some("x30")`. ARM/ARM-BE/ARM-Thumb: `lr`. PPC32/64 (all variants): `LR`. All match CLAUDE.md's documented deviation. `calling_convention/mod.rs:444,484,586,636`.

✓ **`Builder::for_arch` migration** — all production callers use `for_arch`. Only one deprecated call survives (CA-3 above, test-only).

✓ **`per_arch_test!` coverage** — macro expands across all 16 arch variants including ArmBe, ArmThumb, Mips64le/be, Ppc32/64 LE/BE. `crates/strider/tests/common/mod.rs:424-439`.

✓ **`apply_elf_relocations` arch coverage** — RELATIVE/IRELATIVE for x86_64, i386, aarch64, arm, ppc64, ppc32, mips/mips64. GLOB_DAT/JUMP_SLOT for the same set. `R_MIPS_REL32` correctly 4 bytes on both MIPS32 and MIPS64. `crates/reader/src/elf.rs:877-1015`.

✓ **Sub-register aliasing** — `vn_mask` covers widths 1/2/4/8/10/16/32/64 bytes, handling x86 byte-regs, x87 ST0-ST7 (10-byte), AArch64 B/H/S/D/Q (1/2/4/8/16), ARM S/D/Q (4/8/16), AVX YMM/ZMM (32/64 via u128::MAX). Sub-register aliasing within containers > 16 bytes correctly errors. `pcode-lift/src/vn_io.rs:38-48, 217-231`.

✓ **`StackLoadForward` endianness** — threaded via `from_convention(cc, arch)` to `realize()`. BE narrow-load: `Truncate(ShiftRight(data, shift_bits))`. LE narrow-load: `Truncate(data)`. `crates/opt/src/stack_load_forward/mod.rs:52-57, 383-398`.

✓ **CallOther dispatch arch grouping** — ARM three-way `Arm | ArmBe | ArmThumb` for `swi`; AArch64+AArch64Be for SMCCC; X86+X86_64 for x86 user-ops. No cross-arch drift. `call_other_abi.rs:83-181`.

✓ **`ret_stack_pop` consistency** — x86: 4, x86_64: 8, all link-register ISAs: 0. Syscall overrides (`x86_64_linux_syscall`, `x86_linux_syscall`) correctly set `ret_stack_pop = 0`. `calling_convention/mod.rs:717, 860-881`.

## Files reviewed

- `crates/target/src/{arch.rs,call_other_abi.rs,calling_convention/{mod.rs,tests.rs}}`
- `crates/reader/src/elf.rs`
- `crates/pcode-lift/src/vn_io.rs`
- `crates/opt/src/stack_load_forward/mod.rs`
- `crates/cfg/src/cfg/builder/mod.rs`
- `crates/strider/src/strider/pipeline.rs`, `crates/strider/tests/common/mod.rs`
