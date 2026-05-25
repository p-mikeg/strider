# `strider-target` — architecture descriptors and calling conventions

Pure target-description data.  No IR, no rsleigh state machine — just
descriptors, the calling-convention DSL, the `PositionalArgLayout` DTO
consumed by stack-arg passes, and the `CallOther` ABI classification
table.  Lives below `strider-lift` and `strider-analyze` in the
workspace dependency graph; every layer that needs ABI information
names the same types.

## Public surface

- `ArchPreset` — closed enum covering every supported arch family:
  `X86_64`, `X86`, `Aarch64`, `Aarch64Be`, `Arm`, `ArmBe`, `ArmThumb`,
  `MipsBe32`, `MipsLe32`, `MipsBe64`, `MipsLe64`, `Ppc32Be`, `Ppc32Le`,
  `Ppc64Be`, `Ppc64Le`.  Threaded into
  `strider_lift::cfg::Builder::for_arch` and `CallOther` classification.
- `Endianness` — `Little` | `Big`.
- `SleighArch` — pairs an SLA spec path + PSPEC + `Endianness`.
  Constructors mirror `ArchPreset` (15 presets):
  `SleighArch::x86_64()`, `x86()`, `aarch64()`, `aarch64be()`, `arm()`,
  `arm_be()`, `arm_thumb()`, `mipsbe32()`, `mipsle32()`, `mipsbe64()`,
  `mipsle64()`, `ppc32be()`, `ppc32le()`, `ppc64be()`, `ppc64le()`.
- `CallingConvention` — static-string register names DSL.  Carries the
  stack-pointer name, integer + float return-value regs, callee-saved
  regs, positional `stack_arg_offsets`, the `ret_stack_pop` delta (8
  on x86_64, 0 on AAPCS), the optional link-register name (`lr` on
  AArch64/ARM, `ra` on MIPS, `None` on x86/x86_64), and an optional
  `syscall_number_reg_name` that marks Linux syscall conventions.
  Userland presets: `x86_cdecl`, `x86_64_systemv`,
  `x86_64_all_preserving` (zero-side-effect hooks like `__fentry__` /
  `mcount`; sets `no_memory_clobber: true` so `Call` nodes don't
  advance the memory chain), `aarch64_aapcs64`, `arm_aapcs`,
  `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`,
  `powerpc64_elf_v2`.  Linux kernel internal variants:
  `x86_linux_kernel`, `x86_64_linux_kernel`, `aarch64_linux_kernel`,
  `arm_linux_kernel`, `mips_linux_kernel_o32`,
  `mips_linux_kernel_n64`.  Linux syscall ABIs (sets
  `syscall_number_reg_name`): `x86_linux_syscall`,
  `x86_64_linux_syscall`, `aarch64_linux_syscall`, `arm_linux_syscall`,
  `mips_linux_syscall_o32`, `mips_linux_syscall_n64`.
- `CallingConvention::build(&sleigh_regs) -> Result<BuiltCallingConvention>`
  — resolves register names to `rsleigh::Vn` varnodes.
- `BuiltCallingConvention` — the resolved version.  Same fields as
  `CallingConvention` but with `Vn`s in place of static strings.
  Method `positional_arg_layout() -> PositionalArgLayout` produces the
  DTO below.
- `PositionalArgLayout` / `PositionalArg` — canonical positional-arg
  enumeration DTO consumed by `FunctionArgDetect`,
  `CallStackArgCollect`, and `StackLoadForward`.
  Indices `0..arg_passing_regs.len()` are register slots; indices
  `arg_passing_regs.len()..` are stack slots at the convention's
  `stack_arg_offsets`.  Construct via
  `PositionalArgLayout::from_convention(&cc)` or
  `BuiltCallingConvention::positional_arg_layout()`.  Single source of
  truth so every pass sees the same slot order.
- `MissingPresetError` — error returned when a `CallingConvention`
  preset constructor fails to look up its static register names.
- `call_other_abi::CallOtherAbi` — describes the *implicit* (ISA-fixed,
  not pcode-explicit) channel of a CallOther beyond Sleigh's pcode
  operands.  Fields: `implicit_reads: &'static [&'static str]`,
  `implicit_writes: &'static [&'static str]`,
  `mem_clobbers: &'static [AliasClass]` (per-partition memory clobber
  set — replaces the old coarse `memory_edge: bool`; `MEM_CLOBBER_NONE`
  for pure compute, `MEM_CLOBBER_HEAP_UNKNOWN` for barriers / atomics
  / port-I/O, `MEM_CLOBBER_FULL` for kernel-entry paths that can also
  mutate the user stack frame).
