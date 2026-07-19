# strider-target

Pure target-description data: architecture presets, calling conventions, and the
CallOther ABI table. No IR and no Sleigh state, just the descriptors the lifting
and optimizing layers read. It sits at the bottom of the dependency graph so
every layer names the same ABI types.

## What's here

- `SleighArch`: an architecture (SLA spec, PSPEC, endianness), with a constructor
  per supported target (`x86_64()`, `aarch64()`, `arm_thumb()`, `mipsle32()`, ...).
- `CallingConvention`: a register-name description of how arguments are passed and
  results returned, with presets for userland, Linux kernel, and syscall ABIs.
  `build(&regs)` resolves the names to varnodes.
- `StackArgs`: the stack-slot geometry (offsets and slot indices) that the
  stack-argument passes read, so every pass agrees on slot order.
- `call_other_abi`: `classify(preset, name)` and the CallOther ABI table below.

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
- `no_return`: whether control ever comes back (false for a trap or `sysret`).

`classify(arch, name)` says how to lift a given user-op: `NoOp` emits nothing (a
hint with no effect on the model), and `Call(abi)` emits a CallOther node
carrying that footprint. A missing entry is deliberate: it becomes a lift error,
so the table grows from real binaries instead of guessing a wrong footprint.

Depends only on `rsleigh` and `anyhow`. The source is the reference for the full
surface.
