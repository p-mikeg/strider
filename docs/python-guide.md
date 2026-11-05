# Using the Python API

A walkthrough of the Python interface, assuming the terms in
[vocabulary.md](vocabulary.md).

## The handle: one object per binary

Everything starts from a *lifter handle*. `load_elf` builds one for an ELF
(an executable, a shared library or an unlinked `.o`), detecting the CPU and
calling convention:

```python
import strider

prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
```

`prog.symbol("array_sum")` returns a `Symbol` (name, address, size, ...);
[ElfLifter metadata](python-api.md#elflifter-metadata) lists the other
symbol and memory reads. For a stripped image, `prog.add_symbol_file(path)`
reads symbols from another file (a debug object, a `vmlinux`) and
`prog.add_symbols({name: addr})` takes your own. `prog.add_elf(path)` merges a
second image's bytes and symbols.

## Analyzing a function

`analyze` runs the whole pipeline (CFG, lift, optimize, resolve branches) on a
symbol name or an address:

```python
cfg, function, unresolved = prog.analyze("array_sum")
```

- `cfg`: the control flow graph.
- `function`: the optimized IR you query.
- `unresolved`: indirect branches Strider could not resolve, the
  `"unresolved_branch"` entries of `cfg.incomplete`
  ([python-api.md](python-api.md#12-the-cfg-stridercfg)).

Or keep the result whole: `result.cfg`, `result.function`, `result.unresolved`.

**Options** go through `LifterOptions`:

```python
opts = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
)
addr = prog.symbol("array_sum").address
cfg, function, unresolved = prog.analyze(addr, opts=opts)
```

By name, the symbol's size bounds the lift and drops
`allow_code_before_start_addr`; hence the address.

Other options:
[LifterOptions](python-api.md#liftlifteroptions-per-call-tuning).

**One handle analyzes many functions:**

```python
for sym in prog.functions():
    result = prog.analyze(sym.address)
    ...
```

**One handle per thread** for parallelism, each built over `prog.arch`,
`prog.reader()` and `prog.rom()`; see
[python-api.md](python-api.md#2-analyzing-a-function).

## Writing a query

A query is a pattern plus captures:

```python
from strider.pattern import load, int_add, int_const, call, var, Capture
```

Captures are the holes you read back: `Capture("name")`, or `Capture()` for
an anonymous one. Two `Capture("off")` are one variable.

```python
# Every load of the form `base + constant`; read the offset back.
base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, int_const(off))), ignore_casts=True):
    print("offset =", hit.uint(off))   # 4 and 8
```

`ignore_casts=True` (`CastMask.all()`) matches through extends, truncations,
`int_bits_to_float` and `float_bits_to_int`; a `CastMask` picks a subset.
`find_all` returns every match; `find_unique` raises unless there is exactly
one.

### Reading a match

Each accessor raises if the capture is unbound or has no value of that kind;
the `_opt` form returns `None`. `hit.has("off")` tests for a capture.

```python
hits = function.find_all(load(addr=int_add(base, off)), ignore_casts=True)
hit = next(h for h in hits if h.uint_opt(off) is not None)   # a constant offset
hit.uint(off)                # unsigned integer constant (raises if not one)
hit.uint_opt(off)            # ... or None instead of raising
hit.sint(off)                # signed version
hit.node(base)               # the matched Node, for deeper inspection
hit.asm_fingerprint(base)    # machine addresses recorded on it
hit.op_opt(base)             # operation name, e.g. "Add", or None
```

Pass the `Capture` or its name (`hit.uint("off")`);
[python-api.md](python-api.md#6-reading-a-match) lists every reader. A capture
on a node with no operation (an `InitialVar`) makes `op` raise; `op_opt` does
not.

### Guards and joins

`.when(...)` filters on a Python predicate:

```python
c = Capture()
def aligned(m):
    v = m.uint_opt(c)
    return v is not None and v % 16 == 0

function.find_all(var(c).when(aligned))
```

A *list* of patterns joins them on shared captures; each result is one merged
match:

```python
from strider.pattern import int_const

base, o1, o2 = Capture(), Capture(), Capture()
for m in function.find_all(
    [load(addr=int_add(var(base), int_const(o1))),
     load(addr=int_add(var(base), int_const(o2)))],
    ignore_casts=True,
):
    print("fields off one base, at", m.uint(o1), "and", m.uint(o2))
```

A join does not make the patterns pick different nodes: on `array_sum` this
prints four rows, two pairing a field with itself (`4 and 4`, `8 and 8`).

### One pattern, several shapes

`one_of` reports every matching alternative; `first_of` stops at the first.
Either can sit in any slot:

```python
from strider.pattern import one_of, load, int_add, var, Capture

base, off = Capture("base"), Capture("off")
# `base + offset`, or a bare pointer.
hits = function.find_all(
    load(addr=one_of([int_add(base, off), var(base)])),
    ignore_casts=True,
)
```

`field` is that alternation with a constant offset; `f.offset(m)` reads 0
for the bare arm:

```python
from strider.pattern import field, function_arg

struct_fn = prog.analyze("struct_field_load").function
f = field(function_arg(0))
for m in struct_fn.find_all(f.load()):
    print("reads p->field at", f.offset(m))   # 0, then 4
```

`code_ptr(x)` matches `x` or `x & -2` (the ISA-mode-bit mask of ARM
interworking and MIPS16). `.ordered()` turns off commutative matching.
`.any_input(p)` matches `p` in any input slot. `load().non_stack()` /
`load().heap_only()` filter by the address's root, separating spills from other
traffic.

## Constraints: relating matches by control flow

A shared capture ties patterns by identity; constraints from
`strider.pattern.constraints` tie them by control flow.

**Dominance.** `dominates(a, b)` holds when every path from entry to `b` passes
through `a` (see [dominator](vocabulary.md)):

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

**Which value a phi picks on a branch.** `phi_input_from_edge(phi, edge,
value)` holds when `phi`'s input along `edge` is `value`. All three are
captures; take the edge off an `if` with `capture_true` / `capture_false`:

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

`input_from` on `phi()` or `mem_phi()` writes the captures and constraint
for you:

```python
merged = phi().input_from(edge, int_const())
matches = function.find_all([if_else().capture_true(edge), merged],
                            constraints=merged.constraints())
```

`negate(c)` keeps matches where `c` does *not* hold, and `any_of([...])` /
`all_of([...])` combine several constraints.

For your own rule, subclass `JoinPredicate`:

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

[`JoinPredicate`](python-api.md#joinpredicate-your-own-logic) has the rules.

## Rewriting the graph

`rewrite` replaces every match of a `strider.pattern` `find` with a
`strider.template` `replace` built from the captures:

```python
from strider.pattern import Capture, int_mul, int_const, var
from strider import template as t

x = Capture()
# Strength-reduce the index scaling: `x * 4` becomes `x << 2`.
n = function.rewrite(find=int_mul(var(x), int_const(4)),
                     replace=t.int_shl(t.var(x), t.int_const(2)))
print("rewrote", n, "sites")   # 3, on array_sum
```

Target a shape the pipeline leaves: `find=int_add(var(x), int_const(0))`
finds 0 sites because `ConstantFold` already removed them. Re-run the
optimizer after a rewrite:

```python
function.optimize()   # collapse phi / dead-branch noise the rewrite exposed
```

`rewrite_all([(find, replace), ...])` walks a snapshot of the nodes once,
applying the first rule that fires at each, so a node is rewritten at most once;
a rule whose output its own `find` matches needs a second call.
`function.clone()` gives a copy to rewrite. See example `03`.

## Looking at the graph

`strider.explore.visualize(function)` serves an interactive explorer at a
local URL; [python-api.md](python-api.md#10-visualizing) has its options,
keys and toolbar. `function.to_text()` prints a header and one line per node,
stable across runs for `diff`. For a static, self-contained picture:

```python
function.to_html("graph.html", pretty=True)   # the IR, register names resolved
function.to_dot("graph.dot")                   # Graphviz DOT instead
cfg.to_html("cfg.html")                        # the control flow graph
```

Omit `pretty` to see the graph as stored, for debugging a non-matching
pattern. For a big function, render one node's neighborhood:

```python
dot = function.neighborhood_dot(function.entry_node(), depth=2, pretty=True)
```

## When a pattern does not match

Usually optimization rewrote the shape ([optimizations.md](optimizations.md)):

- **Subtraction and "not equal" are not primitives**: `a - b` is `a + (-b)`,
  `a != b` is `not (a == b)`. Use the aliases (`int_sub`, `int_le`, `float_ne`,
  ...). `int_sub(x, 16)` also matches the folded `x + (-16)`; with a captured
  constant, write `int_add(x, int_const(c))`, which binds `-16`.
- **Commutative ops try both orders**; the rest keep yours
  ([python-api.md](python-api.md#operators)).
- **`phi()` matches any phi**; `phi_for(vn)` narrows to one register's.
  `mem_phi()` matches the memory merge.

Otherwise dump the raw graph (`function.to_html("graph.html")`) and look for
the actual shape.

## Beyond ELF: custom code and data

A lifter reads code from a reader and, optionally, constant data from a
read-only image; both can come from Python. `BufferReader` serves `bytes`, a
`bytearray` or a sequence of ints at a base:

```python
arch = strider.sleigh.SleighArch.x86_64()
cc = strider.sleigh.CallingConvention.x86_64_systemv()
code_bytes = bytes([0x8D, 0x04, 0x37, 0xC3])   # lea eax, [rdi + rsi] ; ret
mem = strider.reader.BufferReader(0x8000, code_bytes)
lft = strider.lift.lifter(arch, mem)
cfg, function, unresolved = lft.analyze(0x8000, cc)
```

A plain lifter works by address and takes the convention per `analyze`.
For computed bytes subclass `MemReader`, and pass a `ReadOnlyMemory` as `rom=`
so `LoadReadOnly` folds fixed-address loads
([custom memory](python-api.md#custom-memory-readermemreader--readerreadonlymemory)).

Examples `02` (custom `MemReader`), `07` (ROM folding) and `08` (both) in
`crates/strider-py/examples/python/` run these end to end.
