---
name: strider-fixture-author
description: Add a real-ELF fixture under fixtures/ and wire it into per_arch_test! — covers cross-compiler matrix, source layout, lift-time canonicalisation assertions, and asm-fingerprint coverage.
---

# strider-fixture-author

## When to invoke

User wants to add a binary fixture that exercises a specific lift / IR shape across one or more architectures. Triggers include:

- "Add a fixture for `<arch>` exercising `<feature>`."
- "I need a `__fentry__`-instrumented kernel binary fixture."
- "Cover AArch64 jump-table dispatch with a fixture."
- "Add a real-ELF test that asserts `Add(_, Neg(_))` on a `sub` instruction."
- "Add a 128-bit return-value fixture."

Round 8 (`round8-17-graph-soundness.md`) flagged four explicit fixture gaps that justify this skill: no `__fentry__` instrumentation, no AArch64 jump table, no 128-bit return on AArch64 q0, no real-ELF assertion of the lift-time-canonicalised `sub → Add(_, Neg(_))` shape.

## When NOT to invoke

- The feature can be tested fully with `graphmock` and unit tests — fixture overhead unjustified.
- The fixture already exists — extend the existing test or add a new assertion against it rather than adding a parallel binary.
- The user wants a Python smoke test only and a Rust fixture with the same shape already exists — use the existing fixture from Python.

## Files this skill operates on

- `fixtures/Makefile` — top-level driver. New cases require no Makefile edit; new arches do.
- `fixtures/cases/<feature>.c` — C source, compiled for every arch in the matrix.
- `fixtures/arch/<arch>.mk` — per-arch CC + CFLAGS. Add only when introducing a new arch.
- `fixtures/arch/<arch>/<feature>.s` — hand-written assembly when C cannot express the shape (e.g. `__fentry__` instrumentation, hand-rolled jump table, LR-overwriting tail call).
- `fixtures/out/<arch>/<name>.elf` — generated; check `.gitignore` policy before committing.
- `fixtures/kernels/` — for kernel / instrumented fixtures with non-standard CCs.
- `crates/strider/tests/<feature>.rs` — Rust integration test driven by the `per_arch_test!` macro.
- `crates/strider/tests/common/mod.rs` — defines the `per_arch_test!` macro and `lift_fixture` helper.
- `crates/strider-py/tests/python/test_<feature>.py` — Python parity if the fixture is exercised cross-language.

## Procedure

1. **Check for existing similar fixtures first.** `fixtures/cases/` holds C source compiled across the full arch matrix; `fixtures/arch/<arch>/` holds hand-written assembly when C cannot express the desired shape. Existing cases (`abi.c`, `arithmetic.c`, `builtins.c`, `calling_convention.c`, `calls.c`, `complex.c`, `control.c`, `elf_relocs.c`, `floats.c`, `globals.c`, `indirect_branch.c`, `memory.c`, `patterns.c`, `stack.c`, `switch.c`) cover most basic shapes — extend one before adding a new one.

2. **Pick the cross-compiler.** The Makefile already invokes:
   - `gcc` (x86, x64, x86_kernel)
   - `aarch64-linux-gnu-gcc` (aarch64, aarch64be)
   - `arm-linux-gnueabi-gcc` (arm, arm_be, arm_thumb)
   - `mips-linux-gnu-gcc`, `mipsel-linux-gnu-gcc`, `mips64-linux-gnuabi64-gcc`
   - `powerpc-linux-gnu-gcc`, `powerpc-linux-gnu-gcc -mlittle-endian`, `powerpc64-linux-gnu-gcc`, `powerpc64le-linux-gnu-gcc`

   New arches (RISC-V, LoongArch) need a new `fixtures/arch/<arch>.mk` plus an entry in the `ARCHES` list at `fixtures/Makefile:19`.

3. **Keep the fixture minimal.** One function exhibiting one shape, ideally inline-no-stack-frame so the IR is readable. Use `__attribute__((noinline))` to prevent fold-into-`main`. The Makefile passes `-fno-optimize-sibling-calls` globally so recursive / wrapper calls survive as real `call` instructions; do not override that.

4. **Match the per-case naming convention.** `fixtures/cases/<case>.c` containing one or more `__attribute__((noinline))` C functions. Each function becomes a per-arch test entry via `per_arch_test!("<case>", "<fn_name>", <assertion_fn>)` where `<case>` matches the C-file basename and `<fn_name>` matches a function in that file.

