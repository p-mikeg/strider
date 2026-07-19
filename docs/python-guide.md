# Using the Python API

A practical walkthrough of the Python interface: set up a handle, analyze
functions, write queries, read results, relate and rewrite them, and draw the
graph. It assumes the terms from [vocabulary.md](vocabulary.md).

## Install

The project uses [uv](https://docs.astral.sh/uv/). Run from the repository root:

```bash
uv sync --group dev       # create the virtualenv and install dependencies
uv run maturin develop    # build the Rust extension
uv run pytest             # optional: run the test suite
```

Then `import strider` works inside `uv run python ...`.

## The handle: one object per binary

Everything starts from a *lifter handle*. For an ELF file, `load_elf` builds
one and detects the CPU and calling convention for you:

```python
import strider

prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
```

`prog` knows the binary's symbols and memory:

```python
prog.symbol("array_sum")     # address of a symbol
prog.symbols()               # {name: address} for all of them
prog.functions()             # sorted list of function names
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

**One handle analyzes many functions.** The handle is the fixed setup; call
`analyze` as often as you like:

```python
for name in prog.functions():
    cfg, function, unresolved = prog.analyze(name)
    ...
```

## Writing a query

A query is a pattern plus captures. Import the constructors from
`strider.pattern`:

```python
from strider.pattern import load, add, call, var, Capture
```

A pattern describes a shape and leaves holes where you do not care. Captures are
the holes you want to read back. Use string names (auto-created per pattern) or
`Capture()` objects when you want to share one across patterns:

```python
# Every load of the form `base + offset`.
for hit in function.find_all(load(addr=add("base", "off")), ignore_casts=True):
    print(hit.const_uint("base"), hit.const_uint("off"))
```

`ignore_casts=True` tells the matcher to see through width casts (zero/sign
extends and truncations), which the compiler inserts constantly. Leave it on
unless you specifically care about a cast.

`find_all` returns every match. `find_unique` returns the single match and
raises if there is not exactly one.

### Reading a match

A `Match` carries every capture. The accessors return `None` when the capture
did not bind a value of that kind:

```python
hit.const_uint("off")        # unsigned integer constant, or None if symbolic
hit.const_int("off")         # signed version
hit.node("base")             # the matched Node, for deeper inspection
hit.asm_fingerprint("base")  # machine addresses that produced it
hit.op("base")               # operation name, e.g. "Add"
```

### Guards and joins

Attach a Python predicate with `.when(...)` to filter on computed conditions:

```python
c = Capture()
def aligned(m):
    v = m.const_uint(c)
    return v is not None and v % 16 == 0

function.find_all(var(c).when(aligned))
```

Pass a *list* of patterns to `find_all` to join them on shared captures: only
bindings where all patterns match at once are returned, and each result is one
merged match:

```python
from strider.pattern import any_int_const

base, off = Capture(), Capture()
for m in function.find_all(
    [load(addr=var(base)), load(addr=add(var(base), any_int_const(off)))],
    ignore_casts=True,
):
    print("second field at offset", m.const_uint(off))
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
    [call().at(0x1000).capture(alloc), call().at(0x2000).capture(use)],
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
from strider.pattern import if_else, phi, any_int_const, Capture
from strider.pattern.constraints import phi_input_from_edge

edge, the_phi, v = Capture(), Capture(), Capture()
matches = function.find_all(
    [if_else().capture_true(edge), phi().capture(the_phi), any_int_const(v)],
    constraints=[phi_input_from_edge(the_phi, edge, v)],
)
for m in matches:
    print("on the taken branch the phi selects", m.const_uint(v))
```

`negate(c)` keeps matches where `c` does *not* hold, and `any_of([...])` /
`all_of([...])` combine several constraints.

## Rewriting the graph

You can also change the IR, not just read it. `rewrite` replaces every match of a
pattern with a new shape you build from the captures:

```python
from strider.pattern import Capture, add, int_const, var

x = Capture()
# Replace `x + 0` with `x` everywhere.
n = function.rewrite(find=add(var(x), int_const(0)), replace=var(x))
print("rewrote", n, "sites")
```

The `find` side may use string-shorthand captures, but the `replace` side needs
real `Capture` objects: a bare string there would be ambiguous. A rewrite can
expose fresh simplifications, so re-run the optimizer afterwards to tidy up:

```python
prog.optimize(function)   # collapse phi / dead-branch noise the rewrite exposed
```

Use `rewrite_all([(find, replace), ...])` to stage several rules at once; they
apply in order, first match wins per node. `function.clone()` gives you a copy to
rewrite without touching the original. Example `03` walks through this end to end.

## Drawing the graph

Render the IR or the CFG to a self-contained HTML file you open in a browser:

```python
function.to_html("graph.html", pretty=True)   # the IR, register names resolved
function.to_dot("graph.dot")                   # Graphviz DOT instead
cfg.to_html("cfg.html")                        # the control flow graph
```

`pretty=True` inlines constants and resolves register names; omit it to see the
graph exactly as stored, which is what you want when debugging why a pattern
did not match.

For a large function, render just the neighborhood around one node:

```python
dot = function.neighborhood_dot(function.entry_node(), depth=2)
```

There is also an interactive explorer that opens in a browser and re-centers as
you click:

```python
prog.visualize(function)   # prints a URL and blocks until Ctrl-C
```

It blocks the calling thread. If you run it on a background thread, you must
call `strider.explore.shutdown(port)` and join the thread before the interpreter
exits, or the process aborts.

## When a pattern does not match

The usual reason is that optimization already rewrote the shape you expected.
The three that trip people up most:

- **Subtraction and "not equal" style ops are not primitives.** The lifter
  lowers `a - b` to `a + (-b)`, and `a != b` to `not (a == b)`. Use the alias
  constructors (`sub`, `int_le`, `float_ne`, ...) instead of building the raw
  shape.
- **Commutative ops try both orders for you.** `add`, `mul`, `and`, `or`, `xor`
  match either operand order automatically. Non-commutative ops keep the order
  you wrote.
- **`phi()` matches only a register-tagged phi.** Use `mem_phi()` for the
  memory merge.

When a pattern still comes up empty, dump the raw graph
(`function.to_html("graph.html")` without `pretty`) and walk forward from the
entry looking for the actual shape. The README's *Troubleshooting* section has
the full list.

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

A plain lifter works by address only (there is no symbol table) and takes the
calling convention on each `analyze` call.

When the bytes are computed or streamed rather than a flat blob, subclass
`MemReader` and override `read`:

```python
class MyCode(strider.reader.MemReader):
    def read(self, addr, size):
        # Return `size` bytes at `addr`, or None if that range is unmapped.
        return self.bytes_at(addr, size)

lft = strider.lift.lifter(arch, MyCode())
```

To let the `LoadReadOnly` pass fold loads of fixed addresses (constants in
`.rodata`, a vector table, ...) into constants, pass a read-only image as `rom`.
Subclass `ReadOnlyMemory` the same way:

```python
class MyRodata(strider.reader.ReadOnlyMemory):
    def read(self, addr, size):
        return self.rodata_at(addr, size)   # bytes, or None if not read-only

lft = strider.lift.lifter(arch, MyCode(), rom=MyRodata())
```

Now a `Load` from a constant address inside that ROM shows up as a constant in
the IR. Examples `02` (custom `MemReader`), `07` (ROM folding), and `08` (both
together) in `crates/strider-py/examples/python/` show these end to end.
