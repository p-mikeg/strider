# `strider_pattern` macro emission specification

**Phase 4 Task 4.0 — V4 prerequisite.** Reference hand-implementation
lives at `crates/strider-py/src/pattern_reference.rs`. The Task 4.1
proc-macro (`#[strider_pattern]`) MUST emit code shape-identical to
that reference. This document captures the contract.

## Status

| Item | State |
|---|---|
| `pyo3-stub-gen` version pinned | `=0.7.0` (only line still compatible with pyo3 0.22; newer majors require pyo3 >= 0.24) |
| `pyo3` version | `0.22.6` (`abi3-py39`) |
| Reference type | `PyStackStorePatV2` -> exposed as `strider.pattern.StackStorePatV2` |
| Stub generator | `crates/strider-py/examples/stub_gen.rs` (cargo example, opt-in via the `stub_gen` feature) |
| mypy --strict | Passing (`tests/python/test_reference_pyi.py::test_reference_consumer_passes_mypy_strict`) |
| Runtime smoke | Passing (`test_reference_consumer_runs`) |

## Cargo wiring (one-time, already committed)

Add to `crates/strider-py/Cargo.toml`:

```toml
[lib]
name = "strider_py"           # avoids rlib collision with workspace `strider` crate
crate-type = ["cdylib", "rlib"]   # rlib for the stub-gen example

[dependencies]
pyo3 = { version = "0.22", default-features = false, features = ["abi3-py39", "anyhow", "multiple-pymethods"] }
pyo3-stub-gen = { version = "=0.7.0", default-features = false }

[features]
default = ["pyo3/extension-module"]   # the maturin path (default)
stub_gen = ["pyo3/auto-initialize"]   # the cargo-example path

[[example]]
name = "stub_gen"
required-features = ["stub_gen"]
```

Add to `crates/strider-py/src/lib.rs`:

```rust
pyo3_stub_gen::define_stub_info_gatherer!(stub_info);
```

## Per-pattern emission shape

For an input `#[strider_pattern] struct Foo { #[field(name, accepts = Pat)] }`,
the macro MUST emit code that matches this template (substitute
`Foo`, `name`, type, etc., per field).

```rust
use std::sync::{Arc, Mutex};
use std::collections::BTreeSet;
use pyo3::prelude::*;
use pyo3_stub_gen::derive::{gen_stub_pyclass, gen_stub_pymethods};

#[derive(Default)]
struct FooInner {
    field_a: Option<i64>,
    field_b: Option<BTreeSet<i64>>,
    field_c: Option<pattern::Pat>,
    field_d: Option<rsleigh::VnSpace>,
    when: Option<PyObject>,
    capture: Option<pattern::Capture>,
}

#[gen_stub_pyclass]                                  // (1) MUST be before
#[pyclass(name = "Foo", module = "strider.pattern")] // (2) the #[pyclass]
pub struct PyFooV2 {
    inner: Arc<Mutex<FooInner>>,
}

impl PyFooV2 {
    pub(crate) fn finalise(&self) -> pattern::Pat { /* assemble from fields */ }
}

#[gen_stub_pymethods]   // (3) MUST be before
#[pymethods]            // (4) the #[pymethods]
impl PyFooV2 {
    #[new]
    fn new() -> Self { Self { inner: Arc::new(Mutex::new(FooInner::default())) } }

    fn field_a(slf: PyRef<'_, Self>, v: i64) -> PyRef<'_, Self> {
        slf.inner.lock().unwrap_or_else(|p| p.into_inner()).field_a = Some(v);
        slf
    }
    fn field_b(slf: PyRef<'_, Self>, v: BTreeSet<i64>) -> PyRef<'_, Self> { /* ... */ slf }

    fn field_c<'py>(slf: PyRef<'py, Self>, v: PatLike<'py>) -> PyResult<PyRef<'py, Self>> {
        let pat = v.into_pat()?;
        slf.inner.lock().unwrap_or_else(|p| p.into_inner()).field_c = Some(pat);
        Ok(slf)
    }

    fn field_d(slf: PyRef<'_, Self>, v: PyVnSpace) -> PyRef<'_, Self> { /* ... */ slf }

    // Universal — every typed builder MUST expose these four.
    fn capture<'py>(slf: PyRef<'py, Self>, c: PyRef<'py, PyCapture>) -> PyRef<'py, Self> { /* ... */ slf }
    fn cap<'py>(slf: PyRef<'py, Self>, name: &'py str) -> PyResult<PyRef<'py, Self>> { /* ... */ }
    fn when(slf: PyRef<'_, Self>, f: PyObject) -> PyRef<'_, Self> { /* ... */ slf }
    fn into_pat(&self) -> PyPat { PyPat::from_pat(self.finalise()) }
}
```

