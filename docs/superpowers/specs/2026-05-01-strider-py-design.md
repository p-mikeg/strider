# `strider-py` — Python bindings for the Strider analysis pipeline

**Status:** AWAITING USER APPROVAL.
**Date:** 2026-05-01.
**Owner:** mike.
**Workspace placement:** new crate at `crates/strider-py/`.

---

## 1. Goal

Expose the full Strider pipeline to Python so a user can:

1. Provide their own readers from Python — either a fast data-only `MemoryMap` (regions of bytes at fixed addresses) or a callback-style subclass of `MemReader` / `ReadOnlyMemory` for unusual or lazy formats.
2. Drive the analysis (CFG build → IR lift → optimization → indirect-branch resolution) from Python and receive both the `Cfg` and the lifted `Graph` back.
3. Customize the optimizer pipeline (pre-built defaults or pass-by-pass).
4. Query the lifted graph with patterns (full mirror of the Rust `pattern` crate, with a string-shorthand sugar layer for short patterns).
5. Rewrite the graph by pattern → pattern substitution and re-run the optimizer.
6. Visualize either the CFG or the lifted Graph as `.html` / `.dot` (thin wrapper over the existing `dot` crate).

This is the v1 surface. Out-of-scope items are listed at the end.

## 2. Constraints

- **Workspace policy.** No `panic!` / `unwrap` / `expect` / `debug_assert!` / `unreachable!` in production code. PyO3 boundaries convert errors via `?` into `PyResult` only.
- **No PyO3 in inner crates.** All PyO3 wrappers live in `crates/strider-py/`. The `ir`, `pattern`, `opt`, `strider`, `reader`, `target`, `cfg`, `dot` crates stay PyO3-free so they remain usable as plain Rust libraries.
- **TDD.** Every new wrapper ships with at least one Python integration test exercising it end-to-end and at least one Rust unit test for any non-trivial Rust-side adapter logic (capture interning, error classification, GIL handling).
- **anyhow-only.** Errors propagate as `anyhow::Error` and are converted to `PyErr` at the boundary. We do not introduce a new error crate.

## 3. Architecture

```
                  Python user code
                        │
              ┌─────────▼─────────┐
              │   strider (.so)   │  ← maturin-built PyO3 module
              │   crates/strider- │
              │   py/             │
              └─────────┬─────────┘
                        │ wraps (no reverse dep)
       ┌────────────────┼─────────────────┐
       │                │                 │
   strider           pattern             dot
   (run, CFG,      (matcher,         (HTML/DOT
    builder)        rewrite_rule)     dump)
       │                │
   reader, target, ir, opt, rsleigh
```

Single PyO3 module exported as `strider`. Module init registers all classes/functions in `lib.rs` and pulls in the per-area submodules.

## 4. Python module layout

```
strider                            # top-level module
  .run(...) -> RunResult           # convenience entry (Q5/B)
  .build_cfg(...)                  # building-blocks path (Q5/A)
  .analyze(cfg, ...) -> Graph      # building-blocks path (Q5/A)

  # Building-block types (mirror Rust)
  .Sleigh, .SleighArch, .CallingConvention
  .Strider, .Cfg, .RunResult
  .Graph                           # opaque; methods below
  .OptimizerPipeline               # pre-built or build from scratch

  # Reader types (Q3/C: data path + callback path)
  .MemoryMap                       # fast: data-only, Rust-side reads
  .MemReader                       # subclass for callback-style sleigh fetch
  .ReadOnlyMemory                  # subclass for callback-style ROM reads

  .errors                          # exception hierarchy

strider.pattern                    # mirror of Rust pattern crate (Q2/A+B)
  .Capture, .Matcher
  load, store, stack_store, stack_store_phi,
  add, sub, mul, shl, shr, ushr, and_, or_, xor,
  bool_and, bool_or, bool_xor,
  int_eq, int_lt, int_slt, int_carry, int_scarry,
  float_add, float_sub, float_mul, float_div,
  float_eq, float_ne, float_lt, float_le,
  call, call_other, ret, if_,
  phi, phi_for, initial_var, function_arg,
  const, predicate, var, any_, "_"

strider.opt                        # one wrapper class per Rust opt pass
  ConstantFold, KnownBits, RedundantPhis, DeadBranchElim, CallOtherElide,
  LoadReadOnly(rom),
  StackStoreDetect(cc), StackLoadForward(cc, arch),
  FunctionArgDetect(cc), CallStackArgCollect(cc)
```

