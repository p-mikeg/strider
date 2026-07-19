# Getting started with Strider

Strider reads a compiled program (an executable or library, no source code or
debug symbols needed) and lets you ask precise questions about what a function
does: what offset it reads off a pointer, what value it passes to `malloc`,
what it returns when the input looks a certain way.

You write those questions as small patterns in Python and Strider finds every
place in the function that matches.

## How it works

Strider turns raw machine code into something queryable in a few steps:

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the bytes of a function out of the binary.
2. **Lift** each machine instruction into a simpler, CPU-independent form
   (using GHIDRA's Sleigh engine).
3. Build a **CFG**, the map of which regions (straight runs of instructions)
   can jump to which.
4. Build the **IR**, a graph where every value the function computes is a node
   and every dependency is an edge. This is the thing you query.
5. **Optimize** the IR so equivalent code always looks the same, which makes
   patterns simple to write.
6. **Query** it: describe a shape, get back every match with the values you
   asked to capture.

If words like *CFG*, *IR*, *region*, or *phi* are new, read
[vocabulary.md](vocabulary.md) first. It defines each term in a sentence or two.

## A first taste

```python
import strider
from strider.pattern import load, add

# Load a binary. `load_elf` figures out the CPU and calling convention itself.
prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")

# Lift and optimize one function, by name or by address.
cfg, function, unresolved = prog.analyze("array_sum")

# Ask: where does this function load from `pointer + offset`?
for hit in function.find_all(load(addr=add("base", "off")), ignore_casts=True):
    print("offset =", hit.const_uint("off"))
```

That is the whole loop: load, analyze, query. Everything else is writing richer
questions.

## Where to go next

- **[vocabulary.md](vocabulary.md)** defines the terms (phi, region,
  dominator, varnode, ...).
- **[python-guide.md](python-guide.md)** is the practical walkthrough of the
  Python API: analyzing many functions, writing patterns, reading results,
  rewriting the graph, drawing it, and what to check when a pattern does not
  match.
- **[optimizations.md](optimizations.md)** explains the passes `analyze` runs,
  which is the usual reason a pattern's shape looks different from the source.
- **`crates/strider-py/examples/python/`** has nine runnable example scripts,
  numbered from a quickstart up to custom memory readers.
- The top-level **[README](../README.md)** is the dense reference once you are
  past getting started.
