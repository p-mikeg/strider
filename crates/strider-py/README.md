# strider-py

Python bindings for the Strider binary analysis pipeline.

Strider lifts native binaries to a sea-of-nodes IR, runs an
optimization pipeline, and exposes the lifted IR for arbitrary pattern
queries.  This crate is the Python entry point.

> **See [`examples/python/`](examples/python/) for runnable end-to-end
> walkthroughs of every major surface — quickstart, custom Python
> readers, pattern rewrites, custom optimizer pipelines, visualization,
> complex pattern queries, callback-style ROMs.**

## Build (development)

The recommended workflow uses [`uv`](https://docs.astral.sh/uv/). The uv
project is hosted at the **workspace root**, so run from there (not this
directory):

    uv sync --group dev          # creates .venv and installs dev deps
    uv run maturin develop       # builds the Rust extension into .venv
    uv run pytest                # runs the test suite

That's it.  `uv sync` reads `[dependency-groups].dev` from the root
`pyproject.toml` (PEP 735), `uv run` activates `.venv` automatically,
and the `[tool.pytest.ini_options]` block points discovery at
`crates/strider-py/tests/python/`.  maturin reaches this crate via
`manifest-path = crates/strider-py/Cargo.toml`.

The integration tests need fixtures built once via:

    (cd fixtures && make)

For release builds:

    uv run maturin develop --release

### Without `uv` (legacy pip flow)

    python -m venv .venv
    source .venv/bin/activate
    pip install -e ".[dev]"   # if you also added the optional extra
    # or directly:
    pip install maturin pytest pyelftools
    maturin develop
    pytest

## Building a release wheel

    uv run maturin build --release

The wheel is written under
`/path/to/workspace/target/wheels/strider-0.1.0-cp39-abi3-<platform>.whl`.
The wheel is `abi3` (Python 3.9+) so a single artifact works for every
CPython 3.9+ version.  No CI / PyPI upload is configured; install via
`uv pip install /path/to/wheel.whl` directly.

## Quick example

```python
import strider
from strider.pattern import Capture, var, add, load

# 1. Load — auto-picks arch + calling convention from the ELF header.
#    `s` IS a `Lifter` (`isinstance(s, strider.Lifter)` is true).
s = strider.load_elf("fixtures/out/x86/memory.elf")

# 2. Analyze a single function by symbol name (or by address int) —
#    returns the same `(Function, unresolved)` tuple as `Lifter.analyze`.
function, unresolved = s.analyze("array_sum")

# 3. Query the optimized graph with a pattern.
base, off = Capture(), Capture()
pat = load(addr=add(var(base), var(off)))
for hit in function.find_all(pat, ignore_casts=True):
    print(hit.uint(off))
    # `function.asm_fingerprint(hit.root)` returns the
    # contributing-instruction machine addresses — proof-of-correctness
    # audit trail.

# 4. Visualize.
function.to_html("graph.html")

# Walk every function in the binary:
for name in s.functions():
    if "init" in name:
        print(name, hex(s.symbol(name)))
```

### Analyze many functions with one setup

An `ElfLifter` *is* the analyse-many handle (it IS a `Lifter`): it
bundles the arch + cc + memory once, so analysing many functions is
just calling `analyze` repeatedly.  Any option can be passed (or
overridden) per call:

```python
for name in s.functions():
    function, unresolved = s.analyze(name)   # only the target per call
# Per-call option:
function, unresolved = s.analyze("array_sum", function_max_size=64)
```

For a raw firmware blob / non-ELF source, build a `Lifter` directly over
a `BufferReader` (address targets only — there's no ELF symbol table; do
the name → address lookup at the call site if you have one).
`Lifter.analyze(addr, cc, ...)` returns the same `(Function, unresolved)`
tuple as `ElfLifter.analyze`:

```python
mem = strider.BufferReader(0x8000, firmware_bytes)
lift = strider.lifter(strider.SleighArch.arm_thumb(), mem)
function, unresolved = lift.analyze(
    0x8000, strider.CallingConvention.arm_aapcs()
)
```

For workflows that need granular control — explicit calling
conventions (Linux kernel / syscall / custom), callback-style memory
readers, custom optimizer pipelines, per-address CC overrides — drop
down to the building blocks documented below.

## Low-level API — building blocks

```python
import strider
from strider.pattern import Capture, var, add, load

# 1. Build a raw code reader.  For an ELF, `strider.load_elf(path)`
#    parses sections + symbols + relocations and answers `.symbol(name)`;
#    for a firmware blob / custom source, populate a `MemoryMap`
#    directly with `.add_region(addr, bytes)`.
elf = strider.load_elf("fixtures/out/x86/memory.elf")
mem = strider.MemoryMap()
mem.add_region(elf.entry_point(), elf.read(elf.entry_point(), 0x1000) or b"")

# 2. Run the full pipeline (CFG → IR → optimize, including the
#    indirect-branch fixed-point loop) in one call.
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem,
    rom=mem,
    entry=elf.symbol("array_sum"),     # or any address int
    allow_code_before_start_addr=True,
)

# 3. Query the optimized graph with a pattern.
base, off = Capture(), Capture()
pat = load(addr=add(var(base), var(off)))
for hit in result.function.find_all(pat, ignore_casts=True):
    print(hit.uint(off))

# 4. Visualize.
result.cfg.to_html("cfg.html")
result.function.to_html("graph.html")
```

## Building blocks

The convenience `strider.run` wraps these explicit steps:

```python
arch = strider.SleighArch.x86()
cc = strider.CallingConvention.x86_cdecl()
elf = strider.load_elf("fixtures/out/x86/memory.elf")
mem = strider.MemoryMap()
mem.add_region(elf.entry_point(), elf.read(elf.entry_point(), 0x1000) or b"")

# `Lifter` is the LOW-LEVEL lift-driver (lift one CFG, no indirect-branch
# resolution) — distinct from the high-level `ElfLifter` returned by
# `strider.load_elf` and the `Strider` run handle from `strider.strider`.
# It OWNS the Sleigh (built from `mem`); `cc` is bound at construction.
s = strider.Lifter(arch, mem, cc)
cfg = s.build_cfg(0x401000)
graph = s.analyze_cfg(cfg).function

pipe = s.build_optimizer_pipeline()
pipe.add(strider.opt.LoadReadOnly(mem))
graph.optimize(pipe)
```

> **Note:** the `Lifter` owns its `Sleigh`, so `lifter.build_cfg(entry)`
> and `lifter.analyze_cfg(cfg)` both go through it — no separate `Sleigh`
> handle to thread.  The returned `Cfg` keeps rendering by holding a
> reference back to its `Lifter`.

## Custom optimizer pipeline

```python
pipe = strider.OptimizerPipeline.empty()
pipe.add(strider.opt.ConstantFold())
pipe.add(strider.opt.KnownBits())
pipe.add(strider.opt.LoadForward(sleigh, cc, arch))
pipe.add_post(strider.opt.FunctionArgDetect(sleigh, cc))
pipe.add_post(strider.opt.CallStackArgCollect(sleigh, cc))
graph.optimize(pipe)
```

## Pattern → pattern rewrite

```python
from strider.pattern import Capture, var, add, int_const

x = Capture()
n = graph.rewrite(find=add(var(x), int_const(0)), replace=var(x))
graph.reoptimize()  # re-run the default pipeline to collapse downstream effects
```

## Patterns and captures

Two equivalent ways to bind a sub-expression:

```python
# Capture-object form.
ptr, off = Capture(), Capture()
pat = load(addr=add(var(ptr), var(off)))

# String-shorthand form (auto-interned per process).
pat = load(addr=add("ptr", "off"))

# Back-reference: same name twice → must agree.
pat = add("x", "x")  # x + x

# Predicate guard via .when:
hits = graph.find_all(load().when(lambda m: True))

# Typed builder with .ordered() to disable commutative retry:
from strider.pattern import int_binary
pat = int_binary("Add", "x", "y").ordered()  # left-to-right only
```

The `_` and `any_` strings are reserved wildcards (they convert to
`any_()`); using them via `.cap(...)` raises `StriderError`.

## Pattern coverage

`strider.pattern` mirrors the entire `pattern` Rust crate:

* **Wildcards / consts:** `any_()`, `var(c)`, `int_const(n)`,
  `bool_const(b)`, `float_const(bits)`, `any_int_const(c)`,
  `any_bool_const(c)`, `any_float_const(c)`, `predicate(f)`.
* **Integer binary / unary / cmp:** `add`, `sub`, `mul`, `div`,
  `sdiv`, `rem`, `srem`, `shl`, `shr`, `sshr`, `and_`, `or_`,
  `xor`, `int_eq`, `int_lt`, `int_le`, `int_slt`, `int_sle`,
  `int_carry`, `int_scarry`, `int_sborrow`, `neg`, `not_`.
* **Bool binary / unary:** `bool_and`, `bool_or`, `bool_xor`,
  `bool_not`.
* **Float binary / unary / cmp:** `float_add`, `float_sub`,
  `float_mul`, `float_div`, `float_neg`, `float_abs`,
  `float_sqrt`, `float_ceil`, `float_floor`, `float_round`,
  `float_eq`, `float_ne`, `float_lt`, `float_le`.
* **Conversions / bitcasts:** `int_to_float`, `float_to_int`,
  `float_to_float`, `int_bits_to_float`, `float_bits_to_int`.
* **Casts / widths:** `truncate`, `popcount`, `lzcount`,
  `zero_extend`, `sign_extend`, `extend(op_str, x)`.  (There are no
  `cast_to_*` builders: booleans are the integer `I1`, an int→float
  cast is `int_bits_to_float`, and a float reprecision is
  `float_to_float` — see the conversions list above.)
* **Memory & control:** `load`, `store` (use `.stack_only()` /
  `.stack_offset(k)` for SP-relative accesses), `call`, `call_other`,
  `ret`, `if_`, `phi`, `mem_phi`, `value_phi`, `initial_var`,
  `function_arg`, `function_arg_any`.
* **Typed family dispatchers:** `int_binary(op_str, l, r)`,
  `bool_binary(op_str, l, r)`, `float_binary(op_str, l, r)` —
  return builder objects that chain `.ordered()` / `.capture(c)` /
  `.cap("name")` / `.when(f)` / `.into_pat()`.
* **Variant-agnostic:** `int_bin_any`, `int_un_any`, `int_cmp_any`,
  `bool_bin_any`, `float_bin_any`, `float_un_any`, `float_cmp_any` —
  bind the matched op variant to a Capture.  (`bool_un_any` was removed
  alongside the former BitNot unary-op; a 1-bit logical NOT is
  `Xor(_, IntConst(1)):I1`, matched via `bool_bin_any`.)

`float_is_nan(x)` desugars to `Xor(FloatEqual(x, x), IntConst(1)):I1` —
the same shape the pcode lifter emits for Sleigh's `FLOAT_NAN` op (the
1-bit logical NOT is `Xor(_, IntConst(1))` since the former BitNot
unary-op was removed), so it matches both that lifted shape and any
explicit `x != x` written in the source.  No dedicated `FloatIsNan`
IR node is needed.

