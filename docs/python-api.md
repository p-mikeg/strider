# Python API reference

The Python API by topic. The typed `.pyi` stubs under
`crates/strider-py/strider/` list every name with its exact signature,
including the arithmetic operators this page groups rather than enumerates.
For the task-first walkthrough, read the [Python guide](python-guide.md).

Blocks showing a whole flow run against the committed fixture ELFs under
`fixtures/out/`; the signature listings use placeholder names.

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

### `lift.load_elf`, the common path

```python
prog = strider.lift.load_elf("fixtures/out/x86/switch.elf")
# Detects the architecture and calling convention from the ELF header.
# Override for kernels or custom ABIs:
#   load_elf(path, arch=sleigh.SleighArch.x86_64(), cc=sleigh.CallingConvention.x86_64_systemv())
# from_segments=True (default) walks PT_LOAD; False forces per-section regions.
# apply_relocations=True (default) patches relocations in place, and also
# selects what is mapped: False drops writable mappings rather than serving
# their on-disk bytes. Exec beats write, so a single RWX PT_LOAD still maps.
```

`load_elf` returns an `ElfLifter`: a lifter that also carries the symbol table,
the loaded memory, and a default calling convention.

The file is mapped, not copied. Rebuilding the binary under a live handle
raises `StriderError: mapped file ... changed on disk since it was mapped`.
Re-open it, or set `STRIDER_NO_MMAP=1` to read the file into memory instead,
which a network or 9p mount also needs.

On ppc64 ELFv1 a `STT_FUNC` symbol addresses a 24-byte `.opd` descriptor rather
than code. `load_elf` reads its first doubleword, so `symbol("f").address` and
`analyze("f")` name the entry point; `size` is `None` there.

An unlinked object file (`ET_REL`) loads from sections whatever `from_segments`
says. Its sections typically all sit at `sh_addr` 0, so strider gives each
colliding section its own base the way a linker would. Every address you get
back is that synthetic one, comparable only against other addresses from the
same load. With `apply_relocations=True` the code relocations are applied on
x86, x86-64, AArch64, ARM and Thumb, PowerPC and MIPS:

- An undefined or `SHN_COMMON` symbol resolves to a distinct address in an
  unmapped range past the image, so a call to an external function targets
  that address, and `prog.symbol("ext_fn").address` names it.
- A GOT reference loads from a synthetic GOT slot holding the symbol's address.
- A relocation strider does not compute leaves no bytes behind: outside a
  writable mapping its field is a hole no read serves, so decoding stops there,
  and a decode starting on it fails with an error naming the relocation type.

### `lift.lifter`, raw bytes and no ELF

```python
mem = strider.reader.BufferReader(0x1000, b"\x48\x01\xd8\xc3")  # add rax,rbx; ret
lft = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)      # rom=... optional
```

`lifter(arch, mem, rom=None)` builds a plain `Lifter`. `mem` is the instruction
source; `rom` is optional read-only memory for constant folding.
`BufferReader(base_addr, data)` takes `bytes`, a `bytearray` or a sequence of
ints. The guide's [Beyond ELF](python-guide.md#beyond-elf-custom-code-and-data)
section walks through it.

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
either. The `reader.MemLike` / `reader.RomLike` type aliases name what each
argument accepts.

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
_cfg, fn, _u = lft.analyze(0x1000, sleigh.CallingConvention.x86_64_systemv())
```

### `lift.LifterOptions`, per-call tuning

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
        stack_global_disjoint=True,
        assume_incoming_args_survive_calls=True,
        escape_analysis=False,
        noalias_allocators=[],            # callee addresses of malloc-like allocators
        distinct_sp_bases_disjoint=False,
        callee_preserves_stack_args=False,
    ),
    compact=True,                      # drop unreachable nodes at the end
    resolve_indirect_branches=True,    # False leaves every site an IndirectBranch placeholder
    per_address_ccs=None,              # {callee_addr: CallingConvention}; keyed by
                                       # the direct-call TARGET, not the call site
    pipeline=None,                      # replace the optimizer pipeline for this call
)
prog.analyze("dispatch_value", opts=opts)
```

`AssumptionOptions` holds claims about the analysed code that strider cannot
check, so a wrong one can make the answer wrong. Each field's risky value is
the positive one:

- `stack_global_disjoint` (default on): no constant address equals `sp + K` at
  runtime.
- `assume_incoming_args_survive_calls` (default on): a callee leaves an
  incoming stack-argument slot as it found it, so an argument read after a
  call is still found.