5. **Write the test using `per_arch_test!`.** The macro lives in `crates/strider/tests/common/mod.rs` (search for `macro_rules! per_arch_test`) and expands to one `#[test]` per arch (16 today: x86, x86_kernel, x64, aarch64, aarch64be, arm, arm_be, arm_thumb, mips32le, mips32be, mips64le, mips64be, ppc32be, ppc32le, ppc64be, ppc64le). Basic form:

   ```rust
   per_arch_test!("<case>", "<fn_name>", <assertion_fn>);
   ```

   Per-arch ignores when only some arches exhibit the shape (or when an arch hits a known bug — reference an entry in `docs/superpowers/plans/2026-04-25-analyzer-known-issues.md`):

   ```rust
   per_arch_test!("<case>", "<fn_name>", <assertion_fn>, ignore = {
       Mips32le: "BUG-N: <one-line reason>",
       Mips32be: "BUG-N: <one-line reason>",
   });
   ```

6. **The lift driver `lift_fixture(arch, case, fn_name)` does the heavy lifting.** It reads `fixtures/out/<arch>/<case>.elf`, locates the symbol, builds the CFG using `Builder::for_arch(...)` (round-8 fix — both endianness and `ArchPreset` derived atomically; never use `Builder::with_endianness` here), runs `analyze_cfg`, then runs the full optimizer pipeline including `LoadReadOnly`. Your assertion fn receives the post-optimised `BuiltFunctionGraph`.

7. **Write the assertion against the *post-optimisation* IR shape, not the pre-lift asm.** This is the round-8 RT-4 lesson. If the source uses `sub`, do NOT assert `IntBinaryOp::Sub` exists in the IR — `Sub` is not an IR primitive. The lift-time canonicalisation in `pcode-lift` lowers `IntSub(a, b)` → `Add(a, IntUnaryOp::Neg(b))`. Use the pattern crate's `pattern::sub` alias which constructs the lowered `Add(_, Neg(_))` shape directly.

   Other lift-time canonicalisations to assert against:
   - `int_le(a, b)` → `BoolNeg(IntLess(b, a))` (operand swap intentional)
   - `int_sle(a, b)` → `BoolNeg(IntSless(b, a))`
   - `IntNotEqual(a, b)` → `BoolNeg(IntEqual(a, b))`
   - `FloatSub(a, b)` → `FloatAdd(a, FloatUnaryOp::Neg(b))`
   - `FloatNotEqual` / `FloatLessEqual` similar
   - `If(BoolNeg(C)){A}{B}` → `If(C){B}{A}` after `IfCondInversion`

8. **Asm-fingerprint coverage.** This is the proof-of-correctness aid. After matching a value node, call `match.asm_fingerprint(c, &graph)` and assert the resulting addresses align with the expected machine instructions in the fixture's disassembly. Disassemble with `objdump -d fixtures/out/<arch>/<case>.elf` and confirm the captured node's fingerprint contains the expected addresses. The contract is **superset-only**: passes may grow fingerprints, never shrink them.

9. **For round-8 RT-1 (LR-overwriting tail call).** AAPCS64 has a deliberate LR-as-callee-saved tradeoff (see `strider-cc-preset-extend`). Add a hand-written assembly fixture that overwrites x30 then branches to it (`mov x30, x1; br x1`) and assert the post-call value of x30 is **not** `InitialVar(x30)` — this confirms the lifter still tracks LR mutations even though the CC pretends the convention preserves it.

10. **For round-8 RT-2 / RT-3 (AArch64 jump table, 128-bit return).** Hand-roll the jump table in assembly (compilers tend to lower switches inconsistently). For 128-bit return, write a C function that returns a `__int128_t` and assert the `Return` node's value-output kind is `U128`.

11. **Build and run.** `make -C fixtures` (or `make -C fixtures ARCH=<arch> CASE=<case>` for a single combination). The build is idempotent; `cargo test` does NOT auto-build fixtures. If the ELF is missing, the `lift_fixture` helper fails with a file-not-found error.

## Verification

- `make -C fixtures` (or `make -C fixtures ARCH=<arch> CASE=<case>`) — must build clean. The Makefile uses `-Werror`, so warnings are build breaks.
- `cargo test --package strider <test_name>` (uses `per_arch_test!` expansion).
- `cargo test --package strider --test <feature>` to run only the new test file.
- `uv run pytest crates/strider-py/tests/python/test_<feature>.py` if a Python parity test exists.
- `cargo clippy --workspace -- -D warnings`.

## Exit criteria

- Fixture builds in CI on all arches in the matrix (or has explicit per-arch ignores tied to known issues).
- The `per_arch_test!` invocation exists for the new case + fn_name.
- The test asserts the *expected post-opt IR shape* (lift-time canonicalisations applied), not the source-level operator name.
- Asm-fingerprint assertion ties at least one captured node back to a known disassembly address in the fixture.
- `cargo clippy --workspace -- -D warnings` clean.

