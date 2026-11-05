<p align="center">
  <img src="https://media.githubusercontent.com/media/p-mikeg/strider/master/docs/strider.png" alt="Strider" width="320">
</p>

# Strider

Strider answers precise questions about a compiled function, with no source
and no debug symbols.

It reads ELF images (and raw bytes) for x86 and x86-64, ARM/Thumb (little-endian, BE32, BE8),
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

# An interactive graph explorer at a local URL; background=True returns its port.
port = strider.explore.visualize(function, background=True)
strider.explore.shutdown(port)
```

## What's new since 0.1.0

[CHANGELOG.md](CHANGELOG.md) has the details, breaking changes first.

- **Inputs.** Unlinked `.o` files, ppc64 ELFv1 descriptors, symbols added to a
  stripped image (`add_symbol_file`, `add_symbols`), and GCC `.cold` fragments
  followed as part of their function.
- **Completeness.** `cfg.incomplete` names every address the analysis cannot
  vouch for, under one of seven reasons, including a return whose target is
  not provably the caller's.
- **Mixed ISAs.** ARM/Thumb and MIPS16 targets decode in the mode their edge
  carries; `$d` literal pools are skipped.
- **Your own answers.** `CfgOptions` takes branch targets, no-return callees,
  data ranges and user-op ABIs; `SleighArch.preset`, `CallingConvention.preset`
  and `custom` describe a target.
- **Assumptions.** `AssumptionOptions` states the nine claims the analysis
  cannot check (five on by default); `none()` is the sound floor.
- **Queries.** `first_of` / `one_of` in any slot, `field`, `code_ptr`,
  `phi().input_from`, `.output(slot)`, heap / non-stack filters on loads and
  stores, stack-passed and float arguments, typed wildcards, `JoinPredicate`,
  and constants of any width.
- **Tooling.** `Function.optimize`, `Function.to_text`, `strider.template`'s
  `int_ne` / `int_le` / `int_sle` / `float_ne` and `capture_typed`, a
  background explorer, and handles usable from any thread.

## Upgrading

Most renames and removals raise, and [CHANGELOG.md](CHANGELOG.md) says how to
port each. Its [0.3.0 section](CHANGELOG.md#breaking-python) opens with the
changes that alter an answer without an error. From 0.1.0, these do too:

- `ConstantFold` turns `x + x*2` into `x*3`, and a `Call`'s outputs are
  numbered per clobbered register.
- Five `AssumptionOptions` claims default on.
- `hit[c] is None` is always false: test `c in hit`.

## Documentation

The guides in [`docs/`](docs/):

- [Getting started](docs/getting-started.md): what Strider is and how the
  pipeline fits together.
- [Vocabulary](docs/vocabulary.md): the terms (region, phi, dominator, varnode,
  value types, ...) in plain language.
- [Python guide](docs/python-guide.md): a walkthrough of analyzing functions,
  queries, constraints, rewrites and custom memory readers.
- [Python API reference](docs/python-api.md): the Python API by topic, with
  runnable examples.
- [Optimizations](docs/optimizations.md): what each pass does, usually why a
  pattern's shape differs from the source.

[`crates/strider-py/examples/python/`](crates/strider-py/examples/python/) has
seventeen runnable scripts, from a quickstart to custom target ABIs.

## Install

The Sleigh fork is a submodule, so clone with it:

```bash
git clone --recursive https://github.com/p-mikeg/strider
# already cloned:
git submodule update --init --recursive
```

The fixture binaries under `fixtures/out/` are in Git LFS; fetch them to run
the examples and tests without a cross-compiler:

```bash
git lfs install && git lfs pull
```

Strider needs Rust 1.91 or newer and Python 3.9 or newer, and uses
[uv](https://docs.astral.sh/uv/). From the repository root:

```bash
uv sync --group dev       # virtualenv and dev dependencies
uv run maturin develop    # build the Rust extension
uv run pytest             # run the test suite
uv run pyright            # type-check against the stubs
```

After changing a case, rebuild it with `make -C fixtures ARCH=<arch>
CASE=<case>`; [fixtures/README.md](fixtures/README.md) says what to re-bless.

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

The crates work without the bindings:
`strider_orchestrator::Strider::new(...)` builds a handle and
`.analyze(entry, cc, &AnalyzeOptions, pipeline)` runs the whole pipeline.
`strider_cfg::DecodeEngine` builds a CFG alone, and `strider_opt` has the
passes, `rewrite_rule` and the branch classifiers (`classify_sites`).
[`orchestrator_demo.rs`](crates/strider-orchestrator/examples/orchestrator_demo.rs)
drives the stages by hand (`Lifter::new`, `build_cfg`, `build_ir`,
`pipeline.run`) over the committed fixtures and dumps each one (`cargo run -p
strider-orchestrator --example orchestrator_demo`).

## Build & test

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo test --workspace --release   # overflow-checks on, debug-assertions off
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links' cargo doc --workspace --no-deps
cargo +1.91.0 check --workspace --all-targets   # the declared MSRV
```

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) is the full gate list.

## License

MIT. See [LICENSE](LICENSE). The redistributed components keep their own
notices: GHIDRA's Sleigh (Apache-2.0), the rsleigh bindings (MIT), the linked
Rust crates and the two vendored browser bundles.
[THIRD-PARTY.md](THIRD-PARTY.md) lists them and [`licenses/`](licenses) holds
the full texts.
