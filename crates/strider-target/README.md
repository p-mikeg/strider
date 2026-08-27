# strider-target

Pure target-description data: architecture presets, calling conventions, and the
CallOther ABI table, the descriptors the lifting and optimizing layers read. It
sits at the bottom of the dependency graph so every layer names the same ABI
types.

## What's here

- `SleighArch`: an architecture (SLA spec, PSPEC, endianness), with a constructor
  per supported target (`x86_64()`, `aarch64()`, `arm_thumb()`, `mipsle32()`, ...).
  `entry_mode_context(entry_addr)` gives the context variable and value a cold
  entry decodes in, keyed off the address low bit, on the two families whose
  ISA mode the entry address carries (ARM Thumb, MIPS16e); it is `None`
  elsewhere. The two MIPS pspecs (`PSPEC_MIPS32`, `PSPEC_MIPS64`, shared by the
  four MIPS presets) pin `RELP=1`, so the alternate ISA is MIPS16e and a
  microMIPS image decodes its odd-addressed functions against MIPS16e tables.
- `CallingConvention`: a register-name description of how arguments are passed
  and results returned, with a preset per userland ABI plus `x86_linux_kernel`,
  the one kernel-internal ABI. `build(&regs)` resolves the names to varnodes.
  Float and vector arguments have their own positional list,
  `arg_passing_regs_float`, drawn from a register file the integer list never
  names. ARM32 splits over the float variant the ELF header does not pin down:
  `arm_aapcs` is hard-float (VFP), `arm_aapcs_soft` is `-mfloat-abi=soft` /
  `softfp`.
- `StackArgs`: the stack-slot geometry (offsets and slot indices) that the
  stack-argument passes read, so every pass agrees on slot order.
- `call_other_abi`: the CallOther ABI table below and the `classify` /
  `classify_with` lookups over it.

## What the CallOther ABI is

Sleigh lifts most instructions into explicit p-code operations. A few it cannot:
special instructions like `cpuid`, `rdtsc`, `syscall`, cache flushes, memory
barriers, and coprocessor accesses. For those it emits a placeholder,
`CALLOTHER(user_op_id, args)` with an optional output, and leaves the meaning to
the consumer.

The catch is that such an instruction usually reads or writes registers, or
touches memory, that the p-code around it never mentions. Ignore that and the IR
becomes unsound: a later read of a register the instruction clobbered would see a
stale value.

So this crate keeps a table, keyed by architecture and user-op name, describing
that implicit footprint:

- `implicit_reads` / `implicit_writes`: registers the instruction reads or writes
  beyond the p-code operands.
- `clobbers_memory`: whether it advances the IR's memory edge (true for atomics,
  barriers, port I/O, syscalls; false for pure compute like `rdtsc`).
- `no_return`: whether control never comes back (true for a trap or `sysret`).

`classify(preset, name)` says how to lift a given user-op: `NoOp` emits nothing (a
hint with no effect on the model), and `Call(abi)` emits a CallOther node
carrying that footprint. An unclassified name is a lift error, so the table
grows from real binaries instead of guessing a wrong footprint.
`classify_with(overrides, preset, name)` lets a caller answer first, which is
what `CfgOptions::call_other_overrides` (`CfgOptions(call_other_abis=...)` from
Python) feeds. An override is either a `CallOtherClass` shaped like a table row
or a `BuiltCallOtherAbi` the caller resolved against a `SleighRegs` itself,
which is how a footprint whose register names are not `&'static str` gets in.
Overrides are per-analysis, the way a calling convention is: the table states
what the architecture does, an override states what one binary's build of the
op does.

Depends only on `rsleigh` and `anyhow`. The source is the reference for the full
surface.
