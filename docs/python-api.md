# Python API reference

Every user-facing Python API, in depth. For the practical, task-first
walkthrough instead, read the [Python guide](python-guide.md).

The typed `.pyi` stubs under `crates/strider-py/strider/` are the source of
truth for exact signatures (and list every arithmetic operator, which this doc
groups rather than enumerates). Blocks showing a whole flow run against the
committed fixture ELFs in `fixtures/out/x86/`; the signature listings use
placeholder names.

The public surface is eight domain submodules plus one top-level error.
`explore` is bound too, and backs `visualize`, but is outside `__all__`:

```python
import strider
from strider import ir, lift, cfg, sleigh, reader, opt, pattern, template, explore
strider.StriderError      # the analysis error; bad arguments raise builtins
strider.__version__
```

---

## 1. Loading a binary

### `lift.load_elf` -- the common path

```python
prog = strider.lift.load_elf("fixtures/out/x86/switch.elf")
# Detects the architecture and calling convention from the ELF header.
# Override for kernels or custom ABIs:
#   load_elf(path, arch=sleigh.SleighArch.x86_64(), cc=sleigh.CallingConvention.x86_64_systemv())
# from_segments=True (default) walks PT_LOAD; False forces per-section regions.
# apply_relocations=True (default) patches relocations in place, and also
# selects what is mapped: False drops the writable sections entirely rather
# than serving their on-disk bytes.
```

`load_elf` returns an `ElfLifter`: a lifter that also carries the symbol table,
the loaded memory, and a default calling convention.

An unlinked object file (`ET_REL`) has no program headers, so it loads from
sections whatever `from_segments` says. Its sections are pre-link, and typically
all sit at `sh_addr` 0; Strider rebases the collisions apart the way a linker
would. Every address you get back is that synthetic one, not a file offset and
not an address the object will ever be loaded at, so `prog.symbol("f")` on a
`.o` is only comparable against other addresses from the same load.

### `lift.lifter` -- raw bytes, no ELF

```python
mem = strider.reader.BufferReader(0x1000, b"\x48\x01\xd8\xc3")  # add rax,rbx; ret
lift = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)     # rom=... optional
```

`lifter(arch, mem, rom=None)` builds a plain `Lifter`. `mem` is the instruction
source; `rom` is optional read-only memory for constant folding.

### Custom memory: `reader.MemReader` / `reader.ReadOnlyMemory`

Subclass and override `read(addr, size)` to feed the pipeline from anything:

```python
class Firmware(strider.reader.MemReader):
    def __init__(self, blob): self.blob = blob
    def read(self, addr, size):
        off = addr - 0x8000_0000
        return self.blob[off:off + size] if 0 <= off else None

blob = open("fixtures/out/arm/arithmetic.elf", "rb").read()
fw = strider.lift.lifter(sleigh.SleighArch.arm(), Firmware(blob))
```

`ReadOnlyMemory` is the same shape, used as the `rom=` argument so
`LoadReadOnly` can fold loads from constant addresses. `BufferReader` works as
either. The `MemLike` / `RomLike` type aliases name what each argument accepts.

---

## 2. Analyzing a function

`analyze` lifts, optimizes, and resolves one function.

```python
result = prog.analyze("dispatch_value")     # by symbol name (ElfLifter) or address
cfg, function, unresolved = result           # an AnalyzeResult, a named 3-tuple
result.cfg, result.function, result.unresolved   # or read the fields
# unresolved: machine addresses of indirect branches that stayed unresolved
# (a list, not an error).
```

A plain `Lifter` needs an address and a calling convention:

```python
_cfg, fn, _u = lift.analyze(0x1000, sleigh.CallingConvention.x86_64_systemv())
```

### `lift.LifterOptions` -- per-call tuning