- `distinct_sp_bases_disjoint` (off): a store rooted at an SP base other than
  the entry SP, such as an `sp & -16` frame local, does not alias an incoming
  argument slot. Only incoming-argument detection reads it.
- `escape_analysis` (off): when no stack address escapes the frame, a spill
  load forwards across a call. An address escapes through a call argument, a
  stored value, a return, or a register the call clobbers. The proof misses a
  callee reading an address from a register its convention preserves, and
  reads an alignment hole in the outgoing-argument window as the window's end.
- `noalias_allocators` (empty): the listed callees return fresh pointers that
  overlap no live allocation. Nothing models deallocation, so after a free a
  stale pointer is taken as disjoint from storage the allocator hands out
  again. A non-empty list also forwards spills across a listed call, with the
  same gaps as `escape_analysis`.
- `callee_preserves_stack_args` (off): a callee leaves the outgoing-argument
  slots unchanged, which the psABIs do not require. It changes nothing unless
  `escape_analysis` is on or `noalias_allocators` is non-empty.

`AssumptionOptions.none()` clears all six; `AssumptionOptions()` does not. It
is sound for any input whose memory behaves as RAM: a load reads back the last
value the analysed code stored at its address, so a memory-mapped register
polled after a write still reads as the value written.

`call_other_abis` reclassifies a Sleigh user-op by name ahead of the built-in
table. Each value is a `strider.sleigh.CallOtherAbi`: one of the four
footprint-free classes `CallOtherAbi.noop()` / `.pure()` / `.mem_clobber()` /
`.no_return()`, or one naming implicit registers:

```python
sl = strider.sleigh.Sleigh(sleigh.SleighArch.x86_64(), mem)
strider.sleigh.CallOtherAbi.custom(
    sl,                              # resolves the register names here and now
    implicit_reads=["RAX", "RDI"],   # read beyond the p-code operands
    implicit_writes=["RAX"],         # written beyond the p-code result
    clobbers_memory=True,
    no_return=False,
)
```

An unknown register name raises `StriderError` at construction. An ABI stated
here holds for this analysis only.

`lft.user_op_names()` lists every user-op name the architecture can emit, and
`lft.call_other_abi(name)` reads back the classification in force: the
built-in one, or the `cfg_opts` entry when `lft.call_other_abi(name, cfg_opts)`
is given one. `None` means strider has no answer for the name, which fails the
lift of any function containing it.
`crates/strider-py/examples/python/17_custom_abis.py` runs the discovery, then
analyses one `int 0x80` stub with and without an override.

### ElfLifter metadata

```python
prog.arch                      # SleighArch
prog.cc                        # default CallingConvention
prog.endianness                # "little" or "big"
prog.is_arm_be8                # EF_ARM_BE8 set; False off ARM
prog.entry_point()             # ELF entry address
list(prog.functions())         # one Symbol per function address, address order
list(prog.iter_symbols())      # every Symbol, pulled one at a time (a SymbolIter)
prog.symbols()                 # {name: Symbol}

prog.symbol("f")               # Symbol; raises if nothing has that name
prog.symbol_opt("f")           # ... or None
prog.symbol_at(0x401234)       # the Symbol covering an address, or None

prog.read(addr, size)          # raw bytes, or None when unmapped
prog.reader()                  # the BufferReader over the mapped regions: PT_LOAD
                               # segments unless from_segments=False
prog.rom()                     # the read-only image LoadReadOnly folds from, or None
prog.add_elf("libc.so")        # merge another ELF (shared library)
```

Symbols can also be attached after loading, which is how a stripped image gets
its names back:

```python
prog.add_symbol_file("vmlinux.debug")   # its symbols, none of its bytes
prog.add_symbols({                      # names that live in no ELF at all
    "handle_irq": 0xffffffff81001200,
    "irq_table":  (0xffffffff81800000, 0x400),   # (address, size)
})                                      # is_function=False for data names
```

`add_symbol_file` is for a separate debug or symbol file: `objcopy
--only-keep-debug` output and distro debuginfo are linked at the SAME addresses
as the image they describe, so `add_elf` refuses them as an overlap. Only the
symbols are taken. `add_symbols` takes an address, or an `(address, size)` pair
when the extent is known, which lets `symbol_at` resolve an address inside the
symbol. A name an already-loaded ELF carries keeps its ELF answer.