## Attribute-order rules (V4 verification, confirmed by 4.0)

1. `#[gen_stub_pyclass]` MUST come BEFORE `#[pyclass]`.  The proc-
   macro walks the inner `#[pyclass]` attribute to discover the
   Python `name` and `module`.

2. `#[gen_stub_pymethods]` MUST come BEFORE `#[pymethods]`.  Same
   reason — the macro reads parameter names off the function
   signatures inside the `impl` block.

3. **No `#[pyo3(signature = (..., x = default))]`** when using
   `#[gen_stub_pyfunction]`.  `pyo3-stub-gen` 0.7 emits a reference
   to `pyo3::IntoPyObjectExt` (added in pyo3 0.23) for default-value
   defaults; the build fails on pyo3 0.22.  Either:
   - Use plain `Option<T>` (pyo3 auto-defaults `None`), then drop
     the `#[gen_stub_pyfunction]` and emit a hand-written `.pyi`
     stub for the free function, OR
   - Upgrade `pyo3-stub-gen` once a 0.22-compatible release ships
     IntoPyObjectExt support.

## Type-info rules

- Every `PyClass` referenced as a method-argument type or return
  type (e.g. `PyCapture`, `PyPat`, `PyVnSpace`) MUST have
  `#[gen_stub_pyclass]` on its struct definition, even if the rest
  of the type's surface is hand-written in `pattern.pyi`.  Without
  this, `pyo3-stub-gen`'s `PyStubType` trait is unimplemented and
  the build fails with E0277.

- Closure-typed args (`.when(f: PyObject)`) translate to
  `typing.Any` in the generated stub.  The hand-written
  `pattern.pyi` overrides this to
  `Callable[[PartialMatch], bool]` for ergonomics.  Task 4.1's
  macro MUST emit `PyObject` (not a typed closure) on the Rust
  side; the `.pyi` override is a separate concern handled by
  Task 4.3 (hand-written aliases module).

- Heterogeneous-enum args like `PatLike<'_>` need a manual
  `impl pyo3_stub_gen::PyStubType for PatLike<'_>` returning
  `TypeInfo::with_module("strider.pattern.PatLike", "strider.pattern")`.
  The macro MUST NOT attempt to auto-derive PyStubType for such
  enums; the hand-written impl in `crates/strider-py/src/pattern.rs`
  is the canonical bridge.

## Generated stub re-write loop

The cargo example at `crates/strider-py/examples/stub_gen.rs`:

1. Iterates `stub_info.modules` (populated by inventory at link time).
2. Writes each module to `crates/strider-py/strider/_generated/`
   (NOT `strider/`, which holds the wheel-shipped hand-written
   stubs).
3. `_generated/` is gitignored — the dev re-runs the example after
   editing a `#[strider_pattern]` struct and diffs the result
   against the hand-written `pattern.pyi`.

Phase 4 Task 4.2 flips the dependency direction: the generated
stubs become canonical, and `pattern.pyi` is auto-rewritten from
them (a small `cargo xtask sync-stubs` script will land then).

## Field annotations — extended set (Task 4.2b)

The base spec (Task 4.1) covers four field shapes: `#[field]` for
primitives, `#[field(accepts = "Pat")]` for `PatLike` operands, and
`#[field(accepts = "VnSpace")]` for `rsleigh::VnSpace`.  Task 4.2b
adds:

