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
  with its family, byte orders, Ghidra language id and ldefs file.
- `CcPreset` / `CallingConvention::presets()`: the named calling conventions,
  each with the arch presets it is for and the Ghidra prototypes it is held to.
- `call_other_abi`: the CallOther ABI table below and the `classify` /
  `classify_with` lookups over it.

## Layout

Everything one architecture family knows lives in one file under
`src/family/` (`x86`, `arm32`, `aarch64`, `mips`, `ppc`): a `Family` holding
its Ghidra ldefs path, decode facts, CallOther rows (exact, prefix, fallback),
rounding / msr special ops and calling conventions. A preset reaches its
family through its `arch_presets!` row (`ArchPreset::family()`), which is how
`classify` and `SleighArch` read it; `family::FAMILIES` is what
`CallingConvention::presets()` iterates, and
`every_preset_family_is_registered_once` checks it lists every preset's
family once. `call_other_abi/common.rs` holds the arch-independent rows.

The data is checked against the vendored sla and Ghidra:
`tests/family_facts_against_sla.rs` (`has_delay_slots` against `delayslot(`
use), `src/family/tests.rs` (every register name a family carries resolves
on its presets), `tests/every_committed_context_var_is_covered.rs` (the
flow-commit lists against every `globalset`), `tests/ghidra_oracle.rs` (each
convention against its cspec prototype, each preset against its language
definition), `tests/cc_roster.rs` (convention roster) and
`tests/call_other_snapshot.rs` (how every user-op of every preset
classifies).

## Adding to strider-target

Each place below is pinned: leaving one out fails the named test.

- A CallOther row: a `Row` in the family file's `call_other` (or in
  `call_other_abi/common.rs` if arch-independent), then re-bless the
  snapshot with `UPDATE_EXPECT=1 cargo test -p strider-target --test
  call_other_snapshot` and review `git diff`
  (`call_other_classification_snapshot`). The snapshot's `UNCLASSIFIED`
  lines are the ops still unanswered.
- A calling convention:
  - a `CallingConvention` constructor in the family file and a `CcPreset`
    entry in its `CALLING_CONVENTIONS`, with its `ghidra` pairing
    (`every_calling_convention_preset_agrees_with_its_ghidra_cspec`);
  - its line in `named()` in `tests/cc_roster.rs`
    (`preset_names_are_unique_and_are_the_constructors`);
  - its line in the `forall_preset!` list in `crates/strider-py/src/cc.rs`
    (`test_cc_presets.py::test_every_rust_cc_preset_is_bound`) and in
    `crates/strider-py/strider/sleigh.pyi` (`test_stub_parity.py`).

  To draft one from a Ghidra cspec: `CSPEC=RISCV/data/languages/riscv64-fp.cspec
  cargo test -p strider-target --test ghidra_oracle -- --ignored
  draft_preset_from_cspec --nocapture`.
- A preset in an existing family:
  - its `arch_presets!` row in `src/arch.rs`, with its Ghidra id;
  - its line in the two spelled-out tables in `src/arch.rs`'s tests
    (`every_preset_is_in_its_own_family`,
    `every_preset_pins_its_ghidra_id_and_byte_orders`), in `tests/arch_smoke.rs` (`presets_endianness_matches_arch`) and in
    `src/arch_names.rs` (`every_preset_has_its_own_name`);
  - its sla and pspec files in `sla()` / `pspec()` in `tests/ghidra_oracle.rs`
    if they are new (`every_arch_preset_is_its_ghidra_ldefs_language`);
  - a `tests/snapshots/call_other/<name>.txt` to bless as above
    (`call_other_classification_snapshot`);
  - an `arches` entry in at least one `CcPreset` of its family
    (`every_arch_has_a_calling_convention`);
  - its constructor in the list in `crates/strider-py/src/arch.rs`
    (`test_arch.py::test_every_rust_preset_is_bound`) and in
    `crates/strider-py/strider/sleigh.pyi` (`test_stub_parity.py`).
- A family, on top of its presets and conventions:
  - a `family/<name>.rs` exporting a `Family`, with `mod` and `use` lines in
    `family/mod.rs` and an entry in `FAMILIES`
    (`every_preset_family_is_registered_once`);
  - its `arch_presets!` rows naming it;
  - its pins in `src/arch.rs`'s tests: the family name in
    `every_preset_is_in_its_own_family`, its ldefs in
    `every_preset_pins_its_ghidra_ldefs`, its volatile counters in
    `volatile_registers_per_preset`, and its ISA-mode presence in
    `isa_mode_var_matches_entry_mode_context_on_every_preset` if it has a mode;
  - its language directory's committed context vars in `expected()` in
    `tests/every_committed_context_var_is_covered.rs`.

  The snapshot's `UNCLASSIFIED` lines are its CallOther to-do list.
  `ValueType` widths (`strider-ir`) and reader relocations (`strider-reader`)
  live elsewhere.

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
