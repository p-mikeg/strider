# Linux kernel calling-convention support

## Background

`target::CallingConvention` describes how a function's caller passes
arguments and receives return values: which registers, which stack
slots, which registers the callee must preserve, and so on.  The
existing presets cover userland ABIs:

* `x86_64_systemv_abi`, `x86_cdecl`
* `aarch64_aapcs64`, `arm_aapcs`
* `mips_o32`, `mips_n64`
* `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`

This spec adds two new families of presets describing the conventions
binaries use when they are *kernel code* or when they are *crossing
the user→kernel boundary via a syscall*.  The user wants both as
distinct presets — they are conceptually different surfaces a binary
analyser cares about (kernel internals vs. user→kernel transitions).

## Goals

1. Add `<arch>_linux_kernel` and `<arch>_linux_syscall` factory
   methods to `target::CallingConvention` for the architectures we
   already support: x86, x86_64, aarch64, arm, mips32, mips64.  PPC
   is intentionally out of scope for this round (its
   function-descriptor ABIs warrant a separate pass).
2. Make the syscall-number register a first-class part of the CC
   description, so a future syscall-aware analysis (e.g. a
   `SyscallNumberDetect` pass parallel to `FunctionArgDetect`) has
   the data it needs.
3. Expose all the new presets from `strider-py` as classmethods on
   `PyCallingConvention`.
4. Add Rust + Python tests covering the new presets.

## Non-goals

* Identifying syscall callsites and labelling them.  That is a
  follow-up analysis pass; this spec just provides the data.
* PowerPC kernel / syscall presets.  Different code path due to ELF
  v1/v2 + function descriptors; deferred to a follow-up.
* Any change to `Strider`, `strider::run`, the orchestrator, or any
  existing pipeline pass.  This is purely additive at the data layer.

## Architectural facts (Linux kernel ABI per arch)

* **x86 (32-bit)** — kernel-internal CC is `regparm(3)`: first three
  args in `eax, edx, ecx`; remaining args on the stack.  Differs
  from x86 cdecl (all args on the stack).  Syscall ABI uses
  `int 0x80`: args in `ebx, ecx, edx, esi, edi, ebp`; syscall number
  in `eax`; return in `eax`.
* **x86_64** — kernel-internal CC == SystemV.  Syscall ABI uses
  `syscall`: args in `rdi, rsi, rdx, r10, r8, r9` (note: `r10` not
  `rcx`, because the `syscall` instruction clobbers `rcx`); syscall
  number in `rax`; return in `rax`.
* **aarch64** — kernel-internal CC == AAPCS64.  Syscall ABI uses
  `svc #0`: args in `x0..x5`; syscall number in `x8`; return in
  `x0`.
* **arm (32-bit)** — kernel-internal CC == AAPCS.  Syscall ABI uses
  `svc 0`: args in `r0..r6`; syscall number in `r7`; return in
  `r0`.  Same on Thumb.
* **mips o32** — kernel-internal CC == O32.  Syscall ABI uses
  `syscall`: args in `a0..a3`, more on the stack; syscall number in
  `v0`; return in `v0`.
* **mips n64** — kernel-internal CC == N64.  Syscall ABI uses
  `syscall`: args in `a0..a5`; syscall number in `v0`; return in
  `v0`.

## Design

### Data-layer changes

Add a single new field to `target::CallingConvention`:

```rust
pub struct CallingConvention {
    // ... existing fields ...
    /// Register that holds the syscall number on entry to a kernel
    /// from a user-mode syscall instruction.  `None` for userland and
    /// kernel-internal CCs; `Some("rax") / Some("x8") / Some("v0")`
    /// for the `*_linux_syscall()` presets.  Resolved into a varnode
    /// in `BuiltCallingConvention::syscall_number_vn`.
    pub syscall_number_reg: Option<&'static str>,
}
```

…and the matching resolved field on `BuiltCallingConvention`:

```rust
pub struct BuiltCallingConvention {
    // ... existing resolved fields ...
    pub syscall_number_vn: Option<rsleigh::Vn>,
}
```

`CallingConvention::build(&sleigh_regs)` resolves the new field
through `sleigh_regs` exactly the way `link_register_vn` is
resolved today.

All existing presets keep `syscall_number_reg = None`.  The struct
literal patterns inside the existing factory methods need a single
extra field — diff is mechanical.