## Match accessors

```python
# Best-effort __getitem__ (int / bool / float bits).
print(m["off"])

# Typed accessors.
m.uint("off")        # Optional[int] — unsigned, masked to width
m.int("off")         # Optional[int] — signed, sign-extended
m.bool("flag")       # Optional[bool]
m.float_bits("f")    # Optional[int]

# Capture presence.
"off" in m
m.has(off_capture)
```

## Predicate guards via `.when`

```python
from strider.pattern import any_int_const

c = Capture()

# Restrict to constants smaller than 0x100.
pat = any_int_const(c).when(lambda m: (m.uint(c) or 0) < 0x100)
hits = graph.find_all(pat)

# `predicate(f)` is shorthand for `any_().when(f)`.
from strider.pattern import predicate
graph.find_all(predicate(lambda m: True))
```

The callback receives a transient `PartialMatch` proxy with the same
accessor set as `Match` (`uint` / `int` / `bool` / `float_bits` /
`has` / `[]` / `in`).  Returning `False` (or raising) fails the
match; for commutative binary ops this triggers the swapped-operand
retry automatically.

The proxy is only valid during the synchronous predicate call; the
underlying graph pointer is cleared right after the predicate returns
to prevent accidental use-after-free.

## Python-implemented memory readers

`strider.MemReader` and `strider.ReadOnlyMemory` are subclassable
abstract base classes.  Override `read(...)` and pass an instance
anywhere the API accepts a `MemoryMap`.

