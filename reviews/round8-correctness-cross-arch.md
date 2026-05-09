# Round 8 / Ask 18-5 — Cross-arch consistency audit

**Branch:** `review/ai2`.  Independent audit.

## Per-arch verification matrix

| Arch | Lift canonicalisation | Delay slot | FlagCmpCanonicalize | Endianness | CC offsets | CallOther preset | LR classification | Fingerprint attribution |
|------|----|----|----|----|----|----|----|----|
| x86 | PASS | n/a | direct IntCmpOp | PASS (LE) | PASS | PASS | n/a | PASS |
| x86_64 | PASS | n/a | direct IntCmpOp | PASS (LE) | PASS | PASS | n/a | PASS |
| AArch64 LE/BE | PASS | n/a | PASS (7 rules) | PASS | PASS (AAPCS64) | **FAIL** (gets X86_64) | PASS (x30) | PASS |
| ARM LE/BE/Thumb | PASS | n/a | PASS (rules 8-9) | PASS | PASS (AAPCS) | **FAIL** (gets X86_64) | PASS (lr) | PASS |
| MIPS BE/LE 32/64 | PASS | PASS (delayslot) | n/a | PASS | PASS (O32/N64) | **FAIL** (gets X86_64) | PASS (ra) | PASS |
| PPC 32 BE/LE | PASS | n/a | **GAP (no CR rules)** | PASS | PASS (SYSV32) | **FAIL** (gets X86_64) | PASS (LR) | PASS |
| PPC 64 BE/LE | PASS | n/a | **GAP (no CR rules)** | PASS | PASS (ELFv1/v2) | **FAIL** (gets X86_64) | PASS (LR) | PASS |

## CRITICAL findings

### 1. `strider::run` passes wrong `ArchPreset` to CFG Builder for all non-x86_64 architectures

- **Confidence:** 97.
- **Severity:** HIGH.
- **Where:** `crates/strider/src/orchestrator.rs:826`.
- **What's wrong:** `strider::run` constructs the CFG builder via:
  ```rust
  Builder::with_endianness(sleigh, opts.start_addr, cfg_opts, arch_endianness)
  ```
  `Builder::with_endianness` hardcodes `preset: target::ArchPreset::X86_64` (`crates/cfg/src/cfg/builder/mod.rs:113`).  No subsequent `.with_preset(arch.preset)` call.

  The CFG region builder reads `self.builder.preset` when classifying CallOther:
  ```rust
  // crates/cfg/src/cfg/builder/region_builder.rs:409
  let preset = self.builder.preset;
  let class = name.and_then(|n| target::call_other_abi::classify(preset, n));
  ```

  `for_arch` exists (`builder/mod.rs:129`) and atomically sets both endianness and preset.  Documented as preferred constructor.  The orchestrator never uses it.
- **Per-arch impact:**
  - **ARM/ArmBE/ArmThumb**: `"swi"` looked up with `X86_64` preset → matches x86 stub (`implicit_reads:[], implicit_writes:[]`) instead of ARM arm (`r7`, `r0..r6` reads; `r0` write).  Syscall register channel silently lost.  Pattern queries for ARM syscall args return empty.
  - **AArch64/AArch64Be**: `"CallHyperVisor"` / `"CallSecureMonitor"` not in `X86_64` arch-specific table and not arch-independent → `classify` returns `None` → `UnknownCallOtherError`.  Any AArch64 binary with HVC/SMC fails to lift.
  - **MIPS/PowerPC**: No arch-specific entries today, so no immediate error — but future additions will be silently unreachable via `strider::run`.
- **Fix:** Replace `Builder::with_endianness(..., arch_endianness)` with `Builder::for_arch(opts.strider.arch(), ...)` at `orchestrator.rs:826`.  Same one-line fix needed at:
  - `crates/strider/tests/common/mod.rs:215`
  - `crates/strider/benches/scaling.rs:89`

## IMPORTANT findings

### 2. `FlagCmpCanonicalize` has no rules for PowerPC CR-based conditional branches

