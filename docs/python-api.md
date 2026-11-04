# Python API reference

The Python API by topic. The `.pyi` stubs under `crates/strider-py/strider/`
give every exact signature; the [Python guide](python-guide.md) is the
task-first walkthrough. Whole-flow blocks run against the fixture ELFs under
`fixtures/out/`; signature listings use placeholder names.

Eight submodules plus one error are public; `explore` (the `visualize`
explorer) is bound outside `__all__`:

```python
import strider
from strider import ir, lift, cfg, sleigh, reader, opt, pattern, template, explore
strider.StriderError      # the analysis error; see Errors below
strider.__version__
```

## 1. Loading a binary

### `lift.load_elf`, the common path

```python
prog = strider.lift.load_elf("fixtures/out/x86/switch.elf")
# Architecture and calling convention come from the ELF header (a PowerPC
# kernel is detected by e_entry >= KERNELBASE). Override with
#   load_elf(path, arch=sleigh.SleighArch.x86_64(), cc=sleigh.CallingConvention.x86_64_systemv())
# from_segments=True (default) walks PT_LOAD; False uses sections.
# apply_relocations=True (default) patches relocations; False also drops
# writable, non-executable mappings.
# relocated_got_is_rom=True (needs apply_relocations=True) also serves .got
# (PPC32 .got2) and .data.rel.ro as read-only, so a load through a GOT slot
# folds; wrong for code that relocates its own GOT.
```

`load_elf` returns an `ElfLifter`: a lifter carrying the symbol table, the
loaded memory and a default calling convention.

The file is memory-mapped. Writing into it (same inode) raises
`StriderError: mapped file ... changed on disk since it was mapped` (checked by
size and whole-second mtime); replacing it (`mv`, a rebuild) keeps serving the
loaded image, so re-open to see a new build. `STRIDER_NO_MMAP=1` reads the
file instead, which a network or 9p mount needs.

On ppc64 ELFv1 a function symbol names its `.opd` descriptor; `load_elf`
follows it, so `symbol("f").address` and `analyze("f")` name the code.

An unlinked object (`ET_REL`) loads from sections, each colliding section at
its own synthetic base; addresses are comparable only within one load. With
`apply_relocations=True` its code relocations are applied on every supported
arch:

- An undefined or `SHN_COMMON` symbol gets a distinct address in an unmapped
  range past the image, which `prog.symbol("ext_fn").address` names.
- A GOT reference loads from a synthetic slot holding the symbol's address.
- A relocation strider does not compute is a hole outside writable data:
  decoding stops there, with an error naming the relocation type.

### `lift.lifter`, raw bytes and no ELF

```python
mem = strider.reader.BufferReader(0x1000, b"\x48\x01\xd8\xc3")  # add rax,rbx; ret
lft = strider.lift.lifter(sleigh.SleighArch.x86_64(), mem)      # rom=... optional
```

`lifter(arch, mem, rom=None)` builds a plain `Lifter`: `mem` is the instruction
source, `rom` the optional read-only memory for constant folding.
`BufferReader(base_addr, data)` takes `bytes`, a `bytearray` or a sequence of
ints. No preset decodes microMIPS (a MIPS preset reads an odd address as
MIPS16e); `load_elf` refuses a microMIPS image, a raw `lifter` cannot tell.
See the guide's [Beyond ELF](python-guide.md#beyond-elf-custom-code-and-data).

### Custom memory: `reader.MemReader` / `reader.ReadOnlyMemory`

Subclass and override `read(addr, size)`:

```python
class Firmware(strider.reader.MemReader):
    def __init__(self, blob): self.blob = blob
    def read(self, addr, size):
        off = addr - 0x8000_0000
        return self.blob[off:off + size] if 0 <= off else None

blob = open("fixtures/out/arm/arithmetic.elf", "rb").read()
fw = strider.lift.lifter(sleigh.SleighArch.arm(), Firmware(blob))
```

`ReadOnlyMemory` is the same shape, passed as `rom=` so `LoadReadOnly` can fold
loads from constant addresses. `BufferReader` works as either; `reader.MemLike`
/ `reader.RomLike` name what each argument accepts.

A Python subclass of `MemReader`, `ReadOnlyMemory` or `JoinPredicate` is never
freed once instantiated (its instances are), so define it once, not in a loop.

## 2. Analyzing a function

`analyze` lifts, optimizes and resolves one function.

```python
result = prog.analyze("dispatch_value")     # by symbol name (ElfLifter) or address
cfg, function, unresolved = result           # an AnalyzeResult, a named 3-tuple
result.cfg, result.function, result.unresolved   # or read the fields
# unresolved: machine addresses of indirect branches that stayed unresolved
```

