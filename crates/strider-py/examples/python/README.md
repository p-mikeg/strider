# `strider-py` examples

Runnable scripts covering every major surface of `strider-py`. Most run
against the fixture ELFs in `fixtures/out/x86/`; 02, 08, 10, 11, 12, 15 and 17
supply bytes from Python and need no ELF.

## Prerequisites

Everything runs from the workspace root. `maturin develop` MUST run from
there: from `crates/strider-py` it exits 0 but builds the wrong package and
leaves a stale `.so`, so the examples then run against old bindings.

The fixture ELFs are committed, so only the Python setup is needed:

```bash
uv sync --group dev
uv run maturin develop
```

Then run any example; `uv run` uses the environment `maturin develop`
installed into:

```bash
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
| 06 | [`06_complex_patterns.py`](06_complex_patterns.py) | Multi-level captures, shared-capture back-references, `.when` predicate guards, commutative matching, the `any_*` variant-agnostic constructors. |
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
and works as both the `mem` and `rom` argument to `strider.lift.lifter`. Reads
stay on the Rust side, so use it when you already hold the bytes (shellcode, a
firmware dump, a carved slice); subclass `MemReader` / `ReadOnlyMemory`
(examples 02, 07, 08) only for lazily-produced bytes.

Two requirements common to every raw-bytes lift:
- **Pad past the instruction stream**: the disassembler prefetches beyond the
  last instruction and the reader must answer those addresses, so append a
  handful of bytes. The value is irrelevant: these examples append 16 or 64
  zero bytes, and the `MemReader` ones (02, 08) 64 `0x90` NOPs.
- **`allow_code_before_start_addr=True`**: raw blobs often have code below the
  entry.

The fixtures are small (a few dozen instructions per function) to keep output
readable. For a full end-to-end run see
`crates/strider-orchestrator/examples/orchestrator_demo.rs`;
`crates/strider-py/tests/python/` has the stress coverage.
