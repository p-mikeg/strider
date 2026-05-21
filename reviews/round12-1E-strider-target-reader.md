# Round 12 Audit 1E — strider + target + reader

Branch: `review/ai6` · Scope: `crates/strider/{src,tests}`, `crates/target/{src,tests}`, `crates/reader/{src,tests}` · Trust model: strict (no prior-round reviews consulted)

## Verdict

**No HIGH findings — clean.**

The scope is broadly well-structured and the focused-area changes from W3 / W6 / W11 / W14 hold up against the ISA / ABI specs and the rsleigh SLA / pspec tables. A handful of LOW-confidence observations are listed below for completeness; none rise above the 80-confidence reporting threshold.

## Coverage of focus areas

### strider

1. **`LoopState::step` fixed-point convergence + `Decision`**
   `orchestrator.rs:416-460` — `next_known = self.known_targets.clone()`, then `edge_set_changed = edge_set_of(&next_known) != edge_set_of(&self.known_targets)`. Both `LinkRegister` (which goes to in-place edits, not `next_known`) and the count-stable case are correctly distinguished from edge-set growth. Mutual exclusion between in-place edits and `next_known` insertion is preserved by `classify_and_partition` (`orchestrator.rs:507-554`).

2. **`locate_spliced_call` 2-level walk (W3 fix)**
   `orchestrator.rs:740-761` walks both the direct `Call -> Return` and the `Call -> ControlState -> Return` region-join shapes. Returns `None` on shape mismatch with no panic. Best-effort behaviour is documented at the call site (`apply_in_place_edit` only uses it to write the override side-table).

3. **`apply_link_register` / `apply_tail_call` idempotency + fingerprint propagation**
   `crates/opt/src/indirect_branch_resolve/inplace.rs:43-72` — `apply_link_register` mutates `IndirectBranch → Return` in place; the leading `matches!` guard makes the second call fail-loud (not silently corrupt). The NodeId persists, so the existing fingerprint side-table entry survives.
   `inplace.rs:103-191` — `apply_tail_call` snapshots `placeholder_fingerprint` BEFORE `detach_node_inputs`, then `extend_asm_fingerprint`s the new `IntConst`, `Call`, and `Return`. CLAUDE.md's superset-only contract is honoured.

4. **`GraphRewriter::apply_rule` + `re_optimize` doc-contracts**
   `crates/strider/src/rewrite.rs:127-148` — pre-collects candidates before mutating (correct: prevents iterator-while-mutating UB). `re_optimize` doc explicitly warns that destructive passes invalidate external `NodeId`s (`rewrite.rs:185-192`). The unit tests at `rewrite_tests.rs` pin idempotency and use-list integrity.

5. **Per-arch `test_utils` wrappers (W11 added MIPS/PPC)**
   `crates/strider/src/test_utils.rs:36-95` — `strider_for_arch`, `strider_x86_64`, `strider_x86`, `strider_aarch64`, `strider_arm`, `strider_mips_o32`, `strider_mips_o32_be`, `strider_ppc32`, `strider_ppc64le`, `strider_ppc64be`. The `#![allow(clippy::expect_used, clippy::panic)]` is appropriately scoped to test-only fixture code.

6. **`handle_call_other` clobber-loop `out_vn`-skip (W3 fix for RDPKRU)**
   `crates/strider/src/strider/insn/mod.rs:229-238` — pcode-explicit output written first, then per-clobber loop skips any clobber-slot whose `Vn` matches `insn.output`. For `rdpkru_u32` (whose ABI table lists `EAX, EDX` as implicit-writes while pcode emits `EAX = rdpkru_u32()`), the EAX clobber slot is correctly suppressed so the modeled value isn't shadowed.

7. **Empty-Branch IR control-edge wiring (W3 fix)**
   `crates/strider/src/strider/pipeline.rs:436-454` — `RegionEdgeKind::Branch` edges are now linked at the post-loop edge walk only when `src_region.insns.is_empty()`. The non-empty case is wired by `handle_branch` per-insn (avoiding double-link → Layer-C predecessor-count regression). Bounded-lift's CondBranch-OOB collapse path (which produces empty `Branch` regions) is handled.

8. **`apply_in_place_edits` `InitialVar` arity check (W6 fix)**
   `orchestrator.rs:582-597` — reads via `node_outputs_exact::<1>`, surfaces an arity mismatch as `anyhow::Err` ("InitialVar(...) has wrong output arity (expected 1)"). Uses `preorder` (reachable-only), not `all_node_ids`, so zombie `InitialVar`s left detached by `FunctionArgDetect` don't get re-indexed and resurrected. Comment correctly explains the rationale.

9. **Production panics outside `#[cfg(test)]`**
   None observed. Every `.unwrap()` / `.expect()` in `crates/strider/src/` lives inside `#[cfg(test)]` mods or is a `.unwrap_or(_)` default. `crates/target/src/` panics are all in the test sub-mod of `call_other_abi.rs`. `crates/reader/src/elf.rs:1130` is a comment, not actual code.