```python
opts = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(
        allow_code_before_start_addr=True,
        function_max_size=0x400,       # how far past the entry to decode; None is unbounded
        known_targets={0x1234: [0x2000, 0x2010],   # this branch goes to these
                       0x1250: "return"},          # and this one returns
        call_other_abis={"trap": sleigh.CallOtherAbi.no_return()},  # reclassify a user-op
    ),
    assumptions=strider.lift.AssumptionOptions(
        escape_analysis=False,            # True forwards private-frame spills across calls
        noalias_allocators=[],            # callee addresses of malloc-like allocators
        distinct_sp_bases_disjoint=False, # True: a store off another SP base cannot alias
        callee_preserves_stack_args=False,# True: a callee leaves the argument slots alone
    ),
    compact=True,                      # drop unreachable nodes at the end
    alias_mode="stack_global_disjoint",  # or "strict", the structural floor
    assume_incoming_args_survive_calls=True,  # False: a call shadows the slot an arg arrived in
    resolve_indirect_branches=True,    # False leaves every site an IndirectBranch placeholder
    per_address_ccs=None,              # {call_addr: CallingConvention} overrides
    pipeline=None,                      # replace the optimizer pipeline for this call
)
prog.analyze("dispatch_value", opts=opts)
```

Everything in `AssumptionOptions` is a claim about the code being analyzed that
strider cannot check, so a wrong one can make the answer wrong. `alias_mode` is
separate: `"strict"` is sound under any input as long as `noalias_allocators` is
empty, and the default `"stack_global_disjoint"` additionally assumes no
constant address equals `sp + K` at runtime.

`assume_incoming_args_survive_calls` and
`AssumptionOptions(distinct_sp_bases_disjoint=...)` together decide how an
incoming argument is found: whether a later call shadows the slot it arrived in,
and whether a store rooted at an SP base other than the entry SP (an
alignment-masked `sp & -16` frame local, say) counts as disjoint from it. The
defaults are survival on and disjointness off, so an argument survives a later
call and a differently-based store is treated as a possible alias.

`callee_preserves_stack_args=True` empties the outgoing-argument window,
so a value spilled at the stack top forwards across a call. The psABIs let a
callee write the slots holding its own parameters, so this asserts something
about compiler output rather than proving it.

`call_other_abis` reclassifies a Sleigh user-op by name ahead of the built-in
table, for an op strider reads wrongly or an OS convention it cannot know. Each
value is a `strider.sleigh.CallOtherAbi`: one of the four footprint-free classes
`CallOtherAbi.noop()` / `.pure()` / `.mem_clobber()` / `.no_return()`, or

```python
strider.sleigh.CallOtherAbi.custom(
    sl,                           # a Sleigh; resolves the register names here and now
    implicit_reads=["RAX", "RDI"],   # read beyond the p-code operands
    implicit_writes=["RAX"],         # written beyond the p-code result
    clobbers_memory=True,
    no_return=False,
)
```

for one naming implicit registers. An unknown register name raises
`StriderError` at construction. Like a calling convention, an ABI stated here
holds for this analysis only, so two analyses of the same binary can disagree
about what `syscall` reads.

`lift.user_op_names()` lists every user-op name the architecture can emit, and
`lift.call_other_abi(name)` reads back the classification in force --
the built-in one, or the `opts` entry when `lift.call_other_abi(name, cfg_opts)`
is given one. `None` means strider has no answer for the name, which fails the
lift of any function containing it.
`crates/strider-py/examples/python/17_custom_abis.py` runs the discovery, then
analyses one `int 0x80` stub with and without an override.

`AssumptionOptions(escape_analysis=True)` buys precision by assuming two things
the analysis cannot
always see. No callee returns a struct by value, because an sret hidden pointer
is a frame-address escape. And the outgoing-argument window is read off the
argument stores the caller is seen to make, so a store hidden behind an earlier
call ends the window early and a load from a slot the next callee owns can
forward across that callee. Both hold for ordinary compiler output; leave the
knob off for hand-written or obfuscated code.

### ElfLifter metadata

```python
prog.arch                      # SleighArch
prog.cc                        # default CallingConvention
prog.endianness                # "little" or "big"
prog.entry_point()             # ELF entry address
list(prog.functions())         # one Symbol per function address, address order
list(prog.iter_symbols())      # every Symbol, pulled one at a time
prog.symbols()                 # {name: Symbol}

prog.symbol("f")               # Symbol; raises if undefined
prog.symbol_opt("f")           # ... or None
prog.symbol_at(0x401234)       # the Symbol covering an address, or None

prog.read(addr, size)          # raw bytes, or None when unmapped
prog.reader()                  # the BufferReader over the loaded sections
prog.rom()                     # the read-only image LoadReadOnly folds from, or None
prog.add_elf("libc.so")        # merge another ELF (shared library)
```