## Pitfalls

- **Asserting against pre-canonicalisation shapes.** Writing `pat = int_binary_op(IntBinaryOp::Sub, a, b)` is structurally wrong — `Sub` is not an IR primitive. Use `pattern::sub(a, b)` which constructs the lowered `Add(_, Neg(_))` shape.
- **Asserting `IfPat` against un-optimised IR.** `IfPat` matches direct layout only; `IfCondInversion` runs in the stable pipeline and is required before the assertion. The `lift_fixture` helper runs the full pipeline so this is automatic — but if you write a custom driver, run the stable pipeline first.
- **Forgetting `__attribute__((noinline))`.** GCC inlines small leaves into `main` and the lifted function-symbol disappears. The fixture passes `-fno-optimize-sibling-calls` for the same reason at the call-graph level.
- **Skipping per-arch ignores when the shape only exists on some arches.** The macro expands to 16 tests; an unconditional assertion will fail noisily on every arch where the source compiles to a different idiom. Tie ignore reasons to entries in `docs/superpowers/plans/2026-04-25-analyzer-known-issues.md`.
- **Committing generated `out/<arch>/*.elf` against gitignore policy.** Check `.gitignore` first. Some shared-library fixtures (listed in `SHARED_CASES`) are checked in; most are not.
- **Hand-rolled assembly fixture without a build rule.** `fixtures/Makefile` only auto-discovers C files in `cases/`. Assembly fixtures need explicit Makefile rules; copy from existing patterns under `fixtures/arch/<arch>/`.

## Background: the fixture matrix

The fixture build is driven by `fixtures/Makefile`. The relevant pieces:

- `ARCHES` (line 19) lists every arch the matrix covers: `x86 x86_kernel x64 aarch64 aarch64be arm arm_be arm_thumb mips32le mips32be mips64le mips64be ppc32be ppc32le ppc64be ppc64le`. Adding a new arch requires extending this list AND adding `fixtures/arch/<arch>.mk` with the cross-compiler invocation.
- `CASES` is auto-discovered from `cases/*.c` — drop a C file in and `make` picks it up. No Makefile edit needed for a new C-source fixture.
- `SHARED_CASES` (line 59) lists fixtures built as ET_DYN shared libraries (with `-shared -fPIC`); used for `elf_relocs.c` and any fixture that needs real `R_*_RELATIVE` relocations.
- `COMMON_CFLAGS` includes `-fno-optimize-sibling-calls` (preserves real `call` instructions; without this GCC turns recursive wrappers into branches and call-counting patterns silently fail — BUG-6 in `analyzer-known-issues`) and `-Werror -Wall -Wextra`.

The fixture output layout is `out/<arch>/<case>.elf`. The `lift_fixture` helper in `crates/strider/tests/common/mod.rs` reads from this layout via `reader::load_elf`.

## Edge cases worth flagging

- **Symbol-name mangling on PowerPC ELFv1.** Function symbols on PPC64 ELFv1 point at function descriptors, not entry addresses. `lift_fixture` resolves them via the `obj.symbol_by_name(...)?.address()` path which returns the entry directly on userspace builds; if you write a custom driver, account for this.
- **AArch64BE / ARMBE rare in production.** The matrix builds them anyway, but `aarch64be-linux-gnu-gcc` may not be available on every CI image. If a fixture only covers LE arches, add explicit per-arch ignores rather than silently skipping the BE variants.
- **Thumb fixtures.** ARM Thumb-2 lifts use a different rsleigh decoder mode. The `arm_thumb` arch entry sets `-mthumb` in `arch/arm_thumb.mk`; do not pass `-marm` flags in case-level CFLAGS.
- **`__attribute__((no_sanitize("kcfi")))` and similar.** Some kernel-instrumented fixtures need explicit attributes to disable kernel-CFI lowering. Look at `fixtures/kernels/` for examples.
- **Floating-point CC traps.** ARM with `-mfloat-abi=soft` puts FP results in r0/r1, not d0/d1. The CC presets list both register sets so soft-float and hard-float builds both work, but a fixture asserting on `d0` will silently miss soft-float lifts. Use the integer return regs for cross-build assertions.

## Related skills

- `strider-cli-runner` — to triage a new fixture before writing the assertion.
- `strider-pattern-author` — for writing the IR-shape pattern the assertion uses.
- `strider-debug-pattern` — when the assertion's pattern returns zero matches.
- `strider-cc-preset-extend` — when the fixture is a regression test for an LR-overwriting tail call (round-8 RT-1).
- `strider-target-arch` — when the fixture introduces a brand-new arch.
- `strider-fingerprint-audit` — for confirming asm-fingerprint coverage on the new fixture.
