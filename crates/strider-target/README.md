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
- `PositionalArgLayout`: the shared slot order the stack-argument passes read.
- `call_other_abi`: `classify(preset, name)` maps a special-instruction name to
  how it should be lifted (no-op, no-return, or a call with its register and
  memory footprint). Unknown names are a lift error on purpose, so the table
  grows on demand rather than guessing.

Depends only on `rsleigh` and `anyhow`. The source is the reference for the full
surface.