A `Symbol` carries `name`, `address`, `size`, `end`, `is_function` and
`region`. `size` is `None` when the ELF records no extent (`st_size == 0`),
which a hand-written `.S` entry point with no `.size` directive hits, so it is
not the same as a zero-length symbol; `end` is `None` there too. `region` is
the `(start, end)` of the loaded region the symbol maps into, such as `.text`.

`symbol_at` takes the nearest symbol at or below the address whose recorded
extent reaches it. A symbol with no recorded size covers only its own address,
and aliases sharing an address resolve to the code symbol among them.

---

## 3. The `Function` and its `Node`s

```python
function.node_count()          # total node ids
function.entry_node()          # id of the Entry node
function.node_ids()            # every id
n = function.node(some_id)     # a Node handle
```

A `Node`:

```python
n.id                           # stable id (invalidated by optimize)
n.kind()                       # "IntBinaryOp(Add)", "Region", "Load(RAM)"
n.op()                         # "Add" / "Less" / ... or None for op-less kinds
n.value_type()                 # "I64" / "I1" / "F64" / ... or None
n.inputs(), n.outputs()        # neighbouring nodes (Node handles, not ids)
n.uint(), n.sint(), n.boolean(), n.float_bits()   # constants
n.vn()                         # varnode for an InitialVar / clobber, else None
n.asm_fingerprint()            # machine addresses that produced this node
```

Walking the graph (each returns a list of `Node`):

```python
function.data_walk()             # every reachable node, pre-order
function.cfg_walk()              # the control-flow skeleton (control edges only)
function.mem_walk()              # memory-touching nodes along the memory chain
function.walk(some_node_id)      # everything reachable from one node
```

Rendering:

```python
function.to_dot(pretty=True)                 # Graphviz DOT text
function.to_html("graph.html")               # dark-themed standalone page (path=None returns text)
function.neighborhood_dot(function.entry_node(), depth=2, pretty=True)  # local subgraph DOT
```

---

## 4. Patterns

A pattern is a shape to match, built from `strider.pattern`.

```python
from strider import pattern as p
```

### Leaves

```python
p.anything()                   # matches any value
p.var(p.Capture("x"))          # a wildcard that captures (var takes a Capture object,
                               # not a bare string)
p.int_const(8)                 # the integer constant 8 (any width)
p.int_const([0x10, 0x20])      # any constant from a set
p.any_int()                    # any integer-typed node, constant or not
p.int_const()                  # any integer constant
p.int_const(c)                 # ... capturing it
p.int_const_any_width(-1)      # match the value at whatever width it was extended from
p.bool_const(True); p.bool_const()
p.float_const(bits); p.float_const()
p.initial_var()                # an initial register/stack read
p.initial_var_for(vn)          # ... a specific varnode
p.float_is_nan(x)              # the IEEE self-inequality shape
```

A **raw int** anywhere an operand is expected is `int_const(that int)`, so
`p.int_add(c, 8)` is `p.int_add(c, p.int_const(8))`. Narrow the width by hand with
`p.int_const(8).of_width(32)` when you need it.

### Operators

Every integer / float / boolean operator is a free function; the `.pyi` lists
them all. The shapes match the canonical IR (see
[optimizations](optimizations.md)), and the alias constructors build the
canonical form for you: `int_sub(a, b)` is `int_add(a, int_neg(b))`, `int_le`
and `int_ne` are their lowered comparisons.

```python
p.int_add(a, b); p.int_mul(a, b); p.int_neg(a); p.int_and(a, b); p.int_xor(a, b)
p.int_shl(a, b); p.int_shr(a, b); p.int_sshr(a, b)
p.int_eq(a, b); p.int_lt(a, b); p.int_slt(a, b); p.int_ne(a, b)     # -> I1
p.float_add(a, b); p.float_lt(a, b); p.float_sqrt(a)
p.int_zero_extend(x); p.int_sign_extend(x); p.int_truncate(x)                   # width casts
```

