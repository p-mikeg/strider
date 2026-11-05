# Getting started with Strider

Strider answers questions about a compiled function without source or debug
symbols: write a pattern in Python, get back every place it matches. The
[README](../README.md#quickstart) shows this end to end; this page maps the
pipeline.

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the function's bytes. Executables, shared libraries and unlinked
   object files load mapped, with relocations applied as bytes are read; an
   object file's undefined symbols get addresses of their own.
   [python-api.md](python-api.md#1-loading-a-binary) covers the loaders,
   symbols for a stripped image (`add_symbol_file`, `add_symbols`) and
   `relocated_got_is_rom`.
2. **Lift** each instruction into p-code, the CPU-independent form of GHIDRA's
   Sleigh. An instruction Sleigh leaves opaque (a syscall, a trap) is
   classified by a built-in ABI table, which `CfgOptions(call_other_abis=...)`
   overrides. A call is modelled by its calling convention, never by reading
   the callee.
3. Build a **CFG**: which regions (straight runs of instructions) jump to
   which. Each address decodes once, in the ISA mode of the edge that reached
   it, so ARM and Thumb code in one binary both decode; an edge arriving in
   the other mode is reported as `isa_mode_conflict`. Literal pools
   marked by `$d` mapping symbols are not decoded;
   `CfgOptions(data_ranges=...)` names ranges the symbols miss.
4. Build the **IR**, a graph with one node per computed value and one edge per
   dependency. Values carry their exact width, from the 1-bit `I1` up to
   `I512`. This is what you query; `function.to_text()` prints one line per
   node and `function.validate()` checks its invariants.
5. **Optimize** so equivalent code reaches a query in fewer shapes: constants
   fold, `x + (x << 1)` becomes `x * 3`, a phi carrying one value collapses
   ([optimizations.md](optimizations.md)). `LifterOptions(assumptions=...)`
   trades soundness for precision; `AssumptionOptions.none()` makes no
   unchecked claim ([python-api.md](python-api.md#2-analyzing-a-function)).
6. **Resolve** indirect branches: classify each against the optimized IR, feed
   the targets back, re-lift, repeat until the edges stop changing.
   `cfg.incomplete` lists what is left
   ([reasons](python-api.md#12-the-cfg-stridercfg)); `analyze` returns even
   when incomplete, so check it before trusting an absence.
7. **Query** it with patterns, or **rewrite** it with `strider.template`.
   [python-api.md](python-api.md#4-patterns) is the reference and
   [python-guide.md](python-guide.md) the walkthrough.

## Where the API lives

```python
strider.lift      # load_elf, lifter, LifterOptions, AssumptionOptions,
                  # AnalyzeResult
strider.ir        # Function and Node: the graph you query
strider.cfg       # Cfg (with Cfg.incomplete) and CfgOptions
strider.pattern   # the query DSL, plus .pattern.constraints for joins
strider.template  # the build side of a rewrite
strider.opt       # OptimizerPipeline and its passes
strider.reader    # BufferReader, Symbol, and the memory interfaces
strider.sleigh    # SleighArch, CallingConvention, CallOtherAbi, Vn, VnSpace
strider.StriderError    # the one top-level name
```

`strider.explore.visualize(fn)` serves the graph as a browser explorer
([python-api.md](python-api.md#10-visualizing)).

## Where to go next

- [vocabulary.md](vocabulary.md) defines the terms (region, phi, dominator,
  varnode, ...).
- [python-guide.md](python-guide.md) walks through analyzing many functions,
  patterns, constraints, rewrites, drawing the graph, and debugging a pattern
  that does not match.
- The [README](../README.md) indexes the other guides and the runnable
  examples; its [Upgrading](../README.md#upgrading) section lists the changes
  that alter an answer silently.
