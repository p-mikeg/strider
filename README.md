<p align="center">
  <img src="docs/strider.png" alt="Strider" width="320">
</p>

# Strider

> *"He's one of them Rangers. Dangerous folk they are, wandering the Wilds."*
> Barliman Butterbur, *The Fellowship of the Ring*

Strider is named after Aragorn's ranger alias: a tracker who finds what others
miss. It hunts through compiled binaries, letting you ask precise questions
about how a function behaves with no source and no debug symbols.

## What it does

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

You describe a shape you are looking for and get back every place in the
optimized IR that matches, with the values you asked to capture. Typical
questions: what offset does this function read off a pointer, what value does it
pass to `malloc`, what does it return when the input matches a condition.

## Quickstart

```python
import strider
from strider.pattern import load, int_add, int_const, Capture

# The CPU and calling convention come from the ELF header.
prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")

# Lift and optimize one function, by name or by address.
cfg, function, unresolved = prog.analyze("array_sum")

# A capture is a hole the pattern binds; read it back by object or by name.
base, off = Capture("base"), Capture("off")
pattern = load(addr=int_add(base, int_const(off)))
for hit in function.find_all(pattern, ignore_casts=True):
    print("offset =", hit[off].uint)   # 4 and 8: the two stack arguments

# The explorer draws the whole graph; visualize(whole=False) opens on the
# neighborhood around the entry instead, which stays usable on large functions.
# On a thread it prints a local URL and returns, so you can keep querying:
port = prog.visualize(function, background=True)
strider.explore.shutdown(port)

# Without background= it serves on this thread and blocks until interrupted,
# so nothing after it runs:
prog.visualize(function)
```

## What's new in 0.2.0

[CHANGELOG.md](CHANGELOG.md) has the full list, breaking changes first.

- **More images.** Unlinked `ET_REL` objects analyse with their code
  relocations applied; an undefined symbol gets an address of its own, and a
  relocation strider does not compute stops any read that reaches it. ppc64
  ELFv1 symbols resolve through their `.opd` descriptors.
  `prog.add_symbol_file(path)` and `prog.add_symbols({name: addr})` name the
  functions of a stripped image. Images are memory-mapped; `STRIDER_NO_MMAP=1`
  reads them instead.
- **Completeness you can check.** `cfg.is_complete()` reads six channels: an
  unresolved indirect branch, a return whose target is not provably the
  caller's, a site settled only by your own seed, an address reached in two ISA
  modes, a branch target off an instruction boundary, outside the image or on
  bytes that hold no instruction.
- **Mixed instruction sets.** Each address decodes in the ISA mode of the edge
  that reached it, and a resolved indirect target carries its own mode, so
  ARM/Thumb interworking and MIPS16 code decode correctly.
  `CfgOptions(known_targets=...)` seats answers of your own, and
  `LifterOptions(resolve_indirect_branches=False)` turns the classifier off.
- **ABIs.** `CallingConvention.custom(sleigh, ...)`, `cc.preserves_all()` /
  `cc.preserves_regs()` and `arm_aapcs_soft`, applied per callee through
  `LifterOptions(per_address_ccs=...)`. Analysis never reads a callee, so a
  callee that pops its caller's stack (`ret $imm16`) or an i386 PC thunk needs
  such an entry. `CfgOptions(call_other_abis=...)` classifies a Sleigh user-op
  strider has no ABI for; `prog.user_op_names()` lists the ones the
  architecture can emit.
- **Memory precision.** `LifterOptions(assumptions=AssumptionOptions(...))`
  states claims about the code the analysis cannot check;
  `AssumptionOptions.none()` makes none of them.
- **Queries.** `first_of`, and `one_of` in any slot; `field` and `code_ptr` for
  the shapes `ConstantFold` leaves; `find_unique_value`; `JoinPredicate` for
  relations of your own; float arguments; `load().non_stack()` /
  `store().heap_only()`; `.ordered()` on any binary pattern; a raw int wherever
  a constant goes. The builders share one vocabulary, declared in the stubs as
  `runtime_checkable` protocols.
- **Optimizer.** `ConstantFold` turns sums of scaled copies (`x + (x << 1)`)
  into one `x * C`, `PhiCollapse` folds cycles of phis carrying one value, and
  a pass that changed nothing is skipped until the graph changes.
  `Function.validate()` also checks the order of memory effects.
- **Looking at results.** `Function.to_text()` prints the IR one line per node,
  stable across runs. `visualize(background=True)` serves the explorer while
  you keep querying. `StriderError.backtrace` carries the Rust trace.
- **Failing safely.** A pattern too large or too deep for the thread's stack,
  a lift handle used from another thread and an out-of-range operand index all
  raise `StriderError` instead of crashing the interpreter, and the wheel is
  built with overflow checks.

## Upgrading from 0.1.0

