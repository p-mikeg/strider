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
Seating an answer changes the CFG the classifier reads, so a wrong seed can
stop it deriving and take the site's real arms with it;
`cfg.unverified_seeded_sites()` names the dispatch addresses whose answer is
exactly your seed, plus every site the CFG consumed outright as a return or a
tail call, seeded or classifier-derived.
An unresolved branch is a result, never an error. A site whose answer never
settles is abandoned and reported, as is one still growing when the iteration
cap runs out, and a resolved target that turns out not to be code is dropped
with its site reported. `cfg.isa_mode_conflicts()` names any address two paths
reached in different ISA modes, where one region owns the bytes and the losing
path's arm is not the stream it believes, and `cfg.interior_branch_targets()`
names a target interior to a region but off every instruction boundary, whose
edge is seated on the region that owns the bytes rather than on the stream the
branch jumps to. Asking whether a result may be incomplete means reading all
four. `analyze` raises only on a genuine lift, CFG or optimizer failure.

A loaded image is mapped rather than copied, and its relocations are applied as
bytes are read, so a large object opens in tens of milliseconds and faults in
only what you analyse. Set `STRIDER_NO_MMAP=1` to read the file instead, which
a network or 9p mount needs: a paging error through a mapping is a SIGBUS no
caller can catch.

Linked images and unlinked object files both load. `load_elf` walks `PT_LOAD`
program headers, falls back to sections for an `ET_REL` object, and applies the
file's relocations as it reads. `apply_relocations=False` also narrows what is
mapped to code and read-only data, so a writable section like `.data` or `.got`
reads as `None` rather than as its on-disk bytes.

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
it. `lift.user_op_names()` lists what the architecture can emit,
`lift.call_other_abi(name)` reads back the classification in force, and
`CfgOptions(call_other_abis={"movmskps": strider.sleigh.CallOtherAbi.pure()})`
supplies the missing one; `CallOtherAbi.custom(sleigh, implicit_reads=[...])`
states an implicit register footprint.

Float arguments have their own index space: `function_arg_float(2)` reaches a
float parameter, and at a call site the float arguments follow the integer ones,
so on SysV `call().arg(6)` is the first of them.

`analyze` returns an `AnalyzeResult` (`.cfg` / `.function` / `.unresolved`),
which also unpacks as the 3-tuple above. `prog.symbol_at(addr)` reverse-resolves
an address to the `Symbol` covering it. A failure raised by strider itself
carries its Rust trace on `.backtrace`, and `STRIDER_BACKTRACE=1` folds it into
the message.

Nothing in an ELF header distinguishes hard-float ARM32 from soft-float, so
`load_elf` picks `arm_aapcs`; a soft-float binary needs
`cc=strider.sleigh.CallingConvention.arm_aapcs_soft()` or its float arguments
read as empty
registers.

Memory precision is tunable per analysis. `alias_mode` picks how far a
disjointness claim may reach: `"strict"` forwards only what the IR structurally
proves, and is sound under any input with `AssumptionOptions` left at its
defaults; the default `"stack_global_disjoint"` additionally assumes no constant
address equals `sp + K` at runtime. Every assumption knob adds Disjoint verdicts
that `"strict"` does not gate, so `"strict"` alone is not a soundness guarantee.
`assume_incoming_args_survive_calls` is one of them and defaults to `True`: it
assumes a callee leaves an incoming stack-argument slot as it found it, and it
reaches which loads count as incoming arguments, nothing else. The claims the IR
cannot check at all sit together in `AssumptionOptions`:
`escape_analysis=True` forwards a spill across a call when no stack address
escapes to the callee; `noalias_allocators=[malloc_addr]` lets a load step
through a pure allocator call, whose result is a fresh disjoint object; and
`callee_preserves_stack_args=True` treats the outgoing argument slots as
untouched by the callee, which the psABI permits it to write, and
`distinct_sp_bases_disjoint=True` says a store off one stack base cannot alias
one off another. Pass them as
`LifterOptions(assumptions=AssumptionOptions(...))`.

Some indirect-dispatch shapes do not resolve yet, and come back in
`unresolved` rather than as an error: AArch64 big-endian stack-array dispatch
built through a `bfi` insert against an alignment-masked SP; MIPS64 PIC
GOT-indirect dispatch, where the table values lift as `Add(Load[gp+off], const)`
rather than a raw constant; and PowerPC stack-array dispatch on ppc32 and ppc64.

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

The fixture object files are stored in Git LFS, so fetch them too:

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

The fixture binaries under `fixtures/out/` are committed, the object files
through Git LFS, so the examples and tests run without a cross-compiler. Rebuild
them with `cd fixtures && make` after changing the sources.

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
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --workspace --release   # a debug_assert hides from the debug run
cargo doc --workspace --no-deps
```

The Python side, from the repository root:

```bash
uv run maturin develop && uv run pyright && uv run pytest
```

## License

MIT. See [LICENSE](LICENSE).
