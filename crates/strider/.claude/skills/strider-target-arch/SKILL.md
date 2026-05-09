---
name: strider-target-arch
description: Add a new SleighArch and CallingConvention preset to the strider target crate, including ArchPreset variant, Python parity, and a smoke fixture.
---

# strider-target-arch

## When to use

User wants to add support for a new architecture or a new CC preset on an existing arch. Triggers include "add support for RISC-V", "add an `arm_thumb_be` preset", "register a new SleighArch and CallingConvention", "I need to lift `<arch>` binaries".

## When NOT to use

- The arch is already supported and the user wants only a new calling convention on it — that is a CC-only addition (still in `crates/target/src/calling_convention/mod.rs`, but skip the `SleighArch` step and `ArchPreset` variant).
- The user is modifying register-aliasing logic for an existing arch — that lives in `crates/pcode-lift/src/vn_io.rs`.
- The user wants to add a CallOther entry only — route to `strider-callother-abi`.

## Inputs the skill expects

- The Sleigh `.sla` and `.pspec` names from `../rsleigh/src/sla_spec.rs` and `pspec.rs` (look for the `SLA_SPEC_*` / `PSPEC_*` constants).
- ABI documentation: arg-passing regs, return-value regs, callee-saved regs, stack-arg layout, return-stack-pop delta, link-register name.
- Endianness.

## Procedure

1. Add an `ArchPreset` variant in `crates/target/src/arch.rs::ArchPreset`. Keep the granularity per-preset (BE / LE / Thumb each get their own variant).
2. Add a constructor in `crates/target/src/arch.rs::SleighArch` (e.g. `SleighArch::riscv64()`). Wire `sla_spec`, `pspec`, `endianness`, `preset`. Existing presets to mirror: `x86_64`, `x86`, `aarch64`, `aarch64be`, `arm`, `arm_be`, `arm_thumb`, `mipsbe32`, `mipsle32`, `mipsbe64`, `mipsle64`, `ppc32be`.
3. Add a CC preset in `crates/target/src/calling_convention/mod.rs`. Mirror the structure of `aarch64_aapcs64` (line 249), `arm_aapcs` (line 289), `mips_o32` (line 330), `mips_n64` (line 364), `x86_cdecl` (line 493), or `x86_64_systemv` (line 178). Fill: stack-pointer reg name; integer + float arg regs (positional); integer + float return-value regs; callee-saved regs; `stack_arg_offsets` (positional offsets for stack-passed args); `ret_stack_pop` (`0` for callee-cleanup AAPCS, non-zero for caller-cleanup cdecl which pops 4/8); link-register varnode name (`Some("ra")`, `Some("lr")`, or `None`).
4. Verify register names against the Sleigh spec. `CallingConvention::build` will fail if a name doesn't resolve. Cross-check by lifting one instruction with rsleigh and inspecting its `Vn` table.
5. Add CallOther entries for arch-specific user-ops via the `strider-callother-abi` skill — e.g. RISC-V's `ECALL` will need a syscall ABI entry in `classify_arch_specific`.
6. Register-aliasing. If the arch has overlapping registers strider doesn't yet handle (something other than the documented widths 1, 2, 4, 8, 10, 16 bytes), extend `crates/pcode-lift/src/vn_io.rs::vn_mask` and `find_largest_fitting_register`.
7. Python parity. Add a `SleighArch` factory in `crates/strider-py/src/arch.rs` and a CC factory in `crates/strider-py/src/cc.rs`. Mirror the Rust constructor names and parameters exactly.
8. Fixture. Build a small `hello` binary in `fixtures/Makefile` for the new arch (output under `fixtures/out/<arch>/`), plus a smoke test in `crates/strider-py/tests/python/test_arch.py`.

## Verification

- `cargo test --package target` (CC builds, register-name resolution, ArchPreset enum coverage).
- `cargo test --package strider` (full pipeline on a fixture, if one is built).
- `uv run pytest crates/strider-py/tests/python/test_arch.py crates/strider-py/tests/python/test_smoke.py`.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- `SleighArch::<arch>()` and the new `CallingConvention::<cc>()` build without error.
- Lifting the new fixture ELF produces a valid `BuiltFunctionGraph` (`validate` passes).
- At least one Python smoke test exercises the new arch.
- All arch-specific user-ops emitted by the fixture have CallOther entries (no `UnknownCallOtherError`).

## Pitfalls

- Sleigh register names are case-sensitive and arch-specific. `RAX` is not `rax` is not `eax`. A typo becomes a build break only at first lift.
- Forgetting `ret_stack_pop`. Wrong stack-frame size silently breaks `CallStackArgCollect`.
- Missing `lr` / link-register Vn breaks `LinkRegister` indirect-branch resolution.
- Not updating Python. The test surface still uses the CC, so the `MemoryMap` / `Strider` constructors need a Python factory or downstream tests fail.
- Forgetting to add an `ArchPreset` variant means `call_other_abi::classify_arch_specific` cannot dispatch ABI variants for the new arch.
- Mismatch between the BE / LE flag in `SleighArch` and the bytes the assembler emits — surface in `StackLoadForward`'s endianness-aware partial-overlap reads.

## Related skills

- `strider-callother-abi` — invoked for every arch-specific user-op the fixture exercises.
- `strider-py-binding` — for the `arch.rs` / `cc.rs` Python wrappers.
- `strider-fingerprint-audit` — confirms the new arch's lifter produces fingerprints on every reachable non-exempt node.
