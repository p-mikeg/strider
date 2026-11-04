<p align="center">
  <img src="docs/strider.png" alt="Strider" width="320">
</p>

# Strider

> *"He's one of them Rangers. Dangerous folk they are, wandering the Wilds."*
> Barliman Butterbur, *The Fellowship of the Ring*

Strider is named after Aragorn's ranger alias: a tracker who finds what others
miss. It hunts through compiled binaries, letting you ask precise questions
about how a function behaves with no source and no debug symbols.

It reads ELF images for x86 and x86-64, ARM/Thumb (little-endian, BE32, BE8),
AArch64 (LE/BE), MIPS32/64 (LE/BE, MIPS16e) and PowerPC 32/64 (LE/BE).

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
    print("offset =", hit.uint(off))   # 4 and 8: the two stack arguments

# The explorer draws the whole graph, pannable and zoomable;
# visualize(whole=False) opens on the neighborhood around the entry instead,
# which stays usable on large functions. It prints a local URL and serves
# until Ctrl-C; background=True serves on a thread and returns the port, so
# you can keep querying:
port = strider.explore.visualize(function, background=True)
strider.explore.shutdown(port)
```

## What's new since 0.1.0

[CHANGELOG.md](CHANGELOG.md) has every change, breaking ones first.

- **More inputs.** Unlinked `.o` files (relocations applied, undefined symbols
  given addresses), ppc64 ELFv1 descriptors, stripped images named with
  `add_symbol_file` / `add_symbols`, and a second image merged with `add_elf`.
- **Complete or says why.** `cfg.incomplete` lists each address the analysis
  cannot vouch for, under one of seven reasons; empty means complete.
- **Mixed instruction sets.** ARM/Thumb and MIPS16 code decodes in the mode of
  the edge that reached it; `$d`-marked literal pools and `data_ranges` are
  not decoded.
- **Your own answers.** `known_targets`, `no_return_targets`, `data_ranges`,
  `call_other_abis`, `per_address_ccs`, and `CallingConvention.custom` /
  `CallOtherAbi.custom` for targets strider has no table for.
- **Stated assumptions.** `AssumptionOptions` holds nine claims the analysis
  cannot check (five on by default); `AssumptionOptions.none()` is the sound
  floor and `function.assumptions` reads them back.
- **Queries.** `one_of` / `first_of` in any slot, `field`, `code_ptr`,
  `find_unique_value`, `JoinPredicate`, edge joins (`capture_true`), and
  constants of any width; rewrites build from `strider.template`.
- **Tooling.** `function.to_text()`, `strider.explore.visualize(...,
  background=True)`, `StriderError.backtrace`; handles work on any thread and
  `analyze` drops the GIL.

## Upgrading

Most renames and removals raise, and [CHANGELOG.md](CHANGELOG.md) says how to
port each. These change an answer without an error.

From 0.2.0:

- A `one_of` / `first_of` arm in a memory slot, and a `call()` /
  `call_other()` in a raw slot (`input`, `any_input`, `phi_token`), also match
  the call's memory or control edge: `store().mem(one_of([call().res()]))`
  matched nothing and now matches.
- A builder's `into_pat()` follows later edits to the builder.
- `m[c] == v` compares at the constant's full width.
- Rust: a repeated `CallOtherPat::name` or `PhiPat::for_vn` intersects.

From 0.1.0:

- `ConstantFold` turns `x + x*2` into `x*3`, and a `Call`'s outputs are
  numbered per clobbered register.
- Five `AssumptionOptions` claims default on.
- `hit[c] is None` is always false: test `c in hit`.

Replacements for what is gone: `cfg.is_complete()` is `not cfg.incomplete`;
`IfPat.true_branch` is an edge join, `[if_else().capture_true(t),
region().any_input(t)]`; `strider.opt.CfgDetach`'s work is done by
`DeadBranchElimination`; `entry()` and `region()` as a value operand raise.

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
- [Changelog](CHANGELOG.md): every release's changes, breaking entries first.

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

MIT. See [LICENSE](LICENSE). The redistributed components keep their own
notices: GHIDRA's Sleigh (Apache-2.0), the rsleigh bindings (MIT), the linked
Rust crates and the two vendored browser bundles.
[THIRD-PARTY.md](THIRD-PARTY.md) lists them and [`licenses/`](licenses) holds
the full texts.