The query API renamed, which is every line of the 0.1.0 quickstart: `add` is
`int_add`, a bare string is no longer a capture operand (`Capture` is), and
`hit.const_uint(c)` is `hit.uint(c)`. `hit[c]` returns a `BoundCapture`, so
`hit[c] is None` is always false; test `c in hit`. `prog.symbol(name)` returns a
`Symbol` (`name`, `address`, `size`, `end`, `is_function`, `region`,
`is_thumb`) where 0.1.0 returned a bare address, so arithmetic on one needs
`.address`, and `symbol_size` is gone.

`LifterOptions(alias_mode=, calls_clobber=, assume_distinct_sp_bases_disjoint=)`
raise `TypeError`: those claims moved into
`LifterOptions(assumptions=AssumptionOptions(...))` as `stack_global_disjoint`,
`assume_incoming_args_survive_calls` (inverted) and
`distinct_sp_bases_disjoint`.

A lift handle decodes only on the thread that built it, so a script that
analysed from a worker raises `StriderError` there. Build a second handle on
that thread over the same `prog.arch`, `prog.reader()` and `prog.rom()`.

A 0.1.0 pattern that now matches nothing has most often met `ConstantFold`'s
repeated-term factoring: `x + x*2` reaches the query as `x*3`. Draw the function
with `prog.visualize(function)` to see the shape the optimizer left behind.

## Documentation

The guides in [`docs/`](docs/):

- [Getting started](docs/getting-started.md): what Strider is and how the
  pipeline fits together.
- [Vocabulary](docs/vocabulary.md): the terms (region, phi, dominator, varnode,
  value types, ...) in plain language.
- [Python guide](docs/python-guide.md): the practical walkthrough of analyzing
  functions, writing queries, constraints, rewrites, and custom memory readers.
- [Python API reference](docs/python-api.md): the Python API by topic, with
  runnable examples.
- [Optimizations](docs/optimizations.md): what each pass does, which is usually
  why a pattern's shape differs from the source.
- [Changelog](CHANGELOG.md): what 0.2.0 changed, breaking entries first.

[`crates/strider-py/examples/python/`](crates/strider-py/examples/python/) has
seventeen runnable scripts, from a quickstart to custom target ABIs.

## Install

The Sleigh fork strider lifts through is a submodule, and every crate that
touches machine state depends on it, so clone with it:

```bash
git clone --recursive https://github.com/p-mikeg/strider
# already cloned:
git submodule update --init --recursive
```

The fixture binaries under `fixtures/out/` are stored in Git LFS, so fetch them
too and the examples and tests run without a cross-compiler:

```bash
git lfs install && git lfs pull
```

Strider needs Rust 1.91 or newer and Python 3.9 or newer, and uses
[uv](https://docs.astral.sh/uv/). From the repository root:

```bash
uv sync --group dev       # virtualenv and dev dependencies
uv run maturin develop    # build the Rust extension
uv run pytest             # run the test suite
```

Rebuild the fixtures with `cd fixtures && make` after changing their sources.

## Architecture

A Rust workspace of sixteen crates. The ones you meet first:

| Crate | Role |
|-------|------|
| `strider-reader` | Loads an ELF and serves its memory to the lifter. |
| `strider-cfg` | Builds the control-flow graph of regions. |
| `strider-lift` | Lifts the CFG into the IR, handling register aliasing. |
| `strider-ir` | The sea-of-nodes IR. |
| `strider-opt` | Optimization passes and the indirect-branch classifiers. |
| `strider-pattern` | The pattern and rewrite engine. |
| `strider-orchestrator` | Runs the lift / optimize / re-lift resolution loop. |
| `strider-py` | The Python bindings, the primary query interface. |

The rest are `strider-target` (arch and ABI descriptions),
`strider-ir-test-utils` (a dev-dependency of the test suites), and six generic
utility crates (`dot`, `entity-utils`, `graph-algorithms`, `read-only-memory`,
`strider-graph`, `vn-container`).

## Rust API

The crates are usable without the bindings.
`strider_orchestrator::Strider::new(...)` builds a handle and `.analyze(...)`
runs the whole pipeline.
[`examples/orchestrator_demo.rs`](crates/strider-orchestrator/examples/orchestrator_demo.rs)
runs against the committed fixtures (`cargo run -p strider-orchestrator
--example orchestrator_demo`), driving the stages by hand (`Lifter::new`,
`build_cfg`, `build_ir`, `pipeline.run`) and dumping each one.
[`examples/analyze_kernel.rs`](crates/strider-orchestrator/examples/analyze_kernel.rs)
is the one-call form, but it is a profiling harness: it takes the image path in
`argv[1]` (or `$STRIDER_KERNEL`), and the symbol and the architecture are
constants at the top of the file that you edit.

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --workspace --release   # a debug_assert hides from the debug run
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links' cargo doc --workspace --no-deps
cargo +1.91.0 check --workspace --all-targets   # the declared MSRV
```

The Python side, from the repository root:

```bash
uv run maturin develop && uv run pyright && uv run pytest
```

## License

MIT. See [LICENSE](LICENSE). The vendored browser bundles keep their own
notices; [THIRD-PARTY.md](THIRD-PARTY.md) lists them.
