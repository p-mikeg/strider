# strider-py

Strider's Python bindings: load a binary, lift and optimize a function, and
query it with patterns.

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
from strider.pattern import load, int_add, int_const, Capture

prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
cfg, function, unresolved = prog.analyze("array_sum")

base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, int_const(off))), ignore_casts=True):
    print("offset =", hit.uint(off))   # 4 and 8
```

## The module layout

`StriderError` is the only top-level class; everything else lives in a
submodule.

| Module | Main names |
|---|---|
| `strider.lift` | `load_elf`, `lifter`, `Lifter` / `ElfLifter`, `LifterOptions`, `AssumptionOptions`, `AnalyzeResult` |
| `strider.ir` | `Function`, `Node` |
| `strider.cfg` | `Cfg`, `CfgOptions`, `DotStyle` |
| `strider.sleigh` | `SleighArch` (with `reg` / `reg_name`), `CallingConvention`, `CallOtherAbi`, `Vn`, `VnSpace` |
| `strider.reader` | `BufferReader`, `MemReader`, `ReadOnlyMemory`, `Symbol`, `SymbolIter` |
| `strider.pattern` | the match DSL: `Pat`, `Capture`, `Match`, the builders, `.constraints` |
| `strider.template` | the build side of a rewrite: `Template` |
| `strider.opt` | `OptimizerPipeline` and the individual passes |
| `strider.explore` | the CFG / IR explorer: `visualize(target)` serves it, `shutdown(port)` stops it |

## Learn more

- [`../../docs/`](../../docs/): getting started, vocabulary, the Python guide,
  the Python API by topic, and the optimizer passes.
- [`examples/python/`](examples/python/): runnable scripts.
- The `.pyi` stubs in [`strider/`](strider/): the typed reference.

## Lifting raw bytes

`strider.reader.BufferReader(base_addr, data)` serves `data` at `base_addr`,
which is all `strider.lift.lifter` needs. See the guide's
[Beyond ELF](../../docs/python-guide.md#beyond-elf-custom-code-and-data) section
and [`examples/python/10_buffer_reader.py`](examples/python/10_buffer_reader.py)
onward.
