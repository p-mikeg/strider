# `target` — architecture descriptors and calling conventions

Pure target-description data. Sits below [`ir`](../ir), [`opt`](../opt), and
[`strider`](../strider) so every layer that needs ABI information names the
same types. No IR, no rsleigh state machine — just descriptors and the
`CallOther` ABI table.

## Public surface

- `ArchPreset` — `X8664`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`,
  `ArmThumb`, `Mipsbe32`, `Mipsle32`, `Mipsbe64`, `Mipsle64`.
- `Endianness` — `Little` | `Big`.
- `SleighArch` — pairs an SLA spec path + PSPEC + `Endianness`. Presets:
  `SleighArch::x86_64()`, `x86()`, `aarch64()`, `aarch64be()`, `arm()`,
  `arm_be()`, `arm_thumb()`, `mipsbe32()`, `mipsle32()`, `mipsbe64()`,
  `mipsle64()`.
- `CallingConvention` — static-string register names. Carries the stack
  pointer, integer + float return-value regs, callee-saved regs, positional
  `stack_arg_offsets`, the `ret_stack_pop` delta (8 on x86_64, 0 on AAPCS),
  and the optional link-register name (`lr` on AArch64/ARM, `ra` on MIPS,
  `None` on x86/x86_64). Presets: `x86_cdecl`, `x86_64_systemv_abi`,
  `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`.
- `CallingConvention::build(&sleigh) -> Result<BuiltCallingConvention>` —
  resolves register names to `rsleigh::Vn` varnodes.
- `BuiltCallingConvention` — the resolved version. Same fields as
  `CallingConvention` but with `Vn`s in place of static strings.
- `call_other_abi::CallOtherAbi` — describes the *implicit* (ISA-fixed,
  not pcode-explicit) channel of a CallOther beyond Sleigh's pcode
  operands. Fields: `implicit_reads: &'static [&'static str]`,
  `implicit_writes: &'static [&'static str]`, `memory_edge: bool`.
- `call_other_abi::CallOtherClass` — `NoOp` | `NoReturn` |
  `Call(CallOtherAbi)`. The classifier's verdict for a given user-op
  name.
- `call_other_abi::classify(preset, name) -> Option<CallOtherClass>` —
  single source of truth for `name → CallOtherClass`. Consulted by
  [`cfg::region_builder`](../cfg) (terminate trap regions) and
  [`strider::IrStrider::handle_call_other`](../strider) (NoOp skips
  emission, NoReturn emits a terminal CallOther via
  `ir::FunctionBuilder::build_call_other_terminal`, `Call(abi)` emits a
  precise CallOther via `ir::FunctionBuilder::build_call_other_modeled`
  with explicit register footprint and memory edge).
- `Result<T>` alias (`anyhow::Result<T>`).

## Architecture

`src/arch.rs` defines `Endianness`, `ArchPreset`, and `SleighArch` with
its constructors that pin SLA / PSPEC paths.

`src/calling_convention/` defines `CallingConvention` (compile-time
static-string register names) and `BuiltCallingConvention` (runtime
`Vn` varnodes). The split exists so a `CallingConvention` is `const`-
constructible and the `Vn` resolution happens once per analysis when the
`Sleigh` context is available. Six ABI presets cover Linux on the
strider-supported architectures.

`src/call_other_abi.rs` is the single-source-of-truth `name →
CallOtherClass` table. Unknown user-op names raise
`ir::error::UnknownCallOtherError` so the table grows incrementally with
what real lifts emit (rather than silently misclassifying).

## Key invariants

- `CallingConvention` register names are verified at `build()` time; an
  unknown name raises an error rather than silently dropping the
  register.
- `BuiltCallingConvention::callee_saved` lists the registers a callee
  must restore — these are NOT clobbered by `Call` nodes.
- `CallOtherAbi::implicit_reads` / `implicit_writes` describe registers
  that Sleigh's pcode does *not* mention but the ISA defines as part of
  the user-op (e.g. `cpuid` reads `eax` and writes `eax`/`ebx`/`ecx`/`edx`
  through Sleigh's channel, but `rdtsc` writes `edx`/`eax` *without* an
  explicit pcode varnode — that's the implicit-write set).
- `classify` returns `None` for unknown names; the IR builder turns this
  into `UnknownCallOtherError` so the table can grow incrementally.
- No `Opaque` variant in `CallOtherClass` — every previously-Opaque entry
  was reclassified to `NoOp`, `NoReturn`, or precise `Call(abi)`. See
  `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`.

## Tests

Integration tests in `crates/target/tests/` (`arch_smoke.rs`,
`linux_cc_presets.rs`). Inline tests in
`src/calling_convention/tests.rs`.

```
cargo test --package target
```

## Gotchas

- `SleighArch` paths are resolved by rsleigh at runtime via
  `Sleigh::new(arch)`. A missing SLA file becomes a runtime error, not a
  compile-time one.
- Per-call ABI overrides (e.g. a function that breaks the standard ABI)
  are applied by passing a custom `CallingConvention` at the call site,
  not by mutating presets.
- `mips_o32` and `mips_n64` differ in stack-arg offsets and integer/float
  reg sets — pick the correct one for the binary's ABI.
- Depends only on `rsleigh` and `anyhow`. No dependency on
  [`ir`](../ir), [`opt`](../opt), or [`pattern`](../pattern).
