# strider-py

Python bindings for the Strider binary analysis pipeline.

Strider lifts native binaries to a sea-of-nodes IR, runs an
optimization pipeline, and exposes the lifted IR for arbitrary pattern
queries.  This crate is the Python entry point.

## Build (development)

From this directory:

    pip install maturin pyelftools patchelf
    maturin develop

Then run the test suite:

    pytest tests/python/

The integration tests need fixtures built once via:

    cd ../../fixtures && make

## Quick example

```python
import strider
from strider.pattern import Capture, var, add, load

# 1. Load a binary into a MemoryMap.
mem = strider.MemoryMap()
mem.add_region_from_elf("fixtures/out/x86/memory.elf")

# 2. Run the full pipeline (CFG → IR → optimize) in one call.
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem,
    rom=mem,
    entry=0x401000,  # whatever symbol you care about
    allow_code_before_start_addr=True,
)

# 3. Query the optimized graph with a pattern.
base, off = Capture(), Capture()
pat = load(addr=add(var(base), var(off)))
for hit in result.graph.find_all(pat, ignore_casts=True):
    print(hit.uint(off))

# 4. Visualize.
result.cfg.to_html("cfg.html")
result.graph.to_html("graph.html")
```

## Building blocks

The convenience `strider.run` wraps these explicit steps:

```python
arch = strider.SleighArch.x86()
cc = strider.CallingConvention.x86_cdecl()
mem = strider.MemoryMap()
mem.add_region_from_elf("fixtures/out/x86/memory.elf")

sleigh = strider.Sleigh(arch, mem)
s = strider.Strider(arch, sleigh, cc)            # before build_cfg
cfg = strider.build_cfg(sleigh, entry=0x401000)  # consumes Sleigh
graph = s.analyze_cfg(cfg).graph

pipe = s.build_optimizer_pipeline()
pipe.add(strider.opt.LoadReadOnly(mem))
graph.optimize(pipe)
```

## Custom optimizer pipeline

```python
pipe = strider.OptimizerPipeline.empty()
pipe.add(strider.opt.ConstantFold())
pipe.add(strider.opt.KnownBits())
pipe.add(strider.opt.StackStoreDetect(sleigh, cc))
pipe.add(strider.opt.StackLoadForward(sleigh, cc, arch))
pipe.add_post(strider.opt.FunctionArgDetect(sleigh, cc))
pipe.add_post(strider.opt.CallStackArgCollect(sleigh, cc))
graph.optimize(pipe)
```

## Pattern → pattern rewrite

```python
from strider.pattern import Capture, var, add, int_const

x = Capture()
n = graph.rewrite(find=add(var(x), int_const(0)), replace=var(x))
graph.reoptimize(destructive=True)  # collapse downstream effects
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

# Predicate guard isn't yet exposed at the python-level — wrap in
# Python by filtering the result list:
hits = [m for m in graph.find_all(pat) if (m.uint("off") or 0) < 0x100]
```

The `_` and `any_` strings are reserved wildcards (they convert to
`any_()`); using them via `.cap(...)` raises `PatternError`.

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

## Errors

```python
try:
    mem.add_region(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")
except strider.errors.ReaderError as e:
    print(e)
```

The exception hierarchy (`StriderError → LiftError | ReaderError |
PatternError | RewriteError`) is in `strider.errors`.

## What's NOT in v1

- Indirect-branch fixed-point loop in `strider.run` (the Rust
  orchestrator requires `Sleigh<BufMemReader<B>>`; `PyMemoryMapReader`
  doesn't satisfy that bound).  Use the Rust API for indirect-heavy
  binaries.
- Python-implemented `MemReader` / `ReadOnlyMemory` subclass readers —
  use `MemoryMap` for now.
- Float / cast pattern builders (a small subset is wired; the rest can
  be added as needed).
- Pythonic predicate guards on patterns (`.when(lambda m: ...)`) —
  filter the result list in Python instead.
- Wheel CI matrix / PyPI release.

## Design

See `docs/superpowers/specs/2026-05-01-strider-py-design.md` for the
full design.
