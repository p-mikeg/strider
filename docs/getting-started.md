# Getting started with Strider

Strider reads a compiled program and lets you ask precise questions about what a
function does: what offset it reads off a pointer, what value it passes to
`malloc`, what it returns when the input looks a certain way. No source code and
no debug symbols needed. You write those questions as small patterns in Python,
and Strider finds every place in the function that matches.

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the bytes of a function out of the binary. An executable, a shared
   library, or an unlinked object file all load, with relocations applied as the
   bytes are read. The image is mapped rather than copied, so it must not change
   on disk while a handle over it lives;
   [python-api.md](python-api.md#1-loading-a-binary) has that and the knobs over
   it. Sections of an object file that shared an address are rebased apart,
   which moves every `ET_REL` symbol address, and a ppc64 ELFv1 function symbol
   is followed through its `.opd` descriptor to the code it names. A stripped
   binary can borrow names from elsewhere: `add_symbol_file` takes a debug file,
   `add_symbols` takes a dict, so a `System.map` you have parsed into one.
2. **Lift** each machine instruction into a simpler, CPU-independent form
   (using GHIDRA's Sleigh engine). An instruction Sleigh leaves opaque, like a
   syscall or a trap, is classified by a built-in ABI table saying whether it
   returns and what it clobbers; `CfgOptions(call_other_abis=...)` overrides an
   entry.
3. Build a **CFG**, the map of which regions (straight runs of instructions)
   can jump to which. Each address decodes once, in the ISA mode carried by the
   edge that reached it, so ARM and Thumb code in one binary each decode
   correctly. Two edges reaching one address in different modes is reported by
   `cfg.isa_mode_conflicts()`, since only one of them can win.
4. Build the **IR**, a graph where every value the function computes is a node
   and every dependency is an edge. Values carry their exact width, from the
   1-bit `I1` up to `I512`, so the odd widths SIMD and long-double code produce
   get their own types instead of being rounded to a machine word. This is the
   thing you query.
5. **Optimize** the IR so equivalent code always looks the same, which makes
   patterns simple to write. Equivalent shapes really do collapse, so a pattern
   written against the source shape often will not match: `x + x*2` arrives as
   `x*3`, and [optimizations.md](optimizations.md) lists what each pass
   reshapes. How far it goes is set by
   `LifterOptions(assumptions=AssumptionOptions(...))`, six claims the IR cannot
   check, of which `AssumptionOptions.none()` is the sound floor;
   [python-api.md](python-api.md#2-analyzing-a-function) says what each buys.
   `LifterOptions(pipeline=...)` takes a `strider.opt.OptimizerPipeline` and
   replaces the pass list outright.
6. **Resolve** the indirect branches: classify each one against the optimized
   IR, feed the targets back, re-lift, and repeat until the edge set stops
   changing. What is left over is reported, never raised; it arrives through
   four channels, described in
   [python-api.md](python-api.md#12-the-cfg-stridercfg). `cfg.is_complete()`
   tests all four at once.
7. **Query** it: describe a shape, get back every match with the values you
   asked to capture. `one_of` and `first_of` spell alternatives inside one
   pattern, over an empty list too, which matches nothing rather than raising;
   a list of patterns joins on the captures they share, and `constraints=`
   relates the halves by control flow (`dominates`) or by your own
   `JoinPredicate`. Arguments index by ABI position with floats in a space of
   their own, so `function_arg(0)` and `function_arg_float(0)` name different
   registers.
8. **Rewrite** it, if you want the graph changed rather than read.
   `function.rewrite(find=, replace=)` matches with the same pattern language
   and rebuilds with `strider.template`; `function.rewrite_all` stages several
   rules in one walk. Both return how many sites fired.

## Where the API lives

Everything is under a submodule named for what it does, and importing from the
home submodule is the supported spelling:

```python
strider.lift      # the entry point: load_elf and lifter, plus LifterOptions,
                  # AssumptionOptions and the AnalyzeResult they produce
strider.ir        # Function and Node: the graph you query
strider.cfg       # Cfg, CfgOptions, and three of the four incompleteness
                  # channels; the fourth, unresolved, rides on AnalyzeResult
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

The handle itself is pinned to the thread that built it: `analyze`,
`build_cfg`, `optimize` and the rest raise `StriderError` from anywhere else,
so a background worker builds its own handle over the same `arch` / `reader()`
/ `rom()`. The handle moves and drops on any thread; only decoding is pinned.
`analyze` runs without the GIL, so such a worker really does run alongside you.

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