| Annotation | Field type | Setter signature | Used by |
|---|---|---|---|
| `#[field]` on `Option<String>` | `Option<String>` | `fn name(slf, s: String) -> PyRef<Self>` | `PyCallOtherPat.name` |
| `#[field(accepts = "Vn")]` | `Option<rsleigh::Vn>` | `fn for_vn(slf, vn: PyVn) -> PyRef<Self>` | `PyPhiPat.for_vn` |
| `#[field(multi, accepts = "Pat")]` | `Option<Vec<(usize, pattern::Pat)>>` | `fn arg(slf, idx: usize, p: PatLike) -> PyResult<PyRef<Self>>` | `PyCallPat.arg`, `PyPhiPat.input`, `PyRetPat.ret_val`, … |

Notes on `#[field(multi)]`:

- The inner-state field must be typed `Option<Vec<(IDX, T)>>` where
  `IDX` is `usize` or `u32`; the macro extracts `IDX` from the tuple
  to build the right setter signature.
- The vec is lazily allocated on first push, so an unset field stays
  `None` (matching the non-multi field contract).  The `finalise()`
  body iterates the vec in insertion order and applies the
  underlying builder's `.<py_name>(idx, value)` for each entry.
- The current implementation only supports `accepts = "Pat"` for
  multi-arg fields.  Non-Pat accumulators (e.g. `Vec<(u32, u64)>`)
  would need a separate emission path and have no in-tree call site
  today.

## Field annotations — Phase 4 Task 4.3 additions

Two macro extensions were added in Phase 4 Task 4.3 to migrate
`PyIntBinaryPat` / `PyBoolBinaryPat` / `PyFloatBinaryPat`:

- **Crate attribute `constructor_args = "name: Ty, name: Ty, ..."`**
  enables required-construction.  When set, the macro stores those
  names as plain (non-`Option`) fields in the inner state, emits a
  `pub(crate) fn new(name, ...) -> Self` constructor (NOT
  `#[new]`-annotated — Python can't construct the type directly), and
  the `finalise()` body calls `base_builder(name, ...)` with the
  stored args.  Every required type must be `Clone` (e.g. `Pat`) or
  `Copy` (e.g. `IntBinaryOp`).  No `Default` is derived on the inner
  struct.

- **`#[field(terminal)]`** marks a no-arg setter that toggles the
  underlying `Option<bool>` to `Some(true)` and immediately returns
  the finalised `PyPat` instead of `PyRef<Self>`.  The underlying
  builder method takes no args (`b.ordered()`, not
  `b.ordered(true)`).  Used by `.ordered()` on the binary-op
  builders so it remains a terminal operation that finalises to
  `Pat` (matches v1 behaviour).  Mutually exclusive with
  `#[field(multi)]` and `#[field(accepts = ...)]`.

## Patterns that intentionally stay hand-written

| Pattern | Reason |
|---|---|
| `PyFunctionArgPat` | Enum-dispatch source: `.index(u32)`, `.source_register(vn)`, `.source_stack(space, offset)` all write the same underlying `Option<FunctionArgSource>`.  Out of scope for the current `Option<T>`-per-field macro shape — adding a `#[field_setter(name = ..., variant = ..., args = ...)]` annotation that emits multiple named setters writing different enum variants into one field would gain ~30 LOC at the call site versus a chunky proc-macro change, so this is the one type left hand-written after Phase 4.3. |

## Coexistence with v1

The reference type is named `PyStackStorePatV2` (Rust) /
`StackStorePatV2` (Python) so the v1 hand-mirror
`PyStackStorePat` / `StackStorePat` continues to pass every existing
test during the migration.  Phase 4 Task 4.2 swaps the v1 type
out for a macro-generated one of identical shape; the `V2` suffix
disappears in the same commit.

## Verification

```bash
# Build the rlib + cdylib (default flow used by maturin)
cargo build -p strider-py

# Generate the .pyi files for the V2 reference
cargo run -p strider-py --example stub_gen --features stub_gen --no-default-features

# Rebuild + install the Python extension via maturin
( cd crates/strider-py && uv run maturin develop --release )

# Type-check the consumer
( cd crates/strider-py && uv run --with mypy mypy --strict tests/python/_consumer_reference.py )

# Run the runtime + mypy regression test
( cd crates/strider-py && uv run --with mypy pytest tests/python/test_reference_pyi.py )
```

All five commands MUST exit 0 before Task 4.1 can begin.
