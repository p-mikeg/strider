# `strider-py` examples

Most run against the committed fixture ELFs in `fixtures/out/x86/`; 02, 08,
10, 11, 12, 15 and 17 supply code bytes from Python, 07 a ROM.

## Prerequisites

Run from the workspace root; from `crates/strider-py`, `maturin develop`
succeeds but leaves stale bindings.

```bash
uv sync --group dev
uv run maturin develop
uv run python crates/strider-py/examples/python/01_quickstart.py
```

## Index

| # | Script | Demonstrates |
|---|---|---|
| 01 | [`01_quickstart.py`](01_quickstart.py) | `strider.lift.load_elf` + `ElfLifter.analyze`: load an ELF, lift a function, query it, dump HTML. |
| 02 | [`02_python_reader.py`](02_python_reader.py) | A custom `MemReader` subclass serving bytes from Python; lifts a synthetic instruction stream without touching disk. |
| 03 | [`03_pattern_rewrite.py`](03_pattern_rewrite.py) | `rewrite` the `x * 4` -> `x << 2` strength reduction and re-optimize after the structural change; `rewrite_all` for a rule list. |
| 04 | [`04_custom_optimizer.py`](04_custom_optimizer.py) | Build an optimizer pipeline pass-by-pass; compare the lifted graph before and after. |
| 05 | [`05_visualize.py`](05_visualize.py) | Dump the CFG and the lifted IR graph as standalone HTML files for browser viewing. |
| 06 | [`06_complex_patterns.py`](06_complex_patterns.py) | Multi-level captures, shared-capture back-references, `.when` predicate guards, commutative matching and `.ordered()`. |
| 07 | [`07_callback_rom.py`](07_callback_rom.py) | A `ReadOnlyMemory` subclass serving bytes from Python as the lifter's rom, which is what `LoadReadOnly` folds constant-address loads against. |
| 08 | [`08_custom_readers.py`](08_custom_readers.py) | Combine a custom `MemReader` and `ReadOnlyMemory` in one lift; capture a folded constant and template-rewrite it on a clone. |
| 09 | [`09_neighborhood.py`](09_neighborhood.py) | Render the CFG and IR neighborhood around a center node with `neighborhood_dot`; launch the interactive `visualize` explorer with `--serve`. |
| 10 | [`10_buffer_reader.py`](10_buffer_reader.py) | Lift hand-assembled bytes with no ELF: a base address and a `bytes` object are the whole setup. |
| 11 | [`11_firmware_multiarch.py`](11_firmware_multiarch.py) | The same `add` as x86-64, AArch64 and MIPS blobs; one `int_add` pattern finds it in all three. |
| 12 | [`12_rom_table.py`](12_rom_table.py) | A `BufferReader` as ROM: `LoadReadOnly` folds a constant-address table read into an `IntConst`. |
| 13 | [`13_pattern_cookbook.py`](13_pattern_cookbook.py) | The combinators beyond node shapes: `one_of` / `first_of`, `int_const([...])`, `var(c).when(...)` guards, `find_unique`, `any_int_binary`. |
| 14 | [`14_idiom_hunt.py`](14_idiom_hunt.py) | A table of (name, pattern) run over a function to fingerprint its compiler idioms. |
| 15 | [`15_carve_blob.py`](15_carve_blob.py) | Carve several functions from one flat image with a single `BufferReader`, lifting each at its own entry offset. |
| 16 | [`16_pattern_joins.py`](16_pattern_joins.py) | Correlate separate patterns via shared captures: a producer -> consumer join, a `JoinPredicate` over two matches, and `negate` wrapping one. |
| 17 | [`17_custom_abis.py`](17_custom_abis.py) | Describe a target strider does not know: `user_op_names` / `call_other_abi` discovery, then `CallOtherAbi.custom` for a Sleigh user-op and `CallingConvention.custom` for a register ABI. |

### The `BufferReader` group (10, 11, 12, 15, 17)

`strider.reader.BufferReader(base_addr, data)` serves `data` at `base_addr`
as the `mem` or `rom` of `strider.lift.lifter`, read on the Rust side.
Subclass `MemReader` / `ReadOnlyMemory` (02, 07, 08) only for lazily produced
bytes.

- **Padding**: the disassembler prefetches past the last instruction. A
  `BufferReader` short-reads there, so its padding is harmless; a `MemReader`
  returning `None` needs real padding (02 and 08 carry 64 bytes).
- **`allow_code_before_start_addr=True`** (all five; also 03, 04, 07 and
  08): otherwise an unconditional branch below the entry is a tail call, not
  an edge.
