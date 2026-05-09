---
name: strider-cc-preset-extend
description: Add a new CallingConvention preset for an already-supported SleighArch — covers the LR-as-callee-saved tradeoff, ret_stack_pop traps, no_memory_clobber, and Python parity.
---

# strider-cc-preset-extend

## When to invoke

User wants to add or audit a CallingConvention preset on an arch that already has a `SleighArch` constructor. Triggers include:

- "Add a CC preset for `<existing arch>`."
- "Add `mips_n32` / `riscv_lp64d` / `<new ABI>`."
- "AAPCS64 says x30 (LR) is caller-saved — why does strider list it under `callee_saved_regs`?"
- "Document the LR-as-callee-saved tradeoff."
- "Add a zero-side-effect CC for an instrumentation hook (`__fentry__`-style)."

## When NOT to invoke

- Adding a brand-new arch (new `SleighArch` constructor + `ArchPreset` variant) → use `strider-target-arch` (this skill is its CC-only sibling).
- Modifying CallOther classification for an arch (`mfence`, `swi`, etc.) → use `strider-callother-abi`.
- Modifying register-aliasing (sub-register reads) → that lives in `crates/pcode-lift/src/vn_io.rs`.

## Files this skill operates on

- `crates/target/src/calling_convention/mod.rs` — preset definitions. Each preset is a single `pub fn <name>() -> CallingConvention { ... }` block.
- `crates/target/src/calling_convention/tests.rs` — preset-build assertions and register-name resolution checks.
- `crates/strider-py/src/cc.rs` — Python factory mirror.
- `crates/target/src/call_other_abi.rs` — only if the new CC implies a CallOther classification change (e.g. a fence/barrier user-op specific to the ABI).
- A regression fixture under `fixtures/cases/` if the CC change has observable behaviour (e.g. LR-overwriting tail-call shim).

## Procedure

1. **Pick the closest existing preset and copy its block.** The preset table in `crates/target/src/calling_convention/mod.rs` is alphabetised by family. Closest mirrors:
   - x86 family: `x86_cdecl` (line 590ish), `x86_64_systemv` (line 278ish), `x86_64_all_preserving` (line 321ish — the zero-side-effect variant).
   - AArch64 / ARM: `aarch64_aapcs64` (line 359), `arm_aapcs` (line 399).
   - MIPS: `mips_o32` (line 440), `mips_n64` (line 460ish).
   - PPC: `powerpc_sysv32` (line 501), `powerpc64_elf_v1` (line 538), `powerpc64_elf_v2` (line 572).

2. **Preserve field order in the struct literal.** The canonical order is: `stack_ptr_reg_name`, `arg_passing_regs`, `callee_saved_regs`, `ret_val_regs`, `ret_val_regs_float`, `stack_arg_offsets`, `ret_stack_pop`, `link_register_reg_name`, `syscall_number_reg_name`, `no_memory_clobber`. Out-of-order fields compile but make audits harder.

3. **Note the renamed `x86_64_systemv` preset.** Round 8 renamed `x86_64_systemv_abi` → `x86_64_systemv` (drops the `_abi` suffix for naming consistency with other presets). The old name is retained as a `#[deprecated]` shim that delegates to the new one. New presets should follow the bare-name convention (no `_abi` suffix). Do NOT use `x86_64_systemv_abi` in new code.

4. **Link-register-as-callee-saved tradeoff (round-8 finding B-1/B-2/B-3).** This is the single most important call-out for AAPCS64, AAPCS, PPC SysV, PPC ELFv1, and PPC ELFv2. The ABI specs say LR is **caller-saved** (the caller is responsible for preserving it across calls), but strider intentionally lists it under `callee_saved_regs` so:

   - `InitialVar(LR)` propagates through call sites unchanged, and
   - `IndirectBranchResolve`'s `LinkRegister` arm can recognise the canonical return shape `BranchIndirect(InitialVar(LR))` and lower it to a `Return`.

   If LR were honestly modelled as caller-clobbered, every `BranchIndirect` consumer would see an opaque post-call value and indirect-branch resolution would fail on every function with a callee. The tradeoff is **silent** in user-visible behaviour (good) but **deliberate** in code (must be commented). Concretely:

   - `aarch64_aapcs64::callee_saved_regs` includes `"x30"` (= `lr`).
   - `arm_aapcs::callee_saved_regs` includes `"lr"`.
   - `powerpc_sysv32::callee_saved_regs`, `powerpc64_elf_v1`, `powerpc64_elf_v2` all include `"lr"`.

   When adding a new preset on these arches, document the deviation in a comment at the preset definition. If your CC genuinely needs LR honestly modelled (e.g. a tail-call-only shim ABI where the caller has already saved LR and indirect-branch return resolution is not needed), you must add a `link_register_preserved_by_convention: bool` field to `CallingConvention` (default `true`), set `false` for the new preset, and thread it through `pcode_lift::ValueLifter::clobbered_outputs` so the lifter clobbers LR at call sites. This is a wider change — flag it explicitly to the user.