`int_add`, `int_mul`, `int_and`, `int_or`, `int_xor`, `float_add`, `float_mul`,
`int_eq`, `int_carry`, `int_scarry` and `float_eq` match **commutatively**;
every other operator, `int_shl` / `int_shr` / `int_lt` / `int_div` and the rest,
keeps the order you wrote. `.ordered()` pins the order on any binary pattern,
the `int_add` sugar as much as the `int_binary` builder; it raises only on a
shape with no operand pair, such as `anything()`:

```python
p.int_binary("Add", p.int_const(k), p.anything()).ordered()   # k on the left
```

### Node builders

Control, memory, and call nodes have typed builders with named slots. Each
returns a builder you keep chaining; `find_all` accepts the builder directly, or
call `.into_pat()`.

```python
base, off, x = p.Capture("base"), p.Capture("off"), p.Capture("x")
p.load(addr=p.int_add(base, off))              # a memory load
p.load().stack_only(); p.load().stack_offset(8)  # address is a (given) stack slot
p.load().non_stack(); p.store().heap_only()       # not-stack / a heap allocation
p.store(addr=p.anything(), data=p.anything())    # a store
p.call().target(0x1000).arg(0, x)          # a direct call to 0x1000, arg0 = x
p.call().target([0x1000, 0x2000])          # a call to any of these addresses
p.if_else(cond=p.int_eq(x, 0))             # an If on a condition
p.phi()                                     # a value phi
p.mem_phi()                                 # a memory phi
p.function_arg(0)                           # the first integer argument
p.function_arg_float(0)                     # the first float argument
p.any_function_arg()                        # an argument of either class
p.ret(); p.ret().ret_val(0, p.Capture("v")) # returns
p.entry(); p.region(); p.switch(); p.indirect_branch(); p.unreachable()
```

Call slots: `.target(p)` (callee, a raw int address or a list of them is fine),
`.arg(i, p)`, `.mem(m)`, `.ctrl(p)` (control predecessor), `.res()` (pin to the
return value when nested as a value), `.output(slot)` (a specific output).
`.target(p)` takes a list on `p.indirect_branch()` too, as does
`p.switch().selector(p)`.

A call's float arguments are appended after its integer ones, never interleaved,
so an integer argument keeps the index it would have had without them: on x86-64
SysV `.arg(6)` is the first float argument. Each class indexes by ABI position
off the convention's own register list, the float positions starting at
`len(arg_passing_regs)`, so `.arg(6)` is XMM0 whether or not the analyzed
function names it. The incoming-argument patterns index the two classes
separately instead.

If slots: `.cond(p)`, `.ctrl(p)`, `.true_branch(p)` / `.false_branch(p)` (what
an edge leads to), `.capture_true(c)` / `.capture_false(c)` (bind the edge for
constraints).

### The shared builder vocabulary

Every builder carries the same small set of methods, declared once in the stubs
as `typing.Protocol` mixins and true of the runtime objects structurally:

| mixin | methods | on |
| --- | --- | --- |
| `NodePat` | `.capture(c)`, `.when(f)`, `.into_pat()` | every builder |
| `InputPat` | `.input(i, p)`, `.any_input(p)` | every node builder with input slots; not `entry()` (`Entry` has none), and not the operand builders `function_arg` / `int_binary` / `float_binary` / `bool_binary`, whose operands are their arguments |
| `CtrlPat` | `.ctrl(p)` | `if_else`, `switch`, `call`, `call_other`, `ret`, `indirect_branch`, `unreachable` |
| `MemPat` | `.mem(p)` | `call`, `call_other`, `indirect_branch`, `load`, `store` |
| `MemAccessPat` | `.addr(p)`, `.bit_width(n)`, `.space(s)`, `.stack_offset(k)`, `.stack_only()`, `.non_stack()`, `.heap_only()` | `load`, `store` |
| `OrderedPat` | `.ordered()` | `Pat`, `int_binary`, `float_binary`, `bool_binary` |
| `OutputPat` | `.output(slot)`, `.any_output()` | every node builder with outputs; not the sinks `ret` / `indirect_branch` / `unreachable`, and not the operand builders listed above. `.any_output()` is satisfied by ANY output of the node, not a fixed slot |

