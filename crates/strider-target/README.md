# strider-target

Target-description data: architecture presets, calling conventions, and the
CallOther ABI table.

## API

- `SleighArch`: an architecture preset (`x86_64()`, `aarch64()`,
  `arm_thumb()`, `mipsle32()`, ...). `entry_mode_context(entry_addr)` gives the
  ISA mode a cold entry decodes in (Thumb, MIPS16e).
- `ArchNames` (`SleighArch::names()`): register and space names for rendering.
- `DecodeTables` (`SleighArch::decode_tables()`): what the sla says about
  decoding (user-op names, context vars and their defaults, volatile and
  internal registers).
- `CallingConvention`: register names for arguments, results and preserved
  state, one preset per userland ABI plus the `*_linux_kernel` ones (x86,
  PowerPC); `build(&regs)` resolves them to varnodes. `CcPreset` /
  `CallingConvention::presets()` list them with their arches and Ghidra
  prototypes.
- `ArchPreset`: the preset roster (`arch_presets!` in `src/arch/mod.rs`).
- `call_other_abi`: the CallOther table and its `classify` / `classify_with`
  lookups.

Each family is a `Family` in `src/family/<name>.rs`: ldefs path, decode
facts, CallOther rows and calling conventions. `call_other_abi/common.rs`
holds rows matched by op name on any family that emits the op. Tests check the
data against the vendored sla and Ghidra.

## CallOther ABI

Sleigh lifts what it cannot express (`cpuid`, `syscall`, barriers) to
`CALLOTHER(user_op_id, args)`, which may touch registers and memory its p-code
never names. The table gives that footprint per arch and user-op name:
`implicit_reads` / `implicit_writes`, `clobbers_memory` and `no_return`.
`classify` answers `NoOp` or `Call(abi)`; an unclassified name fails the lift.
`classify_with` lets a caller's overrides answer first.

## Adding to strider-target

Each step is checked: leaving one out fails a test.

- **CallOther row**: a `Row` in the family's `call_other` (or `common.rs`),
  then `UPDATE_EXPECT=1 cargo test -p strider-target --test call_other_snapshot`
  and review `git diff`. `UNCLASSIFIED` snapshot lines are the ops still
  unanswered.
- **Calling convention**: a constructor and a `CcPreset` in the family's
  `CALLING_CONVENTIONS` with its `ghidra` pairing
  (`every_calling_convention_preset_agrees_with_its_ghidra_cspec`); a line in
  `named()` in `tests/cc_roster.rs`; a line in `forall_preset!` in
  `crates/strider-py/src/cc.rs` and in
  `crates/strider-py/strider/sleigh.pyi`. Draft one from a cspec with
  `CSPEC=RISCV/data/languages/riscv64-fp.cspec cargo test -p strider-target
  --test ghidra_oracle -- --ignored draft_preset_from_cspec --nocapture`.
- **Preset in an existing family**: an `arch_presets!` row with its Ghidra id;
  its line in the tables of `src/arch/tests.rs`, `tests/arch_smoke.rs` and
  `src/arch/names.rs`; new sla / pspec files in `tests/ghidra_oracle.rs`; a
  `tests/snapshots/call_other/<name>.txt`, blessed as above; an `arches` entry
  in a `CcPreset`; its constructor in `crates/strider-py/src/arch.rs` and
  `crates/strider-py/strider/sleigh.pyi`.
- **Family**: `family/<name>.rs` exporting a `Family`, registered in
  `FAMILIES` in `family/mod.rs`; its `arch_presets!` rows; its name, ldefs,
  volatile registers and ISA mode in `src/arch/tests.rs`; its committed
  context vars in `expected()` in
  `tests/every_committed_context_var_is_covered.rs`.
