---
name: strider-callother-abi
description: Add or refine a CallOther ABI entry in target::call_other_abi for an unhandled Sleigh user-op, choosing NoOp / NoReturn / Call(CallOtherAbi) and the implicit register footprint.
---

# strider-callother-abi

## When to use

User wants to extend the CallOther classification table for a Sleigh user-op the lifter currently rejects. Triggers include "add a CallOther ABI for `<opname>`", "I'm hitting `UnknownCallOtherError: <name>`", "lifting fails on `cpuid` / `rdtsc` / `<sleigh user-op>`", "this user-op should be a NoReturn / NoOp".

## When NOT to use

- The user-op is fully described by Sleigh's pcode operands and has no implicit side-effects — that is `NoOp`, not a new ABI entry, but you still classify it; just pick `NoOp`.
- The user-op is an architecture-private trap that should terminate the function — `NoReturn` is the answer; still classify it.
- The error is not `UnknownCallOtherError` but a different lift-time failure — debug separately.

## Inputs the skill expects

- The exact user-op name as Sleigh emits it (case-sensitive — pull from the failing `UnknownCallOtherError` message).
- The arch preset (`X86_64`, `Arm`, ...) — relevant only if the ABI varies by arch.
- The ISA reference for the instruction (so you know which registers it implicitly reads / writes and whether it touches memory).

## Procedure

1. Locate the table at `crates/target/src/call_other_abi.rs`. Two functions: `classify_arch_specific(preset, name)` for ABI-varies-by-arch entries (currently `swi`, `syscall`, `CallHyperVisor`, `CallSecureMonitor`); `classify_arch_independent(name)` for everything else. The public dispatch is `classify(preset, name)` which tries arch-specific first.
2. Decide the class. `NoOp` emits no IR node; control and memory are unchanged and the pcode-explicit output is discarded. `NoReturn` terminates the region with a dangling-output terminal CallOther emitted via `ir::FunctionBuilder::build_call_other_terminal` — use for trap instructions (`ud2`, `hlt`). `Call(CallOtherAbi { implicit_reads, implicit_writes, memory_edge })` is the most common case and emits via `ir::FunctionBuilder::build_call_other_modeled`.
3. Fill `CallOtherAbi` (`crates/target/src/call_other_abi.rs:12`). `implicit_reads` is the slice of register names beyond Sleigh's pcode `inputs[1..]`. Use exact Sleigh register names (case-sensitive — `RAX` on x86_64, `r0` on ARM, `x0` on AArch64). `implicit_writes` is the slice of registers the op writes / clobbers beyond pcode `output`; each becomes one extra clobber output slot. `memory_edge` is `true` for ops with observable memory effects (syscall, port I/O, cache writeback) and `false` for pure register-level ops (`cpuid`, `rdtsc`, NEON math).
4. Add a doc comment explaining the ABI source: ISA manual section, ELF ABI doc, kernel source path. Match the style of the existing `swi` / `syscall` entries.
5. Verify register names resolve on the target's Sleigh spec. `CallingConvention::build` (and `build_call_other_modeled`) will fail if a name does not resolve. Cross-check against `rsleigh::sla_spec::SLA_SPEC_<arch>`'s register table by lifting one instruction and inspecting its `Vn` table.
6. Test. Add a unit test in `crates/target/src/call_other_abi.rs::tests` (or the sibling test module) asserting `classify(preset, "<name>") == Some(CallOtherClass::Call(abi))`. Add an integration test against a fixture that emits the user-op (build with `fixtures/Makefile`).

## Verification

- `cargo test --package target call_other`.
- `cargo test --package strider <fixture_test>` — the failing fixture should now lift cleanly with no `UnknownCallOtherError`.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- `classify(preset, "<name>")` returns the right class (`NoOp`, `NoReturn`, or `Call(abi)` with the correct register footprint).
- The originally-failing `UnknownCallOtherError` is gone for all affected fixtures.
- A unit test pins the entry so accidental deletion regresses.
- `validate` passes on the lifted IR for the fixture.

## Pitfalls

- Register-name capitalisation differs by arch: x86_64 uses `RAX`, AArch64 uses `x0`, ARM uses `r0`. The strict-on-emission policy converts a typo into a build break, but the failure mode is opaque.
- `memory_edge: false` is wrong for any I/O or syscall-like op. Subsequent loads will commute through it incorrectly.
- Putting an arch-varying entry in `classify_arch_independent` causes collisions across presets (e.g. `swi` is ARM-only).
- Picking `NoOp` to avoid filling out the ABI silently drops side effects. If the op writes a register and you say `NoOp`, downstream patterns over that register are wrong.
- Forgetting to update the test snapshot when adding a new entry — `cargo test --package target` should pin the public table.

## Related skills

- `strider-target-arch` — when a new arch needs both a `SleighArch` and arch-specific user-op classifications.
- `strider-fingerprint-audit` — when adding `Call(abi)` entries that affect IR shape; the fingerprint must propagate from the originating insn.
