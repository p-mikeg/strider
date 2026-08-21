<p align="center">
  <img src="docs/strider.png" alt="Strider" width="320">
</p>

# Strider

> *"He's one of them Rangers. Dangerous folk they are, wandering the Wilds."*
> -- Barliman Butterbur, *The Fellowship of the Ring*

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
decodes once, in the mode the function runs in, and a resolved target carries
its own ISA mode, so an ARM/Thumb interworking branch or a MIPS16 entry lands in
the right decoder. Whatever stays unresolved comes back in `unresolved`; you can
hand in your own answers with
`CfgOptions(known_targets={dispatch_addr: [target, ...]})`, or turn the
classifier off entirely with `LifterOptions(resolve_indirect_branches=False)`.
An unresolved branch is a result, never an error. `analyze` raises only when the
loop oscillates, meaning some site lost a target; running out of iterations while
every site still grows is the chain-depth limit, and those sites come back in
`unresolved` too.

Linked images and unlinked object files both load. `load_elf` walks `PT_LOAD`
program headers, falls back to sections for an `ET_REL` object, and applies the
file's relocations as it reads; `apply_relocations=False` gives you the bytes as
they sit on disk.

```python
obj = strider.lift.load_elf("fixtures/out/x86/memory.o")   # an unlinked .o
cfg, function, unresolved = obj.analyze("array_sum")
```

The image `LoadReadOnly` folds constants out of is the loaded file minus its
writable mappings, so a load out of an RWX segment is fetched from but never
folded.

A convention can be narrowed for one analysis: `cc.preserves_all()` clobbers
nothing, and `cc.preserves_regs()` preserves the registers but still clobbers
memory. Pair either with `LifterOptions(per_address_ccs={addr: cc})` to model a
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

# The explorer draws the neighborhood around a node you pick, not the whole
# graph, so it stays usable on large functions.
prog.visualize(function)   # prints a local URL; Ctrl-C to stop
```

You can also decide matches with your own logic. `.when(f)` filters one pattern
against a callable, backtracking so other bindings are still tried; to correlate
captures across several patterns, subclass `JoinPredicate`, declare the captures
it reads, and return a bool from `constraint`:

```python
from strider.pattern import call, int_const, Capture
from strider.pattern.constraints import JoinPredicate

n = Capture("n")

class MultipleOfEight(JoinPredicate):
    def captures(self):       return [n]           # the captures it correlates
    def constraint(self, m):  return m.uint(n) % 8 == 0

# Calls whose first argument is a constant multiple of eight.
function.find_all([call().arg(0, int_const(n))], constraints=[MultipleOfEight()])
```

`one_of([a, b])` matches either arm and reports the bindings of both;
`first_of` cuts to the first that matches. Either nests in a value, memory or
control slot. `load().non_stack()` and `store().heap_only()` filter by where the
address lives. An integer literal stands in for `int_const` in any slot, so
`int_add(base, 4)` is the same pattern as `int_add(base, int_const(4))`. Call
sites are selected with `call().target(addr)`, or `call().target([a, b])` for a
set of addresses.

Memory precision is tunable per analysis. `alias_mode` picks how far a
disjointness claim may reach, and is sound at either setting. The claims that
are not, which the IR cannot check, sit together in `AssumptionOptions`:
`escape_analysis=True` forwards a spill across a call when no stack address
escapes to the callee; `noalias_allocators=[malloc_addr]` lets a load step
through a pure allocator call, whose result is a fresh disjoint object; and
`callee_preserves_stack_args=True` treats the outgoing argument slots as
untouched by the callee, which the psABI permits it to write.

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

[`crates/strider-py/examples/python/`](crates/strider-py/examples/python/) has
seventeen runnable scripts, from a quickstart to custom target ABIs.

## Install

Strider uses [uv](https://docs.astral.sh/uv/). From the repository root:

```bash
uv sync --group dev       # virtualenv and dev dependencies
uv run maturin develop    # build the Rust extension
uv run pytest             # run the test suite
```

The fixture binaries under `fixtures/out/` are committed, so the examples and
tests run without a cross-compiler. Rebuild them with `cd fixtures && make` after
changing the sources.

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
runs the whole pipeline. See
[`crates/strider-orchestrator/examples/orchestrator_demo.rs`](crates/strider-orchestrator/examples/orchestrator_demo.rs)
for a runnable end-to-end example.

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## License

MIT. See [LICENSE](LICENSE).