A `Symbol` carries `name`, `address`, `size`, `end`, `is_function`, `is_thumb`
and `region`. `size` is `None` when the ELF records no extent (`st_size == 0`,
as for a hand-written `.S` entry point with no `.size` directive); `end` is
`None` there too. `region` is the `(start, end)` of the loaded region the
symbol maps into, such as `.text`. A Thumb function's `address` keeps the ISA
bit, which is what makes `analyze` enter it in Thumb mode; `end`, `region` and
`symbol_at` measure it from `address & ~1`.

`symbol(name)` resolves to a definition first, and otherwise to an undefined
symbol that has an address: a PLT stub, or an object file's extern address.

`symbol_at` takes the nearest symbol at or below the address whose recorded
extent reaches it. A symbol with no recorded size covers only its own address.
Aliases sharing an address are ranked by recorded extent first and by being
code second, so a sized data alias wins over an unsized function one.
`functions()` filters to code first and keeps the sized one of what remains, so
the two accessors can name different symbols at one address.

---

## 3. The `Function` and its `Node`s

```python
function.node_count()          # total node ids
function.entry_node()          # id of the Entry node
function.node_ids()            # every id
function.count_regions()       # Region nodes reachable from entry
function.cfg                   # the Cfg it was lifted from
function.clone()               # an independent copy, sharing the Cfg
function.compact()             # drop nodes unreachable from entry; renumbers ids
function.validate()            # None, or a message naming the broken invariant

n = function.node(some_id)     # a Node handle
```

A `Node`:

```python
n.id                           # node id; the handle goes stale after
                               # optimize / rewrite / rewrite_all / compact
n.kind()                       # "IntBinaryOp(Add)", "Region", "Load(RAM)"
n.op()                         # "Add" / "Less" / ... or None for op-less kinds
n.value_type()                 # "I64" / "I1" / "F64" / ... or None
n.inputs(), n.outputs()        # neighbouring nodes (Node handles, not ids)
n.uint(), n.sint(), n.boolean(), n.float_bits()   # constants
n.wide_const_bytes()           # little-endian bytes of a constant over 64 bits
n.call_other_name()            # a CallOther's user-op name, else None
n.vn()                         # the varnode this node names: an InitialVar's entry
                               # register, a Call's return register (its FIRST value
                               # output, never one clobber), else None
n.asm_fingerprint()            # machine addresses this node was lifted or folded from
```

A rewrite adds the addresses of the nodes it folded into the node it builds; a
flag cone that was already dead when the rewrite ran adds nothing.

Walking the graph (each returns a list of `Node`):

```python
function.data_walk()             # every reachable node, pre-order
function.cfg_walk()              # the control-flow skeleton (control edges only)
function.mem_walk()              # memory-touching nodes along the memory chain
function.walk(some_node_id)      # everything reachable from one node
```

Rendering:

```python
function.to_dot(pretty=True)                 # Graphviz DOT text; path= writes a file
function.to_text()                           # canonical text, one line per node; diff two runs
function.to_text(fingerprints=True)          # ... each line ending in its asm addresses
function.to_html("graph.html")               # dark-themed standalone page (path=None returns text)
function.neighborhood_dot(function.entry_node(), depth=2, pretty=True)  # local subgraph DOT
# neighborhood_dot also takes hub_cap=, max_nodes= and count_producers=, the
# explorer's toolbar limits (section 10).
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
p.var(p.Capture("x"))          # a wildcard that captures
p.int_const(8)                 # the integer constant 8 at any width (truncated to it)
p.int_const([0x10, 0x20])      # any constant from a set
p.any_int()                    # any integer-typed node, constant or not
p.any_bool(); p.any_float()    # any I1 / float-typed node
p.int_const()                  # any integer constant
p.int_const(c)                 # ... capturing it
p.int_const_any_width(-1)      # -1 held at a narrower width and zero- or sign-extended
p.bool_const(True); p.bool_const()
p.float_const(bits); p.float_const()
p.initial_var()                # an initial register/stack read
p.initial_var_for(vn)          # ... a specific varnode
p.float_is_nan(x)              # the IEEE self-inequality shape
p.value_of_width(32)           # any value exactly 32 bits wide
```

A **raw int** anywhere an operand is expected is `int_const(that int)`, so
`p.int_add(c, 8)` is `p.int_add(c, p.int_const(8))`. Narrow the width by hand
with `p.int_const(8).of_width(32)` when you need it.

