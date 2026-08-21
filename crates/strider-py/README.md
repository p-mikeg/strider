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

# Captures are `Capture` objects; read one back by the object or its name.
base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, off)), ignore_casts=True):
    print("offset =", hit.uint_opt(off))   # None if it is not a constant
```

## The module layout

Every class and free function lives in a domain submodule; `StriderError` is
the one top-level name.

| Module | Holds |
|---|---|
| `strider.lift` | `load_elf`, `lifter`, `Lifter` / `ElfLifter`, `LifterOptions`, `AnalyzeResult` |
| `strider.ir` | `Function`, `Node` |
| `strider.cfg` | `Cfg`, `CfgOptions` |
| `strider.sleigh` | `SleighArch`, `CallingConvention`, `Vn` |
| `strider.reader` | `BufferReader`, `MemReader`, `ReadOnlyMemory` |
| `strider.pattern` | the match DSL: `Pat`, `Capture`, `Match`, the builders, `.constraints` |
| `strider.template` | the build side of a rewrite: `Template` |
| `strider.opt` | `OptimizerPipeline` and the individual passes |
| `strider.explore` | the CFG / IR explorer `Lifter.visualize` serves; `shutdown(port)` stops it |

## Learn more

- The guides in [`../../docs/`](../../docs/): getting started, vocabulary, the
  full Python walkthrough, the complete Python API reference, and the optimizer
  passes.
- [`examples/python/`](examples/python/): sixteen runnable scripts, from a
  quickstart to the `BufferReader` group (10, 11, 12, 15) that lifts raw bytes
  with no ELF.
- The `.pyi` stubs in [`strider/`](strider/) are the typed reference for every
  module.

## Lifting raw bytes

`BufferReader(base_addr, data)` serves a `bytes` object as the memory mapped at
`base_addr`, and is all `strider.lift.lifter` needs:

```python
import strider
from strider import reader, sleigh
from strider.pattern import int_add, Capture

# lea eax, [rdi + rsi] ; ret, padded so the disassembler can prefetch
mem = reader.BufferReader(0x400000, bytes([0x8D, 0x04, 0x37, 0xC3]) + bytes(16))
lft = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)
_cfg, fn, _ = lft.analyze(
    0x400000, sleigh.CallingConvention.x86_64_systemv(),
    opts=strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)),
)
print(fn.find_all(int_add(Capture(), Capture())))  # the add from the lea
```

See [`examples/python/10_buffer_reader.py`](examples/python/10_buffer_reader.py)
onward for the multi-arch, ROM-folding, and firmware-carving variants.
