# strider-py

The Python bindings, and Strider's primary interface. Load a binary, lift and
optimize a function, and query the result with patterns, all from Python.

## Getting started

Build the extension from the workspace root (not this directory):

```bash
uv sync --group dev
uv run maturin develop
uv run pytest
```

Then:

```python
import strider
from strider.pattern import load, int_add, Capture

prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
cfg, function, unresolved = prog.analyze("array_sum")

base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, off)), ignore_casts=True):
    print("offset =", hit.uint_opt(off))   # None if it is not a constant
```

## The module layout

Every class and free function lives in a domain submodule; `StriderError` is
the one top-level name.

| Module | Main names |
|---|---|
| `strider.lift` | `load_elf`, `lifter`, `Lifter` / `ElfLifter`, `LifterOptions`, `AssumptionOptions`, `AnalyzeResult` |
| `strider.ir` | `Function`, `Node` |
| `strider.cfg` | `Cfg`, `CfgOptions`, `DotStyle` |
| `strider.sleigh` | `SleighArch`, `Sleigh`, `CallingConvention`, `CallOtherAbi`, `Vn`, `VnSpace` |
| `strider.reader` | `BufferReader`, `MemReader`, `ReadOnlyMemory`, `Symbol`, `SymbolIter` |
| `strider.pattern` | the match DSL: `Pat`, `Capture`, `Match`, the builders, `.constraints` |
| `strider.template` | the build side of a rewrite: `Template` |
| `strider.opt` | `OptimizerPipeline` and the individual passes |
| `strider.explore` | the CFG / IR explorer `Lifter.visualize` serves; `shutdown(port)` stops it |

## Learn more

- The guides in [`../../docs/`](../../docs/): getting started, vocabulary, the
  full Python walkthrough, the complete Python API reference, and the optimizer
  passes.
- [`examples/python/`](examples/python/): seventeen runnable scripts, from a
  quickstart to the `BufferReader` group (10, 11, 12, 15, 17) that lifts raw bytes
  with no ELF.
- The `.pyi` stubs in [`strider/`](strider/) are the typed reference for every
  module.

## Lifting raw bytes

`strider.reader.BufferReader(base_addr, data)` serves bytes as the memory
mapped at `base_addr`, and is all `strider.lift.lifter` needs. The guide's
[Beyond ELF](../../docs/python-guide.md#beyond-elf-custom-code-and-data) section
shows it, and
[`examples/python/10_buffer_reader.py`](examples/python/10_buffer_reader.py)
onward cover the multi-arch, ROM-folding, and firmware-carving variants.
