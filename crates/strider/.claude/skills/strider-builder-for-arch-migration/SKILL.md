---
name: strider-builder-for-arch-migration
description: Migrate a `cfg::Builder::new(...)` or `cfg::Builder::with_endianness(...)` call site to `cfg::Builder::for_arch(arch, ...)` to fix the silent `preset = X86_64` default that misclassifies CallOther on non-x86_64 binaries.
---

# strider-builder-for-arch-migration

## When to use

Triggers:
- "this CFG uses `Builder::new(...)` for an ARM/AArch64/MIPS/PPC test"
- "the orchestrator's `analyze_cfg` is misclassifying `swi` / `CallHyperVisor` / etc. on a non-x86 binary"
- "round-9 R9-Ask-8 R5 C-1 / R9-1B Finding 3 — Builder::with_endianness misuse"
- a reviewer flags `Builder::new` or `Builder::with_endianness` in a non-x86 context

## When NOT to use

- The call site is genuinely x86_64-only and fixed-arch (e.g. an x86_64 byte sequence in a hand-built CFG test). Silent default is correct there. But still consider migrating for consistency.
- The arch is determined dynamically from a `target::SleighArch` parameter — `for_arch` is exactly what you want; this is the canonical case.

## The bug

`cfg::Builder::new(sleigh, base, opts)` and `cfg::Builder::with_endianness(sleigh, base, opts, endianness)` both initialise `preset: target::ArchPreset::X86_64` unconditionally. The preset is consulted in `cfg::region_builder::process_insn` at the `Opcode::CallOther` arm to dispatch through `target::call_other_abi::classify(preset, name)`. With the wrong preset, arch-specific entries like `(Arm, "swi")` (`r7/r0..r6` ABI), `(Aarch64, "CallHyperVisor")` (SMCCC), or kernel-side calls miss their arch row and either fall through to a stub or raise `UnknownCallOtherError`.

The bug is silent because:
- Tests using x86_64 byte sequences naturally hit x86_64 entries that happen to be correct.
- Many CallOthers are arch-independent (`mfence`, `sfence`, `lfence`, etc.) and route through `classify_arch_independent` regardless of preset.
- Only an arch-specific row plus the wrong preset actually misclassifies.

## Procedure

1. Identify all current usages:
   ```
   rg "Builder::with_endianness\b|Builder::new\(sleigh" crates/
   ```
2. For each match, locate a `target::SleighArch` value in scope. If not present, construct one via `SleighArch::arm()`, `SleighArch::aarch64()`, etc. — these are zero-cost factory functions.
3. Replace:
   ```rust
   // Before:
   cfg::Builder::with_endianness(sleigh, base, opts, sleigh_arch.endianness)
   // After:
   cfg::Builder::for_arch(&sleigh_arch, sleigh, base, opts)
   ```
   And:
   ```rust
   // Before:
   cfg::Builder::new(sleigh, base, opts)
   // After:
   cfg::Builder::for_arch(&target::SleighArch::arm(), sleigh, base, opts)
   ```
4. Run `cargo build --workspace` to confirm no signature mismatches.
5. Run `cargo test --workspace --exclude strider-py` and confirm no failures. (Tests that previously passed under the wrong preset may now fail or pass differently if any arch-specific CallOther is exercised — this is the bug surfacing.)

## Verification

- `rg "Builder::with_endianness\b" crates/` returns zero hits (or only references inside `cfg/builder/mod.rs` itself).
- `rg "Builder::new\(sleigh" crates/` likewise.
- `cargo test --workspace --exclude strider-py` passes.
- For non-x86 fixtures, `target::call_other_abi::classify` now sees the correct preset and dispatches the arch-specific row.

## Production vs test gotcha

`crates/strider/src/orchestrator.rs` already uses `Builder::for_arch` in production. The bug surfaces only in test files that construct CFGs manually. Round 9 Phase A migrated 8 sites: `crates/strider/tests/indirect_branch.rs:91`, `crates/cfg/tests/known_targets.rs` (×6), `crates/cfg/tests/indirect_dispatch.rs:159`. Future tests must follow the migrated pattern.

## See also

- `target::call_other_abi::classify(preset, name) -> Option<CallOtherClass>` — the dispatch site whose preset is at stake.
- `cfg::Builder::for_arch(arch, sleigh, base, opts)` — the correct constructor; reads `arch.preset` and `arch.endianness` together.
- Round 9 review: `reviews/round9-correctness-cross-arch.md` C-1, `reviews/round9-1B-pcode-lift-cfg.md` Finding 3.