### target

1. **CC presets vs Sleigh SLA tables**
   Spot-checked against rsleigh's SLA / pspec definitions:
   - `arm.sinc:0x0020` defines `r0..r12 sp lr pc` (lowercase) — matches `arm_aapcs`/`arm_linux_syscall`/`arm_linux_kernel`.
   - `AARCH64instructions.sinc` defines `x0..x30, xzr` and `q0..q15` — matches `aarch64_aapcs64` and `aarch64_linux_syscall`.
   - `mips.sinc:offset=0 size=REGSIZE` defines `zero at v0 v1 a0..a3 t0..t9 s0..s7 t8 t9 k0 k1 gp sp s8 ra` — `mips_o32` and `mips_n64` register names all resolve.
   - `ia.sinc` defines `R8..R15`, `EAX..EDI`, `RAX..RDI`, and `ST0..ST7` — matches `x86_64_systemv` (including `R10` for syscall), `x86_cdecl` (including `ST0` for x87 float return), and `x86_linux_kernel`/`x86_linux_syscall`.
   - `ppc_common.sinc` defines uppercase `LR` — matches `powerpc_sysv32` / `powerpc64_elf_v1` / `powerpc64_elf_v2`.

2. **`call_other_abi::classify` per-entry verification**
   - **`syscall` (x86_64)**: ABI is `{reads: [RAX, RDI, RSI, RDX, R10, R8, R9], writes: [RAX, RCX, R11]}`. Linux x86_64 syscall convention per `arch/x86/entry/calling.h` puts the syscall number in RAX, args 1–6 in RDI/RSI/RDX/R10/R8/R9, and the SYSCALL instruction clobbers RCX (return RIP) and R11 (rflags). **Matches.**
   - **`CallHyperVisor` / `CallSecureMonitor` (aarch64)**: `{reads: x0..x7, writes: x0..x3, memory_edge: true}` matches ARM DEN 0028E (SMC Calling Convention) §2.6.1 — x0..x7 used for input, x0..x3 for output.
   - **`swi` (ARM family)**: `{reads: [r7, r0..r6], writes: [r0], memory_edge: true}` matches the EABI Linux syscall ABI (r7 = syscall number, r0..r6 = args, r0 = retval).
   - **`rdpkru_u32` (x86)**: `{reads: [ECX], writes: [EAX, EDX], memory_edge: false}` — per Intel SDM vol 2, RDPKRU requires ECX=0, writes EAX (PKRU value) and clears EDX. **Correct.**
   - **`rdtsc` / `rdtscp` (x86)**: RDTSC writes EAX:EDX with TSC; RDTSCP additionally writes ECX (IA32_TSC_AUX). Memory edge is correctly false (TSC read doesn't observe RAM).
   - **`mfence` / `sfence` / `lfence`**: PURE_WITH_MEM_EDGE (arch-independent) — correct: x86 memory fences are ordering primitives, opt passes must not forward across them.
   - **`swapgs`**: PURE_WITH_MEM_EDGE — correct rationale (the IA32_GS_BASE swap changes the virtual base used by subsequent `%gs:`-relative accesses).
   - **`rdmsr` / `wrmsr` / `readfsbase` / `writefsbase` / `readgsbase` / `writegsbase`**: PURE for reads, PURE_WITH_MEM_EDGE for writes. Correct per Intel SDM — Sleigh emits the explicit register operands via pcode so no implicit reg channel needed.
   - **`cpuid` family**: PURE (no memory edge) — Sleigh's cpuid lift uses a tmpptr the subsequent EAX/EBX/ECX/EDX loads read from. The CallOther itself doesn't touch RAM.

3. **`BuiltCallingConvention::try_from_parts` validation + `CallingConvention::build` routing**
   `calling_convention/mod.rs:172-273` enforces every documented invariant:
   - `arg_passing_regs ∩ callee_saved_regs == ∅`
   - `ret_val_regs / ret_val_regs_float ∩ callee_saved_regs == ∅`
   - `stack_ptr_vn` not in any of the four lists
   - No duplicates within a list
   - When `link_register_vn` is `Some`, must be present in `callee_saved_regs`
   - `ret_stack_pop >= 0`

   `CallingConvention::build` (line 737-778) routes through `try_from_parts` so preset typos fail at build time.

4. **`SleighArch` presets**
   `arch.rs:159-352` exposes 15 presets covering x86 / x86_64 / arm / arm_be / arm_thumb / aarch64 / aarch64be / mipsbe32 / mipsle32 / mipsbe64 / mipsle64 / ppc32be / ppc32le / ppc64be / ppc64le. PPC64 correctly uses the `ISA_Altivec` SLA spec so Power7+ scalars decode (the stripped `PPC_64_BE` rejects `popcntd` etc.). ARM Thumb uses the same `ARM8_le` SLA as ARM with the `ARMCORTEX` pspec — correct.

5. **`BuiltCallingConventionParts` `#[non_exhaustive]` status**
   Per the brief ("attempted but deferred"): the struct is `pub` without `#[non_exhaustive]` (`calling_convention/mod.rs:127`). All fields are `pub`, so adding a new field is a breaking change for callers. Matches the stated deferred state.

### reader

1. **Per-arch reloc support**
   `elf.rs:866-942` — `image_relative_reloc`: x86_64, i386, aarch64, arm, ppc64, ppc32, mips/mips64 (all RELATIVE + IRELATIVE except MIPS which has only REL32). **MIPS REL32 is correctly 4 bytes** on both Mips32 and Mips64 per the MIPS64 ELF supplement (the "32" suffix is the field width, not the address width — comment at lines 920-935 explicitly documents this). M-3 W6 fix holds.
   `elf.rs:959-1018` — `got_or_plt_slot_reloc_size`: covers GLOB_DAT and JUMP_SLOT for all 7 arches with correct width per arch (8 for 64-bit, 4 for 32-bit; MIPS64 uses 8).

2. **`apply_elf_relocations_autoload` rollback semantics**
   `elf.rs:714-729` — Pass 2 catches the patch-loop `Err`, `regions.truncate(base_len)` restores the pre-call length, then propagates the Err. The "partial rollback" contract is clearly documented at lines 671-681 and again at the implementation site (lines 714-722). The Pass-1 extender error path leaves `regions` untouched (line 696 comment: "We never mutate `regions` here so an extender error mid-pass leaves it untouched").

3. **`RelocationStats::autoload_section_parse_failures`**
   `elf.rs:407-411` declares the field with a clear doc. `find_loadable_section_containing` (line 796-832) increments via the `&mut usize` counter — no `eprintln!` (library code, no stderr writes). Surfaced on the returned stats via `stats.autoload_section_parse_failures = parse_failures;` at line 787. The default `RelocationStats::default().autoload_section_parse_failures == 0` is pinned by `relocation_stats_default_includes_autoload_parse_failure_counter` in tests.

4. **`ElfFileMemReader` endianness on cross-endian binaries**
   `elf.rs:255-258` stores `is_little_endian: bool` from `obj.endianness()` at construction. `ReadOnlyMemory::read` (line 316-343) uses `is_little_endian` to place the read bytes in the endianness-appropriate end of an 8-byte buffer, then decodes via `from_le_bytes` / `from_be_bytes`. Cross-endian binaries (e.g. a BE PowerPC binary analysed on an LE host) get correct numeric values for a multi-byte `Load`.

## Low-confidence observations (below the 80 threshold; not reported)

The audit noticed a few items that are intentional or working as designed:

- `arm_linux_syscall` keeps `lr` in `callee_saved_regs` while `link_register_reg_name = None`. The validator only enforces "LR ∈ callee_saved when LR is Some", not the reverse. Intentional — the user-mode kernel-trap return path doesn't write `lr`, but a tail-call shim might still want to recover `InitialVar(lr)`.
- `mips_linux_syscall_o32` / `_n64` inherit `ra` in `callee_saved_regs` for the same reason.
- `apply_link_register` is *fail-loud-on-double-call*, not idempotent in the strict "second call is a no-op" sense. This is acceptable: the orchestrator's `recompute_unresolved` removes detached placeholders from `self.unresolved` so a second call shouldn't happen in practice; if it does, the typed `Err` is the right failure mode.
- `BuiltCallingConventionParts` is not `#[non_exhaustive]`. The brief flags this as "deferred" — matches observed state.

## Files reviewed

Absolute paths under the audit scope:

- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/lib.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/errors.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/rewrite.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/rewrite_tests.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/test_utils.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/indirect_resolve/{mod.rs,classify.rs,inplace.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/{mod.rs,pipeline.rs,vn_io.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/strider/insn/{mod.rs,control.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/{lib.rs,arch.rs,call_other_abi.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/src/calling_convention/{mod.rs,tests.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/target/tests/cc_validation.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/reader/src/{lib.rs,elf.rs}`
- `/mnt/c/Users/mikeg/Documents/strider/crates/reader/tests/elf_relocations.rs`
- `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/mod.rs`
- Cargo.toml for each in-scope crate

Cross-referenced against:

- `/mnt/c/Users/mikeg/Documents/rsleigh/sleigh/processors/{ARM,AARCH64,MIPS,PowerPC,x86}/data/languages/*.sinc`
- System V x86_64 ABI; AAPCS / AAPCS64; MIPS o32 / n64 ELF supplements; PPC SysV / ELFv1 / ELFv2
- Intel SDM (RDPKRU, RDTSC, RDTSCP, SWAPGS, MSR / FSBASE / GSBASE ops, memory fences)
- ARM DEN 0028E (SMC Calling Convention)
- Linux kernel `arch/x86/entry/calling.h` (x86_64 syscall ABI)
