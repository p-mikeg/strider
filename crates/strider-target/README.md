# strider-target

Target-description data: architecture presets, calling conventions, and the
CallOther ABI table, the descriptors the lifting and optimizing layers read. It
sits at the bottom of the dependency graph, above only `vn-container`, so every
layer names the same ABI types.

## What's here

- `SleighArch`: an architecture (SLA spec, PSPEC, endianness), with a constructor
  per supported target (`x86_64()`, `aarch64()`, `arm_thumb()`, `mipsle32()`, ...).
  `entry_mode_context(entry_addr)` gives the context variable and value a cold
  entry decodes in, keyed off the address low bit, on the two families whose
  ISA mode the entry address carries (ARM Thumb, MIPS16e); it is `None`
  elsewhere. The two MIPS pspecs (`PSPEC_MIPS32`, `PSPEC_MIPS64`, shared by the
  four MIPS presets) pin `RELP=1`, so the alternate ISA is MIPS16e and a
  microMIPS image decodes its odd-addressed functions against MIPS16e tables.
- `ArchNames`: an arch's register and space names, default code space and
  `ram` address width, all a render reads. `SleighArch::names()` builds it
  once per preset per process off a Sleigh over an empty image.
- `DecodeTables`: what an arch's sla says about decoding and lifting (user-op
  names, flowing and committed context vars with their pspec defaults, the
  declared, volatile and internal registers). `SleighArch::decode_tables()`
  builds it once per preset per process off a Sleigh of its own, so this
  crate does construct decoders, two per preset used.
- `CallingConvention`: a register-name description of how arguments are passed
  and results returned, with a preset per userland ABI plus four
  kernel-internal ones: `x86_linux_kernel` and the three PowerPC
  `*_linux_kernel` flavours. `build(&regs)` resolves the names to varnodes.
  Every other architecture's kernel ABI has its userland preset's register
  SET, so a caller picks that directly. A role can still differ without the
  set doing so, which is why PowerPC has presets of its own: ppc64 `r13` is the
  per-CPU `paca` pointer there rather than the userland TLS pointer. Float and
  vector arguments have their own positional list,
  `arg_passing_regs_float`, drawn from a register file the integer list never
  names; where the float and vector banks are physically distinct, as on
  PowerPC, the vector window (`v2`-`v13`) follows the floats, and the PowerPC
  kernel presets name no part of it, Linux there being built with
  `-mno-altivec -mno-vsx`. ARM32 splits over the float variant the ELF header
  does not pin down: `arm_aapcs` is hard-float (VFP), `arm_aapcs_soft` is
  `-mfloat-abi=soft` / `softfp`.
- `StackArgs`: the stack-slot geometry (offsets and slot indices) that the
  stack-argument passes read, so every pass agrees on slot order.
- `ArchPreset`: the roster of presets (`arch_presets!` in `src/arch.rs`), each
  with its family, byte orders and Ghidra language id.
- `CcPreset` / `CallingConvention::presets()`: the named calling conventions,
  each with the arch presets it is for and the Ghidra prototypes it is held to.
- `call_other_abi`: the CallOther ABI table below and the `classify` /
  `classify_with` lookups over it.

## Layout

Everything one architecture family knows lives in one file under
`src/family/` (`x86`, `arm32`, `aarch64`, `mips`, `ppc`): a `Family` holding
its decode facts, CallOther rows (exact, prefix, fallback), rounding / msr
special ops and calling conventions. `family::FAMILIES` is the roster;
`classify`, `SleighArch` and `CallingConvention::presets()` all read through
it. `call_other_abi/common.rs` holds the arch-independent rows.

The data is checked against the vendored sla and Ghidra: `tests/family_facts_against_sla.rs`
(decode facts, register names), `tests/ghidra_oracle.rs` (each convention against
its cspec prototype, each preset against its language definition),
`tests/cc_roster.rs` (convention roster) and `tests/call_other_snapshot.rs`
(how every user-op of every preset classifies).

## Adding to strider-target

- A CallOther row: add a `Row` to `call_other` in the family file (or to
  `call_other_abi/common.rs` if arch-independent). The snapshot's
  `UNCLASSIFIED` lines are the ops still unanswered; re-bless with
  `UPDATE_EXPECT=1 cargo test -p strider-target --test call_other_snapshot`
  and review `git diff`.
- A calling convention: a `CallingConvention` constructor and a `CcPreset`
  entry (with its `ghidra` pairing) in the family file. Python picks it up
  through `CC_PRESETS` / `CallingConvention.preset(name)`; the classmethod
  list in `crates/strider-py/src/cc.rs` and
  `crates/strider-py/strider/sleigh.pyi` need one line each, guarded by
  `tests/python/test_cc_presets.py`. To draft one from a Ghidra cspec:
  `CSPEC=RISCV/data/languages/riscv64-fp.cspec cargo test -p strider-target
  --test ghidra_oracle -- --ignored draft_preset_from_cspec --nocapture`.
- A preset in an existing family: one `arch_presets!` row with its Ghidra id.
  Python needs the constructor in the list in `crates/strider-py/src/arch.rs`
  and the `.pyi` stub, guarded by `tests/python/test_arch.py`.
- A family: a new `family/<name>.rs` exporting a `Family`, registered in
  `FAMILIES`, then its `arch_presets!` and `CcPreset` rows and the cspec
  draft above. The snapshot's `UNCLASSIFIED` lines are the CallOther to-do
  list. `ValueType` widths (`strider-ir`) and reader relocations
  (`strider-reader`) live elsewhere.

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
- `no_return`: whether control never comes back (true for `sysret`, an ERET, or
  a trap that cannot fall through). The PowerPC `tw`/`td` family is conditional,
  so the table row keeps it returning, but `trap_is_unconditional` reads the
  `TO` mask and ends the region for the 7 of 32 masks that cover a trichotomy.

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

Depends only on `rsleigh`, `anyhow`, `rustc-hash` and `vn-container`.