Type stubs (`.pyi`) ship hand-written, mirroring every public symbol with full type signatures. Lives under `crates/strider-py/strider/`.

## 5. Reader API (Q3/C)

Two reader styles, both implementing the same Rust traits internally.

### 5.1 `MemoryMap` — fast data-only path

```python
mem = strider.MemoryMap()
mem.add_region(0x401000, code_bytes)
mem.add_region(0x600000, rodata_bytes)
mem.add_region_from_elf(elf_path)        # convenience: load ELF segments
```

Internally a thin wrapper around the existing `reader::MemRegion` +
`reader::MemRegionsLookupTable`. Implements both `rsleigh::MemReader` and
`reader::ReadOnlyMemory`. Reads happen entirely in Rust — no Python
callback per read. This is the hot path.

### 5.2 Subclass — callback path

```python
class LazyRom(strider.ReadOnlyMemory):
    def read(self, space, addr, size):     # -> int | None
        ...

class LazyMem(strider.MemReader):
    def read(self, addr, size):            # -> bytes | None
        ...
```

PyO3 wrapper implements the Rust trait by `Python::with_gil` on each
call. Acknowledged-slow; documented loudly. Sleigh disassembly will pay
one GIL acquire per byte-fetch — usable for small/lazy/exotic readers,
not for scanning large binaries.

### 5.3 Threading / GIL contract

- All reader callbacks must `Python::with_gil`.
- The Rust side must not hold any non-trivial Rust lock across a callback
  into Python (would deadlock if the Python code re-enters any Rust API
  that takes the same lock). The wrapper enforces this by acquiring the
  GIL only inside the `read` method body, not while holding analysis
  state.
- Reader subclasses must not re-enter Strider (e.g. by running a fresh
  analysis from inside `read`). This is documented; not enforced at
  runtime.

## 6. Pattern API (Q2/A + B unified)

Internally everything is `Capture`. The string form is preprocessed at
pattern-build time: each unique string in a single `Pat` tree is
interned to one `Capture` instance via a per-pattern table.

```python
from strider.pattern import load, add, mul, const, Capture

# Capture-object form (option A)
ptr, off = Capture(), Capture()
pat = load().addr(add(var(ptr), var(off)))

# String form (option B sugar, identical underlying Pat)
pat = load(addr=add("ptr", "off"))

# Mixed: bind a sub-expression's output with .cap()
pat = load(addr=add("p", "off").cap("addr_expr"))

# Back-reference: same name twice → must agree
pat = add("x", "x")                       # x + x

# Predicate guard
pat = load(addr=add("p", "off")).when(lambda m: m.uint("off") < 0x100)
```

### 6.1 Capture rules

- Strings intern to `Capture` instances at the **point a `Pat` is
  finalised** (i.e. when the outermost builder is converted to `Pat`
  for use in `find_all` / `match_at` / a sub-pattern field). Within
  one finalised pattern, every occurrence of the same string aliases
  to the same `Capture` — back-reference.
- Composing builders before finalisation share the table: in
  `compound = and_(add("x", const(2)), mul("x", "y"))`, both `"x"`s
  alias because the table is per-finalised-pattern, and `compound` is
  the finalised pattern.
- Two **separately finalised** patterns (e.g. two `Pat`s passed to two
  different `find_all` calls) get distinct interning tables — their
  `"x"`s don't collide. To bridge them, use an explicit `Capture()`
  object shared between the two.
- `any_` and `"_"` are reserved for "anonymous wildcard, do not bind".
  Using either as a string elsewhere raises `PatternError` at build
  time.
- `Match.__getitem__` accepts `str | Capture`; both resolve to the
  same binding.

### 6.2 Match API

```python
m["off"]          # best-effort: int / bool / float bits depending on bound node kind
m.uint("off")     # explicit, mirrors Rust get_uint
m.int("off")      # signed, mirrors get_int
m.bool("flag")
m.float_bits("f")
m.vn("p")         # rsleigh.Vn for InitialVar/Phi captures
m.node_id("off")  # opaque NodeId
m.output_id("off")# opaque NodeOutputId
"off" in m
list(m)
```

`Match` holds a strong reference to its parent `Graph` so the user
cannot accidentally drop it mid-iteration.

### 6.3 Builder coverage