`analyze` returns even when it cannot vouch for every path: `cfg.incomplete`
lists each such address and why. Check it before trusting an absence ("no
store to X").

A plain `Lifter` needs an address and a calling convention:

```python
_cfg, fn, _u = lft.analyze(0x1000, sleigh.CallingConvention.x86_64_systemv())
```

A handle works on any thread. `analyze`, `build_cfg` and `add_elf` take it
exclusively; any other call on it meanwhile raises `StriderError: this
Lifter is already in use`. For parallelism use one handle per thread, each
built over `prog.arch`, `prog.reader()` and `prog.rom()` (gathered before any
analysis is in flight and after every `add_elf`), and call
`analyze(addr, prog.cc)`. `analyze` drops the GIL unless a reader is a Python
object; `Function.optimize` holds it.

### `lift.LifterOptions`, per-call tuning

```python
opts = strider.lift.LifterOptions(
    cfg=strider.cfg.CfgOptions(
        function_max_size=0x400,       # how far past the entry to decode; None is unbounded
        known_targets={0x1234: [0x2000, 0x2010],   # this branch goes to these
                       0x1250: "return"},          # and this one returns
        call_other_abis={"trap": sleigh.CallOtherAbi.no_return()},  # reclassify a user-op
    ),
    assumptions=strider.lift.AssumptionOptions(
        stack_global_disjoint=True,
        assume_incoming_args_survive_calls=True,
        return_slots_survive_untracked_writes=True,
        escape_analysis=False,
        noalias_allocators=[],            # callee addresses of malloc-like allocators
        distinct_sp_bases_disjoint=False,
        callee_preserves_stack_args=False,
        msr_register_keeps_mode=True,
        fp_rounding_mode_is_nearest=True,
    ),
    compact=True,                      # drop unreachable nodes at the end
    resolve_indirect_branches=True,    # False leaves every site an IndirectBranch
    per_address_ccs=None,              # {callee_addr: CallingConvention}, keyed by
                                       # the direct-call TARGET
    pipeline=None,                     # replace the optimizer pipeline
)
prog.analyze("dispatch_value", opts=opts)
```

`AssumptionOptions` holds claims strider cannot check; a wrong one makes the
answer wrong. Each field's risky value is the positive one:

- `stack_global_disjoint` (on): no constant address equals `sp + K` at
  runtime.
- `assume_incoming_args_survive_calls` (on): a callee leaves the incoming
  stack-argument slots alone, so an argument read after a call is found.
  Wrong when the function passes a callee a pointer into those slots or the
  register-argument homes below them (detected; the slots then stay unknown),
  when a callee reaches them through a pointer in a preserved register (not
  detected), or when the code stores its entry SP (APCS frames: nothing is
  found). A `CallingConvention.custom` whose ABI homes register arguments
  next to the stack ones (ARM, MIPS, PPC64, riscv, Win64) must pass
  `reg_args_adjoin_stack_args=True`.
- `return_slots_survive_untracked_writes` (on): a store strider cannot place
  on the entry stack, a callee or a user-op leaves the saved return address
  and entry stack pointer alone. Only return-target verification reads it;
  off, a return after such a write is reported unverified.
- `distinct_sp_bases_disjoint` (off): a store off an SP base other than the
  entry SP (an `sp & -16` local) does not alias an incoming argument slot.
- `escape_analysis` (off): when no stack address escapes the frame (through a
  call argument, a stored value, a return or a clobbered register), a spill
  load forwards across a call. It misses a callee reading an address from a
  preserved register, and is inert on a frame that stores its entry stack
  pointer (every PowerPC frame, an ARM APCS frame).
- `noalias_allocators` (empty): the listed callees return fresh pointers that
  overlap no live allocation, stack slot or global, so an arena allocator does
  not qualify. Deallocation is not modelled. A non-empty list also forwards
  spills across a listed call, as `escape_analysis` does.
- `callee_preserves_stack_args` (off): a callee leaves the outgoing-argument
  slots unchanged, which the psABIs do not require. Inert without
  `escape_analysis` or `noalias_allocators`.
- `msr_register_keeps_mode` (on): an ARM register-form `msr cpsr_c, rN`
  writes the mode already in force, so it re-banks nothing (masks and flags
  still update; immediate `msr` and `cps` still switch). Wrong for a mode
  built in a register (Linux's `safe_svcmode_maskall`, `fiqasm.S`) or a saved
  mode restored after a switch.
- `fp_rounding_mode_is_nearest` (on): the dynamic rounding mode of
  float-to-integer, narrowing and integer-to-float conversions is
  round-to-nearest-even (x86 MXCSR.RC and x87 RC, PowerPC FPSCR.RN, ARM FPSCR
  and AArch64 FPCR RMode, MIPS FCSR.RM). Off, each such conversion reads the
  mode, so a mode the function sets folds. Wrong after `fesetround`. Float
  arithmetic is taken to round to nearest either way.

The last two are read by the lift: `analyze` honours them and
`Function.optimize` refuses to add them.

`AssumptionOptions.none()` clears all nine; `AssumptionOptions()` keeps the
defaults. `none()` is the sound floor for memory that behaves as RAM (a load
reads back the last value stored) and IEEE-default float controls (MXCSR
FTZ/DAZ, PowerPC FPSCR NI/OE/UE, ARM/AArch64 FZ, MIPS FS clear; so not for
`-ffast-math` code). `none(...)` takes the constructor's keywords and sets only
those: `AssumptionOptions(escape_analysis=True)` holds six claims,
`AssumptionOptions.none(escape_analysis=True)` one.

Claims accumulate per `Function`: `function.assumptions` reads those in force.
`function.optimize(assumptions=...)` adds claims and withdraws none; a set that
leaves out a claim already in force raises `StriderError`. Add one with a
union (`a | b`, `a.union(b)`):

```python
from strider.lift import AssumptionOptions

function.optimize(
    assumptions=function.assumptions | AssumptionOptions.none(escape_analysis=True))
```

To drop one, analyse again without it.

`call_other_abis` reclassifies a Sleigh user-op by name ahead of the built-in
table. Each value is a `strider.sleigh.CallOtherAbi`: `noop()`, `pure()`,
`mem_clobber()`, `no_return()`, or one naming implicit registers:

```python
arch = sleigh.SleighArch.x86_64()
strider.sleigh.CallOtherAbi.custom(
    arch,                            # resolves the register names now
    implicit_reads=["RAX", "RDI"],   # read beyond the p-code operands
    implicit_writes=["RAX"],         # written beyond the p-code result
    clobbers_memory=True,
    no_return=False,
)
```

An unknown register name raises `StriderError` at construction.

x86 `hlt` is the `halt` user op and falls through, as kernel code resumes
after the interrupt. For userland code (a libc `_start`) pass
`call_other_abis={"halt": CallOtherAbi.no_return()}`.

`lft.user_op_names()` lists the user-op names the architecture can emit;
`lft.call_other_abi(name, opts=None)` reads the classification in force.
`None` means strider has no answer, which fails the lift of a function using
it. `crates/strider-py/examples/python/17_custom_abis.py` walks through this.

### ElfLifter metadata

```python
prog.arch                      # SleighArch
prog.cc                        # default CallingConvention
prog.endianness                # "little" or "big"
prog.is_arm_be8                # EF_ARM_BE8 set; False off ARM
prog.entry_point()             # ELF entry address
list(prog.functions())         # one Symbol per function address, address order
list(prog.iter_symbols())      # every Symbol, lazily (a SymbolIter)
prog.symbols()                 # {name: Symbol}

prog.symbol("f")               # Symbol; raises if nothing has that name
prog.symbol_opt("f")           # ... or None
prog.symbol_at(0x401234)       # the Symbol covering an address, or None

prog.read(addr, size)          # raw bytes, or None when unmapped
prog.reader()                  # the BufferReader over the mapped regions
prog.rom()                     # the read-only image LoadReadOnly folds from, or None
prog.add_elf("libc.so")        # merge another ELF (shared library)
```

Symbols can be attached after loading, to name a stripped image:

```python
prog.add_symbol_file("vmlinux.debug")   # its symbols, none of its bytes
prog.add_symbols({                      # names that live in no ELF at all
    "handle_irq": 0xffffffff81001200,
})
prog.add_symbols(                       # is_function applies to the whole call
    {"irq_table": (0xffffffff81800000, 0x400)},   # (address, size)
    is_function=False,
)
```

`add_symbol_file` takes a debug file linked at the image's own addresses
(`objcopy --only-keep-debug`, distro debuginfo), which `add_elf` would refuse
as an overlap. `add_symbols` takes an address or an `(address, size)` pair; a
size lets `symbol_at` resolve addresses inside it. A name an ELF already
carries keeps its ELF answer.

A `Symbol` carries `name`, `address`, `size`, `end`, `is_function`, `is_thumb`,
`local_entry` and `region`. `size` and `end` are `None` when the ELF records no
extent. `region` is the `(start, end)` of the loaded region it maps into. A
Thumb function's `address` keeps the ISA bit (so `analyze` enters in Thumb);
`end`, `region` and `symbol_at` measure from `address & ~1`. `local_entry` is
the ppc64 ELFv2 local entry (`address` plus the `st_other` offset), and
`address` elsewhere.

`symbol(name)` prefers a definition, else an undefined symbol with an address
(a PLT stub, an object file's extern). `symbol_at` takes the nearest symbol at
or below the address whose extent reaches it, ranking aliases by extent, then
by being code; `functions()` prefers code, so the two can name different
symbols at one address.

## 3. The `Function` and its `Node`s

```python
function.node_count()          # total node ids
function.entry_node()          # id of the Entry node
function.node_ids()            # every id
function.count_regions()       # Region nodes reachable from entry
function.cfg                   # the Cfg it was lifted from
function.assumptions           # the AssumptionOptions in force (section 2)
function.clone()               # an independent copy, sharing the Cfg
function.compact()             # drop nodes unreachable from entry; renumbers ids
function.validate()            # None, or a message naming the broken invariant

n = function.node(some_id)     # a Node handle
```

A `Node`:

```python
n.id                           # node id; the handle goes stale after
                               # optimize / rewrite / rewrite_all / compact
n.kind()                       # "IntBinaryOp(Add)", "IntConst", "Load(RAM)"
n.op()                         # "Add" / "Less" / ... or None for op-less kinds
n.value_type()                 # "I64" / "I1" / "F64" / ... or None
n.inputs(), n.outputs()        # neighbouring nodes (Node handles, not ids)
n.uint(), n.sint(), n.boolean(), n.float_bits()   # constants, at any width
n.call_other_name()            # a CallOther's user-op name, else None
n.vn()                         # an InitialVar's register, a Call's return
                               # register (first value output), else None
n.asm_fingerprint()            # machine addresses this node was lifted or folded from
```

A rewrite adds the addresses of the nodes it folded to the node it builds.

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
function.to_text()                           # a header, then one line per node
function.to_text(fingerprints=True)          # ... each line ending in its asm addresses
function.to_html("graph.html")               # dark-themed standalone page (path=None returns text)
function.neighborhood_dot(function.entry_node(), depth=2, pretty=True)  # local subgraph DOT
# also hub_cap=, max_nodes=, count_producers= (section 10)
```

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

A raw int operand is `int_const(that int)`: `p.int_add(c, 8)` is
`p.int_add(c, p.int_const(8))`. Pin a width with `.of_width(32)`.

`int_const_any_width(v)` matches a constant whose zero or sign extension is
`v`, so `0x1234` never matches `0x34` at `I8`.

### Operators

Every operator is a free function (the `.pyi` lists them). They match the
canonical IR ([optimizations](optimizations.md)); the aliases build it for you:
`int_sub(a, b)` is `int_add(a, int_neg(b))`, and `int_le` / `int_ne` are their
lowered comparisons.

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

`int_add`, `int_mul`, `int_and`, `int_or`, `int_xor`, `bool_and` / `bool_or` /
`bool_xor`, `float_add`, `float_mul`, `int_eq`, `int_carry`, `int_scarry`,
`float_eq`, and so `int_ne` / `float_ne`, match **commutatively**; every other
operator keeps the order written. `.ordered()` pins the order on any binary
pattern (it raises on one with no operand pair, such as `anything()`):

```python
p.int_binary("Add", p.int_const(k), p.anything()).ordered()   # k on the left
```

### Node builders

Control, memory and call nodes have typed builders with named slots.
`find_all` takes a builder directly, or call `.into_pat()`.

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

Call slots: `.target(p)` (an int address or a list is fine), `.arg(i, p)`,
`.mem(m)`, `.ctrl(p)`, `.res()` (pin to the return value when nested as a
value), `.output(slot)`. `indirect_branch().target` and `switch().selector`
take a list too.

A call's argument index counts the integer registers, then the float ones,
then any vector window, then stack arguments: on x86-64 SysV `.arg(6)` is
XMM0; on PPC64 the first stack argument is `8+25` (userland) or `8+13`
(`*_linux_kernel`). `function_arg` / `function_arg_float` index the two
classes separately.

If slots: `.cond(p)`, `.ctrl(p)`, `.capture_true(c)` / `.capture_false(c)`.
To match what an edge leads to, join on the edge:
`fn.find_all([if_else().capture_true(t), region().any_input(t)])`.

A pattern of more than 256 nodes (a shared sub-pattern counts once per slot;
an alternation as its deepest arm), or nested deeper than the thread's stack
allows, raises `StriderError`.

### The shared builder vocabulary

The builders share these methods, declared in the stubs as `typing.Protocol`
mixins:

| mixin | methods | on |
| --- | --- | --- |
| `NodePat` | `.capture(c)`, `.when(f)`, `.into_pat()` | every builder |
| `CtrlPat` | `.ctrl(p)` | `if_else`, `switch`, `call`, `call_other`, `ret`, `indirect_branch`, `unreachable` |
| `MemAccessPat` | `.addr(p)`, `.bit_width(n)`, `.space(s)`, `.stack_offset(k)`, `.stack_only()`, `.non_stack()`, `.heap_only()` | `load`, `store` |
| `MemPat` | `.mem(p)` | `call`, `call_other`, `indirect_branch`, `load`, `store` |
| `InputPat` | `.any_input(p)`, `.input(i, p)` | every node builder with inputs, except `entry()` and the operand builders `function_arg` / `int_binary` / `float_binary` / `bool_binary` |
| `OrderedPat` | `.ordered()` | `Pat`, `int_binary`, `float_binary`, `bool_binary` |
| `OutputPat` | `.output(slot)`, `.any_output()` | every node builder with outputs, except the sinks `ret` / `indirect_branch` / `unreachable` and the operand builders |

```python
isinstance(p.load(), p.InputPat)      # True
isinstance(p.entry(), p.InputPat)     # False
```

`.input(i, p)` and `.output(slot)` address raw slots, numbered per node kind
(`crates/strider-ir/src/node/signature.rs`): `Call` inputs are
`[ctrl, mem, target, sp, arg0, ...]`, `Load`'s `[mem, addr]`, `If`'s
`[ctrl, cond]`; `Call` outputs are `[ctrl, mem, result, ...clobbers]`, `Load`'s
`[value]`. In a raw slot a typed value pattern binds a value edge, a wildcard
any edge, a control producer (`entry()`, `region()`, `if_else()`) its control
edge, and a `call()` / `call_other()` any of its outputs (`.res()` keeps it to
the value).

Every typed builder nests in every slot but a memory one; one that can never
match where it sits (a `store()` or `ret()`, `entry()` or `region()` as a value)
raises at query time. A memory slot (`.mem(p)`, `mem_phi().phi_input(i, p)`)
takes memory producers, a wildcard (`anything()`, `var(c)`, a bare `Capture`)
or a `one_of` over them; a width or type there raises.

`phi()` / `mem_phi()` `.phi_input(i, p)` is raw slot `i + 1`; `.input(0, p)` is
the phi token.

`.output(slot)` and `.any_output()` return a terminal taking `.capture(c)`,
`.of_width(w)` or `.of_type("i64")`, which hands the builder back.

### `any_input`, matching *some* input

```python
p.mem_phi().any_input(p.store(addr=p.anything(), data=p.anything()))  # some input is a store
p.call().any_input(p.anything())                                      # some input, any kind
p.load().input(1, x)                                                  # raw slot 1, the address
```

`any_input` binds by the same rules as a raw slot; a wildcard matches any
input, the phi token included.

### `one_of` / `first_of`, alternation (OR)

```python
base, off = p.Capture("b"), p.Capture("off")
p.one_of([p.int_add(base, p.int_const(off)), p.var(base)])   # base+K, or bare base
```

`one_of` is OR, dual to a `find_all([...])` list (AND). An arm is anything a
top-level pattern is, and the result nests in any slot:

```python
p.load().mem(p.one_of([p.store(), p.mem_phi()]))        # memory slot
p.ret().ctrl(p.one_of([p.call(), p.region()]))          # control slot
```

`one_of` is a **union**: every matching arm is reported with its own bindings.
A match is identified by what it binds, so arms matching one node with the same
bindings are one match; give them captures to tell them apart.

`first_of` is **ordered**: at each value, the first listed arm that matches
wins, so list the most specific first. Several `first_of`s settle in pattern
order: operands left to right, inner before outer, a builder's own slots before
its `any_input`s and `input`s. The own-slot orders are `load` addr, mem;
`store` addr, data, mem; `call` target, ctrl, args, mem; `call_other` ctrl,
mem, args; `ret` ctrl, return values; `indirect_branch` target, ctrl, mem;
`switch` selector, ctrl; `if_else` cond, ctrl; `phi` / `mem_phi` inputs, token.
A query holding a `first_of` runs every configuration before answering.

### `field` / `code_ptr`, the two canonical-form alternations

```python
f = p.field(p.function_arg(0))              # base+K, or bare base at offset 0
function.find_all(f.load())                  # f.store(data) for a write
f.offset(hit)                                # 0 for the bare arm
p.field(p.function_arg(0), offset=8)         # only that offset
p.call().target(p.code_ptr(p.function_arg(0)))   # f, or f & -2
```

`ConstantFold` rewrites `base + 0` to `base`; `field` matches both forms and
`offset(m)` reads 0 for the bare one. `offset` takes an `int`, a `Capture` or a
capture name. `code_ptr` accepts only the ISA-mode mask `-2`.

### Captures

```python
c = p.Capture()                # fresh, anonymous
off = p.Capture("off")         # named; two Capture("off") are one variable
p.int_add(off, p.anything())   # a Capture operand binds
p.int_add("base", off)         # a bare string is NOT a capture: this builds,
                               # and find_all raises on it
```

Read a capture back by object or name (`hit.uint(off)`, `hit.uint("off")`).
`"_"` and `"any_"` are reserved.

### Chaining methods

Value-op functions (`int_add`, `int_const`, ...) return a finished `Pat`; typed
builders (`load`, `call`, `int_binary`, ...) return a builder. Both carry
`.capture(c)` (a `Capture` or a name) and `.when(f)`; only a builder has
`.into_pat()`, which holds the builder by reference, so later edits to the
builder show through.

Only a value pattern takes `.of_width(bits)` / `.value_ty("i64")` /
`.bool_valued()`:

```python
p.int_const(c).of_width(32)             # constrain the constant's width
p.var(c).value_ty("i64")                # ... or a captured value's type
```

## 5. Running queries

```python
function.find_all(pat)                       # every match, deduplicated
function.find_all(pat, ignore_casts=True)    # CastMask.all(); default False
function.find_all(pat, ignore_root=True)     # dedup on captures alone
function.find_all([pat1, pat2], constraints=[...])   # a join; constraints in 7
function.find_unique(pat)                     # the single match, else StriderError
function.find_unique_value(pat, off)          # the single captured VALUE, or None
```

`ignore_casts` takes a bool or a `CastMask` naming the casts to walk through:
`CastMask.zero_extend()`, `.sign_extend()`, `.extend()` (both), `.truncate()`,
`.int_bits_to_float()`, `.float_bits_to_int()`, `.all()`, `.none()`, combined
with `|` and `&`. Under a mask each operand keeps the matches at the outermost
cast level that matches, settling in pattern order as `first_of` does.

`find_unique` fails on two structurally distinct matches even when they bind
the same value. `find_unique_value(pat, capture, signed=False)` dedups by the
captured constant: `None` for no match, the value when all agree,
`StriderError` for two distinct values. A list `pat` and `constraints=` work as
in `find_all`.

```python
stride = p.Capture("stride")
# The jump-table stride: one value across distinct multiply nodes.
function.find_unique_value(p.int_mul(p.anything(), p.int_const(stride)), stride)   # -> 4
```

## 6. Reading a `Match`

```python
for hit in function.find_all(pat):
    hit.root                     # id where the top pattern matched
    hit.roots                    # one root id per pattern of a joined
                                 # find_all([p1, p2]); hit.root is roots[0]
    hit.has("off")               # did this capture bind?
```

Each reader takes the capture or its name and **raises** when the capture is
unbound or its node lacks that aspect; the `_opt` form returns `None`.

```python
hit.uint(off)                    # unsigned int (raises if not one)
hit.uint_opt(off)                # ... or None
hit.sint(off)                    # signed
hit.boolean(flag)                # bool
hit.float_bits(f)                # raw float bits
hit.op(node)                     # "Add" / ...
hit.value_type(node)             # "I64" / ...
hit.node(base)                   # a Node handle
hit.vn(reg)                      # a varnode
hit.asm_fingerprint(node)        # machine addresses (a list; [] if unbound)

hit.uint("off")                  # by name, when the capture has one
int(hit[off]); hit[off] == 8     # hit[c] converts and compares a numeric capture
```

An anonymous capture suits a hole you read back once:

```python
off = p.Capture()                                  # no name
for hit in function.find_all(p.load(addr=p.int_add(p.anything(), p.int_const(off))),
                             ignore_casts=True):
    print("constant addend", hit.uint(off))
```

`pattern.CaptureKey` is a `Capture` or its name. `hit[c]` is a `BoundCapture`
carrying `has`, `int()` and `==`; it has no truth value (`if hit[c]` raises
`TypeError`), so test `hit[c].has` or `c in hit`.

## 7. Constraints (`strider.pattern.constraints`)

A joined `find_all([...])` correlates captures shared across its patterns;
constraints filter the joined rows.

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

`dominates(a, b)` is control-flow dominance over captured nodes or `If` edge
captures (not reachability: no single incoming edge dominates a merge).
`phi_input_from_edge(phi, edge, value)` says the value `phi` merges from that
edge is `value`.

`phi().input_from(edge, value)` and `mem_phi().input_from(edge, value)` build
that constraint: a pattern `value` becomes a phi input, a `Capture` names a
value another pattern binds. Pass the phi's `constraints()` to the query; the
[guide](python-guide.md#constraints-relating-matches-by-control-flow) shows
one.

### `JoinPredicate`, your own logic

Subclass `k.JoinPredicate` and override two methods:

```python
class MyRule(k.JoinPredicate):
    def captures(self):       return []      # the captures it reads; default []
    def constraint(self, m):  return True    # m is the joined Match
```

Declared captures connect otherwise independent patterns; `constraint` runs
once they are bound, and an exception in it surfaces at the query. It composes
in `any_of` / `all_of` / `negate`. The built-in relations return a
`constraints.JoinConstraint`.

## 8. Rewrites (`strider.template`)

```python
function.rewrite(find=pat, replace=tmpl)             # -> how many times it fired
function.rewrite_all([(pat1, tmpl1), (pat2, tmpl2)]) # -> total fire count
```

`rewrite(find, replace)` replaces every match with a built value. `replace` is
a `strider.template.Template`, a `Capture`, or a capture's name. `strider.template`
builds the value ops and reuses the match's captures (`var(c)`); `float_le`,
`float_is_nan` and the wildcards match but do not build. A
`strider.pattern.Pat` is never a right-hand side: `TypeError` as `replace`,
`StriderError` nested in a template.

A match is skipped, uncounted, when the replacement has another width, or (for
a `find` through a phi or region) would read the matched value or is not
available where it is read.

`rewrite_all` makes one walk; at each node the first rule that fires wins.
Neither runs to a fixed point. Both stale every `Node` handle, even when they
return 0. The [guide](python-guide.md#rewriting-the-graph) walks through one.

## 9. The optimizer (`strider.opt`)

`analyze` runs the default pipeline. To run your own:

```python
pipe = strider.opt.OptimizerPipeline.empty()     # no passes
pipe.add(strider.opt.ConstantFold())             # a main pass, repeated to a fixed point
pipe.add_post(strider.opt.CallStackArgCollect()) # a post-pass, run once at the end
pipe.passes                                      # ["ConstantFold"]
pipe.post_passes                                 # ["CallStackArgCollect"]
pipe = strider.opt.OptimizerPipeline.default()   # the standard set, in analyze's order
function.optimize(pipeline=pipe)                 # runs in place, against the lifter's rom
```

Main passes (`add`): `ConstantFold`, `LoadReadOnly`, `KnownBits`,
`FlagCmpCanonicalize`, `IfCondInversion`, `PhiCollapse`, `RegionCollapse`,
`DeadBranchElimination`, `LoadForward`. Post-passes (`add_post` only):
`CallStackArgCollect`, `FunctionArgDetect`; `add_post` also takes a main pass,
run once. The `MainOptimizerPass`, `PostOptimizerPass` and `OptimizerPass`
aliases name those groups. [optimizations.md](optimizations.md) says what each
pass does.

## 10. Visualizing

```python
strider.explore.visualize(function)   # interactive explorer; prints a URL, blocks
strider.explore.visualize(cfg)        # a Cfg works too
```

The explorer opens on the whole graph; `visualize(whole=False)` opens on the
neighborhood around the entry, which stays fast on a large function.

Pan by dragging or with the arrow keys (shift for a longer step); ctrl+wheel
zooms about the pointer, `+` / `-` about the centre, `f` fits, `0` returns to
100%. The toolbar: **depth** (hops from the centre, seeded by
`visualize(depth=...)`), **hub cap** (nodes with more consumers are not
expanded), **max nodes**, **+prod** (count inputs toward the hub cap),
**pretty** (inlined constants, register names), **whole** and **reset**. The
three limits start at `0`, no limit; **pretty** and **whole** start on.

`visualize()` binds `host='127.0.0.1'`, `port=0` (a free port), both keyword
only. A non-loopback `host` serves the graph and the bytes behind it to anyone
who can reach the port, unauthenticated.

`visualize()` blocks until interrupted and returns the port; `background=True`
serves on a non-daemon thread and returns at once:

```python
port = strider.explore.visualize(fn, background=True)
# ... keep querying, analysing and rendering on this thread ...
strider.explore.shutdown(port)   # stops the server and joins its thread
```

A render does not use the handle, so it stays free to `analyze`. A server left
running is shut down at interpreter exit.

For static output use the renderers in
[section 3](#3-the-function-and-its-nodes).

## 11. Registers and architecture (`strider.sleigh`)

```python
arch = sleigh.SleighArch.x86_64()      # a preset; also x86(), arm(), arm_thumb(),
                                       # aarch64(), mipsbe32(), ppc64le(), ...
arch.name()                            # "x86_64"
arch.endianness()                      # "little" / "big"
arch.reg("RAX")                        # the Vn for a register name, or None;
                                       # exact case: upper on x86, lower elsewhere
arch.reg_name(vn)                      # the name for a Vn, or None

lft.pcode_at(entry, addr)              # one instruction's p-code as text; an odd
                                       # entry decodes as Thumb, as in analyze

lft.user_op_names()                    # every Sleigh user-op name this arch emits
lft.call_other_abi("rdtsc")            # how one is classified, or None
```

`Vn` is a varnode (a register or memory location); `VnSpace` names its space
(`VnSpace.RAM`, `.REGISTER`, `.CONST`, `.UNIQUE`). `CallingConvention` presets
(`x86_64_systemv()`, ..., also `CallingConvention.preset(name)` over
`sleigh.CC_PRESETS`) describe argument passing; `CallingConvention.custom(arch,
...)` builds one from register names, and `.preserves_all()` /
`.preserves_regs()` derive a variant for a per-address override. `CallOtherAbi`
describes one user-op. `crates/strider-py/examples/python/17_custom_abis.py`
uses each.

A call is modelled by its target's calling convention, never by reading the
callee. On 32-bit x86 three kinds of callee need their own convention in
`LifterOptions(per_address_ccs={callee: cc})`:

- A callee that pops its caller's stack (`ret $imm16`: stdcall, fastcall,
  thiscall, a struct return's hidden pointer) needs `ret_stack_pop=4 + imm16`,
  or stack reads after the call are off by `imm16`.
- A PC thunk (`__x86.get_pc_thunk.bx`) needs a convention that does not list
  its register as callee-saved.
- Under `x86_linux_kernel()`, a variadic (`printk`) or `asmlinkage` callee
  takes every argument on the stack and needs `x86_cdecl()`; otherwise its
  first argument reads as `arg(3)`.

`CallingConvention.custom` states the first two, a preset the third:

```python
x86 = sleigh.SleighArch.x86()
pops_8 = sleigh.CallingConvention.custom(
    x86,
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
kernel = strider.lift.LifterOptions(
    per_address_ccs={printk_addr: sleigh.CallingConvention.x86_cdecl()}
)
```

ARM32 hard-float: the convention names the double carriers `d0..d7`, so for
`float` arguments only position 0 is right (float argument n is `s_n`, inside
`d_{n/2}`, while `function_arg_float(n)` reports `d_n`), and positions 8..15
have no entry.

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
cfg.incomplete                         # [(address, reason)], empty when complete
```

`cfg.incomplete` lists every machine address the CFG cannot vouch for, with
one of seven reasons (`strider.cfg.INCOMPLETE_REASONS`):

- `"unresolved_branch"`: an indirect branch with no complete answer (lost a
  successor, oscillated or narrowed, still growing at the iteration cap), or a
  return whose target is not provably the caller's (`push rsi; ret`) that you
  did not seed `"return"`. `analyze`'s `unresolved` holds exactly these.
- `"unverified_seed"`: a site settled only by your `known_targets` (including
  a seeded `"return"` whose target is not provably the caller's), or consumed
  as a tail call. Also an arm whose ISA mode was copied from its seeded
  siblings rather than derived. A derived return is re-checked each round
  under `return_slots_survive_untracked_writes`; analyse with it cleared to
  audit one.
- `"isa_mode_conflict"`: an address reached in two ISA modes (32-bit ARM and
  MIPS presets only).
- `"interior_target"`: a branch target off every instruction boundary, or a
  region start a later decode stepped over (overlapping code).
- `"unmapped_target"`: a target or fall-through no image byte backs, seated as
  an empty tail-call stub.
- `"undecodable_target"`: a target or post-call fall-through that holds no
  instruction (bytes Sleigh rejects, or `data_ranges`). A branch there leaves
  through an empty stub; a call there ends as no-return.
- `"past_function_end"`: where code falls through past `function_max_size`
  or a `fragment_ranges` end (sequentially, on a not-taken branch, or a
  conditional instruction skipping itself), at the first address after the
  last decoded instruction. The region tail-calls it; a call whose return
  address is there is reported too unless its target is in
  `no_return_targets`.

A seeded arm that would be unmapped, undecodable or past the end is dropped
and named. The five target reasons accumulate across resolution rounds; the
two site reasons are the final round's.

`CfgOptions` (via `LifterOptions.cfg` or `Lifter.build_cfg`) tunes CFG
construction:

```python
strider.cfg.CfgOptions(
    function_max_size=None,
    allow_code_before_start_addr=False,
    known_targets={0x401000: [0x401020, 0x401040]},   # your own answers, seated
    call_other_abis={"syscall": sleigh.CallOtherAbi.mem_clobber()},   # per user-op
    data_ranges=[(0x401100, 0x401108)],               # [start, end) never decoded
    fragment_ranges=[(0x400f00, 0x400f20)],           # [start, end) also this function's code
    no_return_targets=[0x401200],                     # direct-call targets that never return
)
```

`function_max_size` bounds the decode to `[entry, entry + size)` (`0` raises
`ValueError`). `allow_code_before_start_addr` follows branches below the entry;
with a size it raises `ValueError`.

`known_targets` seats indirect-branch answers and seeds the resolution loop,
which adds its own findings, so it composes with
`resolve_indirect_branches=False`. A seeded site leaves `unresolved` unless the
CFG cannot seat it. One target outside the function is a tail call; several
with one outside or off an instruction boundary defer the site. A wrong seed
can hide the real arms, so seeded sites are `"unverified_seed"`.

`data_ranges` marks bytes as data (an ARM literal pool). `ElfLifter.analyze`
and `build_cfg` fill it from the ELF's `$d` mapping symbols unless the options
name some.

`fragment_ranges` adds code outside the bound to the function, such as GCC's
`<name>.cold` partition of unlikely blocks: a branch into one is followed, not
a tail call, and a fall-through past one's end is `"past_function_end"`.
`ElfLifter.analyze` and `build_cfg` fill it, unless the options name some,
from the sized function symbols `<name>.cold` and `<name>.cold.N` of every
function symbol at the entry. Fragments are matched by name alone, so local
functions sharing a name share their fragments. A `<name>.part.N`,
`.constprop.N` or `.isra.N` clone is a separate callee, and a branch into a
cold partition named for another function stays a tail call.

`no_return_targets` lists direct-call targets that never return (`exit`,
`abort`); it is the only way to mark one. List the address the call names: a
Thumb or MIPS16 callee at its even address (`addr & ~1`). `ElfLifter.analyze`
and `build_cfg` also cover a ppc64 ELFv2 symbol's `local_entry` (and `analyze`
does the same for `per_address_ccs`); a plain `Lifter` takes both tables as
given.

## Errors

Analysis failures are `strider.StriderError`. A wrongly typed argument raises
the usual builtin (`TypeError`, `OverflowError`). `load_elf` raises
`FileNotFoundError` for a missing path, `ValueError` for a non-ELF file or an
unsupported arch or ABI (microMIPS included, as does `add_elf`), and
`StriderError` for a malformed ELF. Range-checked constructor arguments
raise `ValueError`: `CfgOptions(function_max_size=0)`, a size with
`allow_code_before_start_addr=True`, a `known_targets` string other than
`"return"`, a string `call_other_abis` value or a repeated name, an empty
`data_ranges` range, a `BufferReader` region past the address space. Every
other bad value is a `StriderError`: `add_elf` / `add_symbol_file` on a bad
path, an unknown `style=`, a reserved capture name, an unknown node id, an
unbound capture read without `_opt`, and a `Match` read after `compact`,
`optimize`, `rewrite` or `rewrite_all`.

The message is the error and its causes; `.backtrace` adds the Rust
backtrace:

```python
import logging

log = logging.getLogger(__name__)

try:
    prog.analyze("no_such_function")
except strider.StriderError as e:
    log.error("%s", e)            # the error and its causes
    log.debug("%s", e.backtrace)  # plus the Rust frames
```

`STRIDER_BACKTRACE=1` (also via `os.environ`) puts the trace in the message.
Importing strider enables backtrace capture unless `RUST_LIB_BACKTRACE` or
`RUST_BACKTRACE` is set; `RUST_BACKTRACE=0` leaves `.backtrace` holding only the
message.