`int_const_any_width(v)` matches a constant only when `v` is a zero or sign
extension of the constant's low bits at the width it is matched at, so
`0x1234` never matches `0x34` at `I8`.

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
p.int_cmp("Less", a, b)                                             # by name
p.float_add(a, b); p.float_lt(a, b); p.float_sqrt(a)
p.int_zero_extend(x); p.int_sign_extend(x); p.int_truncate(x)       # width casts
p.int_extend("SignExtend", x)                                       # by name
p.any_int_binary(c, a, b)       # any integer binary op, the node bound to c;
                                # also any_int_unary / any_int_cmp / any_bool_binary
                                # and the any_float_* forms
p.inputs_of_width(32, p.int_add(a, b)); p.bool_inputs(p.int_and(a, b))
```

`int_add`, `int_mul`, `int_and`, `int_or`, `int_xor`, their `I1` spellings
`bool_and` / `bool_or` / `bool_xor`, `float_add`, `float_mul`, `int_eq`,
`int_carry`, `int_scarry` and `float_eq` match **commutatively**, and a lowered
comparison inherits that from the comparison it wraps, so `int_ne` and
`float_ne` do too. Every other operator, `int_shl` / `int_shr` / `int_lt` /
`int_div` and the rest, keeps the order you wrote. `.ordered()` pins the order
on any binary pattern, the `int_add` sugar as much as the `int_binary`
builder; it raises only on a shape with no operand pair, such as `anything()`:

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
p.call_other().user_op_id(120)             # a CallOther for one user-op id
p.if_else(cond=p.int_eq(x, 0))             # an If on a condition
p.phi(); p.phi_for(vn)                      # a value phi; one for a register
p.phi().for_vn(vn); p.phi().phi_token(p.anything())
p.mem_phi()                                 # a memory phi
p.function_arg(0)                           # the first integer argument
p.function_arg_float(0)                     # the first float argument
p.any_function_arg()                        # an argument of either class
p.function_arg_reg(vn)                      # an argument arriving in register vn
p.function_arg_stack(sleigh.VnSpace.RAM, 8) # one arriving in a stack slot
p.any_function_arg().index(1)               # also .source_register(vn) / .source_stack(space, k)
p.ret(); p.ret().ret_val(0, p.Capture("v")) # returns
p.entry(); p.region(); p.switch(); p.indirect_branch(); p.unreachable()
```

Call slots: `.target(p)` (callee, a raw int address or a list of them is fine),
`.arg(i, p)`, `.mem(m)`, `.ctrl(p)` (control predecessor), `.res()` (pin to the
return value when nested as a value), `.output(slot)` (a specific output).
`.target(p)` takes a list on `p.indirect_branch()` too, as does
`p.switch().selector(p)`.

A call's float arguments come after its integer ones: on x86-64 SysV `.arg(6)`
is XMM0, the first float argument, since the convention declares six integer
argument registers. The incoming-argument patterns (`function_arg` /
`function_arg_float`) index the two classes separately.

If slots: `.cond(p)`, `.ctrl(p)`, `.true_branch(p)` / `.false_branch(p)` (what
an edge leads to), `.capture_true(c)` / `.capture_false(c)` (bind the edge for
constraints).

A query refuses a pattern of more than 256 nodes, counting those nested through
`.true_branch` / `.false_branch`, and one nested deeper than the thread's stack
allows: both raise `StriderError`.

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
`expected_signature` (`crates/strider-ir/src/node_signature.rs`) is the source
of truth. They are the escape hatch beneath the named accessors, not a
replacement for them. What a slot holds decides what can bind it: only an
untyped wildcard (`var` / `anything`) reaches a control, memory or phi-token
edge, never a typed value sub-pattern.

`phi()` and `mem_phi()` index predecessors with `.phi_input(i, p)`, raw slot
`i + 1`; their `.input(i, p)` is the raw slot every other builder's is, so
`.input(0, p)` is the phi token.

`.output(slot)` and `.any_output()` return a terminal taking one of
`.capture(c)`, `.of_width(w)`, `.of_type("i64")`, which hands the builder back.

### `any_input`, matching *some* input

```python
p.mem_phi().any_input(p.store(addr=p.anything(), data=p.anything()))  # some input is a store
p.call().any_input(p.anything())                                      # some input, any kind
p.load().input(1, x)                                                  # raw slot 1, the address
```

`any_input` matches an input of any kind: a value producer a value input, a
memory producer (`store` / `mem_phi`) a memory input, a wildcard any input at
all including control and phi-token edges.

### `one_of` / `first_of`, alternation (OR)