- **Confidence:** 80.
- **Severity:** MED.
- **Where:** `crates/opt/src/flag_cmp_canonicalize/mod.rs`.
- **What's wrong:** Pass contains 9 rules targeting AArch64 and ARM Thumb flag-tree shapes.  PowerPC `cmp`/`cmpi` writes individual bits of CR fields (`LT`, `GT`, `EQ`, `SO` in CR0–CR7).  If Sleigh's PowerPC spec lifts `bc` (branch conditional) reading raw CR bit nodes — equivalent to AArch64's flag-tree pattern — no canonicalisation rule reduces these to direct `IntCmpOp` nodes.  The jump-table bound walker in `opt::indirect_branch_resolve` requires canonical `IntCmpOp` to compute table size, so PowerPC switch-style indirect dispatches may fail to resolve, surfacing as `UnresolvedIndirectBranchError`.
- **Mitigation note:** Rated MED (not HIGH) because it depends on Sleigh's PowerPC `.sla` lift shape, which was not directly verified at audit time.  Module comment lists only AArch64/ARM as targets — may be a known-but-undocumented gap.  Investigation of the PowerPC Sleigh spec needed to confirm.

## PASS findings (verified correct)

- **Lift-time canonicalisations** (8 lowerings) — implemented arch-agnostically in `pcode-lift/src/value/{arithmetic,float}.rs`.  No arch-conditional paths.  Uniform across 15 presets.
- **MIPS delay slot** — Sleigh's `delayslot(1)` directive inlines delay-slot pcode into the branch's `LiftRes`; `RegionBuilder::next_pcode_addr` advances by `lift_res.machine_insn_len` covering both insns.  `inst_next` is the post-delay address per MIPS convention.
- **Endianness** — three independent paths verified:
  - `pcode-lift/src/vn_io.rs::calculate_reg_shift_from_container` — LE shift = `8*(reg.addr_off - container.addr_off)`; BE shift = `8*(container.size - reg.size - (reg.addr_off - container.addr_off))`.  Correct for all aliased register families.
  - `opt/src/stack_load_forward/mod.rs` `ResolveShape::Narrow` BE path uses `Truncate(ShiftRight(data, (store_size-load_size)*8))` — correct high-byte extraction.
  - `reader/src/elf.rs::ReadOnlyMemory::read` places bytes at LE-low or BE-high end before `from_le_bytes`/`from_be_bytes`.
- **CC stack_arg_offsets** — all 9 presets verified against ABI docs: x86 cdecl=4, x86_64=8, AAPCS/AAPCS64=0, MIPS O32=16, MIPS N64=0, PPC SYSV32=8, PPC ELFv1=48, PPC ELFv2=32.
- **Arch-independent CallOther invariant** — entries' `implicit_reads`/`implicit_writes` are empty (tested).  Arch-specific table content correct for ARM/x86 `swi`, x86_64 `syscall`, AArch64 SMCCC entries.  Bug is in delivery, not table.
- **Link-register classification** — `classify_anchor_with_rom_and_sp` matches `NodeKind::InitialVar(vn) if Some(vn) == link_register_vn`.  Per-arch LR vns: AArch64 x30, ARM lr, MIPS ra, PPC LR.  x86/x86_64 correctly have `link_register_vn = None`.  ARM Thumb `& 0xFFFFFFFE` interworking mask handled.
- **Fingerprint attribution funnel** — `set_lift_addr`/`clear_lift_addr` pair in `process_insn` (`crates/strider/src/strider/insn/mod.rs:46-48`) wraps every pcode insn's IR emission with the machine address.  Arch-agnostic — same path for all 15 presets.

## Summary

- **1 HIGH (conf 97)** — orchestrator passes wrong ArchPreset; non-x86_64 CallOther classification broken via `strider::run`.
- **1 MED (conf 80)** — PowerPC CR canonicalisation gap (pending Sleigh-spec verification).
- **8 PASS** — lift canonicalisations, MIPS delay slot, endianness, CC offsets, CallOther table content, LR classification, fingerprint attribution all arch-uniformly correct.