```python
isinstance(p.load(), p.InputPat)      # True
isinstance(p.entry(), p.InputPat)     # False
```

`.input(i, p)` and `.output(slot)` address RAW slots, and slot numbering is per
node kind: `Call` inputs are `[ctrl, mem, target, sp, arg0, ...]`, `Load`'s are
`[mem, addr]`, `If`'s are `[ctrl, cond]`, while `Call` OUTPUTS are
`[ctrl, mem, result, ...clobbers]` and `Load`'s are `[value]`. The IR's
`expected_signature` (`strider-ir/src/node_signature.rs`) is the source of
truth. They are the escape hatch beneath the named accessors, not a replacement
for them. What a slot holds decides what can bind it: only an untyped wildcard
(`var` / `anything`) reaches a Control, memory or phi-token edge, never a typed
value sub-pattern.

`phi()` and `mem_phi()` index predecessors with `.phi_input(i, p)`, raw slot
`i + 1`; their `.input(i, p)` is the raw slot every other builder's is, so
`.input(0, p)` is the phi token.

`.output(slot)` and `.any_output()` return a terminal taking one of
`.capture(c)`, `.of_width(w)`, `.of_type("i64")`, which hands the builder back.

### `any_input` -- match *some* input

```python
p.mem_phi().any_input(p.store(addr=p.anything(), data=p.anything()))  # some input is a store
p.call().any_input(p.anything())                                      # some input, any kind
p.load().input(1, x)                                                  # raw slot 1, the address
```

`any_input` matches an input of any kind: a value producer a value input, a
memory producer (`store` / `mem_phi`) a memory input, a wildcard any input at
all including control and phi-token edges.

### `one_of` / `first_of` -- alternation (OR)

```python
base, off = p.Capture("b"), p.Capture("off")
p.one_of([p.int_add(base, p.int_const(off)), p.var(base)])   # base+K, or bare base
```

`one_of` is the OR combinator, dual to a `find_all([...])` list (which is AND).
An arm is anything a top-level pattern is -- a value shape, `store()` /
`mem_phi()`, `call()`, ... -- and the result nests in any slot (value, memory,
control):

```python
p.load().mem(p.one_of([p.store(), p.mem_phi()]))        # memory slot
p.ret().ctrl(p.one_of([p.load(), p.call()]))     # control slot
```

`one_of` is a **union**: every arm that matches is enumerated with its own
bindings, so order carries no meaning and a downstream constraint can pick the
arm it needs. `first_of` is the **ordered** variant -- it cuts to the first
matching arm, so a permissive leading arm shadows the rest; list most-specific
first. Any pattern kind is a valid arm, including the node-rooted control
builders (`ret` / `if_else` / `switch` / `indirect_branch` / `unreachable`).

### Captures

```python
c = p.Capture()                # fresh, anonymous
off = p.Capture("off")         # named; two Capture("off") are one variable
p.int_add(off, p.anything())       # a bare string is NOT a capture; use Capture(...)
```

You read a capture back by the object or by its name string
(`hit.uint(off)` or `hit.uint("off")`). `"_"` and `"any_"` are reserved.

### Chaining methods

The value-op functions (`int_add`, `int_mul`, `int_const`, ...) return a finished `Pat`.
The typed builders (`load`, `call`, `int_binary`, ...) return a builder you keep
chaining, finalised with `.into_pat()` or passed straight to `find_all`.

Both a `Pat` and a builder carry `.capture(c)` / `.when(f)`:

```python
p.int_add(a, b).capture(c)                  # bind the matched node to c
p.int_add(a, b).capture("sum")              # ... naming it instead
p.int_add(a, b).when(lambda m: ...)         # keep the match only if the predicate holds
```

A value pattern also takes `.of_width(bits)` / `.value_ty("i64")` /
`.bool_valued()`. The
`int_binary` / `bool_binary` / `float_binary` builders take `.ordered()`. Node
builders (`load`, `call`, ...) have their own slots (`.addr`, `.arg`,
`.cond`, ...).