Every Rust pattern-builder constructor and field method has a Python
counterpart with the same name (with Python keywords escaped: `and_`,
`or_`, `if_`). Field methods (`.addr()`, `.arg(idx, p)`, `.cond(p)`,
`.true_branch(p)`, `.false_branch(p)`, `.preceded_by(p)`, `.ret_val(idx, p)`,
`.space(s)`, `.at(addr)`) accept `str | Pat | Capture` — string is
preprocessed to a `Capture` via the interning table.

### 6.4 Commutative matching

Same as Rust: `add`, `mul`, `and_`, `or_`, `xor`, `bool_and`, `bool_or`,
`bool_xor`, `float_add`, `float_mul`, `int_eq`, `float_eq`, `float_ne`,
`int_carry`, `int_scarry` auto-try both orderings. `.ordered()` opts
out (only on the typed binary-op builders).

### 6.5 Matcher options

```python
matcher = graph.matcher(
    ignore_casts=True,            # equiv. to Matcher::ignore_casts()
    ignore_casts_mask={"extend", "truncate"},  # selective
    ignore_control_states=True,
)
```

Or as kwargs to `graph.find_all`:

```python
graph.find_all(pat, ignore_casts=True)
```

## 7. Graph API

```python
# Querying
hits = graph.find_all(pat)                # -> list[Match]
hit = graph.match_at(node_id, pat)        # -> Match | None
matcher = graph.matcher(...)              # long-lived matcher
matcher.find_all(pat)

# Rewriting (Q4/A — pattern → pattern only)
graph.rewrite(find=pat_in, replace=pat_out)
graph.rewrite_all([(p1, r1), (p2, r2)])
graph.reoptimize()                         # rerun stable opt pipeline
graph.reoptimize(destructive=True)         # also run destructive passes
graph.optimize(custom_pipeline)            # apply user pipeline

# Visualization (thin wrapper over dot crate)
graph.to_html("out.html", style="dark")    # writes file; style ∈ {"dark", "light"}
graph.to_dot("out.dot")
graph.html_str(style="dark")               # returns str

# Inspection
graph.entry()                              # opaque NodeId
graph.nodes()                              # iterator of NodeRef
node.kind(); node.inputs(); node.outputs()
```

`Graph`, `Match`, `NodeRef`, `Capture`, `MemRegion` are opaque PyO3
wrappers. `NodeId` and `NodeOutputId` are likewise opaque (no public
integer accessor for v1 — they are stable handles, not user data).

## 8. CFG API

`Cfg` is returned both by `strider.build_cfg` and as `RunResult.cfg`.
It exposes the same dot-render surface as `Graph`.

```python
cfg = strider.build_cfg(arch=arch, sleigh=sleigh, entry=0x401000,
                        options=strider.CfgOptions(allow_code_before_start_addr=True))
cfg.to_html("cfg.html", style="dark")
cfg.to_dot("cfg.dot")
cfg.html_str(style="dark")
```

V1 does not expose the CFG region/edge structure as Python types
beyond visualization — if a user needs to introspect the CFG they
hand it to `strider.analyze` and inspect the resulting `Graph`. This
is the same pattern the Rust example uses.

## 9. Optimizer pipeline customization

```python
# Pre-built
pipe = strider.OptimizerPipeline.default()
pipe = strider.OptimizerPipeline.stable_default()
pipe = strider.OptimizerPipeline.destructive_default()
pipe = strider.OptimizerPipeline.empty()

# Individual passes
pipe.add(strider.opt.ConstantFold())
pipe.add(strider.opt.KnownBits())
pipe.add(strider.opt.LoadReadOnly(rom))
pipe.add(strider.opt.StackStoreDetect(cc))
pipe.add(strider.opt.StackLoadForward(cc, arch))
pipe.add_post(strider.opt.FunctionArgDetect(cc))
pipe.add_post(strider.opt.CallStackArgCollect(cc))

# Use it
graph = strider.run(..., pipeline=pipe)
graph.optimize(pipe)

# Strider class also exposes the CC-aware pre-built helpers
s = strider.Strider(arch, sleigh.regs(), cc)
s.build_optimizer_pipeline()
s.build_stable_optimizer_pipeline()
s.build_destructive_optimizer_pipeline()
```