- `call_other_abi::CallOtherClass` — `NoOp` | `NoReturn` |
  `Call(CallOtherAbi)`.  The classifier's verdict for a given user-op
  name.
- `call_other_abi::classify(preset, name) -> Option<CallOtherClass>` —
  single source of truth for `name → CallOtherClass`.  Consulted by
  `strider_lift::cfg::region_builder` (terminate trap regions) and
  `strider_analyze::strider::PerRegionDriver::handle_call_other` (NoOp
  skips emission, NoReturn emits a terminal CallOther, `Call(abi)`
  emits a precise CallOther with the explicit register footprint and
  memory edge).
- `Result<T>` alias (`anyhow::Result<T>`).

## Architecture

`src/arch.rs` defines `Endianness`, `ArchPreset`, and `SleighArch` with
its constructors that pin SLA / PSPEC paths.

`src/calling_convention/` defines `CallingConvention` (compile-time
static-string register names), `BuiltCallingConvention` (runtime `Vn`
varnodes), and `PositionalArgLayout` (the DTO consumed by stack-arg
passes).  The split between `CallingConvention` and
`BuiltCallingConvention` exists so a `CallingConvention` is
`const`-constructible and the `Vn` resolution happens once per analysis
when the `Sleigh` context is available.  Six preset families cover
userland, Linux kernel internal, and Linux syscall ABIs for every
supported architecture.

`src/call_other_abi.rs` is the single-source-of-truth `name →
CallOtherClass` table.  Unknown user-op names raise
`ir::error::UnknownCallOtherError` at the IR builder, so the table
grows incrementally with what real lifts emit rather than silently
misclassifying.

## Key invariants

- `CallingConvention` register names are verified at `build()` time;
  an unknown name raises an error rather than silently dropping the
  register.
- `BuiltCallingConvention::callee_saved` lists the registers a callee
  must restore — these are NOT clobbered by `Call` nodes.
- `CallOtherAbi::implicit_reads` / `implicit_writes` describe
  registers that Sleigh's pcode does *not* mention but the ISA defines
  as part of the user-op (e.g. `rdtscp` writes `ECX` as the
  IA32_TSC_AUX low-32 without a downstream pcode op — that's the
  implicit-write set).  Be careful **not** to over-declare: many
  x86 user-ops (`rdtsc`, `rdmsr`) emit the EDX/EAX writes as
  explicit pcode after the CALLOTHER, and declaring them implicitly
  on top would double-clobber the call site.
- `classify` returns `None` for unknown names; the IR builder turns
  this into `UnknownCallOtherError` so the table can grow
  incrementally.
- No `Opaque` variant in `CallOtherClass` — every previously-Opaque
  entry was reclassified to `NoOp`, `NoReturn`, or precise `Call(abi)`.
- **LR is intentionally listed in `callee_saved_regs`** for AArch64,
  ARM, PowerPC, and MIPS conventions even though the callee normally
  clobbers it on a leaf call.  The tradeoff: leaf functions don't save
  LR but the indirect-branch resolver expects LR to retain its
  caller-supplied value at function entry so `bx lr` / `blr` shapes
  classify as `Return` rather than tail call.

## Tests

Integration tests in `crates/strider-target/tests/`.  Inline tests in
`src/calling_convention/tests.rs`.

```bash
cargo test --package strider-target
```

## Gotchas

- `SleighArch` paths are resolved by rsleigh at runtime via
  `Sleigh::new(arch)`.  A missing SLA file becomes a runtime error,
  not a compile-time one.
- Per-call ABI overrides (e.g. a function that breaks the standard
  ABI) are applied by passing a custom `CallingConvention` at the call
  site, not by mutating presets.
- `mips_o32` and `mips_n64` differ in stack-arg offsets and
  integer/float reg sets — pick the correct one for the binary's ABI.
- Depends only on `rsleigh` and `anyhow`.  No dependency on
  `strider-ir`, `strider-lift`, or `strider-analyze`.