```python
base, off = p.Capture("b"), p.Capture("off")
p.one_of([p.int_add(base, p.int_const(off)), p.var(base)])   # base+K, or bare base
```

`one_of` is the OR combinator, dual to a `find_all([...])` list (which is AND).
An arm is anything a top-level pattern is: a value shape, `store()` /
`mem_phi()`, `call()`, and the rest. The result nests in any slot (value, memory,
control):

```python
p.load().mem(p.one_of([p.store(), p.mem_phi()]))        # memory slot
p.ret().ctrl(p.one_of([p.call(), p.region()]))          # control slot
```

`one_of` is a **union**: every arm that matches is enumerated with its own
bindings, so order carries no meaning and a downstream constraint can pick the
arm it needs. A match is identified by what it binds, so two arms matching one
node with the same bindings are one match: at the root of a pattern that
captures nothing, `one_of([load(), anything()])` answers exactly what
`anything()` does, and the arms need a capture to tell them apart. `first_of`
is the **ordered** variant: it cuts to the first matching arm, so a permissive
leading arm shadows the rest; list most-specific first. Any pattern kind is a
valid arm, including the node-rooted control builders (`ret` / `if_else` /
`switch` / `indirect_branch` / `unreachable`).

### `field` / `code_ptr`, the two canonical-form alternations

```python
f = p.field(p.function_arg(0))              # base+K, or bare base at offset 0
function.find_all(f.load())                  # f.store(data) for a write
f.offset(hit)                                # 0 for the bare arm
p.field(p.function_arg(0), offset=8)         # only that offset
p.call().target(p.code_ptr(p.function_arg(0)))   # f, or f & -2
```

`ConstantFold` rewrites `base + 0` to `base`, so a field at offset 0 has no
`Add`; `field` matches both forms and `offset(m)` decodes the bare one.
`offset` takes an `int`, a `Capture` or a capture name. `code_ptr` accepts
exactly `-2`, the ISA-mode mask strider strips when resolving a branch; any
other constant is real arithmetic on the target.

### Captures

```python
c = p.Capture()                # fresh, anonymous
off = p.Capture("off")         # named; two Capture("off") are one variable
p.int_add(off, p.anything())   # a Capture operand binds
p.int_add("base", off)         # a bare string is NOT a capture: this builds,
                               # and find_all raises on it
```

You read a capture back by the object or by its name string
(`hit.uint(off)` or `hit.uint("off")`). `"_"` and `"any_"` are reserved.

### Chaining methods

The value-op functions (`int_add`, `int_mul`, `int_const`, ...) return a
finished `Pat`; the typed builders (`load`, `call`, `int_binary`, ...) return a
builder, finalised with `.into_pat()` or passed straight to `find_all`. Both
carry `.capture(c)` and `.when(f)`, and `.capture(c)` takes a name as readily as
a `Capture`. `.into_pat()` is the builder half only: a `Pat` is finished
already, so it has no `.into_pat()` and `isinstance(int_add(a, b), NodePat)` is
`False`.

Only a value pattern takes `.of_width(bits)` / `.value_ty("i64")` /
`.bool_valued()`:

```python
p.int_const(c).of_width(32)             # constrain the constant's width
p.var(c).value_ty("i64")                # ... or a captured value's type
```

---

## 5. Running queries

```python
function.find_all(pat)                       # every match, deduplicated
function.find_all(pat, ignore_casts=True)    # CastMask.all(); default False
function.find_all(pat, ignore_root=True)     # dedup on captures alone
function.find_all([pat1, pat2], constraints=[...])   # a join; constraints in 7
function.find_unique(pat)                     # the single match, else StriderError
function.find_unique_value(pat, off)          # the single captured VALUE, or None
```

`ignore_casts` takes a bool or a `CastMask`, which picks the casts the matcher
walks through: `CastMask.zero_extend()`, `.sign_extend()`, `.extend()` (both),
`.truncate()`, `.int_bits_to_float()`, `.float_bits_to_int()`, `.all()` and
`.none()`, combined with `|` and `&`.

`find_unique` fails if there are two *structurally distinct* matches even when
they bind the same value. `find_unique_value(pat, capture)` deduplicates by the
captured constant instead: `None` for no match, the value when all matches
agree, `StriderError` for two or more distinct values. Pass `signed=True` to
read the value as two's-complement. Its `pat` and `constraints` behave as in
`find_all`: a list `pat` joins on shared captures, and `constraints=[...]`
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

