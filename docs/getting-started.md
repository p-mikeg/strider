# Getting started with Strider

Strider answers precise questions about a compiled function with no source and
no debug symbols: you write the question as a small pattern in Python and get
back every place in the function that matches. The
[README](../README.md#quickstart) has that end to end; this page is the map of
the pipeline behind it.

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the bytes of a function out of the binary. An executable, a shared
   library, or an unlinked object file all load, mapped rather than copied,
   with relocations applied as the bytes are read; an object file's undefined
   symbols get addresses of their own.
   [python-api.md](python-api.md#1-loading-a-binary) has the loaders, the
   symbol sources for a stripped image (`add_symbol_file`, `add_symbols`), and
   the knobs over the mapping.
2. **Lift** each machine instruction into p-code, GHIDRA's Sleigh engine's
   CPU-independent form. An instruction Sleigh leaves opaque, like a syscall or
   a trap, is classified by a built-in ABI table saying whether it returns and
   what it clobbers; `CfgOptions(call_other_abis=...)` overrides an entry, and
   `CallOtherAbi.custom(sleigh, ...)` names the registers one reads and writes.
3. Build a **CFG**, the map of which regions (straight runs of instructions)
   can jump to which. Each address decodes once, in the ISA mode carried by the
   edge that reached it, so ARM and Thumb code in one binary each decode
   correctly. Bytes that hold no instruction, such as a literal pool an ARM or
   AArch64 ELF's `$d` mapping symbols mark, are not decoded.
4. Build the **IR**, a graph where every value the function computes is a node
   and every dependency is an edge. Values carry their exact width, from the
   1-bit `I1` up to `I512`, so the odd widths SIMD and long-double code produce
   get their own types instead of being rounded to a machine word. This is the
   thing you query; `function.to_text()` prints it one line per node, and
   `function.validate()` checks its invariants. A call is modelled by its
   calling convention, never by reading the callee.
5. **Optimize** the IR so that equivalent code reaches a query in fewer shapes.
   Constants fold, `x + (x << 1)` becomes `x * 3`, and a phi or a cycle of phis
   carrying one value collapses, so a pattern written against the source shape
   often will not match. [optimizations.md](optimizations.md) lists what each
   pass reshapes; [python-api.md](python-api.md#2-analyzing-a-function) says
   what `LifterOptions(assumptions=...)` buys and what it costs, and
   `AssumptionOptions.none()` makes no unchecked claim.
   `LifterOptions(pipeline=...)` replaces the pass list outright.
6. **Resolve** the indirect branches: classify each one against the optimized
   IR, feed the targets back, re-lift, and repeat until the edge set stops
   changing. What is left over is reported, never raised, and so is a return
   whose target is not provably the caller's. `cfg.is_complete()` is the
   one-call question; [python-api.md](python-api.md#12-the-cfg-stridercfg)
   describes the six channels it reads.
7. **Query** it, or **rewrite** it if you want the graph changed rather than
   read. `field`, `code_ptr`, `first_of` and `PhiPat.input_from` cover common
   shapes, and `JoinPredicate` relates matches by your own rule.
   [python-api.md](python-api.md#4-patterns) is the reference for both sides
   and [python-guide.md](python-guide.md) the walkthrough.

## Where the API lives

Everything is under a submodule named for what it does, and importing from the
home submodule is the supported spelling:

```python
strider.lift      # the entry point: load_elf and lifter, plus LifterOptions,
                  # AssumptionOptions and the AnalyzeResult they produce
strider.ir        # Function and Node: the graph you query
strider.cfg       # Cfg, CfgOptions, and five of the six incompleteness
                  # channels; the sixth, unresolved, rides on AnalyzeResult
strider.pattern   # the query DSL, plus .pattern.constraints for joins
strider.template  # the build side of a rewrite
strider.opt       # OptimizerPipeline and the passes it runs
strider.reader    # BufferReader, Symbol, and the memory interfaces
strider.sleigh    # SleighArch, Sleigh, CallingConvention, CallOtherAbi, Vn,
                  # VnSpace; CallingConvention.custom(sleigh, ...) builds an
                  # ABI of your own out of register names
strider.StriderError    # the one top-level name
```

`prog.visualize(fn)` serves the graph as an interactive explorer in a browser
and is the quickest way to see the shape a pattern has to match;
[python-api.md](python-api.md#10-visualizing) has the view it opens on, the
keys and the toolbar.

A lift handle decodes only on the thread that built it; a worker thread builds
its own over the same `arch` / `reader()` / `rom()`. `analyze` releases the GIL,
so the two run in parallel.

## Where to go next

- **[vocabulary.md](vocabulary.md)** defines the terms (region, phi, dominator,
  varnode, ...). Start here if any of them are new.
- **[python-guide.md](python-guide.md)** is the practical walkthrough: analyzing
  many functions, writing patterns, constraints, rewrites, drawing the graph,
  and what to check when a pattern does not match.
- The [quickstart](../README.md#quickstart) is this pipeline end to end in
  Python, and the [README](../README.md) indexes the rest of the guides and the
  runnable examples. [CHANGELOG.md](../CHANGELOG.md) lists what 0.2.0 added over
  0.1.0, breaking entries first.