5. **`ret_stack_pop` traps.** This is the number of bytes the callee pops off the stack on return:
   - `8` on x86_64 SysV (callee pops the return address — actually the call instruction implicitly pushes 8, the `ret` pops 8; the value here is the offset adjustment for `CallStackArgCollect`).
   - `4` on x86 cdecl.
   - `0` on every link-register architecture (AArch64, ARM, MIPS, PPC) because the return address is in a register, not on the stack.

   A wrong value silently breaks `CallStackArgCollect` (positional stack args are read from the wrong offsets relative to `sp` after the call).

6. **`stack_arg_offsets` traps.** This is the *positional* offset table for stack-passed arguments. MIPS O32 reserves the first 16 bytes as "shadow space" for the four register args, so its positional table starts at offset 16: `&[16, 20, 24, 28]`. AAPCS64 does not reserve shadow space, so it starts at 0: `&[0, 8, 16, 24]`. Get this wrong and stack-passed args are read from inside the caller's frame.

7. **`no_memory_clobber: true` for zero-side-effect ABIs.** Set on `x86_64_all_preserving` (the `__fentry__` / `mcount` instrumentation hook ABI) so `Call` nodes do NOT advance the memory chain. Without it, every instrumented prologue would invalidate `StackLoadForward` across the call. If your new CC is a true zero-side-effect hook, set this to `true` and document why.

8. **Verify register names against rsleigh.** `CallingConvention::build` resolves names against `rsleigh::Sleigh::regs()` at runtime, NOT at compile time. A typo (`"X30"` instead of `"x30"`, `"r14"` on AArch64 where Sleigh expects `"x30"` directly) will fail at first lift. Cross-check by lifting one instruction with the target arch and inspecting the `Vn` table in the `Sleigh` regs map. Sleigh names are case-sensitive and arch-specific.

9. **CallOther implications.** If the new CC implies new memory-fence or barrier semantics (e.g. `mfence`/`sfence`/`lfence` on x86_64, `DataMemoryBarrier` on AArch64), confirm `target::call_other_abi::classify` already covers them. Round-8 finding D-1 (regression at `call_other_abi.rs:822`) verifies x86 `mfence`/`sfence`/`lfence` are classified as `PURE_WITH_MEM_EDGE` — i.e. `Call(CallOtherAbi { implicit_reads: &[], implicit_writes: &[], memory_edge: true })`. Without `memory_edge: true`, `StackLoadForward` would forward across the fence, breaking the barrier. New ABIs that introduce new barrier opcodes need entries in `classify_arch_specific`.

10. **Python parity.** Add a CC factory in `crates/strider-py/src/cc.rs` mirroring the Rust constructor name and (lack of) parameters exactly. The `MemoryMap` / `Strider` / `strider.run` Python surface routes through `CallingConvention::build`, so missing parity means the new CC is silently absent from Python tests.

## Verification

- `cargo test --package target` (CC build + name resolution + struct-literal assertions).
- `cargo test --package strider <fixture-name>` for any fixture that exercises the new CC. If LR semantics changed, lift a tail-call shim that overwrites the link register (e.g. AAPCS64 `mov x30, x1; br x1`) and assert post-call `x30` is **not** `InitialVar(x30)` — this is the round-8 RT-1 regression test.
- `cargo test --package opt` (confirm `StackLoadForward` and `CallStackArgCollect` still work on the new CC).
- `uv run pytest crates/strider-py/tests/python/test_arch.py crates/strider-py/tests/python/test_smoke.py`.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- The new preset compiles, builds without name-resolution errors, and runs validate-clean on at least one fixture.
- LR-as-callee-saved tradeoff is documented in a code comment if the arch has a link register.
- `ret_stack_pop` is `0` on link-register arches, non-zero only on x86 family.
- `stack_arg_offsets` accounts for any reserved shadow space.
- Python factory exists and matches Rust by name.
- `no_memory_clobber: true` is set if and only if the ABI is a true zero-side-effect hook.

## Pitfalls

- **Listing LR as caller-saved (the spec-correct choice) breaks indirect-branch return resolution.** Round 8 confirmed this is a deliberate strider-wide tradeoff on AArch64/ARM/PPC. Do NOT "fix" this without also threading a new `link_register_preserved_by_convention` knob through `pcode_lift::ValueLifter::clobbered_outputs` and updating every test that asserts LR survives a call.
- **Wrong `ret_stack_pop` silently corrupts `CallStackArgCollect`.** It's `0` on every link-register arch. Copy from the closest existing preset.
- **Sleigh register name typos fail at runtime, not compile time.** AArch64 needs `"x30"` (not `"X30"`, not `"lr"`); ARM needs lowercase `"lr"`; MIPS needs `"ra"` (not `"r31"`).
- **Forgetting `no_memory_clobber: true` on a zero-side-effect hook.** `__fentry__`-style instrumentation breaks `StackLoadForward` on every caller without it.
- **Using `x86_64_systemv_abi` in new code.** The name was renamed to `x86_64_systemv` in round 8; the `_abi` form is `#[deprecated]` and only exists for back-compat.
- **Skipping the Python mirror.** Cross-arch tests in `strider-py` won't cover the new CC if there's no Python factory.

