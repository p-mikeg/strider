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
   `unresolved` list. Three more channels carry the rest of what a converged CFG
   cannot vouch for -- `unverified_seeded_sites`, `isa_mode_conflicts` and
   `interior_branch_targets` -- and a consumer asking whether a result may be
   incomplete reads all four.
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

## New in 0.2.0

The pipeline above is the same one 0.1.0 ran; these are the parts that were not
there. [CHANGELOG.md](../CHANGELOG.md) lists every change, breaking ones first.

- **Alternation and joins in patterns.** `one_of` / `first_of` put a choice in
  any slot, and passing `find_all` a *list* of patterns joins them on shared
  `Capture`s. `constraints=` then filters those joins by control-flow relation
  (`dominates`, `phi_input_from_edge`).
- **`analyze` returns an `AnalyzeResult`** with `.cfg` / `.function` /
  `.unresolved`, which still unpacks as the three-tuple above.
- **Float and vector argument registers** are part of every ABI. Float
  parameters are counted in their own sequence, so a float argument reads back
  as `function_arg_float(i)`; `function_arg(i)` stays the integer sequence.
- **Tunable memory precision.** `LifterOptions(assumptions=AssumptionOptions(...))`
  holds the six claims the IR cannot prove, each one able to make the answer
  wrong on valid input. `stack_global_disjoint` and
  `assume_incoming_args_survive_calls` default on; the other four default off.
  Clearing all six leaves only what the IR structurally proves.
- **Every width Sleigh emits is an IR type.** `I24`, `I40`, `I56`, `I72`,
  `I96`, `I112`, `F16` and `F128` join the set, so a function touching one lifts
  instead of failing outright.
- **A loaded image is mapped, not copied**, so a large object opens in tens of
  milliseconds and faults in only what you analyse. Set `STRIDER_NO_MMAP=1` to
  read it instead, which a network or 9p mount needs.
- **Unlinked object files load.** `load_elf` falls back to the section table
  for an `ET_REL` object, laying out the sections that all sit at address 0 and
  applying the file's relocations as it reads.
- **Indirect branches resolve in their own ISA mode**, so an ARM/Thumb
  interworking dispatch decodes each target in the mode that target runs in. A
  branch that cannot be settled comes back in `.unresolved` rather than as an
  error, and the CFG reports its own decode conflicts.
- **`CallOther` ABI overrides** let you describe what an unmodelled user-op
  reads, writes and clobbers, which is the remedy when a lift fails on one.
- **Calling-convention transforms** adapt a stock convention to the binary in
  front of you, including `arm_aapcs_soft` for a soft-float ARM image.
- **`symbol_at`** maps an address back to its symbol, and a `StriderError`
  carries a `.backtrace`.

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
- **[CHANGELOG.md](../CHANGELOG.md)** lists what changed in 0.2.0, and what
  breaks if you are coming from 0.1.0.