```python
class MyReader(strider.MemReader):
    def __init__(self, fobj):
        super().__init__()
        self.fobj = fobj

    def read(self, addr: int, size: int) -> Optional[bytes]:
        self.fobj.seek(addr)
        data = self.fobj.read(size)
        return data if data else None

reader = MyReader(open("binary.elf", "rb"))
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=reader,
    entry=0x401000,
)
```

`ReadOnlyMemory` follows the same pattern but for the optimizer's
`LoadReadOnly` pass:

```python
class MyROM(strider.ReadOnlyMemory):
    def read(self, addr: int, size: int) -> Optional[int]:
        # The Rust adapter only forwards RAM-space reads to Python — every
        # other address space is folded by varnode aliasing or constant
        # propagation before reaching `LoadReadOnly`, so the override sees
        # only the calls it can answer.
        if addr < ROM_BASE or addr >= ROM_BASE + len(ROM):
            return None
        chunk = ROM[addr - ROM_BASE : addr - ROM_BASE + size]
        return int.from_bytes(chunk, "little")

pipe.add(strider.opt.LoadReadOnly(MyROM()))
```

Performance note: each callback crosses the Rust↔Python boundary, so
prefer `MemoryMap` for in-process bulk data.

## Errors

```python
try:
    mem.add_region(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")
except strider.errors.StriderError as e:
    print(e)
```

Every error from the Rust layer surfaces as a single
`strider.errors.StriderError` carrying an informative message; the
hierarchy is intentionally flat (no typed subclasses).

## What's NOT in v1

- Pattern constructors for `Piece`, `Extract`, `Insert`, `SegmentOp`,
  `CPoolRef`, `New` — they aren't exposed by the `pattern` Rust crate
  either; once those land in Rust we'll expose them in Python.
- Wheel CI matrix / PyPI release — `maturin build --release` produces
  a local wheel; no upstream publication is configured.

## Design

See `docs/superpowers/specs/2026-05-01-strider-py-design.md` for the
full design.
