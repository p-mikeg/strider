<p align="center">
  <img src="https://media.githubusercontent.com/media/p-mikeg/strider/master/docs/strider.png" alt="Strider" width="320">
</p>

# Strider

Strider answers precise questions about a compiled function, with no source
and no debug symbols.

It reads ELF images (and raw bytes) for x86 and x86-64, ARM/Thumb
(little-endian, BE32, BE8), AArch64 (LE/BE), MIPS32/64 (LE/BE, MIPS16e) and
PowerPC 32/64 (LE/BE).

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

Releases 0.2.0 and 0.3.0. [CHANGELOG.md](CHANGELOG.md) has the details,
breaking changes first.

- **Inputs.** ppc64 ELFv1 (`.opd` descriptors, `r2` set to the TOC on entry),
  symbols for a stripped image (`add_symbol_file`, `add_symbols`), GCC
  `.cold` fragments, ARM / AArch64 `$d` literal pools left undecoded, `.o`
  files linked at a synthetic base, and `load_elf(relocated_got_is_rom=True)`
  to fold loads through the GOT and `.data.rel.ro` (PIC jump tables need it).
- **Completeness.** `cfg.incomplete`: every address the analysis cannot vouch
  for, and why.
- **Mixed ISAs.** ARM/Thumb and MIPS16 code decodes in the mode its edge
  carries.
- **Your own answers.** Branch targets, no-return callees, data ranges and
  user-op ABIs (`CfgOptions`); `cc.preserves_all()` / `preserves_regs()` for
  a `per_address_ccs` callee (a callee-cleanup `ret $imm16`, a PC thunk);
  target presets (`SleighArch.preset`, `CallingConvention.preset`).
- **Kernels.** PowerPC `*_linux_kernel` conventions, which `load_elf` picks
  from the entry address.
- **Assumptions.** `AssumptionOptions`: nine claims the analysis cannot check,
  five on by default; `none()` is the sound floor.
- **Queries.** `one_of` / `first_of` in any slot, `field`, `code_ptr`, joins
  and `JoinPredicate`, heap / non-stack filters, float and stack arguments,
  constants of any width.
- **Tooling.** `Function.optimize`, `Function.to_text`, a background explorer,
  and handles usable from any thread.

New in 0.3.0: `Function.optimize`, the `preset` constructors, constants past
128 bits, handles usable from any thread, `.cold` fragments and the ELFv1 TOC
on entry; see the [0.3.0 section](CHANGELOG.md#030-unreleased).

## Upgrading

Most renames and removals raise, and [CHANGELOG.md](CHANGELOG.md) says how to
port each. Its [0.3.0 section](CHANGELOG.md#breaking-python) opens with the
changes that alter an answer without an error. From 0.1.0, these 0.2.0
changes do too:

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

## Crates

A Rust workspace of sixteen crates; [CLAUDE.md](CLAUDE.md) maps them. From
Rust, `strider_orchestrator::Strider::analyze` runs the pipeline and
[`orchestrator_demo.rs`](crates/strider-orchestrator/examples/orchestrator_demo.rs)
drives each stage by hand. `cargo test --workspace` runs the tests;
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) lists every gate.

## License

MIT. See [LICENSE](LICENSE). The redistributed components keep their own
notices: GHIDRA's Sleigh (Apache-2.0), the rsleigh bindings (MIT), the linked
Rust crates and the two vendored browser bundles.
[THIRD-PARTY.md](THIRD-PARTY.md) lists them and [`licenses/`](licenses) holds
the full texts.