`strider.opt` submodule holds one wrapper class per Rust pass. CC/arch-
bearing passes (`StackStoreDetect`, `StackLoadForward`, `FunctionArgDetect`,
`CallStackArgCollect`) take the relevant configuration in their
constructor; pure passes (`ConstantFold`, `KnownBits`, `RedundantPhis`,
`DeadBranchElim`, `CallOtherElide`) are zero-arg. `LoadReadOnly` takes
a `MemoryMap` or any `ReadOnlyMemory`.

## 10. Top-level entry-point shape (Q5/C — both)

### 10.1 Convenience path

```python
result = strider.run(
    arch="x86_64",                    # or strider.SleighArch.x86_64()
    cc="x86_64_systemv_abi",          # or strider.CallingConvention.x86_64_systemv_abi()
    mem_reader=mem,                   # MemoryMap or MemReader subclass
    rom=mem,                          # MemoryMap, ReadOnlyMemory subclass, or None
                                      # (a single MemoryMap can serve both;
                                      # subclass users must subclass each ABC
                                      # they want to fill)
    entry=0x401000,
    pipeline=None,                    # default = full pipeline incl. LoadReadOnly if rom is set
    cfg_options=None,
)
result.cfg                            # PyCfg
result.graph                          # PyGraph
result.sleigh                         # PySleigh (for resolving Vns by name, etc.)
```

`RunResult` is a small Python-visible class with the three fields as
read-only properties. No mutation; rerunning analysis means calling
`strider.run` again.

### 10.2 Building-blocks path

```python
arch = strider.SleighArch.x86_64()
cc = strider.CallingConvention.x86_64_systemv_abi()
sleigh = strider.Sleigh(arch, mem)             # mem = MemoryMap or subclass
cfg = strider.build_cfg(sleigh=sleigh, entry=0x401000, options=cfg_options)
s = strider.Strider(arch, sleigh.regs(), cc)
graph = s.analyze_cfg(cfg).graph               # mirror of Rust AnalyzeOutcome
pipe = s.build_optimizer_pipeline()
pipe.add(strider.opt.LoadReadOnly(rom))
graph.optimize(pipe)
```

This mirrors the existing Rust `crates/strider/examples/strider.rs` 1-to-1.

## 11. Error handling

```python
strider.errors.StriderError                  # base; wraps anyhow::Error chain in __cause__
  ├─ LiftError       # CFG/IR build / sleigh failures
  ├─ ReaderError     # bad address, overflow, region overlap
  ├─ PatternError    # malformed pattern, capture conflict, reserved-name reuse
  └─ RewriteError    # substitution shape mismatch, validate failure post-rewrite
```

Subclasses are produced at well-defined boundaries (lift, reader
construction, pattern build, rewrite). Other Rust errors fall through
to plain `StriderError`. We do not retro-classify arbitrary downstream
errors.

## 12. Build & distribution

- `pyproject.toml` + `maturin` build backend.
- PyO3 abi3, minimum CPython 3.9 (covers all currently-supported releases).
- `crates/strider-py/Cargo.toml` declares `crate-type = ["cdylib"]`.
- Local development: `maturin develop` from `crates/strider-py/`.
- Wheel CI matrix is **out of scope for v1** — the README documents the
  local build command. Releases are local for now.

## 13. Testing

- **Rust unit tests** in `crates/strider-py/src/**` for non-trivial
  adapter logic: capture interning, string-reservation enforcement
  (`"_"`/`"any_"`), error-class routing, GIL-handling correctness in
  callback readers (verified by re-entering Python under `Python::with_gil`).
- **Python integration tests** in `crates/strider-py/tests/python/` using
  pytest, exercising end-to-end on `fixtures/out/x86/test.elf`:
  - `test_run_and_query.py` — `strider.run` against the test ELF, find a
    known pattern, check captured values.
  - `test_python_reader.py` — small in-memory blob fed via a Python
    `MemReader` subclass; verify a few bytes are lifted correctly.
  - `test_rewrite.py` — apply a pattern → pattern rewrite, re-optimize,
    verify the new shape.
  - `test_visualize.py` — `cfg.to_html` and `graph.to_html` produce
    non-empty output that contains expected node/edge markers.
  - `test_optimizer_pipeline.py` — build a custom pipeline, apply it,
    verify a pass-specific effect (e.g. `LoadReadOnly` folds a
    constant address into a const).
- One `expect-test`-style snapshot of `dir(strider)` and
  `dir(strider.pattern)` to flag accidental public-API changes.