### New factory methods

One method per (arch, role) pair.  Where kernel-internal == userland,
the kernel factory delegates rather than duplicating data.

| Factory                       | Behaviour                                                    |
|-------------------------------|--------------------------------------------------------------|
| `x86_linux_kernel`            | regparm(3): args in `eax, edx, ecx`; rest cdecl              |
| `x86_linux_syscall`           | args in `ebx, ecx, edx, esi, edi, ebp`; sn = `EAX`           |
| `x86_64_linux_kernel`         | delegates to `x86_64_systemv_abi`                            |
| `x86_64_linux_syscall`        | args in `rdi, rsi, rdx, r10, r8, r9`; sn = `RAX`             |
| `aarch64_linux_kernel`        | delegates to `aarch64_aapcs64`                               |
| `aarch64_linux_syscall`       | args in `x0..x5`; sn = `x8`                                  |
| `arm_linux_kernel`            | delegates to `arm_aapcs`                                     |
| `arm_linux_syscall`           | args in `r0..r6`; sn = `r7`                                  |
| `mips_linux_kernel_o32`       | delegates to `mips_o32`                                      |
| `mips_linux_kernel_n64`       | delegates to `mips_n64`                                      |
| `mips_linux_syscall_o32`      | args in `a0..a3`; sn = `v0`; stack args same as O32          |
| `mips_linux_syscall_n64`      | args in `a0..a5`; sn = `v0`                                  |

The mips syscall presets are split by ABI rather than a single
factory — the surrounding non-syscall fields (stack-arg offsets,
callee-saved set) differ between O32 and N64, so a unified factory
would have to re-encode everything.  Splitting matches the existing
`mips_o32` / `mips_n64` userland naming.

For `*_linux_syscall` presets, the convention's `link_register_vn`
is `None` — `syscall` / `svc` returns via `sysret` / `eret` which
do not consult LR.  `ret_stack_pop` is `0`.  `callee_saved_regs`
is set conservatively: kernel-side, the syscall entry preserves
whatever the userland ABI considers callee-saved (so libc syscall
wrappers can be analysed correctly), so we re-use the userland CC's
`callee_saved_regs` for each syscall preset.

### Who consumes these CCs?

* `*_linux_kernel` is consumed by anyone analysing a kernel binary
  (`vmlinux`, a `.ko` module): every C-language kernel function uses
  it.  For all arches except x86 32-bit, this is a thin alias of the
  userland CC; the alias is still useful as self-documentation.
* `*_linux_syscall` is consumed by anyone analysing the kernel's
  syscall *entry* code (`arch/x86/entry/entry_64.S` and friends),
  or by anyone synthesising a fixture binary that follows the
  syscall ABI for testing the analyzer's CC handling.  Userland
  libc syscall wrappers should still be analysed with the userland
  CC — the wrapper's own boundary is userland; only the embedded
  `syscall` / `svc` instruction is the kernel transition.

### Strider-py exposure

`crates/strider-py/src/cc.rs` adds one classmethod per new factory.
The pattern is mechanical:

```rust
#[classmethod]
fn x86_64_linux_kernel(_cls: &Bound<'_, PyType>) -> Self {
    Self {
        inner: target::CallingConvention::x86_64_linux_kernel(),
        preset_name: "x86_64_linux_kernel",
    }
}
```

The existing `name()` method already returns the preset_name verbatim,
so `repr` and round-tripping work without further changes.

`syscall_number_vn` is not exposed to Python in this round — there
is no Python user-visible code that needs it yet.  It will be
exposed when the first analysis surface that consumes it lands.

### Tests

**Rust** (`crates/target/tests/linux_cc_presets.rs`):

For each new factory, the test:
1. Constructs the preset via the factory.
2. Builds it against a probe Sleigh (`SleighArch::probe_regs()`).
3. Asserts the resulting `BuiltCallingConvention` has the expected
   `arg_passing_regs` (sequence of varnodes, each with the right
   register name when round-tripped through `sleigh_regs`).
4. Asserts `syscall_number_vn` is `Some` for every `_linux_syscall`
   preset and `None` for every `_linux_kernel` preset.

**Rust strider-level** (`crates/strider/tests/linux_kernel_cc.rs`):