```python
p.int_binary("Add", a, b).ordered()     # do not also try swapped operands
p.int_const(c).of_width(32)             # constrain the constant's width
p.var(c).value_ty("i64")                # ... or a captured value's type
```

---

## 5. Running queries

```python
function.find_all(pat)                       # every match, deduplicated
function.find_all(pat, ignore_casts=True)    # see through width casts (default False)
function.find_all([pat1, pat2], constraints=[...])   # a join (see below)
function.find_unique(pat)                     # the single match, else StriderError
function.find_unique_value(pat, off)          # the single captured VALUE, or None
```

`find_unique` fails if there are two *structurally distinct* matches even when
they bind the same value. `find_unique_value(pat, capture)` deduplicates by the
captured constant instead: `None` for no match, the value when all matches
agree, `StriderError` for two or more distinct values. Pass `signed=True` to
read the value as two's-complement. Its `pat` and `constraints` behave as in
`find_all` -- a list `pat` joins on shared captures, and `constraints=[...]`
filters the joined tuples before the value dedup.

```python
stride = p.Capture("stride")
# The jump-table stride: every index is scaled by the same constant, so the
# capture collapses to one value across structurally distinct multiply nodes.
function.find_unique_value(p.int_mul(p.anything(), p.int_const(stride)), stride)   # -> 4
```

---

## 6. Reading a `Match`

```python
for hit in function.find_all(pat):
    hit.root                     # id where the top pattern matched
    hit.roots                    # one root id per pattern passed to the query,
                                 # in the order they were passed; what a joined
                                 # find_all([p1, p2]) reads. hit.root is roots[0]
    hit.has("off")               # did this capture bind?
```

Each typed reader **raises** when the capture is unbound or its node lacks that
aspect; the `_opt` form returns `None` instead:

Index the match with the capture to get a `BoundCapture`, which carries the
readers as properties:

```python
hit[off].uint                    # unsigned int (raises if not one)
hit[off].uint_opt                # ... or None
hit[off].sint                    # signed
hit[flag].boolean                # bool
hit[f].float_bits                # raw float bits
hit[node].op                     # "Add" / ...
hit[node].value_type             # "I64" / ...
hit[base].node                   # a Node handle
hit[reg].vn                      # a varnode
hit[node].asm_fingerprint        # machine addresses (a list; [] if unbound)

hit["off"]                       # by name, when the capture has one
int(hit[off]); hit[off] == 8     # a numeric capture converts and compares directly
```

An anonymous capture needs no name at all, which suits a hole you read back
once:

```python
off = p.Capture()                                  # no name
for hit in function.find_all(p.load(addr=p.int_add(p.anything(), p.int_const(off))),
                             ignore_casts=True):
    print("field at offset", hit[off].uint)
```

Every reader also exists as a `Match` method taking the capture
(`hit.uint(off)`, `hit.node(base)`), which reads better when the capture is used
once inline.

---

## 7. Constraints (`strider.pattern.constraints`)

A joined `find_all([...])` correlates captures shared across its patterns.
Constraints add relational conditions, evaluated after the join.

```python
from strider.pattern import constraints as k
```

### Built-in relations

```python
g, t, f, c = p.Capture(), p.Capture(), p.Capture(), p.Capture()
guard = p.if_else().capture_true(t).capture_false(f).capture(g)
call = p.call().capture(c)

# The call sits in the true block of the guard:
function.find_all([guard, call], constraints=[k.dominates(t, c)])

# Compose: negate, OR, AND.
k.negate(k.dominates(t, c))
k.any_of([k.dominates(t, c), k.dominates(f, c)])
k.all_of([k.dominates(t, c), k.dominates(f, c)])
```

`dominates(a, b)` is control-flow dominance over captured nodes or `If`
branch-edge captures; it is not "reachable from", so no single incoming edge
dominates a merge/loop-header phi. `phi_input_from_edge(phi, edge, value)` says
"the value `phi` merges from that branch edge is `value`".

### `JoinPredicate` -- your own logic

Subclass, declare the captures it reads (so it correlates and range-checks like
a built-in), and decide in `constraint`:

```python
n = p.Capture("n")

class Aligned(k.JoinPredicate):
    def captures(self):       return [n]         # default is [], a pure filter
    def constraint(self, m):  return m.uint(n) % 8 == 0

function.find_all([p.call().arg(0, p.int_const(n))], constraints=[Aligned()])
```

Declaring captures lets a predicate connect otherwise-independent patterns. An
exception inside `constraint` surfaces at the query. It composes inside
`any_of` / `all_of` / `negate` like any built-in constraint.

---

## 8. Rewrites (`strider.template`)

`rewrite(find, replace)` replaces every match of a pattern with a built value.
The right-hand side comes from `strider.template`, which covers the value ops
that can be BUILT and reuses the left-hand side's captures. It is a subset of
`strider.pattern`: the alias constructors (`int_ne`, `int_le`, `int_sle`,
`float_ne`, `float_le`, `float_is_nan`) and the wildcards (`anything`, `any_int`)
match but do not build, so spell the canonical shape instead.

```python
from strider import template as t

# Fold `x + 0` to `x`. `count` is how many times it fired.
x = p.Capture("x")
count = function.rewrite(find=p.int_add(p.var(x), p.int_const(0)), replace=t.var(x))

# Several rules in one pass, applied round-robin at every node:
function.rewrite_all([
    (p.int_add(p.var(x), p.int_const(0)), t.var(x)),
    (p.int_mul(p.var(x), p.int_const(1)), t.var(x)),
])
```

Node ids are invalidated by a rewrite (like `optimize`), so re-fetch `Node`
handles afterward.

---

## 9. The optimizer (`strider.opt`)

`analyze` runs the default pipeline. To run your own:

```python
pipe = strider.opt.OptimizerPipeline.empty()    # empty
pipe = strider.opt.OptimizerPipeline.default()  # the standard set
pipe.passes                                      # names of the passes, in order
prog.optimize(function, pipe)                    # optimize lives on the lifter; runs in place

# Individual passes are classes:
strider.opt.ConstantFold(); strider.opt.LoadForward(); strider.opt.PhiCollapse()
```

---

## 10. Visualizing

```python
prog.visualize(function)          # interactive explorer; prints a URL, blocks
prog.visualize(cfg)               # a Cfg works too
```

The explorer renders the **neighborhood** around a node you pick (not the whole
graph), so it stays fast on large functions. The toolbar drives that render:
**depth** (hops from the centered node), **hub cap** (a node with more consumers
than this is drawn but not expanded), **max nodes**, **+prod** (count a node's
inputs toward the hub cap too) and **pretty** (inlined constants, resolved
register names), with
**reset** to go back. Each control starts at the `neighborhood_dot` default
except **pretty**, which the explorer opens on, and **depth** when
`visualize(depth=...)` seeds it.

`visualize` reads the unsendable `Function` / `Cfg` directly, so it runs on the
thread that BUILT them: the lifter, and the `Function` / `Cfg` derived from it,
must all be created on the thread that calls it, or the call raises
`PanicException: unsendable` before the server binds. To serve off the main
thread, build the handle inside that thread, and pair it with
`strider.explore.shutdown(port)` and a thread join before exit:

```python
import threading

def serve():
    prog = strider.lift.load_elf("fixtures/out/x86/switch.elf")
    _cfg, fn, _u = prog.analyze("dispatch_value")
    prog.visualize(fn, port=8080)

t = threading.Thread(target=serve)
t.start()
...
strider.explore.shutdown(8080)   # unblocks the server; also joins the thread
t.join()
```

For static output use `function.to_dot(pretty=True)`, `function.to_html(path)`,
or `function.neighborhood_dot(center, depth=2, pretty=True)`.

---

## 11. Registers and architecture (`strider.sleigh`)

```python
arch = sleigh.SleighArch.x86_64()      # a preset; also arm(), aarch64(), mipsbe32(), ...
arch.name()                            # "x86_64"
arch.endianness()                      # "little" / "big"

lift.reg("RAX")                        # the Vn for a register name (uppercase), or None
lift.reg_name(vn)                      # the name for a Vn, or None
lift.pcode_at(entry, addr)             # decode one instruction's p-code as text

lift.user_op_names()                   # every Sleigh user-op name this arch emits
lift.call_other_abi("rdtsc")           # how one is classified, or None
```