Index the match with the capture to get a `BoundCapture`, which carries the
readers as properties. Each reader **raises** when the capture is unbound or
its node lacks that aspect; the `_opt` form returns `None` instead.

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

An anonymous capture needs no name, which suits a hole you read back once:

```python
off = p.Capture()                                  # no name
for hit in function.find_all(p.load(addr=p.int_add(p.anything(), p.int_const(off))),
                             ignore_casts=True):
    print("constant addend", hit[off].uint)
```

Every reader also exists as a `Match` method taking the capture
(`hit.uint(off)`, `hit.node(base)`), which reads better when the capture is used
once inline. `pattern.CaptureKey` is the type alias for that argument, a
`Capture` or its name, and `.capture()` takes it too.

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

`phi().input_from(edge, value)` and `mem_phi().input_from(edge, value)` build
that constraint for you: a pattern `value` becomes a phi input under a fresh
capture, and a `Capture` names a value another pattern in the list binds (a
`store().capture(s)` feeding a `mem_phi`). Pass the phi's `constraints()` to the
query; the [guide](python-guide.md#constraints-relating-matches-by-control-flow)
shows one.

### `JoinPredicate`, your own logic

Subclass `k.JoinPredicate` and override two methods:

```python
class MyRule(k.JoinPredicate):
    def captures(self):       return []      # the captures it reads; default []
    def constraint(self, m):  return True    # m is the joined Match
```

Declaring captures lets a predicate connect otherwise-independent patterns and
range-checks them like a built-in; it is consulted once they are bound. An
exception inside `constraint` surfaces at the query. It composes inside
`any_of` / `all_of` / `negate` like any built-in constraint.
`constraints.JoinConstraint` is the type of what the built-in relations return.
The [guide](python-guide.md#constraints-relating-matches-by-control-flow) has a
worked example.

---

## 8. Rewrites (`strider.template`)

```python
function.rewrite(find=pat, replace=tmpl)             # -> how many times it fired
function.rewrite_all([(pat1, tmpl1), (pat2, tmpl2)]) # -> total fire count
```

`rewrite(find, replace)` replaces every match of a pattern with a built value.
The right-hand side is a `strider.template.Template`, built from
`strider.template`, which covers the value ops that can be BUILT and reuses the
left-hand side's captures. It is a subset of `strider.pattern`: the alias
constructors (`int_ne`, `int_le`, `int_sle`, `float_ne`, `float_le`,
`float_is_nan`) and the wildcards (`anything`, `any_int`) match but do not
build, so spell the canonical shape instead.

`rewrite_all` makes one walk; at each node the first rule that fires wins.
Neither is a fixed point, so a rule whose output its own `find` matches needs a
second call. Both stale every outstanding `Node` handle, a return of 0
included. The [guide](python-guide.md#rewriting-the-graph) walks through a
strength reduction.

---

## 9. The optimizer (`strider.opt`)

`analyze` runs the default pipeline. To run your own:

```python
pipe = strider.opt.OptimizerPipeline.empty()     # no passes
pipe.add(strider.opt.ConstantFold())             # a main pass, repeated to a fixed point
pipe.add_post(strider.opt.StackOffsetDetect())   # a post-pass, run once at the end
pipe.passes                                      # ["ConstantFold"]
pipe.post_passes                                 # ["StackOffsetDetect"]
pipe = strider.opt.OptimizerPipeline.default()   # the standard set, in analyze's order
prog.optimize(function, pipe)                    # optimize lives on the lifter; runs in place
```

Main passes, which `add` takes: `ConstantFold`, `LoadReadOnly`, `KnownBits`,
`FlagCmpCanonicalize`, `IfCondInversion`, `PhiCollapse`, `RegionCollapse`,
`DeadBranchElimination`, `CfgDetach`, `LoadForward`. Post-passes, which only
`add_post` takes: `StackOffsetDetect`, `CallStackArgCollect`,
`FunctionArgDetect`. `add_post` also takes a main pass, which then runs once.
The `MainOptimizerPass`, `PostOptimizerPass` and `OptimizerPass` type aliases
name those groups. [optimizations.md](optimizations.md) says what each pass
does.

---

## 10. Visualizing

```python
prog.visualize(function)          # interactive explorer; prints a URL, blocks
prog.visualize(cfg)               # a Cfg works too
```

The explorer opens on the **whole graph**: a neighborhood view hides nodes
without saying so, so you cannot tell a small function from a truncated one.
`visualize(whole=False)` opens on the neighborhood around the entry instead,
which stays fast on a large function, and the toolbar's **whole** toggle
switches between them either way.

Drag with the mouse or press the arrow keys to pan (shift for a longer step);
ctrl+wheel zooms about the pointer and `+` / `-` about the window centre, `f`
fits the graph to the window, `0` returns to 100%. A drag that ends over a node
pans instead of re-centering on it. The toolbar drives that render:
**depth** (hops from the centered node), **hub cap** (a node with more consumers
than this is drawn but not expanded), **max nodes**, **+prod** (count a node's
inputs toward the hub cap too) and **pretty** (inlined constants, resolved
register names), with **whole** (draw the entire graph, which the neighborhood
knobs stop applying to) and **reset** to go back. The three limits start at
`0`, which means no limit on each; **pretty** and **whole** start on, and
**depth** takes whatever `visualize(depth=...)` seeds.

`visualize()` blocks until interrupted and returns the port it bound;
`background=True` serves on its own non-daemon thread and returns that port
immediately:

```python
port = prog.visualize(fn, background=True)
# ... keep querying, analysing and rendering on this thread ...
strider.explore.shutdown(port)   # stops the server and joins its thread
```

The server renders through its own decoder, built from the lifter's `arch`,
`reader()` and `rom()`, so your handle stays free to `analyze` while a page
renders. `shutdown` is registered to run before the interpreter joins
non-daemon threads, so an explorer left running does not hang or abort the
process at exit.

For static output use the renderers in
[section 3](#3-the-function-and-its-nodes).

---

## 11. Registers and architecture (`strider.sleigh`)

```python
arch = sleigh.SleighArch.x86_64()      # a preset; also x86(), arm(), arm_thumb(),
                                       # aarch64(), mipsbe32(), ppc64le(), ...
arch.name()                            # "x86_64"
arch.endianness()                      # "little" / "big"

lft.reg("RAX")                         # the Vn for a register name, or None
                                       # matched exactly: x86 spells them upper,
                                       # every other arch lower ("r0", "x0", "r3")
lft.reg_name(vn)                       # the name for a Vn, or None
lft.pcode_at(entry, addr)              # decode one instruction's p-code as text

lft.user_op_names()                    # every Sleigh user-op name this arch emits
lft.call_other_abi("rdtsc")            # how one is classified, or None
```

`Vn` is a varnode (a register/memory location); `VnSpace` names its address
space (`VnSpace.RAM`, `.REGISTER`, `.CONST`, `.UNIQUE`). `CallingConvention`
presets (`x86_64_systemv()`, ...) describe argument passing, and
`CallingConvention.custom(sleigh, ...)` states an ABI they do not cover from
register names. `.no_return()`, `.preserves_all()` and `.preserves_regs()`
derive a variant of a convention for a per-address override. `CallOtherAbi`
describes one Sleigh user-op, for `CfgOptions(call_other_abis=...)`. `Sleigh`
exposes the raw register table when you need it without a lifter, and resolves
the names both `custom` constructors take.
`crates/strider-py/examples/python/17_custom_abis.py` uses each of them.

Analysis stays inside the function being lifted: a call is modelled by the
calling convention of its target, never by reading the callee. On 32-bit x86
two kinds of callee need their own convention through
`LifterOptions(per_address_ccs={callee: cc})`:

- A callee that pops its caller's stack (`ret $imm16`: a struct return's hidden
  pointer, stdcall, fastcall, thiscall) needs `ret_stack_pop` of `4 + imm16`.
  Without it, stack-relative reads after the call are off by `imm16`.
- A PC thunk (`__x86.get_pc_thunk.bx`) needs a convention that does not list
  its register in `callee_saved_regs`. Without it, the register keeps the
  caller's value across the call.

`CallingConvention.custom` states both:

```python
x86 = sleigh.SleighArch.x86()
sl32 = strider.sleigh.Sleigh(x86, mem)
pops_8 = sleigh.CallingConvention.custom(
    sl32,
    arg_passing_regs=[],
    callee_saved_regs=["EBX", "ESI", "EDI", "EBP"],   # drop "EBX" for get_pc_thunk.bx
    ret_val_regs=["EAX", "EDX"],
    ret_val_regs_float=[],
    stack_pointer="ESP",
    stack_arg_base=4,
    stack_arg_increment=4,
    ret_stack_pop=8,                                   # ret $4
)
opts32 = strider.lift.LifterOptions(per_address_ccs={0x1010: pops_8})
```

ARM32 hard-float passes arguments in one bank of 16 single-precision slots
`s0..s15`, aliased as `d0..d7`. The convention names the double carriers, so
for `float` arguments only position 0 lands right: float argument n is in `s_n`,
inside `d_{n/2}`, while `function_arg_float(n)` reports `d_n`. It is a candidate
rather than an answer at every position past the first, and positions 8..15
have no entry at all.

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
cfg.fingerprint_pcode(n)               # [(address, p-code)] for a Node's fingerprint
cfg.is_complete()                      # all six channels below are empty
```

A converged CFG is never silently incomplete, but it says so through SIX
channels, and a consumer asking "may this be incomplete?" reads all six, which
is what `is_complete()` does:

- `unresolved`, the third field of `analyze`'s result: indirect branches with
  no complete answer, such as a site that lost a successor, one whose answer
  oscillated or narrowed, one still growing at the iteration cap, and a return
  whose target is not provably the caller's return address (`push rsi; ret`).
- `cfg.unverified_seeded_sites()`: sites nothing verified. A site seated with
  only your `known_targets`, and every site the CFG consumed as a return or a
  tail call, so an ARM `pop {pc}` epilogue lands here and not in `unresolved`.
- `cfg.isa_mode_conflicts()`: addresses two paths reached in different ISA
  modes. Always empty outside the four 32-bit ARM and four MIPS presets.
- `cfg.interior_branch_targets()`: branch targets off every instruction
  boundary, whose edge is seated on the region owning the bytes and is
  therefore not exact.
- `cfg.unmapped_branch_targets()`: direct-branch targets and fall-throughs no
  byte of the image backs, each seated as an empty tail-call stub.
- `cfg.undecodable_branch_targets()`: addresses a direct branch or a
  fall-through past a call reached that hold no instruction, either bytes Sleigh
  rejects or a range `data_ranges` marks as data. The branch leaves through an
  empty tail-call stub; the call ends as no-return.

All but `unverified_seeded_sites` accumulate across resolution rounds, so a
later round cannot launder an earlier loss.

`CfgOptions` (passed via `LifterOptions.cfg` or `Lifter.build_cfg`) tunes CFG
construction:

```python
strider.cfg.CfgOptions(
    function_max_size=None,
    allow_code_before_start_addr=False,
    known_targets={0x401000: [0x401020, 0x401040]},   # your own answers, seated
    call_other_abis={"syscall": sleigh.CallOtherAbi.mem_clobber()},   # per user-op
    data_ranges=[(0x401100, 0x401108)],               # [start, end) never decoded
)
# with_function_max_size(n) / with_data_ranges(r) return a changed copy.
```

`known_targets` seats indirect-branch answers in the CFG builder and seeds the
resolution loop, which unions its own findings on top, so it composes with
`resolve_indirect_branches=False`. A seeded site drops out of `unresolved`
even when the classifier could not read it. A wrong seed can stop the
classifier deriving the site's real arms; `cfg.unverified_seeded_sites()` names
the sites where that cannot be ruled out.

`data_ranges` marks bytes that are data, such as an ARM literal pool.
`ElfLifter.analyze` fills it from the ELF's ARM and AArch64 `$d` mapping
symbols unless the options already name some.

---

## Errors

Failures inside an analysis are a `strider.StriderError`; bad arguments are
not, so `load_elf` raises `FileNotFoundError` for a missing path and
`ValueError` for a file that is not a supported ELF. Match readers raise
`StriderError` for an unbound capture (use `has()` or the `_opt` readers to
avoid that), and for a `Match` used after the function was reoptimized.

`analyze` never raises for an indirect branch. An unresolvable site, a site
whose answer oscillates or shrank between rounds, and a target chain deeper than
the iteration cap all come back in `unresolved` instead.

The message is the error and its causes; `.backtrace` holds the message and the
Rust backtrace, so a sweep can log it without re-running:

```python
import logging

log = logging.getLogger(__name__)

try:
    prog.analyze("no_such_function")
except strider.StriderError as e:
    log.error("%s", e)            # the one line you can act on
    log.debug("%s", e.backtrace)  # frames, when you are chasing strider itself
```

`STRIDER_BACKTRACE=1` also puts the trace in the message, from the next error
on, including when set through `os.environ`. Importing strider sets
`RUST_LIB_BACKTRACE=1` in the process environment (not visible in
`os.environ`) unless `RUST_LIB_BACKTRACE` or `RUST_BACKTRACE` is already set.
`RUST_BACKTRACE=0` exported before starting the interpreter leaves `.backtrace`
holding only the message.