For each `_linux_kernel` and `_linux_syscall` preset, construct a
`Strider` and assert it built (i.e. every reg name in the CC
resolved against the arch's Sleigh register table).  This is a
typed smoke check — `Strider::new` fails fast on a missing
register, so any typo in the new presets surfaces here.  No deep
lifting / `analyze_cfg` step: the data-layer test above already
covers register-list correctness, and lifting per se is unaffected
by the CC.

**Python** (`crates/strider-py/tests/python/test_linux_cc.py`):

Per-preset existence test — every classmethod returns a
`CallingConvention` whose `name()` matches the factory name.

### Compiled-with-kernel-CC fixtures

Beyond per-preset existence tests, the design lifts every existing
fixture case once more under a kernel-CC ABI by compiling those
fixtures with `gcc -mregparm=3` and analysing them with
`x86_linux_kernel`.  This is the only arch whose kernel CC differs
from its userland CC at the C level (regparm(3) vs. cdecl), so it's
the only arch where a userland-compiled fixture under a kernel-CC
analysis would be wrong; it's also the only arch where compiling
*with* the kernel CC produces a binary that differs from the
existing x86 fixtures.

Concretely:

* New `fixtures/arch/x86_kernel.mk` cloning `x86.mk` with
  `-mregparm=3` appended to `CFLAGS`.  (`-fno-PIC -fno-pic`
  and the rest stay identical.)  Every existing case rebuilds
  cleanly under the new flags — `regparm` is ABI-only, no source
  changes.
* New arch entry `Arch::X86Kernel` in
  `crates/strider/tests/common/mod.rs::Arch` whose
  `sleigh()` returns `SleighArch::x86()` (same Sleigh) and
  `cc()` returns `CallingConvention::x86_linux_kernel()`.
  `binary_path()` lookups under `fixtures/out/x86_kernel/`.
* `per_arch_test!` already iterates over the `Arch` enum's
  variants, so adding `X86Kernel` runs every existing per-arch
  pattern test once more under the kernel CC for free.
* Mirror in `crates/strider-py/tests/python/system/_helpers.py` —
  add an `ArchSpec("x86_kernel", SleighArch.x86,
  CallingConvention.x86_linux_kernel)` row.  Pytest's parametrised
  `arch_id` fixture picks up the new id automatically.

Result: the existing arithmetic / patterns / control / calls
system tests give us a smoke test that the new
`x86_linux_kernel` CC analyses identically to userland on every
fixture function we already exercise — i.e. that swapping the CC
on a real binary doesn't break any pattern query.

For arches where the kernel CC is an alias of the userland CC
(x86_64, aarch64, arm, mips) we don't add separate `*_kernel`
fixture builds; they would produce byte-identical binaries.  A
single Rust-level test that constructs the alias and asserts its
register list matches the userland source CC suffices for
regression coverage.

Syscall-ABI presets are tested at the data layer only (register-
list assertions in `crates/target/tests/linux_cc_presets.rs`).
Compiling a fixture that exercises the syscall ABI from C requires
inline `asm("syscall")` and is out of scope for this round; the
data-layer assertions are sufficient until a downstream pass
actually consumes `syscall_number_vn`.

## Decisions

* **Naming.**  `<arch>_linux_kernel` and `<arch>_linux_syscall` (with
  `_o32`/`_n64` suffixes for mips).  Matches the existing
  `<arch>_<convention>` pattern (`x86_64_systemv_abi`,
  `aarch64_aapcs64`, …).
* **Field on the existing struct.**  `syscall_number_reg` is added
  to `CallingConvention` rather than a parallel `SyscallCC` type.
  This keeps `Strider::new(arch, regs, cc)` the single entry point
  — analysing a syscall wrapper or a kernel function uses the same
  Strider API as analysing a userland function.
* **Skipping PPC.**  Kernel/syscall handling on PPC interacts with
  ELFv1 function descriptors and the TOC pointer; out of scope for
  this round.

## Out-of-scope follow-ups

These are intentionally not part of this spec but are likely future
work:

1. A `SyscallNumberDetect` analyser pass that, when the CC has
   `syscall_number_vn`, recognises the canonical
   `mov $N, syscall_num_reg; syscall` shape at the syscall site
   and labels the call with its target syscall number.
2. PPC kernel / syscall presets with TOC handling.
3. Exposing `syscall_number_vn` on `PyCallingConvention` once a
   Python-level consumer needs it.