- All tests run as part of `cargo test --workspace` via a small Rust
  test harness that invokes pytest against the dev-built `.so`.

## 14. File-level breakdown

```
crates/strider-py/
├── Cargo.toml                # cdylib, pyo3 deps, abi3 feature
├── pyproject.toml            # maturin config, project metadata
├── README.md                 # build command, usage examples
├── src/
│   ├── lib.rs                # PyO3 module init: register classes/fns
│   ├── arch.rs               # PySleighArch, PyCallingConvention, PySleigh
│   ├── reader.rs             # PyMemoryMap, PyMemReader, PyReadOnlyMemory wrappers
│   ├── run.rs                # strider.run, PyRunConfig, PyRunResult, PyStrider
│   ├── cfg.rs                # PyCfg, PyCfgOptions, build_cfg
│   ├── graph.rs              # PyGraph: find_all/rewrite/reoptimize/optimize/to_html
│   ├── node.rs               # PyNodeRef, PyNodeId, PyNodeOutputId opaque wrappers
│   ├── opt.rs                # PyOptimizerPipeline + per-pass classes
│   ├── pattern/
│   │   ├── mod.rs            # PyPat, PyMatcher, PyMatch
│   │   ├── builders.rs       # load/store/add/mul/... constructors
│   │   ├── capture.rs        # PyCapture, str-interning logic
│   │   └── leaves.rs         # const, var, any_, predicate, initial_var, ...
│   ├── errors.rs             # exception classes + anyhow → PyErr conversion
│   └── dot.rs                # to_html/to_dot adapters shared by PyCfg / PyGraph
├── strider/                  # Python package — type stubs only
│   ├── __init__.pyi
│   ├── pattern/__init__.pyi
│   ├── opt/__init__.pyi
│   └── errors.pyi
└── tests/
    ├── adapter_unit.rs       # Rust unit tests for adapter logic
    └── python/
        ├── conftest.py
        ├── test_run_and_query.py
        ├── test_python_reader.py
        ├── test_rewrite.py
        ├── test_visualize.py
        └── test_optimizer_pipeline.py
```

## 15. Risks & mitigations

- **GIL deadlocks on Python-implemented readers.** Mitigated by the
  contract in §5.3 (no Rust locks held across Python callbacks; reader
  subclasses must not re-enter Strider). Verified by a dedicated test
  that exercises the callback path under the smallest non-trivial
  binary.
- **Slow callback readers.** Documented; `MemoryMap` exists as the
  fast escape. README warns up-front.
- **API drift from inner crates.** Mitigated by the public-surface
  snapshot test (§13) — any accidental addition/removal trips the
  snapshot.
- **`unwrap`/`expect` lint enforcement at PyO3 boundaries.** Standard
  PyO3 idiom: every Rust call returning `anyhow::Result` is `?`'d into
  the PyO3 conversion path; never `.unwrap()`. Lint stays as
  `unwrap_used = "deny"`.
- **Sleigh ownership.** `PySleigh` owns the `rsleigh::Sleigh`; the
  PyO3 wrapper must outlive any `PyCfg` / `PyGraph` derived from it.
  Enforced by `RunResult` holding a reference to the Sleigh, and
  building-block users responsible for keeping their `Sleigh` alive
  for as long as derived objects exist.

## 16. Out of scope for v1

- Full Python `FunctionBuilder` / node-construction API (Q4/B deferred).
- Separate `pattern-py` crate split.
- Wheel CI matrix and PyPI release.
- Pythonic visualization (live SVG, Jupyter integration, interactive
  graph navigation) — `to_html` is enough.
- Async / parallel `find_all`.
- Serialization of `Graph` to disk.
- Exposing CFG region/edge structure as Python types beyond visualization.
- Match-time rewriting hooks (apply a Python callback per match instead
  of a substitution pattern).

## 17. Acceptance criteria

- `crates/strider-py/` builds cleanly under `cargo build --workspace`
  and `cargo clippy --workspace`.
- `maturin develop` produces an importable `strider` Python module.
- All Python tests under `crates/strider-py/tests/python/` pass against
  the test ELF fixture.
- All Rust adapter unit tests pass.
- The public-surface snapshot test pins the v1 API shape.
- CLAUDE.md updated to reflect that `strider-py` is no longer
  *(planned)* — it exists.
- No `unwrap` / `expect` / `panic!` / `debug_assert!` / `unreachable!` in the new
  code; clippy clean across the workspace.
