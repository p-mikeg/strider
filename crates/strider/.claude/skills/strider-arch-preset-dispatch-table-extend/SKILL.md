---
name: strider-arch-preset-dispatch-table-extend
description: Register a new (ArchPreset, user-op-name) entry in target::call_other_abi::classify_arch_specific when a new arch's user-ops surface as UnknownCallOtherError.
---

# strider-arch-preset-dispatch-table-extend

## When to use

User just added a new arch (or is exercising an existing arch on a
fixture that emits a Sleigh user-op the table doesn't yet cover) and
needs to register CallOther dispatch per (`ArchPreset`, name) pair.
Triggers include:

- "Add CallOther dispatch for arch X."
- `UnknownCallOtherError` for a new arch's user-op (e.g. RISC-V `fence`,
  LoongArch `dbar`, MIPS `wait`).
- "The classify table fires for x86 `cpuid` but not for the AArch64
  equivalent."
- A new arch's lift fails on the *first* fence / barrier / hint
  instruction.

## When NOT to use

- The user-op is arch-independent (same name, same ABI on every preset)
  — that goes in `classify_arch_independent`, not the per-preset arm.
  See `strider-callother-abi` for the choice.
- The arch itself is missing — register the arch first via
  `strider-target-arch`, then come back here for its user-ops.
- The user-op is fully specified by Sleigh's pcode operands and has no
  implicit side-effects — pick `NoOp` via `strider-callother-abi`; you
  still classify it, but the per-preset dispatch arm is not the right
  surface (the choice is the same on every arch).

## Inputs the skill expects

- The exact user-op name as Sleigh emits it (case-sensitive).
- The `ArchPreset` variant (`MipsBe32`, `Aarch64`, ...).
- The ISA reference describing the implicit register footprint and
  whether the op observes memory.

## Procedure

1. Locate `classify_arch_specific` at
   `crates/target/src/call_other_abi.rs:76`. The function is a `match`
   over `(preset, name)` tuples returning `Option<CallOtherClass>`.
   Existing entries cluster by preset family (`X86 | X86_64`, ARM `swi`
   variants, AArch64 `CallSecureMonitor`, …).
2. Pick the class — `NoOp` / `NoReturn` / `Call(CallOtherAbi {...})` —
   following the decision tree in `strider-callother-abi`. The
   per-preset dispatch is just for *arch-varying* entries; the class
   choice mechanics are identical.
3. Append the new arm. Use the preset variant exactly (`crate::ArchPreset::Aarch64`
   not `Aarch64`). For shape-shared entries (`X86 | X86_64`), reuse the
   existing OR-pattern style. Keep entries clustered by preset family
   so the table reads top-to-bottom by ISA.
4. Cross-check register names against the arch's Sleigh register table.
   AAarch64 uses `x0`/`x30`, ARM uses `r0`/`lr`, RISC-V uses `x0`/`ra`,
   MIPS uses `a0`/`ra`. A typo turns into a build break the next time a
   fixture exercises the entry, so verify by lifting one instruction
   first if uncertain.
5. Add a unit test in the same file's `tests` module asserting
   `classify(preset, "<name>")` returns the expected class with the
   expected register footprint. Mirror the style of the existing
   `swi_arm_*` / `syscall_*` tests.
6. If a real fixture exercises the user-op, add an integration test in
   `crates/strider/tests/<feature>.rs` confirming the lift succeeds
   with no `UnknownCallOtherError` on the new preset.

## Verification

- `cargo test --package target call_other` — unit tests for the table.
- `cargo test --package strider <fixture>` — confirms the originally
  failing lift now passes on the new preset.
- `cargo clippy --workspace --all-targets -- -D warnings`.

## Exit criteria

- `classify(preset, "<name>")` returns the right class for the new
  (preset, name) pair.
- The originally-failing `UnknownCallOtherError` is gone for fixtures
  that lift on the new preset.
- A unit test pins the entry; deletion regresses.
- `validate` passes on the lifted IR for the fixture.

## Pitfalls

- **Putting an arch-varying entry in `classify_arch_independent`.** The
  arch-independent table fires on every preset, so a misplaced entry
  silently mis-classifies the user-op on unrelated arches. The dispatch
  is layered: arch-specific is consulted first, falling through to
  arch-independent only on `None`.
- **Wrong preset variant.** `MipsBe32` vs `MipsLe32` matter — they're
  distinct `ArchPreset` variants. The fixture build matrix uses both.
- **Forgetting to set `memory_edge` on barrier / fence / I/O ops.**
  Loads must not commute through them; `memory_edge: false` lets opt
  passes hoist subsequent reads incorrectly.
- **Wrong register-name capitalisation.** Sleigh's register tables are
  case-sensitive. AArch64 uses lowercase `x0`; x86_64 uses uppercase
  `RAX`. The orchestrator-side error is opaque (build failure when the
  CC builds), so verify against `rsleigh::sla_spec::SLA_SPEC_<arch>`.

## Related skills

- `strider-callother-abi` — the canonical decision tree for class /
  ABI choice (NoOp vs NoReturn vs Call). This skill is the per-preset
  dispatch sub-case.
- `strider-target-arch` — when the arch itself is new and needs both a
  `SleighArch` preset and per-op dispatch entries.
- `strider-fingerprint-audit` — when adding `Call(abi)` entries that
  affect IR shape; the fingerprint must propagate from the originating
  insn.
- `strider-fixture-author` — when the new entry needs a real-ELF
  regression test.