## Worked example: adding `mips_n32`

`mips_n32` is the 32-bit-pointer / 64-bit-register MIPS ABI (uncommon but real). Steps:

1. Locate the closest existing preset: `mips_n64` at `crates/target/src/calling_convention/mod.rs` ~line 460. N32 shares the 8-arg-reg surface (`$4`-`$11`) with N64 but uses 32-bit pointers and 32-bit-aligned stack args.
2. Copy the block, rename to `mips_n32`. Adjust:
   - `arg_passing_regs`: `&["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7"]` (or the N32 alias names — check rsleigh's MIPS spec).
   - `stack_arg_offsets`: 4-byte stride for 32-bit args. N64 uses 8-byte stride.
   - `ret_stack_pop`: `0` (link-register arch).
   - `link_register_reg_name`: `Some("ra")`.
   - `callee_saved_regs`: same as O32/N64 (`s0..s8`, `gp`, `ra`).
3. Document the LR-as-callee-saved tradeoff with the standard comment.
4. Add the Python factory in `crates/strider-py/src/cc.rs`.
5. Add a unit test in `crates/target/src/calling_convention/tests.rs` that calls `mips_n32().build(&sleigh.regs())` and asserts no name-resolution error.
6. If a fixture exists for N32 (it would need a `mips-linux-gnuabin32-gcc` cross-compiler), add it via `strider-fixture-author`.

## Background: how the CC plumbs through strider

A `CallingConvention` is the static description; `BuiltCallingConvention` is the resolved form (varnode-bound). `CallingConvention::build(sleigh_regs)` resolves every register name against the live Sleigh register table and returns a `BuiltCallingConvention` carrying:

- `stack_ptr_vn`, `arg_passing_vns` (integer + float positionals), `ret_val_vns` (integer + float), `callee_saved_vns`, `link_register_vn` (`Option<rsleigh::Vn>`), `stack_arg_offsets`, `ret_stack_pop`.

Downstream consumers:

- `pcode_lift::ValueLifter::clobbered_outputs` (in `crates/pcode-lift/src/`) — uses `callee_saved_vns` to decide which registers survive a `Call`. Anything not in the set is invalidated to `InitialVar(...)` post-call. This is where the LR-as-callee-saved tradeoff lives in practice — listing LR here means the lifter won't clobber it post-call.
- `opt::CallStackArgCollect` — uses `stack_arg_offsets` and `ret_stack_pop` to reconstruct positional stack arguments at call sites.
- `opt::FunctionArgDetect` — uses `arg_passing_vns` (and stack offsets) to canonicalise argument reads at the function boundary into `FunctionArg` nodes.
- `cfg::Builder::with_link_register` — uses `link_register_vn` to mark `BranchIndirect(InitialVar(LR))` as a `Return` candidate during CFG construction.
- `target::call_other_abi::classify_arch_specific` — uses `syscall_number_reg_name` to locate the syscall number on user-op `syscall` / `swi` / `sc` opcodes.

A typo in any of these names becomes a runtime error at first lift. Always lift one fixture function with the new CC before considering it done.

## Edge cases worth flagging

- **AAPCS64's x30 alias.** Sleigh registers AArch64 LR as `"x30"` (NOT `"lr"`). `aarch64_aapcs64::link_register_reg_name` is `Some("x30")` and `callee_saved_regs` includes `"x30"`. ARM, by contrast, registers it as lowercase `"lr"`. MIPS uses `"ra"`.
- **PPC64 ELFv1 vs ELFv2.** ELFv1 uses a function descriptor (TOC pointer + entry); ELFv2 is direct-entry. The CC presets differ in how `r2` (TOC) is treated — preserved-by-callee on ELFv1, scratch on ELFv2. Get this wrong and any TOC-using indirect call breaks.
- **MIPS shadow space.** O32 reserves 16 bytes; N64 does not. Mixing the two breaks `CallStackArgCollect` silently — positional args land at the wrong offsets.
- **Linux kernel CCs.** `x86_64_linux_kernel`, `aarch64_linux_kernel`, etc. exist because kernel code uses internal ABIs that differ from userspace (e.g. extra clobbers, no red zone). Don't reuse a userspace CC for kernel binaries even if the arch matches.

## Related skills

- `strider-target-arch` — when adding a new arch (CC + SleighArch + ArchPreset together).
- `strider-callother-abi` — when the new CC implies new user-op classification (fences, barriers, syscall-number register).
- `strider-fixture-author` — for the LR-overwriting regression fixture (RT-1).
- `strider-py-binding` — for the Python `cc.rs` mirror.
