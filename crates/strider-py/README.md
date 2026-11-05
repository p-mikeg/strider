# strider-py

Strider's Python bindings: load a binary, lift and optimize a function, and
query it with patterns.

Build it from the workspace root, not this directory; the
[README](../../README.md#install) has the commands and the
[quickstart](../../README.md#quickstart).

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

The [README](../../README.md#documentation) indexes the guides;
[`examples/python/`](examples/python/) has runnable scripts, and the `.pyi`
stubs in [`strider/`](strider/) are the typed reference.
