# Using the Python API

A practical walkthrough of the Python interface: set up a handle, analyze
functions, write queries, read results, relate and rewrite them, and draw the
graph. It assumes the terms from [vocabulary.md](vocabulary.md).

## Install

Follow [Install](../README.md#install) in the README. Then `import strider`
works inside `uv run python ...`.

## The handle: one object per binary

Everything starts from a *lifter handle*. For an ELF file, `load_elf` builds
one and detects the CPU and calling convention for you:

```python
import strider

prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
```

`prog` knows the binary's symbols and memory:

```python
prog.symbol("array_sum")     # a Symbol: name, address, size, end, is_function, region
prog.symbols()               # {name: Symbol} for all of them
prog.functions()             # one Symbol per function address, address order
prog.symbol_at(0x401234)     # the Symbol covering an address, or None
prog.entry_point()           # the ELF entry point
prog.read(addr, 16)          # raw bytes, or None if unmapped
```

## Analyzing a function

`analyze` does the whole pipeline (CFG, lift, optimize, resolve branches) in one
call. It takes a symbol name or an address:

```python
cfg, function, unresolved = prog.analyze("array_sum")
```

The result unpacks into three things:

- `cfg`: the control flow graph the function was lifted from.
- `function`: the lifted, optimized IR. This is what you query.
- `unresolved`: addresses of indirect branches Strider could not resolve. An
  empty list is the normal case; a non-empty one is information, not an error.

You can also keep the result whole and use `result.cfg` / `result.function` /
`result.unresolved`.

**Options** go through `LifterOptions`. The common one is CFG behavior:

```python
opts = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
)
cfg, function, unresolved = prog.analyze("array_sum", opts=opts)
```

**One handle analyzes many functions.** The handle is the fixed setup:

```python
for sym in prog.functions():
    cfg, function, unresolved = prog.analyze(sym.address)
    ...
```

## Writing a query

A query is a pattern plus captures:

```python
from strider.pattern import load, int_add, call, var, Capture
```

A pattern describes a shape and leaves holes where you do not care. Captures are
the holes you want to read back, made with `Capture("name")` (or `Capture()`
for a fresh anonymous one). Two `Capture("off")` intern to one variable, and
you read a capture back by the object or by its name string. A bare string is
NOT a capture:

```python
# Every load of the form `base + offset`; read the offset back.
base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, off)), ignore_casts=True):
    print("offset =", hit[off].uint_opt)   # None if it is not constant
```

`ignore_casts=True` is `CastMask.all()`: the matcher walks through zero-extends,
sign-extends, truncations, and the two bit reinterpretations
`int_bits_to_float` / `float_bits_to_int`. Pass a `CastMask` instead of a bool
to pick a subset.

`find_all` returns every match. `find_unique` returns the single match and
raises if there is not exactly one.

### Reading a match

A `Match` carries every capture. Each accessor raises if the capture is
unbound or its node has no value of that kind; the `_opt` form returns `None`
there instead. Guard with `hit.has("off")` when a capture may be absent.

```python
hit[off].uint                # unsigned integer constant (raises if not one)
hit[off].uint_opt            # ... or None instead of raising
hit[off].sint                # signed version
hit[base].node               # the matched Node, for deeper inspection
hit[base].asm_fingerprint    # machine addresses that produced it
hit[base].op_opt             # operation name, e.g. "Add", or None
```

Index by the `Capture` object, or by its name when it has one (`hit["off"]`).
A numeric capture also converts and compares directly, so `int(hit[off])` and
`hit[off] == 0x10` work. The same readers exist as `Match` methods taking the
capture (`hit.uint(off)`) when that reads better.

A capture can land on a node with no operation and no fingerprint; an
`InitialVar`, a register as it stood at entry, is the common case. Reach for
`op_opt` unless you already know the shape.

### Guards and joins

Attach a Python predicate with `.when(...)` to filter on computed conditions:

```python
c = Capture()
def aligned(m):
    v = m.uint_opt(c)
    return v is not None and v % 16 == 0

function.find_all(var(c).when(aligned))
```

Pass a *list* of patterns to `find_all` to join them on shared captures: only
bindings where all patterns match at once are returned, and each result is one
merged match:

```python
from strider.pattern import int_const

base, o1, o2 = Capture(), Capture(), Capture()
for m in function.find_all(
    [load(addr=int_add(var(base), int_const(o1))),
     load(addr=int_add(var(base), int_const(o2)))],
    ignore_casts=True,
):
    print("two fields off one base, at", m.uint(o1), "and", m.uint(o2))
```

## Constraints: relating matches by control flow

Joining on a shared capture ties patterns together by *identity* (the same node).
Constraints tie them by *control-flow relationship* instead: does one thing always
happen before another, which value does a phi pick on a given branch. Build them
from `strider.pattern.constraints` and pass them alongside a pattern list.

**Dominance.** `dominates(a, b)` holds when every path from entry to `b` first
passes through `a` (see [dominator](vocabulary.md)). Capture the two nodes, then
constrain them:

```python
from strider.pattern import call, Capture
from strider.pattern.constraints import dominates

alloc, use = Capture(), Capture()
for m in function.find_all(
    [call().target(0x1000).capture(alloc), call().target(0x2000).capture(use)],
    constraints=[dominates(alloc, use)],
):
    # the 0x2000 call is reached only after the 0x1000 call
    ...
```

**Which value a phi picks on a branch.** `phi_input_from_edge(phi, edge, value)`
holds when the value the `phi` merges along one branch `edge` equals the one bound
to `value`. All three are captures that patterns in the list bind: capture the
edge off an `if` with `capture_true` / `capture_false`, capture the phi, and bind
the candidate value with its own pattern (here, any integer constant):

```python
from strider.pattern import if_else, phi, int_const, Capture
from strider.pattern.constraints import phi_input_from_edge

edge, the_phi, v = Capture(), Capture(), Capture()
matches = function.find_all(
    [if_else().capture_true(edge), phi().capture(the_phi), int_const(v)],
    constraints=[phi_input_from_edge(the_phi, edge, v)],
)
for m in matches:
    print("on the taken branch the phi selects", m.uint(v))
```

`negate(c)` keeps matches where `c` does *not* hold, and `any_of([...])` /
`all_of([...])` combine several constraints.

When control flow is not the relation you need, write the rule yourself.
Subclass `JoinPredicate`, declare the captures it correlates, and return a bool:

```python
from strider.pattern import call, int_const, Capture
from strider.pattern.constraints import JoinPredicate

n = Capture("n")

class MultipleOfEight(JoinPredicate):
    def captures(self):       return [n]
    def constraint(self, m):  return m.uint(n) % 8 == 0

# Calls whose first argument is a constant multiple of eight.
function.find_all([call().arg(0, int_const(n))], constraints=[MultipleOfEight()])
```

`captures()` tells the matcher which bindings the rule reads, so it is only
consulted once those are bound.

## Rewriting the graph

`rewrite` replaces every match of a pattern with a new shape you build from the
captures. The `find` side is a `strider.pattern` pattern; the `replace` side is
a `strider.template` template, which covers the ops that can be built:

```python
from strider.pattern import Capture, int_add, int_const, var
from strider import template as t

x = Capture()
# Replace `x + 0` with `x` everywhere.
n = function.rewrite(find=int_add(var(x), int_const(0)), replace=t.var(x))
print("rewrote", n, "sites")
```

A bare `strider.pattern.Pat` is accepted on the `replace` side for
compatibility, but only its build-valid subset compiles.

Both the `find` and `replace` sides use `Capture` objects (a bare string is not
a capture). A rewrite can expose fresh simplifications, so re-run the optimizer
afterwards to tidy up:

```python
prog.optimize(function)   # collapse phi / dead-branch noise the rewrite exposed
```

Use `rewrite_all([(find, replace), ...])` to stage several rules at once. Every
rule is tried at every node, in order, and the first to fire at a node is what
the graph keeps: it redirects the matched root's uses and the rules after it
find nothing left to redirect. One call walks the graph once over a snapshot of
the nodes, so a rule whose output its own `find` side matches needs a second
call. `function.clone()` gives you a copy to rewrite without touching the
original. Example `03` walks through this end to end.

## Looking at the graph

The interactive explorer is the easiest way to look at a function, and the only
one that stays usable on large graphs: it shows the neighborhood around a node
and re-centers as you click, instead of drawing everything at once.

```python
prog.visualize(function)              # prints a local URL; blocks until Ctrl-C
prog.visualize(function, whole=True)  # the entire graph instead
```

Drag with the mouse or press the arrow keys to pan (hold shift for a longer
step); ctrl+wheel or `+` / `-` zooms, `f` fits the graph to the window and `0`
returns to 100%. Clicking a node re-centers on it, so a drag that ends over one
pans rather than following it.

The toolbar controls the render: depth, hub cap (a node with more consumers than
this is drawn but not expanded), max nodes, whether a node's inputs count toward
the hub cap, and whether to render pretty. They start at the `neighborhood_dot`
defaults except pretty, which the explorer opens on, and depth when
`visualize(depth=...)` seeds it; `reset` puts them back. The `whole` toggle
draws the entire graph, which the neighborhood knobs no longer apply to and
which a few thousand nodes can keep the layout engine busy on for a long
time.

It blocks the calling thread, and it reads the `Function` / `Cfg` on the thread
that BUILT them. Serving off another thread has rules, worked through in
[python-api.md](python-api.md#10-visualizing).

For a static picture, render the IR or the CFG to a self-contained HTML file:

```python
function.to_html("graph.html", pretty=True)   # the IR, register names resolved
function.to_dot("graph.dot")                   # Graphviz DOT instead
cfg.to_html("cfg.html")                        # the control flow graph
```

`pretty=True` inlines constants and resolves register names; omit it to see the
graph exactly as stored, which is what you want when debugging why a pattern did
not match. A full dump gets unwieldy on a big function, so render just the
neighborhood around one node instead:

```python
dot = function.neighborhood_dot(function.entry_node(), depth=2, pretty=True)
```

## When a pattern does not match

The usual reason is that optimization already rewrote the shape you expected.
The three that trip people up most:

- **Subtraction and "not equal" style ops are not primitives.** The lifter
  lowers `a - b` to `a + (-b)`, and `a != b` to `not (a == b)`. Use the alias
  constructors (`int_sub`, `int_le`, `float_ne`, ...) instead of building the raw
  shape.
- **Commutative ops try both orders for you.** The integer `int_add`, `int_mul`,
  `int_and`, `int_or`, `int_xor`, the float `float_add`, `float_mul`, and the
  commutative comparisons `int_eq`, `int_carry`, `int_scarry`, `float_eq` all
  match either operand order. The rest keep the order you wrote.
- **`phi()` matches any phi**, whatever register it carries; `phi_for(vn)`
  narrows to one. Use `mem_phi()` for the memory merge.

When a pattern still comes up empty, dump the raw graph
(`function.to_html("graph.html")` without `pretty`) and walk forward from the
entry looking for the actual shape.

## Beyond ELF: custom code and data

`load_elf` is a convenience. Underneath, a lifter needs two things: somewhere to
read instruction bytes, and optionally a read-only image for constant data. You
can supply both from Python, which is how you analyze firmware or any raw source.

The simplest code source is a block of bytes at a base address:

```python
arch = strider.sleigh.SleighArch.x86_64()
cc = strider.sleigh.CallingConvention.x86_64_systemv()
mem = strider.reader.BufferReader(0x8000, code_bytes)
lft = strider.lift.lifter(arch, mem)
cfg, function, unresolved = lft.analyze(0x8000, cc)
```

A plain lifter works by address and takes the calling convention on each
`analyze` call; names come from an ELF, which a raw blob has none of.

When the bytes are computed or streamed rather than a flat blob, subclass
`MemReader` and override `read`:

```python
class MyCode(strider.reader.MemReader):
    def read(self, addr, size):
        # Your bytes here: return exactly `size` bytes at `addr`,
        # or None if that range is unmapped.
        ...

lft = strider.lift.lifter(arch, MyCode())
```

To let the `LoadReadOnly` pass fold loads of fixed addresses (constants in
`.rodata`, a vector table, ...) into constants, pass a read-only image as `rom`.
Subclass `ReadOnlyMemory` the same way:

```python
class MyRodata(strider.reader.ReadOnlyMemory):
    def read(self, addr, size):
        # Your bytes here, but only where they are genuinely immutable:
        # return `size` bytes at `addr`, or None if that range is not read-only.
        ...

lft = strider.lift.lifter(arch, MyCode(), rom=MyRodata())
```

Now a `Load` from a constant address inside that ROM shows up as a constant in
the IR. Examples `02` (custom `MemReader`), `07` (ROM folding), and `08` (both
together) in `crates/strider-py/examples/python/` show these end to end.
