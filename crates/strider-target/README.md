# strider-target

Target-description data: architecture presets, calling conventions, and the
CallOther ABI table.

## What's here

- `SleighArch`: an architecture (SLA spec, PSPEC, endianness), with a constructor
  per supported target (`x86_64()`, `aarch64()`, `arm_thumb()`, `mipsle32()`, ...).
  `entry_mode_context(entry_addr)` gives the ISA mode a cold entry decodes in
  (ARM Thumb, MIPS16e; never microMIPS).
- `ArchNames`: an arch's register and space names, default code space and
  `ram` address width, all a render reads (`SleighArch::names()`).
- `DecodeTables`: what an arch's sla says about decoding and lifting (user-op
  names, flowing and committed context vars with their pspec defaults, the
  declared, volatile and internal registers; `SleighArch::decode_tables()`).
  Both are read off one Sleigh over an empty image, once per preset used per
  process.
- `CallingConvention`: register names for arguments, results and preserved
  state, with a preset per userland ABI plus `x86_linux_kernel` and the three
  PowerPC `*_linux_kernel` flavours (no vector registers); other kernels use
  their userland preset. `build(&regs)` resolves the names to varnodes.
  `arm_aapcs` is hard-float, `arm_aapcs_soft` soft / softfp.
- `StackArgs`: the stack-slot geometry.
- `ArchPreset`: the roster of presets (`arch_presets!` in `src/arch/mod.rs`), each
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
family through its `arch_presets!` row (`ArchPreset::family()`);
`family::FAMILIES` is what `CallingConvention::presets()` iterates. `call_other_abi/common.rs` holds the arch-independent rows.
`src/arch/` holds `SleighArch` and its roster (`mod.rs`), the byte orders
(`endian.rs`), `ArchNames` (`names.rs`) and `DecodeTables`
(`decode_tables.rs`).

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
  - its `arch_presets!` row in `src/arch/mod.rs`, with its Ghidra id;
  - its line in the two spelled-out tables in `src/arch/tests.rs`
    (`every_preset_is_in_its_own_family`,
    `every_preset_pins_its_ghidra_id_and_byte_orders`), in
    `tests/arch_smoke.rs` (`presets_endianness_matches_arch`) and in
    `src/arch/names.rs` (`every_preset_has_its_own_name`);
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
  - its pins in `src/arch/tests.rs`: the family name in
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

Sleigh lifts what it cannot express (`cpuid`, `syscall`, barriers,
coprocessor moves) to `CALLOTHER(user_op_id, args)`, often reading or writing
registers and memory its p-code never mentions. This crate's table, keyed by
architecture and user-op name, gives that footprint:

- `implicit_reads` / `implicit_writes`: registers beyond the p-code operands.
- `clobbers_memory`: whether it advances the memory chain.
- `no_return`: whether control never comes back. PowerPC `tw`/`td` stay
  returning; `trap_is_unconditional` reads the `TO` mask.

`classify(preset, name)` answers `NoOp` (no node) or `Call(abi)`; an
unclassified name is a lift error. `classify_with(overrides, preset, name)` lets
a caller answer first (`CfgOptions(call_other_abis=...)` from Python), with a
`CallOtherClass` or an already-resolved `BuiltCallOtherAbi`.

Depends only on `rsleigh`, `anyhow`, `rustc-hash` and `vn-container`.