`Vn` is a varnode (a register/memory location); `VnSpace` names its address
space. `CallingConvention` presets (`x86_64_systemv()`, ...) describe argument
passing, and `CallingConvention.custom(sleigh, ...)` states an ABI they do not
cover from register names. `CallOtherAbi` describes one Sleigh user-op, for
`CfgOptions(call_other_abis=...)`. `Sleigh` exposes the raw register table when
you need it without a lifter, and resolves the names both `custom` constructors
take. `crates/strider-py/examples/python/17_custom_abis.py` uses each of them.

---

## 12. The CFG (`strider.cfg`)

```python
cfg.entry()                            # entry region index
cfg.region_at(addr)                    # region index containing an address, or None
cfg.to_dot()                           # DOT of the region graph, "dark_cfg" theme
cfg.to_html("cfg.html", style="dark")  # standalone page, another theme
cfg.neighborhood_dot(cfg.entry(), depth=5)   # local region subgraph
cfg.pcode_at(addr)                     # one instruction's p-code as text, or None
                                       # when this CFG stored no decode for addr
cfg.isa_mode_conflicts()               # addresses two paths reached in different ISA modes
cfg.interior_branch_targets()          # branch targets off every instruction boundary, whose
                                       # edge is seated on the region owning the bytes and is
                                       # therefore not exact
cfg.unverified_seeded_sites()          # sites nothing verified: a seed the classifier
                                       # never confirmed, or a site the CFG consumed
                                       # as a Return / TailCall (seeded or derived)
```

`CfgOptions` (passed via `LifterOptions.cfg` or `Lifter.build_cfg`) tunes CFG
construction:

```python
strider.cfg.CfgOptions(
    function_max_size=None,
    allow_code_before_start_addr=False,
    known_targets={dispatch_addr: [target, ...]},   # your own answers, seated
    call_other_abis={"syscall": sleigh.CallOtherAbi.mem_clobber()},   # per user-op
)
```

`known_targets` seats indirect-branch answers in the CFG builder and seeds the
resolution loop, which unions its own findings on top, so it composes with
`resolve_indirect_branches=False`. A seeded address is also taken as complete,
so it drops out of `unresolved` even when the classifier could not read it.
Seating changes the CFG the classifier reads, so a wrong seed can stop it
deriving and take the site's real arms with it: `cfg.unverified_seeded_sites()`
names the sites that settled holding nothing but your seed. It also names every
site the CFG consumed outright -- a `LinkRegister` answer became a `Return`, a
single out-of-function target became a `TailCall` -- whether that answer was
seeded or derived, since those leave no placeholder for a dispatch with more
arms to show up in.

---

## Errors

Failures inside an analysis are a `strider.StriderError`; bad arguments are
not, so `load_elf` raises `FileNotFoundError` for a missing path and
`ValueError` for a file that is not a supported ELF. Match readers additionally raise it
when you read an unbound capture (use `has()` or the `_opt` readers to avoid
that), and when a `Match` is used after the function was reoptimized (node ids
are invalidated by `optimize`).

`analyze` never raises for an indirect branch. An unresolvable site, a site
whose answer oscillates or shrank between rounds, and a target chain deeper than
the iteration cap all come back in `unresolved` instead.

The message is the error and its causes. The Rust backtrace is always captured
and always reachable on the exception, so a sweep can log it without re-running:

```python
try:
    prog.analyze("f")
except strider.StriderError as e:
    log.error("%s", e)            # the one line you can act on
    log.debug("%s", e.backtrace)  # frames, when you are chasing strider itself
```

`STRIDER_BACKTRACE=1` folds the trace into the message instead; it reads from
`os.environ` and takes effect on the next error. Importing strider sets
`RUST_LIB_BACKTRACE=1` if neither backtrace variable is set, which is what makes
the capture unconditional; export either one beforehand to keep strider from
writing to the environment at all. `RUST_BACKTRACE=0` in the
environment suppresses the capture itself, leaving `.backtrace` without frames.
