---
name: strider-py-binding
description: Expose a Rust strider API to Python via PyO3, choosing the right module file, error taxonomy, GIL handling, and snapshot/test updates.
---

# strider-py-binding

## When to use

User wants to make a Rust API callable from Python. Triggers include "expose `<X>` from Rust to Python", "add a Python binding for `<rust API>`", "I want to call `<rust function>` from Python", "wrap this new pattern ctor / opt pass / matcher method for Python".

## When NOT to use

- The Python module already exposes the API and the user wants ergonomic improvements — route to a focused refactor.
- The user wants to add a test using existing bindings — that is just pytest, no binding work needed.
- The user is debugging a Python-side `PanicException` — that is usually a missing `PyResult` wrapper, but go via systematic debugging if the symptom is unclear.

## Inputs the skill expects

- The Rust API to expose (function, method, type).
- The intended Python module path (`strider`, `strider.opt`, `strider.pattern`, `strider.errors`).

## Procedure

1. Pick the source file in `crates/strider-py/src/`. Existing modules: `arch.rs`, `cc.rs`, `cfg.rs`, `dot.rs`, `errors.rs`, `graph.rs`, `matcher.rs`, `opt.rs`, `pattern.rs`, `reader.rs`, `run.rs`, `sleigh.rs`, `strider_cls.rs`. Use the module that already wraps the Rust crate the API lives in.
2. Define the PyO3 class or function. For opaque wrappers use `#[pyclass(name = "X", module = "strider.<sub>", frozen)]` with `#[pymethods] impl PyX { #[new] fn new(...) -> PyResult<Self> { ... } }`. For free functions use `#[pyfunction]` and register in the `#[pymodule]` block in `crates/strider-py/src/lib.rs`.
3. Handle errors correctly — never `panic!` or `unwrap` on user input. Rust panics from PyO3 land as Python `PanicException` and abort the Python process under `abi3-py39`. Always return `PyResult<T>` with a typed exception via the helpers in `crates/strider-py/src/errors.rs`. Existing taxonomy: `StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError`, `UnresolvedIndirectBranchError`, `UnknownCallOtherError`. Helpers: `into_pattern_err`, `into_reader_err`, `into_lift_err`, `into_rewrite_err`, `into_strider_err`. Do not add new exception types lightly.
4. Apply Python-side ergonomics where they help. String-keyed capture interning (`pattern.rs::intern_capture`) lets users write `add("x", "x")` instead of allocating a `Capture` object. Subclassable Python ABCs are used for callback-style readers (`MemReader`, `ReadOnlyMemory`).
5. Add tests. Place them in `crates/strider-py/tests/python/test_<feature>.py`. Workflow: `uv sync --group dev` once, then `uv run maturin develop` after every Rust edit, then `uv run pytest crates/strider-py/tests/python/test_<feature>.py`.
6. Update the public-API snapshot. `crates/strider-py/tests/python/test_public_api_snapshot.py` pins the exposed symbol list. New symbols change the snapshot intentionally; refresh with `uv run pytest --snapshot-update`.
7. Update the type stubs (`.pyi`) if the package ships any.
8. Watch for sync risk on default-pipeline mirrors. `opt::PipelineState::from_default` reconstructs Rust's `default_pipeline` by hand, so every Rust pass needs a Python wrapper added in lockstep. The recommended Rust-side `optimizer_count()` assertion catches drift.

## Verification

- `uv run maturin develop` — builds and installs the local abi3 wheel.
- `uv run pytest crates/strider-py/tests/python/test_<feature>.py -v`.
- `uv run pytest crates/strider-py/tests/python/test_public_api_snapshot.py` — confirms the API surface didn't change unexpectedly.
- `cargo clippy --workspace -- -D warnings`.
- `maturin build --release` should produce a wheel without warnings.

## Exit criteria

- The new symbol is importable as `from strider.<module> import <name>`.
- A test exercises the happy path and at least one error path (assert `pytest.raises(StriderError)` or a more specific subclass).
- Public-API snapshot updated and reviewed.
- `maturin build --release` completes cleanly.
- No existing tests regress.

## Pitfalls

- Do not propagate `panic!`. Use typed `PyResult<_>` with `into_*_err` from `errors.rs`. A panic across the FFI boundary aborts the Python interpreter.
- `PyPat::ordered()` no-op trap: when wrapping a builder method, mirror Rust's signature exactly. Do not silently no-op.
- `from_default()` desync: every Rust-side pass added to `default_pipeline` must be added to the Python `PipelineState::from_default` by hand. Pin with `optimizer_count()`.
- GIL pitfalls: long-running Rust work should release the GIL via `py.allow_threads(...)` to keep Python responsive.
- Test fixtures must use `uv run`. The local-built abi3 wheel under `target/wheels/` is what the tests import; running plain `pytest` against the system Python imports stale wheels.
- Memory ownership: wrapping `&BuiltFunctionGraph` requires an `Arc<...>` because Python objects outlive Rust scopes. See `PyGraph` and `PyMatcher` for the pattern.

## Related skills

- `strider-pattern-author` — when wrapping pattern ctors / builders for Python.
- `strider-opt-pass-author` — every new Rust pass needs a Python wrapper added in `opt.rs` and registered in `PipelineState::from_default`.
- `strider-target-arch` — when exposing a new arch / CC factory in `arch.rs` / `cc.rs`.
