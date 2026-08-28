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
   bytes are read.
2. **Lift** each machine instruction into a simpler, CPU-independent form
   (using GHIDRA's Sleigh engine).
3. Build a **CFG**, the map of which regions (straight runs of instructions)
   can jump to which. Each address decodes once, in the ISA mode carried by the
   edge that reached it, so ARM and Thumb code in one binary each decode
   correctly. Two edges reaching one address in different modes is reported by
   `cfg.isa_mode_conflicts()`, since only one of them can win.
4. Build the **IR**, a graph where every value the function computes is a node
   and every dependency is an edge. This is the thing you query.
5. **Optimize** the IR so equivalent code always looks the same, which makes
   patterns simple to write.
6. **Resolve** the indirect branches: classify each one against the optimized
   IR, feed the targets back, re-lift, and repeat until the edge set stops
   changing. What is left over is reported, never raised; it arrives through
   four channels, described in
   [python-api.md](python-api.md#12-the-cfg-stridercfg).
7. **Query** it: describe a shape, get back every match with the values you
   asked to capture.

The [quickstart](../README.md#quickstart) in the README is that pipeline in
eight statements of Python. [CHANGELOG.md](../CHANGELOG.md) lists what 0.2.0
added over 0.1.0, breaking entries first.

## Where to go next

- **[vocabulary.md](vocabulary.md)** defines the terms (region, phi, dominator,
  varnode, ...). Start here if any of them are new.
- **[python-guide.md](python-guide.md)** is the practical walkthrough: analyzing
  many functions, writing patterns, constraints, rewrites, drawing the graph,
  and what to check when a pattern does not match.
- **[python-api.md](python-api.md)** is the reference for every user-facing
  Python API, with a runnable example of each.
- **[optimizations.md](optimizations.md)** explains the passes `analyze` runs,
  the usual reason a pattern's shape differs from the source.
- **`crates/strider-py/examples/python/`** has seventeen runnable scripts,
  numbered from a quickstart up to custom target ABIs.
- The top-level **[README](../README.md)** is the dense reference once you are
  past this page.
