<p align="center">
  <img src="docs/strider.png" alt="Strider" width="320">
</p>

# Strider

> *"He's one of them Rangers. Dangerous folk they are, wandering the Wilds."*
> -- Barliman Butterbur, *The Fellowship of the Ring*

Strider is named after Aragorn's ranger alias: a tracker who moves quietly and
finds what others miss. It hunts through compiled binaries, letting you ask
precise questions about how a function behaves with no source and no debug
symbols.

## What it does

Strider loads a native binary, lifts one function into a sea-of-nodes IR,
optimizes it, and lets you query the result from Python. You describe a shape you
are looking for and get back every place it matches, with the values you asked to
capture. Typical questions: what offset does this function read off a pointer,
what value does it pass to `malloc`, what does it return when the input matches a
condition.

```
binary -> CFG -> IR -> optimizations -> pattern queries
```

## Quickstart

```python
import strider
from strider.pattern import load, add

# Load an ELF; the CPU and calling convention come from its header.
prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")

# Lift and optimize one function, by name or by address.
cfg, function, unresolved = prog.analyze("array_sum")

# Where does it load from `pointer + offset`?
for hit in function.find_all(load(addr=add("base", "off")), ignore_casts=True):
    print("offset =", hit.const_uint("off"))

# Explore the result interactively; the viewer scales to large graphs.
prog.visualize(function)   # prints a local URL; Ctrl-C to stop
```

## Documentation

The guides in [`docs/`](docs/) are the place to start:

- [Getting started](docs/getting-started.md): what Strider is and how the
  pipeline fits together.
- [Vocabulary](docs/vocabulary.md): the terms (region, phi, dominator, varnode,
  value types, ...) in plain language.
- [Python guide](docs/python-guide.md): the practical walkthrough of analyzing
  functions, writing queries, constraints, rewrites, and custom memory readers.
- [Optimizations](docs/optimizations.md): what each pass does, which is usually
  why a pattern's shape differs from the source.

[`crates/strider-py/examples/python/`](crates/strider-py/examples/python/) has
nine runnable scripts, from a quickstart to custom readers.

## Install

Strider uses [uv](https://docs.astral.sh/uv/). From the repository root:

```bash
uv sync --group dev       # virtualenv and dev dependencies
uv run maturin develop    # build the Rust extension
uv run pytest             # run the test suite
```

The examples and tests read the fixture binaries in `fixtures/`. Build them once
with `cd fixtures && make`; some are stored via Git LFS (`git lfs install &&
git lfs pull`).

## Architecture

A Rust workspace of sixteen crates. The ones you meet first:

| Crate | Role |
|-------|------|
| `strider-reader` | Loads an ELF and serves its memory to the lifter. |
| `strider-cfg` | Builds the control-flow graph of regions. |
| `strider-lift` | Lifts the CFG into the IR, handling register aliasing. |
| `strider-ir` | The sea-of-nodes IR. |
| `strider-opt` | Optimization passes and indirect-branch resolution. |
| `strider-pattern` | The pattern and rewrite engine. |
| `strider-orchestrator` | Ties lifting, optimizing, and resolving together. |
| `strider-py` | The Python bindings, the primary query interface. |

The rest are `strider-target` (arch and ABI descriptions) and five generic
utility crates (`dot`, `entity-utils`, `graph-algorithms`, `read-only-memory`,
`vn-container`). Each crate has its own `README.md`, and
[`CLAUDE.md`](CLAUDE.md) has the full dependency map and invariants.

## Rust API

The Python bindings are the main interface, but the crates are usable directly.
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
