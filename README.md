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

Indirect branches resolve by re-lifting until the edge set settles. Each address
decodes once, in the ISA mode carried by the edge that reached it, and a
resolved target carries its own mode, so an ARM/Thumb interworking branch or a
MIPS16 entry lands in the right decoder. Whatever stays unresolved comes back
in `unresolved`; you can hand in your own answers with
`CfgOptions(known_targets={dispatch_addr: [target, ...]})`, or turn the
classifier off entirely with `LifterOptions(resolve_indirect_branches=False)`.
An unresolved branch is a result, never an error: `analyze` raises only on a
genuine lift, CFG or optimizer failure, and
[docs/optimizations.md](docs/optimizations.md#dispatch-shapes-that-do-not-resolve)
lists the dispatch shapes that still come back that way. A converged CFG is
never silently incomplete, but it says so through four channels rather than
one; `cfg.is_complete()` reads all four, and
[docs/python-api.md](docs/python-api.md#12-the-cfg-stridercfg) covers each.

`load_elf` maps the image rather than copying it, and applies its relocations as
bytes are read, so a large object opens in tens of milliseconds and faults in
only what you analyse. Linked images, shared libraries and unlinked `ET_REL`
objects all load. A mapped image must not change on disk while a handle over it
lives: rebuilding the binary under a live handle raises `StriderError: mapped
file ... changed on disk since it was mapped`, so re-open it, or set
`STRIDER_NO_MMAP=1` to read it into memory instead. That is the REPL and
notebook failure mode. [docs/getting-started.md](docs/getting-started.md)
covers the mapping, and
[docs/python-api.md](docs/python-api.md#1-loading-a-binary) the knobs over it
(`STRIDER_NO_MMAP=1`, `from_segments`, `apply_relocations`).

```python
import strider

obj = strider.lift.load_elf("fixtures/out/x64/memory.o")   # an unlinked .o
cfg, function, unresolved = obj.analyze("array_sum")
```

Every width Sleigh emits is an IR type, from `I24` through `I512` plus `F16`,
`F80` and `F128`, so a function touching one lifts rather than failing outright.
An unmapped width still fails the whole function's lift, which is the signal
that a spec reached a shape the IR does not model.

The image `LoadReadOnly` folds constants out of is the loaded file minus its
writable mappings, so a load out of an RWX segment is fetched from but never
folded.

A convention can be narrowed for one analysis: `cc.preserves_all()` clobbers
nothing, and `cc.preserves_regs()` preserves the registers but still clobbers
memory. Pair either with `LifterOptions(per_address_ccs={callee_addr: cc})`,
keyed by the direct-call target rather than the call site, to model a
transparent hook such as `__fentry__`.

## Quickstart

```python
import strider
from strider.pattern import load, int_add, Capture

# The CPU and calling convention come from the ELF header.
prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")

# Lift and optimize one function, by name or by address.
cfg, function, unresolved = prog.analyze("array_sum")

# A capture is a hole the pattern binds; read it back by object or by name.
# A bare string is not a capture.
base, off = Capture("base"), Capture("off")
for hit in function.find_all(load(addr=int_add(base, off)), ignore_casts=True):
    print("offset =", hit[off].uint_opt)   # None if it is not a constant

# The explorer draws the whole graph; visualize(whole=False) opens on the
# neighborhood around the entry instead, which stays usable on large functions.
prog.visualize(function)          # prints a local URL; blocks until interrupted

# ...or serve on a thread and keep querying:
port = prog.visualize(function, background=True)
strider.explore.shutdown(port)
```

A lift handle decodes only on the thread that built it: `analyze`,
`build_cfg`, `optimize`, `pcode_at` and the rest raise a catchable
`StriderError` from anywhere else. The handle itself moves and drops anywhere,
so build a second one over the same `arch` / `reader()` / `rom()` to work
off-thread, which is what the background explorer does for its own renderer.

You can also decide matches with your own logic: `.when(f)` filters one pattern
against a callable, backtracking so other bindings are still tried, and a
`JoinPredicate` subclass correlates captures across several patterns. Worked
through in
[docs/python-guide.md](docs/python-guide.md#constraints-relating-matches-by-control-flow).

`find_all` returns every match; `find_unique_value(pat, capture)` returns the
one constant a capture is bound to across all of them, which is the answer to
"what value does this function always pass here", and raises when two matches
disagree.

`one_of([a, b])` yields a separate match for every arm that matches;
`first_of` cuts to the first that matches. `int_add(a, b).ordered()` pins
operand order, overriding commutative both-orders matching. Either nests in a
value, memory or control slot. `load().non_stack()` and `store().heap_only()`
filter by where the address lives. An integer literal stands in for `int_const`
in any slot, so
`int_add(base, 4)` is the same pattern as `int_add(base, int_const(4))`. Call
sites are selected with `call().target(addr)`, or `call().target([a, b])` for a
set of addresses.

A user-op strider has no ABI for fails the lift of every function containing
it. `prog.user_op_names()` lists what the architecture can emit,
`prog.call_other_abi(name)` reads back the classification in force, and
`CfgOptions(call_other_abis={"movmskps": strider.sleigh.CallOtherAbi.pure()})`
supplies the missing one; `CallOtherAbi.custom(sleigh, implicit_reads=[...])`
states an implicit register footprint.

Float arguments have their own index space: `function_arg_float(2)` reaches a
float parameter, and at a call site the float arguments follow the integer ones,
so on SysV `call().arg(6, p)` is the first of them.

`analyze` returns an `AnalyzeResult` (`.cfg` / `.function` / `.unresolved`),
which also unpacks as the 3-tuple above. `prog.symbol(name)` returns a `Symbol`
(`name`, `address`, `size`, `end`, `is_function`, `region`) where 0.1.0 returned
a bare address, so a 0.1.0 script doing arithmetic on one needs `.address` now;
`prog.symbol_at(addr)` reverse-resolves an address to the `Symbol` covering it.
The query API renamed with it, which is every line of the 0.1.0 quickstart:
`add` is `int_add`, a bare string is no longer a capture operand (`Capture` is),
and `const_uint` is `uint`. [CHANGELOG.md](CHANGELOG.md) lists the rest,
breaking entries first.
A failure raised by strider itself carries its Rust trace on `.backtrace`, and
`STRIDER_BACKTRACE=1` folds it into the message.

`load_elf` reads ARM32's float ABI from EABI `e_flags` and picks `arm_aapcs`
or `arm_aapcs_soft`. A relocatable object carries no such bit, and an image
setting neither falls to hard-float; pass
`cc=strider.sleigh.CallingConvention.arm_aapcs_soft()` for those, or float
arguments read as empty registers.

Memory precision is tunable per analysis: `AssumptionOptions` holds six claims
about the code that the IR cannot check, passed as
`LifterOptions(assumptions=AssumptionOptions(...))`. Each one's risky value is
the positive one, and two of the six default `True`, so
`AssumptionOptions.none()` is the only spelling of "assume nothing" that stays
sound as claims are added; `AssumptionOptions()` is not it.
[docs/python-api.md](docs/python-api.md#2-analyzing-a-function) says what each
one buys.

A function that never returns still answers queries. A `while (1)`, a spin loop
or a `panic` helper ending in a self-jump reaches no return instruction, so the
loop body has nothing anchoring it; Strider seats a sink on the cycle at lift
time, which is what keeps its stores and their operands in the graph.

## Documentation

The guides in [`docs/`](docs/):

- [Getting started](docs/getting-started.md): what Strider is and how the
  pipeline fits together.
- [Vocabulary](docs/vocabulary.md): the terms (region, phi, dominator, varnode,
  value types, ...) in plain language.
- [Python guide](docs/python-guide.md): the practical walkthrough of analyzing
  functions, writing queries, constraints, rewrites, and custom memory readers.
- [Python API reference](docs/python-api.md): every user-facing Python API in
  depth, with a runnable example of each.
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

Strider needs Rust 1.91 or newer, and uses [uv](https://docs.astral.sh/uv/).
From the repository root:

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
```

The Python side, from the repository root:

```bash
uv run maturin develop && uv run pyright && uv run pytest
```

## License

MIT. See [LICENSE](LICENSE).
