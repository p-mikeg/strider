# Getting started with Strider

Strider reads a compiled program and lets you ask precise questions about what a
function does: what offset it reads off a pointer, what value it passes to
`malloc`, what it returns when the input looks a certain way. No source code and
no debug symbols needed.

An executable, a shared library, or an unlinked object file all load. Relocations
are applied as the bytes are read, so a `call` whose target the linker had not
filled in yet still resolves; in an `ET_REL` object, sections that share an
address are rebased apart so their symbols do not collide.

You write those questions as small patterns in Python and Strider finds every
place in the function that matches.

## How it works

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

1. **Read** the bytes of a function out of the binary.
2. **Lift** each machine instruction into a simpler, CPU-independent form
   (using GHIDRA's Sleigh engine).
3. Build a **CFG**, the map of which regions (straight runs of instructions)
   can jump to which. Each address decodes once, in the ISA mode the function
   runs in, so ARM and Thumb code in one binary each decode correctly.
4. Build the **IR**, a graph where every value the function computes is a node
   and every dependency is an edge. This is the thing you query.
5. **Optimize** the IR so equivalent code always looks the same, which makes
   patterns simple to write.
6. **Resolve** the indirect branches: classify each one against the optimized
   IR, feed the targets back, re-lift, and repeat until the edge set stops
   changing. A resolved target carries the ISA mode it decodes in. Anything left
   over, including any site whose answer shrank between rounds, is the
   `unresolved` list, so a converged CFG is never silently missing an edge.
7. **Query** it: describe a shape, get back every match with the values you
   asked to capture.

If words like *CFG*, *IR*, *region*, or *phi* are new, read
[vocabulary.md](vocabulary.md) first.

## A first taste

```python
import strider
from strider.pattern import load, int_add, Capture

# Load a binary. `load_elf` figures out the CPU and calling convention itself.
prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")

# Lift and optimize one function, by name or by address.
cfg, function, unresolved = prog.analyze("array_sum")

# Ask: where does this function load from `pointer + offset`?
base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, off)), ignore_casts=True):
    print("offset =", hit[off].uint_opt)  # None if the offset is symbolic
```

Load, analyze, query. Everything else is writing richer questions.

## Where to go next

- **[vocabulary.md](vocabulary.md)** defines the terms (phi, region,
  dominator, varnode, ...).
- **[python-guide.md](python-guide.md)** is the practical walkthrough of the
  Python API: analyzing many functions, writing patterns, reading results,
  rewriting the graph, drawing it, and what to check when a pattern does not
  match.
- **[optimizations.md](optimizations.md)** explains the passes `analyze` runs,
  which is the usual reason a pattern's shape looks different from the source.
- **[python-api.md](python-api.md)** is the reference for every user-facing
  Python API, with a runnable example of each.
- **`crates/strider-py/examples/python/`** has seventeen runnable example
  scripts, numbered from a quickstart up to custom target ABIs.
- The top-level **[README](../README.md)** is the dense reference once you are
  past getting started.
