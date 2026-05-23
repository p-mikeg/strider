# `strider-py` examples

Self-contained scripts that walk through every major surface of
`strider-py`. Each script is a runnable file — read top to bottom for
the explanation, run it to see real output against the fixture ELFs in
`fixtures/out/x86/`.

## Prerequisites

```bash
# from the workspace root
cd fixtures && make && cd ..
cd crates/strider-py
pip install maturin pyelftools patchelf
maturin develop
```

Then run any example. `maturin develop` installs the `strider` package
into the active Python environment, so the imports just work:

```bash
python crates/strider-py/examples/python/01_quickstart.py
```

If you skipped `maturin develop` and only built the wheel, point Python
at the package directory:

```bash
PYTHONPATH=crates/strider-py python crates/strider-py/examples/python/01_quickstart.py
```

## Index

| # | Script | Demonstrates |
|---|---|---|
| 01 | [`01_quickstart.py`](01_quickstart.py) | The five-line "load ELF → lift → query → visualize" flow with `strider.run`. |
| 02 | [`02_python_reader.py`](02_python_reader.py) | A custom `MemReader` subclass serving bytes from Python — lift a synthetic instruction stream without touching disk. |
| 03 | [`03_pattern_rewrite.py`](03_pattern_rewrite.py) | Find a redundant idiom (`x + 0`) and rewrite it to `x`; re-optimize destructively to collapse the dead branch. |
| 04 | [`04_custom_optimizer.py`](04_custom_optimizer.py) | Build an optimizer pipeline pass-by-pass; compare the lifted graph before and after. |
| 05 | [`05_visualize.py`](05_visualize.py) | Dump the CFG and the lifted IR graph as standalone HTML files for browser viewing. |
| 06 | [`06_complex_patterns.py`](06_complex_patterns.py) | Multi-level captures, back-references, `.when` predicate guards, commutative matching, the `_any` variant-agnostic constructors. |
| 07 | [`07_callback_rom.py`](07_callback_rom.py) | A `ReadOnlyMemory` subclass that serves `.rodata` from Python; folds compile-time-constant loads into constants via `LoadReadOnly`. |

## What each example doesn't show

These are not benchmarks and not stress tests. The fixture binaries
are intentionally small (a few dozen instructions per function) so the
output stays readable. For real workloads, see the
`crates/strider-analyze/examples/orchestrator_demo.rs` Rust example for
the canonical end-to-end run, and the `tests/python/` directory for
stress-tested end-to-end coverage.

## Reading order

If you're new to the library, read in numbered order — each script
introduces one or two concepts and reuses what the earlier ones
covered. If you already know the Rust API, jump to whichever script
matches your use case.
