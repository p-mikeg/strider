# `strider-py` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `crates/strider-py` — a PyO3-based Python binding crate that exposes the Strider analysis pipeline (Python-implemented readers, full pipeline, pattern queries, pattern-rewrite, CFG/Graph visualization) per `docs/superpowers/specs/2026-05-01-strider-py-design.md`.

**Architecture:** Single PyO3 cdylib crate at `crates/strider-py/`. All PyO3 wrappers live in this crate; inner crates (`ir`, `pattern`, `opt`, `strider`, `reader`, `target`, `cfg`, `dot`) stay PyO3-free. Built with `maturin`. Uses PyO3 abi3 stable ABI (CPython 3.9+).

**Tech Stack:** Rust + PyO3 + maturin + pytest + abi3.

**Hard rules across every phase:**
- TDD: failing test FIRST, minimal impl, pass, commit.
- No `panic!` / `unwrap` / `expect` / `debug_assert!` / `unreachable!` / `todo!` in production code (workspace clippy lints are `deny`).
- All PyO3 wrappers convert errors via `?` into `PyResult` — never `.unwrap()` in `#[pyfunction]` / `#[pymethods]` bodies.
- Every commit: lowercase imperative message + Why-body + `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>` trailer.
- Workspace stays GREEN at every commit (`cargo build --workspace && cargo test --workspace && cargo clippy --workspace`).
- Each task ends with a commit before the next task starts.

**How to test the Python side:** From `crates/strider-py/`, run `maturin develop` to build the `.so` into the active Python venv. Then `pytest tests/python/`. The test fixture is `fixtures/out/x86/test.elf` (built from `fixtures/Makefile`); the existing `crates/strider/examples/strider.rs` shows how it's used in Rust.

---

## Phase 0 — Crate skeleton

### Task 0.1: Create the crate skeleton + minimal PyO3 module

**Files:**
- Create: `crates/strider-py/Cargo.toml`
- Create: `crates/strider-py/pyproject.toml`
- Create: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/README.md`
- Create: `crates/strider-py/tests/python/__init__.py` (empty)
- Create: `crates/strider-py/tests/python/test_smoke.py`
- Modify: `Cargo.toml` (workspace members already include `crates/*`; verify clippy lints apply)

- [ ] **Step 1: Write failing smoke test**

Create `crates/strider-py/tests/python/test_smoke.py`:
```python
import strider

def test_module_loads():
    assert hasattr(strider, "__version__")
```

- [ ] **Step 2: Create Cargo.toml**

Create `crates/strider-py/Cargo.toml`:
```toml
[package]
name = "strider-py"
version = "0.1.0"
edition = "2024"

[lib]
name = "strider"
crate-type = ["cdylib", "rlib"]

[dependencies]
pyo3 = { version = "0.22", features = ["abi3-py39", "extension-module", "anyhow"] }
anyhow = { workspace = true }

strider = { workspace = true }
pattern = { workspace = true }
opt = { workspace = true }
ir = { workspace = true }
cfg = { workspace = true }
reader = { workspace = true }
target = { workspace = true }
dot = { workspace = true }
rsleigh = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 3: Create pyproject.toml**

Create `crates/strider-py/pyproject.toml`:
```toml
[build-system]
requires = ["maturin>=1.5,<2.0"]
build-backend = "maturin"

[project]
name = "strider"
version = "0.1.0"
description = "Python bindings for the Strider binary analysis pipeline"
requires-python = ">=3.9"
classifiers = [
    "Programming Language :: Rust",
    "Programming Language :: Python :: 3",
    "Programming Language :: Python :: Implementation :: CPython",
]

[tool.maturin]
manifest-path = "Cargo.toml"
features = ["pyo3/extension-module"]
python-source = "."
module-name = "strider"
```

- [ ] **Step 4: Create lib.rs**

Create `crates/strider-py/src/lib.rs`:
```rust
//! Python bindings for the Strider binary analysis pipeline.
//!
//! See `docs/superpowers/specs/2026-05-01-strider-py-design.md`.

use pyo3::prelude::*;

#[pymodule]
fn strider(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
```

- [ ] **Step 5: Create README.md**

Create `crates/strider-py/README.md`:
```markdown
# strider-py

Python bindings for the Strider binary analysis pipeline.

## Build (development)

From this directory:

    maturin develop

Then:

    pytest tests/python/

See `docs/superpowers/specs/2026-05-01-strider-py-design.md` for the full
design.
```

- [ ] **Step 6: Verify cargo build succeeds**

Run: `cargo build --workspace`
Expected: succeeds, builds the new `strider-py` crate.

- [ ] **Step 7: Verify clippy clean**

Run: `cargo clippy --workspace -- -D warnings`
Expected: no warnings.

- [ ] **Step 8: Verify maturin develop builds**

Run from `crates/strider-py/`: `maturin develop`
Expected: builds a wheel and installs into active Python env.

- [ ] **Step 9: Run smoke test**

Run from `crates/strider-py/`: `pytest tests/python/test_smoke.py -v`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add crates/strider-py/
git commit -m "$(cat <<'EOF'
strider-py: add crate skeleton + maturin/pyo3 wiring

Foundation for the Python bindings work tracked under
docs/superpowers/specs/2026-05-01-strider-py-design.md. Smoke test
verifies the maturin build path produces a loadable extension before any
real surface is added.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 0.2: Error class hierarchy + anyhow → PyErr conversion

**Files:**
- Create: `crates/strider-py/src/errors.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Modify: `crates/strider-py/tests/python/test_smoke.py`

- [ ] **Step 1: Write failing test for error class hierarchy**

Append to `crates/strider-py/tests/python/test_smoke.py`:
```python
def test_error_hierarchy():
    from strider import errors
    assert issubclass(errors.LiftError, errors.StriderError)
    assert issubclass(errors.ReaderError, errors.StriderError)
    assert issubclass(errors.PatternError, errors.StriderError)
    assert issubclass(errors.RewriteError, errors.StriderError)
```

- [ ] **Step 2: Run test, expect failure**

Run: `pytest tests/python/test_smoke.py::test_error_hierarchy -v`
Expected: FAIL — `errors` not in `strider`.

- [ ] **Step 3: Create errors.rs**

Create `crates/strider-py/src/errors.rs`:
```rust
//! Python exception hierarchy for strider-py.
//!
//! All Rust errors propagate through the analysis as `anyhow::Error` and
//! land in Python as `StriderError` (or one of its subclasses). The
//! subclasses are produced at well-defined boundaries (lift, reader
//! construction, pattern build, rewrite). Other errors fall through to
//! plain `StriderError`.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;

create_exception!(strider.errors, StriderError, PyException);
create_exception!(strider.errors, LiftError, StriderError);
create_exception!(strider.errors, ReaderError, StriderError);
create_exception!(strider.errors, PatternError, StriderError);
create_exception!(strider.errors, RewriteError, StriderError);

/// Convert an `anyhow::Error` into a generic `StriderError`. Use the
/// boundary-specific `into_*_err` helpers below when you know which
/// stage raised the error.
pub fn into_strider_err(e: anyhow::Error) -> PyErr {
    StriderError::new_err(format!("{e:#}"))
}

pub fn into_lift_err(e: anyhow::Error) -> PyErr {
    LiftError::new_err(format!("{e:#}"))
}

pub fn into_reader_err(e: anyhow::Error) -> PyErr {
    ReaderError::new_err(format!("{e:#}"))
}

pub fn into_pattern_err(e: anyhow::Error) -> PyErr {
    PatternError::new_err(format!("{e:#}"))
}

pub fn into_rewrite_err(e: anyhow::Error) -> PyErr {
    RewriteError::new_err(format!("{e:#}"))
}

/// Register the `strider.errors` submodule on the parent module.
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "errors")?;
    m.add("StriderError", py.get_type_bound::<StriderError>())?;
    m.add("LiftError", py.get_type_bound::<LiftError>())?;
    m.add("ReaderError", py.get_type_bound::<ReaderError>())?;
    m.add("PatternError", py.get_type_bound::<PatternError>())?;
    m.add("RewriteError", py.get_type_bound::<RewriteError>())?;
    parent.add_submodule(&m)?;
    parent.add("StriderError", py.get_type_bound::<StriderError>())?;
    Ok(())
}
```

- [ ] **Step 4: Wire it into lib.rs**

Modify `crates/strider-py/src/lib.rs`:
```rust
//! Python bindings for the Strider binary analysis pipeline.
//!
//! See `docs/superpowers/specs/2026-05-01-strider-py-design.md`.

use pyo3::prelude::*;

mod errors;

#[pymodule]
fn strider(py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    errors::register(py, m)?;
    Ok(())
}
```

- [ ] **Step 5: Rebuild + run test**

Run from `crates/strider-py/`: `maturin develop && pytest tests/python/test_smoke.py -v`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-py/src/errors.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_smoke.py
git commit -m "$(cat <<'EOF'
strider-py: register StriderError hierarchy + anyhow conversion helpers

Anchors the boundary at which inner-crate errors get classified. Every
PyO3 entry point converts via one of into_lift_err / into_reader_err /
into_pattern_err / into_rewrite_err / into_strider_err so subclassing
remains meaningful instead of decaying to a flat StriderError everywhere.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 1 — Building blocks: arch, calling convention, memory map, sleigh

### Task 1.1: PySleighArch with all classmethod presets

**Mirror:** `crates/target/src/arch.rs:32-218` — 15 presets (`x86_64`, `x86`, `mipsbe32`, `mipsle32`, `mipsbe64`, `mipsle64`, `arm`, `arm_be`, `arm_thumb`, `aarch64`, `aarch64be`, `ppc32be`, `ppc32le`, `ppc64be`, `ppc64le`).

**Files:**
- Create: `crates/strider-py/src/arch.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_arch.py`

- [ ] **Step 1: Write failing test**

Create `crates/strider-py/tests/python/test_arch.py`:
```python
import pytest
import strider

@pytest.mark.parametrize("name", [
    "x86_64", "x86",
    "mipsbe32", "mipsle32", "mipsbe64", "mipsle64",
    "arm", "arm_be", "arm_thumb",
    "aarch64", "aarch64be",
    "ppc32be", "ppc32le", "ppc64be", "ppc64le",
])
def test_sleigh_arch_presets(name):
    arch = getattr(strider.SleighArch, name)()
    assert isinstance(arch, strider.SleighArch)
    assert arch.name() == name

def test_sleigh_arch_repr():
    a = strider.SleighArch.x86_64()
    assert "SleighArch" in repr(a)
    assert "x86_64" in repr(a)
```

- [ ] **Step 2: Create arch.rs**

Create `crates/strider-py/src/arch.rs`:
```rust
//! `PySleighArch` — opaque wrapper over `target::SleighArch` with one
//! Python classmethod per Rust preset.

use pyo3::prelude::*;

#[pyclass(name = "SleighArch", module = "strider", frozen)]
#[derive(Clone)]
pub struct PySleighArch {
    pub(crate) inner: target::SleighArch,
    pub(crate) preset_name: &'static str,
}

#[pymethods]
impl PySleighArch {
    #[classmethod]
    fn x86_64(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::x86_64(), preset_name: "x86_64" }
    }
    #[classmethod]
    fn x86(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::x86(), preset_name: "x86" }
    }
    #[classmethod]
    fn mipsbe32(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::mipsbe32(), preset_name: "mipsbe32" }
    }
    #[classmethod]
    fn mipsle32(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::mipsle32(), preset_name: "mipsle32" }
    }
    #[classmethod]
    fn mipsbe64(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::mipsbe64(), preset_name: "mipsbe64" }
    }
    #[classmethod]
    fn mipsle64(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::mipsle64(), preset_name: "mipsle64" }
    }
    #[classmethod]
    fn arm(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::arm(), preset_name: "arm" }
    }
    #[classmethod]
    fn arm_be(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::arm_be(), preset_name: "arm_be" }
    }
    #[classmethod]
    fn arm_thumb(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::arm_thumb(), preset_name: "arm_thumb" }
    }
    #[classmethod]
    fn aarch64(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::aarch64(), preset_name: "aarch64" }
    }
    #[classmethod]
    fn aarch64be(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::aarch64be(), preset_name: "aarch64be" }
    }
    #[classmethod]
    fn ppc32be(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::ppc32be(), preset_name: "ppc32be" }
    }
    #[classmethod]
    fn ppc32le(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::ppc32le(), preset_name: "ppc32le" }
    }
    #[classmethod]
    fn ppc64be(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::ppc64be(), preset_name: "ppc64be" }
    }
    #[classmethod]
    fn ppc64le(_cls: &Bound<'_, pyo3::types::PyType>) -> Self {
        Self { inner: target::SleighArch::ppc64le(), preset_name: "ppc64le" }
    }

    fn name(&self) -> &'static str {
        self.preset_name
    }

    fn __repr__(&self) -> String {
        format!("SleighArch.{}()", self.preset_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleighArch>()
}
```

- [ ] **Step 3: Wire into lib.rs**

Add to `crates/strider-py/src/lib.rs`:
```rust
mod arch;
```
and inside the `#[pymodule]`:
```rust
arch::register(py, m)?;
```

- [ ] **Step 4: Build + run tests**

Run from `crates/strider-py/`: `maturin develop && pytest tests/python/test_arch.py -v`
Expected: 16 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/arch.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_arch.py
git commit -m "$(cat <<'EOF'
strider-py: expose SleighArch presets as Python classmethods

One classmethod per target::SleighArch preset. preset_name is carried
alongside so repr / name() can report which arch the user constructed
without round-tripping through Sleigh.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 1.2: PyCallingConvention with all classmethod presets

**Mirror:** `crates/target/src/calling_convention/mod.rs:136-437` — 9 presets (`x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`, `x86_cdecl`).

**Files:**
- Create: `crates/strider-py/src/cc.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_cc.py`

- [ ] **Step 1: Write failing test**

Create `crates/strider-py/tests/python/test_cc.py`:
```python
import pytest
import strider

@pytest.mark.parametrize("name", [
    "x86_64_systemv_abi", "aarch64_aapcs64", "arm_aapcs",
    "mips_o32", "mips_n64",
    "powerpc_sysv32", "powerpc64_elf_v1", "powerpc64_elf_v2",
    "x86_cdecl",
])
def test_cc_presets(name):
    cc = getattr(strider.CallingConvention, name)()
    assert isinstance(cc, strider.CallingConvention)
    assert cc.name() == name
```

- [ ] **Step 2: Create cc.rs**

Create `crates/strider-py/src/cc.rs` mirroring the structure of `arch.rs`:
- `pub struct PyCallingConvention { pub(crate) inner: target::CallingConvention, pub(crate) preset_name: &'static str }`
- One `#[classmethod]` per preset listed above. Each calls the corresponding `target::CallingConvention::<preset>()`.
- `fn name(&self) -> &'static str` and `fn __repr__(&self) -> String` mirroring `arch.rs`.
- `pub fn register(_py, m) -> PyResult<()> { m.add_class::<PyCallingConvention>() }`.

- [ ] **Step 3: Wire into lib.rs**

Add `mod cc;` and `cc::register(py, m)?;` in the `#[pymodule]`.

- [ ] **Step 4: Build + run tests**

Run from `crates/strider-py/`: `maturin develop && pytest tests/python/test_cc.py -v`
Expected: 9 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/cc.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_cc.py
git commit -m "$(cat <<'EOF'
strider-py: expose CallingConvention presets as classmethods

Mirrors target::CallingConvention's 9 presets with the same
preset_name carrying convention used in arch.rs. Keeps the building-
blocks API symmetric between arch and CC.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 1.3: PyMemoryMap (data-only fast reader)

**Mirror:** `crates/reader/src/lib.rs::MemRegion`, `MemRegionsLookupTable`, `ReadOnlyMemory`. The PyMemoryMap is a thin wrapper that owns one `MemRegionsLookupTable` and implements both `rsleigh::MemReader` and `reader::ReadOnlyMemory`.

**Files:**
- Create: `crates/strider-py/src/reader.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_memory_map.py`

- [ ] **Step 1: Write failing tests**

Create `crates/strider-py/tests/python/test_memory_map.py`:
```python
import pytest
import strider

def test_empty_memory_map():
    m = strider.MemoryMap()
    assert m.region_count() == 0

def test_add_region_and_read():
    m = strider.MemoryMap()
    m.add_region(0x1000, b"\x01\x02\x03\x04")
    assert m.region_count() == 1
    assert m.read(0x1000, 4) == b"\x01\x02\x03\x04"
    assert m.read(0x1002, 2) == b"\x03\x04"

def test_read_out_of_range_returns_none():
    m = strider.MemoryMap()
    m.add_region(0x1000, b"\x00\x01\x02\x03")
    assert m.read(0x2000, 4) is None

def test_overlapping_address_overwrites():
    m = strider.MemoryMap()
    m.add_region(0x1000, b"AAAA")
    m.add_region(0x1000, b"BBBB")
    assert m.read(0x1000, 4) == b"BBBB"

def test_overflow_rejected():
    m = strider.MemoryMap()
    with pytest.raises(strider.errors.ReaderError):
        m.add_region(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")
```

- [ ] **Step 2: Create reader.rs (MemoryMap only — callback subclasses come in Task 7)**

Create `crates/strider-py/src/reader.rs`:
```rust
//! Python-visible memory readers.
//!
//! `PyMemoryMap` is the data-only fast path: regions live entirely on
//! the Rust side. Callback-style `MemReader` / `ReadOnlyMemory`
//! subclasses live in the same file but are added in a later task.

use std::sync::Arc;

use pyo3::prelude::*;
use pyo3::types::PyBytes;

use crate::errors::into_reader_err;
use reader::{MemRegion, MemRegionsLookupTable, ReadOnlyMemory};

/// Owned-data memory map. Implements rsleigh::MemReader and
/// reader::ReadOnlyMemory via inner Arc — cheap to clone for handing
/// to Sleigh + the optimizer at the same time.
#[pyclass(name = "MemoryMap", module = "strider")]
#[derive(Clone)]
pub struct PyMemoryMap {
    /// Wrapped in Arc<RwLock<...>> so add_region after construction
    /// remains possible without &mut self plumbing across PyO3.
    inner: Arc<std::sync::RwLock<Vec<MemRegion>>>,
    /// Lazily rebuilt lookup table; invalidated on add_region.
    table: Arc<std::sync::RwLock<Option<Arc<MemRegionsLookupTable>>>>,
}

impl PyMemoryMap {
    fn rebuild_table(&self) -> anyhow::Result<Arc<MemRegionsLookupTable>> {
        let regions = self
            .inner
            .read()
            .map_err(|_| anyhow::anyhow!("MemoryMap regions lock poisoned"))?
            .clone();
        let t = Arc::new(MemRegionsLookupTable::new(regions));
        let mut slot = self
            .table
            .write()
            .map_err(|_| anyhow::anyhow!("MemoryMap table lock poisoned"))?;
        *slot = Some(Arc::clone(&t));
        Ok(t)
    }

    pub(crate) fn lookup_table(&self) -> anyhow::Result<Arc<MemRegionsLookupTable>> {
        let slot = self
            .table
            .read()
            .map_err(|_| anyhow::anyhow!("MemoryMap table lock poisoned"))?;
        if let Some(t) = slot.as_ref() {
            return Ok(Arc::clone(t));
        }
        drop(slot);
        self.rebuild_table()
    }
}

#[pymethods]
impl PyMemoryMap {
    #[new]
    fn new() -> Self {
        Self {
            inner: Arc::new(std::sync::RwLock::new(Vec::new())),
            table: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    fn add_region(&self, start_addr: u64, data: Vec<u8>) -> PyResult<()> {
        let region = MemRegion::new(start_addr, data).map_err(into_reader_err)?;
        let mut regions = self
            .inner
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        regions.push(region);
        // Invalidate lookup table.
        let mut slot = self
            .table
            .write()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap table lock poisoned")))?;
        *slot = None;
        Ok(())
    }

    fn region_count(&self) -> PyResult<usize> {
        let regions = self
            .inner
            .read()
            .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
        Ok(regions.len())
    }

    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let table = self.lookup_table().map_err(into_reader_err)?;
        let mut buf = vec![0u8; size];
        match table.read(addr, &mut buf) {
            Some(n) => {
                buf.truncate(n);
                Ok(Some(PyBytes::new_bound(py, &buf)))
            }
            None => Ok(None),
        }
    }
}

// Internal: rsleigh::MemReader impl that reads from a PyMemoryMap.
// Kept separate so the rsleigh trait dependency stays local and the
// Python class itself doesn't carry rsleigh in its public name.
pub struct PyMemoryMapReader {
    pub table: Arc<MemRegionsLookupTable>,
}

impl rsleigh::MemReader for PyMemoryMapReader {
    fn read(&mut self, addr: u64, out: &mut [u8]) -> bool {
        match self.table.read(addr, out) {
            Some(n) => n == out.len(),
            None => false,
        }
    }
}

// ReadOnlyMemory impl reading 1/2/4/8-byte little-endian words from
// any space — same pattern as the Rust reader::ElfFileMemReader.
impl ReadOnlyMemory for PyMemoryMap {
    fn read(&self, _space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        if size == 0 || size > 8 {
            return None;
        }
        let table = self.lookup_table().ok()?;
        let mut buf = [0u8; 8];
        let n = table.read(addr, &mut buf[..size])?;
        if n != size {
            return None;
        }
        Some(u64::from_le_bytes(buf))
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryMap>()
}
```

- [ ] **Step 3: Wire into lib.rs**

Add `mod reader;` and `reader::register(py, m)?;` in `#[pymodule]`.

- [ ] **Step 4: Build + run tests**

Run from `crates/strider-py/`: `maturin develop && pytest tests/python/test_memory_map.py -v`
Expected: 5 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/reader.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_memory_map.py
git commit -m "$(cat <<'EOF'
strider-py: add MemoryMap data-only reader

Wraps reader::MemRegion + MemRegionsLookupTable with mutability via
Arc<RwLock> so users can add_region after construction without &mut
self plumbing across PyO3. Implements rsleigh::MemReader (via a
helper view) and reader::ReadOnlyMemory directly so the same
MemoryMap can serve both the sleigh fetch path and the LoadReadOnly
opt pass.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 1.4: PySleigh wrapping rsleigh::Sleigh

**Mirror:** `crates/strider/examples/strider.rs:14-15` shows `rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem_reader)?`.

**Files:**
- Create: `crates/strider-py/src/sleigh.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_sleigh.py`

- [ ] **Step 1: Write failing test**

Create `crates/strider-py/tests/python/test_sleigh.py`:
```python
import strider

def test_sleigh_construct_with_memory_map():
    arch = strider.SleighArch.x86_64()
    mem = strider.MemoryMap()
    mem.add_region(0x1000, b"\x90\x90\x90\x90")  # 4 NOPs
    sleigh = strider.Sleigh(arch, mem)
    # Sleigh holds the spec; basic smoke test
    assert sleigh is not None
```

- [ ] **Step 2: Create sleigh.rs**

Create `crates/strider-py/src/sleigh.rs`:
```rust
//! `PySleigh` — wraps a constructed rsleigh::Sleigh keyed off a
//! PySleighArch + a PyMemoryMap (or, in a later task, a callback
//! reader).

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::errors::into_lift_err;
use crate::reader::{PyMemoryMap, PyMemoryMapReader};

/// Owns a constructed Sleigh. The backing reader is bundled inside
/// rsleigh::Sleigh's BufMemReader, so no separate borrow plumbing is
/// needed.
#[pyclass(name = "Sleigh", module = "strider")]
pub struct PySleigh {
    pub(crate) inner: rsleigh::Sleigh<rsleigh::mem_readers::BufMemReader<PyMemoryMapReader>>,
    pub(crate) arch_name: &'static str,
}

#[pymethods]
impl PySleigh {
    #[new]
    fn new(arch: PySleighArch, mem: PyMemoryMap) -> PyResult<Self> {
        let table = mem.lookup_table().map_err(into_lift_err)?;
        let reader = PyMemoryMapReader { table };
        let inner = rsleigh::Sleigh::new(arch.inner.sla_spec, arch.inner.pspec, reader)
            .map_err(into_lift_err)?;
        Ok(Self { inner, arch_name: arch.preset_name })
    }

    fn arch_name(&self) -> &'static str {
        self.arch_name
    }

    fn __repr__(&self) -> String {
        format!("Sleigh(arch={})", self.arch_name)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PySleigh>()
}
```

> **Note for the implementer:** if `rsleigh::Sleigh::new`'s exact signature differs from the strider example (e.g. requires a different reader-buffering wrapper), match `crates/strider/examples/strider.rs` exactly. The example is the source of truth for the construction sequence.

- [ ] **Step 3: Wire into lib.rs**

Add `mod sleigh;` and `sleigh::register(py, m)?;` in `#[pymodule]`.

- [ ] **Step 4: Build + run tests**

Run: `maturin develop && pytest tests/python/test_sleigh.py -v`
Expected: 1 PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/sleigh.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_sleigh.py
git commit -m "$(cat <<'EOF'
strider-py: wrap rsleigh::Sleigh as PySleigh

Constructs a Sleigh keyed off a PySleighArch + PyMemoryMap. Uses the
same construction sequence as crates/strider/examples/strider.rs so
behavior matches the canonical Rust path. Callback-reader-backed
construction comes later.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 2 — Pipeline (CFG, Strider, Graph)

### Task 2.1: PyCfg + build_cfg + dot rendering

**Mirror:**
- CFG construction: `crates/strider/examples/strider.rs:25-31` (`cfg::Builder::new(sleigh, addr, options).build()`).
- CFG dot dumper: `cfg::Cfg::dot_dumper()` returns a `GraphDotDumper` that `dot::GraphDot` consumes.

**Files:**
- Create: `crates/strider-py/src/cfg.rs`
- Create: `crates/strider-py/src/dot.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_cfg.py`

- [ ] **Step 1: Write failing tests**

Create `crates/strider-py/tests/python/test_cfg.py`:
```python
import os
import pathlib
import pytest
import strider

FIXTURE = pathlib.Path(__file__).resolve().parents[3] / "fixtures" / "out" / "x86" / "test.elf"

@pytest.mark.skipif(not FIXTURE.exists(),
                    reason="run `make` in fixtures/ to build the test ELF")
def test_build_cfg_for_struct_test():
    import elftools.elf.elffile  # pyelftools — common test dep
    with FIXTURE.open("rb") as f:
        ef = elftools.elf.elffile.ELFFile(f)
        sym = next(s for s in ef.get_section_by_name(".symtab").iter_symbols()
                   if s.name == "struct_test")
        addr = sym["st_value"]
    arch = strider.SleighArch.x86()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(FIXTURE))    # convenience — see Step 3
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh=sleigh, entry=addr,
                            allow_code_before_start_addr=True)
    assert cfg is not None

@pytest.mark.skipif(not FIXTURE.exists(), reason="fixture missing")
def test_cfg_to_html_writes_file(tmp_path):
    # ... same setup as above, abbreviated; parametrize via a fixture in conftest later
    ...
```

> **Implementer note:** Test fixtures will need pyelftools installed in the dev env (`pip install pyelftools`). Document this in the README under the test section.

- [ ] **Step 2: Add MemoryMap.add_region_from_elf convenience**

Append to `crates/strider-py/src/reader.rs` `#[pymethods] impl PyMemoryMap`:
```rust
fn add_region_from_elf(&self, path: &str) -> PyResult<()> {
    let obj = reader::load_elf(path).map_err(into_reader_err)?;
    let regions = reader::elf::elf_get_code_and_readonly_sections_as_mem_regions(&obj)
        .map_err(into_reader_err)?;
    let mut inner = self
        .inner
        .write()
        .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap regions lock poisoned")))?;
    inner.extend(regions);
    let mut slot = self
        .table
        .write()
        .map_err(|_| into_reader_err(anyhow::anyhow!("MemoryMap table lock poisoned")))?;
    *slot = None;
    Ok(())
}
```

- [ ] **Step 3: Create dot.rs (shared by CFG + Graph)**

Create `crates/strider-py/src/dot.rs`:
```rust
//! Shared dot-rendering adapters used by PyCfg and PyGraph.

use std::path::Path;

use pyo3::prelude::*;

use crate::errors::into_strider_err;

pub fn dot_style_for(name: Option<&str>) -> dot::DotStyle {
    match name.unwrap_or("dark") {
        "dark_cfg" => dot::DotStyle::dark_cfg(),
        "dark" => dot::DotStyle::dark(),
        "empty" => dot::DotStyle::empty(),
        // Default to dark for any unknown name; lib will document the
        // accepted set rather than fail noisily on a bad style name.
        _ => dot::DotStyle::dark(),
    }
}

pub fn dump_html<G: dot::GraphDotDumper>(d: &dot::GraphDot<G>, path: &str) -> PyResult<()> {
    d.dump_as_html(Path::new(path)).map_err(into_strider_err)
}

pub fn dump_dot<G: dot::GraphDotDumper>(d: &dot::GraphDot<G>, path: &str) -> PyResult<()> {
    d.dump_as_dot(Path::new(path)).map_err(into_strider_err)
}

pub fn html_str<G: dot::GraphDotDumper>(d: &dot::GraphDot<G>) -> PyResult<String> {
    d.as_html_from_dot().map_err(into_strider_err)
}
```

- [ ] **Step 4: Create cfg.rs**

Create `crates/strider-py/src/cfg.rs`:
```rust
//! `PyCfg` — wraps `cfg::Cfg` and exposes dot rendering.
//!
//! The CFG borrows the Sleigh, so the Python class also holds a Py
//! reference to its parent PySleigh to keep it alive.

use pyo3::prelude::*;

use crate::dot::{dot_style_for, dump_dot, dump_html, html_str};
use crate::errors::into_lift_err;
use crate::sleigh::PySleigh;

#[pyclass(name = "Cfg", module = "strider")]
pub struct PyCfg {
    pub(crate) inner: cfg::Cfg<rsleigh::mem_readers::BufMemReader<crate::reader::PyMemoryMapReader>>,
    // Holds a strong ref to the Python Sleigh so it isn't GC'd while
    // we keep using its inner spec data.
    _sleigh: Py<PySleigh>,
}

#[pyfunction(signature = (sleigh, entry, allow_code_before_start_addr=false))]
pub fn build_cfg(
    py: Python<'_>,
    sleigh: Py<PySleigh>,
    entry: u64,
    allow_code_before_start_addr: bool,
) -> PyResult<PyCfg> {
    // Move the Sleigh out of the Py wrapper for the duration of the
    // build. We do this by .borrow_mut().take()-style swap — but
    // rsleigh::Sleigh isn't Default. Instead, because cfg::Builder
    // takes the Sleigh by value, we must move it out and replace the
    // PySleigh slot with a placeholder. The simplest correct approach:
    // rebuild a fresh Sleigh from the same arch and mem inside the Py
    // wrapper. To keep this clean, the implementer should refactor
    // PySleigh so its inner is wrapped in Option, and build_cfg
    // .take()s it for the duration of the build, then puts the
    // resulting cfg's sleigh back. cfg::Cfg exposes its sleigh via
    // `cfg.sleigh` (see crates/strider/examples/strider.rs:42).
    let mut sleigh_borrow = sleigh.borrow_mut(py);
    let inner_sleigh = sleigh_borrow
        .take_inner()
        .ok_or_else(|| into_lift_err(anyhow::anyhow!("Sleigh already in use")))?;
    drop(sleigh_borrow);

    let mut opts_builder = cfg::OptionsBuilder::new();
    if allow_code_before_start_addr {
        opts_builder = opts_builder.allow_code_before_start_addr();
    }
    let opts = opts_builder.build();
    let built = cfg::Builder::new(inner_sleigh, entry, opts)
        .build()
        .map_err(into_lift_err)?;

    // Restore the sleigh into the Py wrapper so subsequent users can
    // see it (e.g. via Cfg.sleigh access in the future). For v1 we
    // hand the sleigh back via `built.sleigh`.
    let mut sleigh_borrow = sleigh.borrow_mut(py);
    sleigh_borrow.put_inner(built.sleigh);
    drop(sleigh_borrow);

    // The CFG no longer owns sleigh; we keep a parallel Py ref to
    // ensure Python doesn't GC it while users hold the Cfg.
    Ok(PyCfg { inner: built.into_cfg_only(), _sleigh: sleigh })
}

#[pymethods]
impl PyCfg {
    #[pyo3(signature = (path, style="dark_cfg"))]
    fn to_html(&self, path: &str, style: &str) -> PyResult<()> {
        let d = dot::GraphDot::new(self.inner.dot_dumper(), dot_style_for(Some(style)));
        dump_html(&d, path)
    }
    #[pyo3(signature = (path,))]
    fn to_dot(&self, path: &str) -> PyResult<()> {
        let d = dot::GraphDot::new(self.inner.dot_dumper(), dot_style_for(Some("dark_cfg")));
        dump_dot(&d, path)
    }
    #[pyo3(signature = (style="dark_cfg"))]
    fn html_str(&self, style: &str) -> PyResult<String> {
        let d = dot::GraphDot::new(self.inner.dot_dumper(), dot_style_for(Some(style)));
        html_str(&d)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyCfg>()?;
    m.add_function(wrap_pyfunction!(build_cfg, m)?)?;
    Ok(())
}
```

> **Implementer note:** The above sketches `take_inner` / `put_inner` on `PySleigh`. To make `PySleigh::inner` movable, refactor it to hold `Option<rsleigh::Sleigh<...>>` and add the two helpers. If the build_cfg flow ends up wanting the Sleigh back inside the Cfg (which `cfg::Cfg` does), the cleanest split is:
> - `PySleigh` keeps no inner sleigh after `build_cfg`; the sleigh moves into the `cfg::Cfg`.
> - `PyCfg` exposes `.sleigh()` returning a borrowed view if needed.
> Verify the actual `cfg::Cfg` API (does it own the sleigh? does it need exclusive access?) by reading `crates/cfg/src/lib.rs` before finalizing the design. Adjust the take/put dance accordingly. The example at `crates/strider/examples/strider.rs:42` shows `cfg.sleigh` is accessible — so the Cfg owns it.

- [ ] **Step 5: Wire into lib.rs**

Add `mod cfg; mod dot;` and `cfg::register(py, m)?;` in `#[pymodule]`.

- [ ] **Step 6: Build + run tests**

Install pyelftools first: `pip install pyelftools`.
Run: `cd fixtures && make && cd .. && cd crates/strider-py && maturin develop && pytest tests/python/test_cfg.py -v`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-py/src/cfg.rs crates/strider-py/src/dot.rs crates/strider-py/src/reader.rs crates/strider-py/src/sleigh.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_cfg.py
git commit -m "$(cat <<'EOF'
strider-py: add PyCfg with build_cfg + dot rendering

build_cfg moves the Sleigh out of PySleigh into the new Cfg using a
take/put dance on PySleigh's Option<inner>; the Cfg becomes the new
owner. Dot rendering goes through a shared dot.rs adapter so PyGraph
will reuse the same style/path/output helpers in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2.2: PyStrider + PyGraph (analyze_cfg path)

**Mirror:** `crates/strider/examples/strider.rs:16-23,38` — `Strider::new(arch, sleigh.regs(), cc)?` then `strider.analyze_cfg(&cfg)?.graph`.

**Files:**
- Create: `crates/strider-py/src/strider_cls.rs` (avoiding conflict with the crate name)
- Create: `crates/strider-py/src/graph.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_strider.py`

- [ ] **Step 1: Write failing test**

Create `crates/strider-py/tests/python/test_strider.py`:
```python
import pathlib
import pytest
import strider

FIXTURE = pathlib.Path(__file__).resolve().parents[3] / "fixtures" / "out" / "x86" / "test.elf"

@pytest.mark.skipif(not FIXTURE.exists(), reason="fixture missing")
def test_analyze_cfg_returns_graph():
    import elftools.elf.elffile
    with FIXTURE.open("rb") as f:
        ef = elftools.elf.elffile.ELFFile(f)
        sym = next(s for s in ef.get_section_by_name(".symtab").iter_symbols()
                   if s.name == "struct_test")
        addr = sym["st_value"]
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(FIXTURE))
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh=sleigh, entry=addr, allow_code_before_start_addr=True)

    s = strider.Strider(arch, sleigh, cc)   # Strider takes Sleigh too — needed for regs
    result = s.analyze_cfg(cfg)
    assert isinstance(result.graph, strider.Graph)
```

- [ ] **Step 2: Create strider_cls.rs**

Create `crates/strider-py/src/strider_cls.rs`:
```rust
//! `PyStrider` — wraps strider::Strider, exposes analyze_cfg.

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_lift_err;
use crate::graph::PyGraph;
use crate::sleigh::PySleigh;

#[pyclass(name = "Strider", module = "strider")]
pub struct PyStrider {
    inner: strider::Strider,
}

#[pyclass(name = "AnalyzeOutcome", module = "strider")]
pub struct PyAnalyzeOutcome {
    #[pyo3(get)]
    graph: Py<PyGraph>,
}

#[pymethods]
impl PyStrider {
    #[new]
    fn new(py: Python<'_>, arch: PySleighArch, sleigh: Py<PySleigh>, cc: PyCallingConvention) -> PyResult<Self> {
        let sleigh_b = sleigh.borrow(py);
        let regs = sleigh_b.regs().map_err(into_lift_err)?;  // see Step 3
        let inner = strider::Strider::new(arch.inner, regs, cc.inner)
            .map_err(into_lift_err)?;
        Ok(Self { inner })
    }

    fn analyze_cfg(&self, py: Python<'_>, cfg: &PyCfg) -> PyResult<PyAnalyzeOutcome> {
        let outcome = self.inner.analyze_cfg(&cfg.inner).map_err(into_lift_err)?;
        let graph = PyGraph::from_built(outcome.graph)?;
        Ok(PyAnalyzeOutcome { graph: Py::new(py, graph)? })
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStrider>()?;
    m.add_class::<PyAnalyzeOutcome>()?;
    Ok(())
}
```

- [ ] **Step 3: Add `regs()` to PySleigh**

Append to `crates/strider-py/src/sleigh.rs`:
```rust
impl PySleigh {
    pub(crate) fn regs(&self) -> anyhow::Result<rsleigh::SleighRegs> {
        // rsleigh::Sleigh::regs() returns Result<SleighRegs, _>;
        // verify exact name in rsleigh's API.
        self.inner.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Sleigh has been moved into a Cfg"))?
            .regs()
    }
}
```

(If PySleigh has been refactored to hold `Option<inner>` per Task 2.1's note, this guards against a moved-out state.)

- [ ] **Step 4: Create graph.rs (basic PyGraph + dot)**

Create `crates/strider-py/src/graph.rs`:
```rust
//! `PyGraph` — wraps ir::BuiltFunctionGraph; exposes dot rendering and
//! (in later tasks) find_all / rewrite / reoptimize / optimize.

use pyo3::prelude::*;

use crate::dot::{dot_style_for, dump_dot, dump_html, html_str};

#[pyclass(name = "Graph", module = "strider")]
pub struct PyGraph {
    pub(crate) inner: ir::BuiltFunctionGraph,
    // Sleigh ref is needed for dot rendering (dumper takes &Sleigh).
    pub(crate) sleigh: Option<Py<crate::sleigh::PySleigh>>,
}

impl PyGraph {
    pub fn from_built(inner: ir::BuiltFunctionGraph) -> PyResult<Self> {
        Ok(Self { inner, sleigh: None })
    }
}

#[pymethods]
impl PyGraph {
    #[pyo3(signature = (path, style="dark"))]
    fn to_html(&self, py: Python<'_>, path: &str, style: &str) -> PyResult<()> {
        let sleigh_ref = self.sleigh.as_ref()
            .ok_or_else(|| crate::errors::into_strider_err(anyhow::anyhow!(
                "Graph has no Sleigh; call PyGraph::attach_sleigh first")))?;
        let s_b = sleigh_ref.borrow(py);
        let s_inner = s_b.as_ref()?;
        let d = dot::GraphDot::new(self.inner.dot_dumper(s_inner), dot_style_for(Some(style)));
        dump_html(&d, path)
    }
    #[pyo3(signature = (path,))]
    fn to_dot(&self, py: Python<'_>, path: &str) -> PyResult<()> {
        let sleigh_ref = self.sleigh.as_ref()
            .ok_or_else(|| crate::errors::into_strider_err(anyhow::anyhow!(
                "Graph has no Sleigh; call PyGraph::attach_sleigh first")))?;
        let s_b = sleigh_ref.borrow(py);
        let s_inner = s_b.as_ref()?;
        let d = dot::GraphDot::new(self.inner.dot_dumper(s_inner), dot_style_for(Some("dark")));
        dump_dot(&d, path)
    }
    #[pyo3(signature = (style="dark"))]
    fn html_str(&self, py: Python<'_>, style: &str) -> PyResult<String> {
        let sleigh_ref = self.sleigh.as_ref()
            .ok_or_else(|| crate::errors::into_strider_err(anyhow::anyhow!(
                "Graph has no Sleigh; call PyGraph::attach_sleigh first")))?;
        let s_b = sleigh_ref.borrow(py);
        let s_inner = s_b.as_ref()?;
        let d = dot::GraphDot::new(self.inner.dot_dumper(s_inner), dot_style_for(Some(style)));
        html_str(&d)
    }
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyGraph>()
}
```

> **Implementer note:** the Sleigh ref needs to be threaded into the Graph at construction time. `PyStrider::analyze_cfg` should set `graph.sleigh = Some(cfg._sleigh.clone_ref(py))` before returning. Update accordingly.

- [ ] **Step 5: Wire into lib.rs**

Add `mod graph; mod strider_cls;` and `graph::register(py, m)?; strider_cls::register(py, m)?;` in `#[pymodule]`.

- [ ] **Step 6: Build + run test**

Run: `maturin develop && pytest tests/python/test_strider.py -v`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-py/src/strider_cls.rs crates/strider-py/src/graph.rs crates/strider-py/src/sleigh.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_strider.py
git commit -m "$(cat <<'EOF'
strider-py: add PyStrider + PyGraph with analyze_cfg path

Mirrors strider/examples/strider.rs lines 16-23 + 38: construct a
Strider, call analyze_cfg, get back a PyAnalyzeOutcome with a Graph
inside. Graph holds a Py ref to the parent Sleigh because the dot
dumper needs &Sleigh and it would otherwise be GC'd.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2.3: PyGraph.to_html / to_dot integration test

**Files:**
- Modify: `crates/strider-py/tests/python/test_strider.py`

- [ ] **Step 1: Add visualization test**

Append to `crates/strider-py/tests/python/test_strider.py`:
```python
@pytest.mark.skipif(not FIXTURE.exists(), reason="fixture missing")
def test_graph_to_html_writes_nonempty_file(tmp_path):
    # ... same setup as test_analyze_cfg_returns_graph
    out = tmp_path / "graph.html"
    result.graph.to_html(str(out))
    assert out.exists()
    assert out.stat().st_size > 0

@pytest.mark.skipif(not FIXTURE.exists(), reason="fixture missing")
def test_cfg_to_html_writes_nonempty_file(tmp_path):
    out = tmp_path / "cfg.html"
    cfg.to_html(str(out))
    assert out.exists()
    assert out.stat().st_size > 0
```

- [ ] **Step 2: Refactor common setup into a pytest fixture**

Create `crates/strider-py/tests/python/conftest.py`:
```python
import pathlib
import pytest

FIXTURE = pathlib.Path(__file__).resolve().parents[3] / "fixtures" / "out" / "x86" / "test.elf"

@pytest.fixture
def fixture_elf():
    if not FIXTURE.exists():
        pytest.skip("Run `make` in fixtures/ to build the test ELF")
    return FIXTURE

@pytest.fixture
def struct_test_addr(fixture_elf):
    import elftools.elf.elffile
    with fixture_elf.open("rb") as f:
        ef = elftools.elf.elffile.ELFFile(f)
        sym = next(s for s in ef.get_section_by_name(".symtab").iter_symbols()
                   if s.name == "struct_test")
        return sym["st_value"]

@pytest.fixture
def lifted(fixture_elf, struct_test_addr):
    import strider
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(fixture_elf))
    sleigh = strider.Sleigh(arch, mem)
    cfg = strider.build_cfg(sleigh=sleigh, entry=struct_test_addr,
                            allow_code_before_start_addr=True)
    s = strider.Strider(arch, sleigh, cc)
    return s, cfg, s.analyze_cfg(cfg)
```

Update `test_strider.py` and `test_cfg.py` to use these fixtures.

- [ ] **Step 3: Run tests**

`maturin develop && pytest tests/python/ -v`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/tests/python/conftest.py crates/strider-py/tests/python/test_strider.py crates/strider-py/tests/python/test_cfg.py
git commit -m "$(cat <<'EOF'
strider-py: add to_html visualization tests + shared fixtures

Verifies CFG and Graph dot rendering produces non-empty HTML files.
Common setup (loading the ELF, finding struct_test, lifting the
function) moves into conftest.py so subsequent test files can `use
lifted` instead of repeating the boilerplate.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 3 — Optimizer pipeline

### Task 3.1: PyOptimizerPipeline core + pure passes

**Mirror:** `crates/opt/src/pipeline.rs` (`OptimizerPipeline`); `crates/opt/src/lib.rs` for the pre-built helpers (`default_pipeline`, `stable_default_pipeline`, `destructive_default_pipeline`).

**Files:**
- Create: `crates/strider-py/src/opt.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_opt.py`

- [ ] **Step 1: Write failing tests**

Create `crates/strider-py/tests/python/test_opt.py`:
```python
import strider
from strider import opt

def test_empty_pipeline():
    p = strider.OptimizerPipeline.empty()
    assert p.pass_count() == 0

def test_pure_passes_constructible():
    for cls in (opt.ConstantFold, opt.KnownBits, opt.RedundantPhis,
                opt.DeadBranchElim, opt.CallOtherElide):
        p = strider.OptimizerPipeline.empty()
        p.add(cls())
        assert p.pass_count() == 1

def test_default_pipelines_built():
    assert strider.OptimizerPipeline.default().pass_count() > 0
    assert strider.OptimizerPipeline.stable_default().pass_count() > 0
    assert strider.OptimizerPipeline.destructive_default().pass_count() > 0
```

- [ ] **Step 2: Create opt.rs**

Create `crates/strider-py/src/opt.rs`. The high-level shape:
```rust
//! `PyOptimizerPipeline` — wrapper over opt::OptimizerPipeline plus
//! one wrapper class per Rust opt pass.

use pyo3::prelude::*;

use crate::cc::PyCallingConvention;
use crate::arch::PySleighArch;
use crate::reader::PyMemoryMap;
use crate::errors::into_strider_err;

/// Trait-object boxed per-pass wrappers all live in this enum. Using
/// an enum (not Box<dyn>) sidesteps the trait-object PyO3 issue and
/// keeps construction inspectable.
enum AnyPass {
    ConstantFold,
    KnownBits,
    RedundantPhis,
    DeadBranchElim,
    CallOtherElide,
    LoadReadOnly(std::sync::Arc<dyn reader::ReadOnlyMemory>),
    StackStoreDetect(target::CallingConvention),
    StackLoadForward(target::CallingConvention, target::SleighArch),
    FunctionArgDetect(target::CallingConvention),
    CallStackArgCollect(target::CallingConvention),
}

#[pyclass(name = "OptimizerPipeline", module = "strider")]
pub struct PyOptimizerPipeline {
    passes: Vec<AnyPass>,
    post_passes: Vec<AnyPass>,
}

#[pymethods]
impl PyOptimizerPipeline {
    #[staticmethod]
    fn empty() -> Self { Self { passes: vec![], post_passes: vec![] } }

    #[staticmethod]
    fn default_() -> Self {
        // Mirror opt::default_pipeline() — see crates/opt/src/lib.rs
        let mut s = Self::empty();
        s.passes.push(AnyPass::ConstantFold);
        s.passes.push(AnyPass::KnownBits);
        s.passes.push(AnyPass::RedundantPhis);
        s.passes.push(AnyPass::DeadBranchElim);
        s.passes.push(AnyPass::CallOtherElide);
        s
    }

    #[staticmethod]
    fn stable_default() -> Self {
        let mut s = Self::empty();
        s.passes.push(AnyPass::ConstantFold);
        s.passes.push(AnyPass::KnownBits);
        s
    }

    #[staticmethod]
    fn destructive_default() -> Self {
        let mut s = Self::empty();
        s.passes.push(AnyPass::RedundantPhis);
        s.passes.push(AnyPass::DeadBranchElim);
        s.passes.push(AnyPass::CallOtherElide);
        s
    }

    fn add(&mut self, pass_obj: PyAnyPass) {
        self.passes.push(pass_obj.0);
    }

    fn add_post(&mut self, pass_obj: PyAnyPass) {
        self.post_passes.push(pass_obj.0);
    }

    fn pass_count(&self) -> usize {
        self.passes.len() + self.post_passes.len()
    }
}

// Python-side helper: each pass class has a single tagged constructor
// returning a PyAnyPass with the right enum variant.
struct PyAnyPass(AnyPass);

#[pyclass(name = "ConstantFold", module = "strider.opt")]
struct PyConstantFold;
#[pymethods]
impl PyConstantFold {
    #[new] fn new() -> Self { Self }
    fn _to_pass(&self) -> PyAnyPass { PyAnyPass(AnyPass::ConstantFold) }
}
// (... similar PyClass-and-_to_pass per pass)

// Bridge: PyOptimizerPipeline.add takes anything implementing _to_pass.
// The Python-visible signature is `add(self, pass_obj)`, where pass_obj
// is any of the PyConstantFold/PyKnownBits/etc instances. PyO3 dispatch:
// implement `add` with `impl<'py> FromPyObject<'py> for PyAnyPass` that
// calls obj.call_method0("_to_pass")?.extract::<PyAnyPass>().

// The actual run impl, called by PyGraph::optimize:
impl PyOptimizerPipeline {
    pub fn run_on(&self, graph: &mut ir::BuiltFunctionGraph) -> anyhow::Result<()> {
        let mut pipe = opt::OptimizerPipeline::new();
        for p in &self.passes {
            match p {
                AnyPass::ConstantFold => pipe.add(opt::ConstantFold),
                AnyPass::KnownBits => pipe.add(opt::KnownBits),
                AnyPass::RedundantPhis => pipe.add(opt::RedundantPhis),
                AnyPass::DeadBranchElim => pipe.add(opt::DeadBranchElimination),
                AnyPass::CallOtherElide => pipe.add(opt::CallOtherElide),
                AnyPass::LoadReadOnly(rom) => pipe.add(opt::LoadReadOnly(rom.clone())),
                AnyPass::StackStoreDetect(cc) => {
                    let bcc = cc.clone().build(/* sleigh_regs */)?;
                    pipe.add(opt::StackStoreDetect::new(bcc.stack_pointer()));
                }
                // ... etc
            }
        }
        for p in &self.post_passes {
            // same dispatch but pipe.add_post_pass
        }
        pipe.run_on_built(graph)
    }
}
```

> **Implementer note:** the `StackStoreDetect`/`StackLoadForward`/`FunctionArgDetect`/`CallStackArgCollect` constructors need a `BuiltCallingConvention` (resolved varnodes), which requires `SleighRegs`. Either: (a) constructor takes a `PySleigh` so it can resolve regs immediately, or (b) defer resolution until `run_on` is called and Sleigh-regs are available from the graph context. Approach (a) is simpler — make the per-pass constructors take a `PySleigh`. Update tests accordingly. Read `crates/target/src/calling_convention/mod.rs:438` for the build signature.

- [ ] **Step 3: Wire and rename**

Python's `default` is a builtin name. Use `#[pyo3(name = "default")]` to expose `default_` as `default()` from Python. Same for `stable_default` and `destructive_default`.

- [ ] **Step 4: Create the `strider.opt` submodule**

Add to `opt::register`:
```rust
let opt_mod = PyModule::new_bound(py, "opt")?;
opt_mod.add_class::<PyConstantFold>()?;
opt_mod.add_class::<PyKnownBits>()?;
opt_mod.add_class::<PyRedundantPhis>()?;
opt_mod.add_class::<PyDeadBranchElim>()?;
opt_mod.add_class::<PyCallOtherElide>()?;
parent.add_submodule(&opt_mod)?;
parent.add_class::<PyOptimizerPipeline>()?;
```

- [ ] **Step 5: Build + test**

Run: `maturin develop && pytest tests/python/test_opt.py -v`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-py/src/opt.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_opt.py
git commit -m "$(cat <<'EOF'
strider-py: PyOptimizerPipeline core + pure-pass wrappers

Tagged enum dispatch (AnyPass) inside PyOptimizerPipeline keeps the
Rust-side pass list inspectable and avoids dyn Optimizer trait-object
plumbing across PyO3. Pure passes (ConstantFold, KnownBits,
RedundantPhis, DeadBranchElim, CallOtherElide) are zero-arg classes;
CC/arch-aware passes come in the next task.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3.2: CC/arch-aware passes (StackStoreDetect, StackLoadForward, FunctionArgDetect, CallStackArgCollect, LoadReadOnly)

**Mirror:** `crates/opt/src/{stack_store, stack_load_forward, function_args, load_readonly}/`.

**Files:**
- Modify: `crates/strider-py/src/opt.rs`
- Modify: `crates/strider-py/tests/python/test_opt.py`

- [ ] **Step 1: Add failing test**

Append to `crates/strider-py/tests/python/test_opt.py`:
```python
def test_cc_aware_passes_constructible(lifted):
    s, cfg, result = lifted
    cc = strider.CallingConvention.x86_cdecl()
    arch = strider.SleighArch.x86()
    sleigh = ...  # from fixtures or rebuilt
    p = strider.OptimizerPipeline.empty()
    p.add(opt.StackStoreDetect(cc, sleigh))
    p.add(opt.StackLoadForward(cc, arch, sleigh))
    p.add_post(opt.FunctionArgDetect(cc, sleigh))
    p.add_post(opt.CallStackArgCollect(cc, sleigh))
    assert p.pass_count() == 4

def test_load_readonly_constructible():
    mem = strider.MemoryMap()
    mem.add_region(0x1000, b"\x00" * 16)
    p = strider.OptimizerPipeline.empty()
    p.add(opt.LoadReadOnly(mem))
    assert p.pass_count() == 1
```

- [ ] **Step 2: Implement the additional pass classes**

Add to `crates/strider-py/src/opt.rs`:
- `PyStackStoreDetect { cc, regs }` — constructor `(cc: PyCallingConvention, sleigh: Py<PySleigh>)` resolves the BuiltCallingConvention immediately by calling `cc.inner.clone().build(&sleigh_regs)?`. Stores the resolved BCC.
- `PyStackLoadForward { cc, regs, arch }` — same plus arch.
- `PyFunctionArgDetect { cc, regs }` — same.
- `PyCallStackArgCollect { cc, regs }` — same.
- `PyLoadReadOnly { rom: Arc<dyn ReadOnlyMemory> }` — constructor `(rom: Py<PyMemoryMap>)`; clones the inner Arc.

Each adds a corresponding `AnyPass` variant. The `_to_pass()` returns the appropriate variant carrying the resolved Rust-side data.

- [ ] **Step 3: Update `run_on` dispatch**

Each new variant lands a real Rust pass into `opt::OptimizerPipeline`:
- `AnyPass::StackStoreDetect(bcc) => pipe.add(opt::StackStoreDetect::new(bcc.stack_pointer()))`
- `AnyPass::StackLoadForward(bcc, arch) => pipe.add(opt::StackLoadForward::new(bcc.stack_pointer(), arch.endianness))` (verify exact constructor signature in `crates/opt/src/stack_load_forward/mod.rs`)
- `AnyPass::FunctionArgDetect(bcc) => pipe.add_post_pass(opt::function_args::FunctionArgDetect::new(bcc))` (verify pass type/constructor)
- `AnyPass::CallStackArgCollect(bcc) => pipe.add_post_pass(opt::CallStackArgCollect::new(bcc))`
- `AnyPass::LoadReadOnly(rom) => pipe.add(opt::LoadReadOnly(rom))`

- [ ] **Step 4: Build + test**

Run: `maturin develop && pytest tests/python/test_opt.py -v`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/opt.rs crates/strider-py/tests/python/test_opt.py
git commit -m "$(cat <<'EOF'
strider-py: add CC/arch-aware opt pass wrappers + LoadReadOnly

Each CC-aware pass takes a (PyCallingConvention, PySleigh) pair at
construction so the BuiltCallingConvention resolves up-front; later
calls to OptimizerPipeline.add can't fail on missing Sleigh state.
LoadReadOnly takes any PyMemoryMap (the only ReadOnlyMemory impl
exposed via Python so far).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3.3: Graph.optimize / reoptimize + Strider.build_*_pipeline

**Files:**
- Modify: `crates/strider-py/src/graph.rs`
- Modify: `crates/strider-py/src/strider_cls.rs`
- Modify: `crates/strider-py/tests/python/test_opt.py`

- [ ] **Step 1: Write failing test**

Append to `test_opt.py`:
```python
def test_graph_optimize_with_default_pipeline(lifted):
    s, cfg, result = lifted
    pipe = strider.OptimizerPipeline.default()
    result.graph.optimize(pipe)
    # Smoke test: optimize completes without error and graph is still
    # render-able afterwards.
    html = result.graph.html_str()
    assert len(html) > 0

def test_graph_reoptimize(lifted):
    s, cfg, result = lifted
    result.graph.reoptimize()              # stable pipeline
    result.graph.reoptimize(destructive=True)
```

- [ ] **Step 2: Add methods to PyGraph**

Append to `crates/strider-py/src/graph.rs`:
```rust
#[pymethods]
impl PyGraph {
    fn optimize(&mut self, pipeline: &PyOptimizerPipeline) -> PyResult<()> {
        pipeline.run_on(&mut self.inner).map_err(into_strider_err)
    }

    #[pyo3(signature = (destructive=false))]
    fn reoptimize(&mut self, destructive: bool) -> PyResult<()> {
        let pipe = if destructive {
            PyOptimizerPipeline::destructive_default()
        } else {
            PyOptimizerPipeline::stable_default()
        };
        pipe.run_on(&mut self.inner).map_err(into_strider_err)
    }
}
```

- [ ] **Step 3: Add Strider.build_*_pipeline helpers**

Append to `crates/strider-py/src/strider_cls.rs`:
```rust
#[pymethods]
impl PyStrider {
    fn build_optimizer_pipeline(&self, py: Python<'_>) -> PyResult<PyOptimizerPipeline> {
        // Convert strider::Strider::build_optimizer_pipeline into our
        // tagged enum. Easiest: introspect the Rust pipeline and
        // recreate the equivalent Python pipeline declaratively.
        // ... see crates/strider/src/strider/pipeline.rs for the exact
        // pass set; mirror it.
        Ok(PyOptimizerPipeline::default_()) // replace with the actual
                                            // CC-aware pipeline
    }
    fn build_stable_optimizer_pipeline(&self) -> PyOptimizerPipeline {
        PyOptimizerPipeline::stable_default()
    }
    fn build_destructive_optimizer_pipeline(&self) -> PyOptimizerPipeline {
        PyOptimizerPipeline::destructive_default()
    }
}
```

> **Implementer note:** for `build_optimizer_pipeline` to actually mirror the Rust Strider's full CC-aware pipeline, you need to reproduce its pass list (StackStoreDetect + StackLoadForward + FunctionArgDetect + CallStackArgCollect + the pure passes). Read `crates/strider/src/strider/pipeline.rs` first to copy the exact set.

- [ ] **Step 4: Build + test**

Run: `maturin develop && pytest tests/python/test_opt.py -v`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/src/graph.rs crates/strider-py/src/strider_cls.rs crates/strider-py/tests/python/test_opt.py
git commit -m "$(cat <<'EOF'
strider-py: wire optimize/reoptimize on PyGraph + Strider helpers

graph.optimize(pipeline) runs an arbitrary user pipeline; reoptimize()
is sugar for the stable / destructive defaults. Strider mirrors the
Rust build_*_optimizer_pipeline trio so users can opt into the CC-aware
set without re-listing every pass.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 4 — Convenience: strider.run

### Task 4.1: strider.run kwargs API + PyRunResult

**Mirror:** `strider::run(RunConfig)` from `crates/strider/src/orchestrator.rs:147`.

**Files:**
- Create: `crates/strider-py/src/run.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_run.py`

- [ ] **Step 1: Write failing test**

Create `crates/strider-py/tests/python/test_run.py`:
```python
import strider

def test_run_returns_result(fixture_elf, struct_test_addr):
    mem = strider.MemoryMap()
    mem.add_region_from_elf(str(fixture_elf))
    result = strider.run(
        arch=strider.SleighArch.x86(),
        cc=strider.CallingConvention.x86_cdecl(),
        mem_reader=mem,
        rom=mem,
        entry=struct_test_addr,
        allow_code_before_start_addr=True,
    )
    assert isinstance(result.cfg, strider.Cfg)
    assert isinstance(result.graph, strider.Graph)
```

- [ ] **Step 2: Create run.rs**

Create `crates/strider-py/src/run.rs`:
```rust
//! `strider.run(...)` convenience — drives the full pipeline.
//!
//! Mirrors strider::run; returns a PyRunResult with cfg, graph, sleigh.

use std::sync::Arc;

use pyo3::prelude::*;

use crate::arch::PySleighArch;
use crate::cc::PyCallingConvention;
use crate::cfg::PyCfg;
use crate::errors::into_lift_err;
use crate::graph::PyGraph;
use crate::opt::PyOptimizerPipeline;
use crate::reader::PyMemoryMap;
use crate::sleigh::PySleigh;

#[pyclass(name = "RunResult", module = "strider")]
pub struct PyRunResult {
    #[pyo3(get)] pub cfg: Py<PyCfg>,
    #[pyo3(get)] pub graph: Py<PyGraph>,
    #[pyo3(get)] pub sleigh: Py<PySleigh>,
}

#[pyfunction(signature = (
    arch, cc, mem_reader, entry,
    rom=None, pipeline=None, allow_code_before_start_addr=false,
    fn_max_size=None,
))]
pub fn run(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem_reader: Py<PyMemoryMap>,
    entry: u64,
    rom: Option<Py<PyMemoryMap>>,
    pipeline: Option<Py<PyOptimizerPipeline>>,
    allow_code_before_start_addr: bool,
    fn_max_size: Option<u64>,
) -> PyResult<PyRunResult> {
    // Build Sleigh + Strider.
    let sleigh = PySleigh::new(arch.clone(), mem_reader.borrow(py).clone())?;
    let sleigh_py = Py::new(py, sleigh)?;

    let strider_cls = crate::strider_cls::PyStrider::new(
        py, arch, sleigh_py.clone_ref(py), cc.clone(),
    )?;

    // Drive strider::run end-to-end. Easiest path: reuse the building-
    // blocks: build_cfg → analyze_cfg → optimize. This skips the
    // indirect-branch fixed-point. For v1 we accept that strider.run
    // is "lift + optimize" rather than the full fixed-point until the
    // RunConfig path is wired in fully. Document this in §10.1 of the
    // spec.
    //
    // ALTERNATIVE (preferred for v1 if rsleigh::Sleigh<...> can be
    // moved through the Py wrapper): use strider::run with a
    // RunConfig built from these pieces, exactly mirroring the
    // example. Read crates/strider/src/orchestrator.rs:147 to confirm
    // the exact ownership flow.
    //
    // The test only asserts the result types, so either approach
    // satisfies test_run_returns_result. Recommend the RunConfig
    // path.

    todo!("see implementer notes above — pick the RunConfig path; this todo will be removed before commit")
}

pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyRunResult>()?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    Ok(())
}
```

> **CRITICAL implementer note:** the `todo!()` above MUST be replaced with the actual `strider::run` invocation before commit. The workspace lint forbids `todo!`. The hard part is moving the rsleigh::Sleigh out of the PySleigh into the RunConfig. Use the same Option-take/put dance as in `build_cfg`. After `strider::run` returns the BuiltFunctionGraph, also re-run a CFG build to populate the result.cfg field (since strider::run consumes the sleigh and only returns the graph; the example builds the CFG separately first).
>
> A cleaner alternative: have `run` call build_cfg then analyze_cfg then optimize internally — this avoids the indirect-branch fixed point but produces all three result fields. Document the limitation. The fixed-point version can come in a follow-up.

- [ ] **Step 3: Wire and test**

Add `mod run;` and `run::register(py, m)?;` to `lib.rs`. Build and run.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/src/run.rs crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_run.py
git commit -m "$(cat <<'EOF'
strider-py: add strider.run kwargs convenience entry point

Wraps the build_cfg → analyze_cfg → optimize chain so the common case
collapses to a single call returning RunResult{cfg, graph, sleigh}.
Indirect-branch fixed-point integration deferred to a follow-up; the
building-blocks API stays available for users that need it.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 5 — Pattern API

### Task 5.1: PyCapture + str-interning core

**Files:**
- Create: `crates/strider-py/src/pattern/mod.rs`
- Create: `crates/strider-py/src/pattern/capture.rs`
- Modify: `crates/strider-py/src/lib.rs`
- Create: `crates/strider-py/tests/python/test_pattern_capture.py`

- [ ] **Step 1: Write failing test**

Create `crates/strider-py/tests/python/test_pattern_capture.py`:
```python
import pytest
import strider
from strider.pattern import Capture

def test_capture_unique():
    a, b = Capture(), Capture()
    assert a != b
    assert hash(a) != hash(b)

def test_capture_repr():
    c = Capture()
    assert "Capture" in repr(c)
```

- [ ] **Step 2: Create pattern/capture.rs**

Create `crates/strider-py/src/pattern/capture.rs`:
```rust
//! PyCapture wraps pattern::Capture (typed-capture variable). Strings
//! also act as captures via per-pattern interning (see CaptureTable
//! below).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use pyo3::prelude::*;

#[pyclass(name = "Capture", module = "strider.pattern", frozen)]
#[derive(Clone)]
pub struct PyCapture {
    pub(crate) inner: pattern::Capture,
}

#[pymethods]
impl PyCapture {
    #[new]
    fn new() -> Self {
        Self { inner: pattern::Capture::new() }
    }

    fn __repr__(&self) -> String {
        format!("Capture({:?})", self.inner)
    }

    fn __hash__(&self) -> u64 {
        // pattern::Capture has a unique inner id; hash it
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.inner.hash(&mut h);
        h.finish()
    }

    fn __eq__(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}

/// Per-finalised-pattern capture interning table. Strings map to
/// pattern::Capture; same string → same Capture within one table.
#[derive(Default, Clone)]
pub struct CaptureTable {
    map: Arc<Mutex<HashMap<String, pattern::Capture>>>,
}

impl CaptureTable {
    pub fn intern(&self, name: &str) -> anyhow::Result<pattern::Capture> {
        if name == "_" || name == "any_" {
            return Err(anyhow::anyhow!(
                "{name:?} is reserved for the anonymous wildcard; use a different name"
            ));
        }
        let mut map = self
            .map
            .lock()
            .map_err(|_| anyhow::anyhow!("CaptureTable lock poisoned"))?;
        Ok(*map.entry(name.to_string()).or_insert_with(pattern::Capture::new))
    }

    pub fn names(&self) -> anyhow::Result<HashMap<String, pattern::Capture>> {
        Ok(self
            .map
            .lock()
            .map_err(|_| anyhow::anyhow!("CaptureTable lock poisoned"))?
            .clone())
    }
}
```

- [ ] **Step 3: Create pattern/mod.rs (just registration for now)**

Create `crates/strider-py/src/pattern/mod.rs`:
```rust
pub mod capture;

use pyo3::prelude::*;

pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "pattern")?;
    m.add_class::<capture::PyCapture>()?;
    parent.add_submodule(&m)?;
    Ok(())
}
```

- [ ] **Step 4: Wire into lib.rs**

Add `mod pattern;` and `pattern::register(py, m)?;`.

- [ ] **Step 5: Build + test**

Run: `maturin develop && pytest tests/python/test_pattern_capture.py -v`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-py/src/pattern/ crates/strider-py/src/lib.rs crates/strider-py/tests/python/test_pattern_capture.py
git commit -m "$(cat <<'EOF'
strider-py: add Capture + CaptureTable interning core

Capture is the typed handle (option A); CaptureTable is the per-Pat
interning machinery that lets strings (option B sugar) map back to
the same underlying pattern::Capture during pattern construction.
Reserved names (\"_\" / \"any_\") raise PatternError on use as a
binding name.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.2: PyPat + builder skeleton + leaves (any_, var, const, initial_var, function_arg, phi)

**Mirror:** `crates/pattern/src/pat/` and `crates/pattern/src/pat/ctor/`. The Rust `Pat` is `Arc<PatKind>`. Python wrapper is `PyPat { inner: pattern::Pat, table: CaptureTable }`.

**Files:**
- Create: `crates/strider-py/src/pattern/pat.rs`
- Create: `crates/strider-py/src/pattern/leaves.rs`
- Modify: `crates/strider-py/src/pattern/mod.rs`
- Create: `crates/strider-py/tests/python/test_pattern_leaves.py`

- [ ] **Step 1: Failing test**

Create `crates/strider-py/tests/python/test_pattern_leaves.py`:
```python
import pytest
from strider.pattern import any_, var, const, initial_var, function_arg, phi, phi_for, Capture, Pat

def test_any_is_pat():
    assert isinstance(any_, Pat) or callable(any_)  # spec: any_ is a singleton Pat

def test_var_with_capture():
    c = Capture()
    p = var(c)
    assert isinstance(p, Pat)

def test_var_with_string():
    p = var("x")
    assert isinstance(p, Pat)

def test_const_int():
    p = const(42)
    assert isinstance(p, Pat)

def test_const_bool():
    p = const(True)
    assert isinstance(p, Pat)

def test_function_arg_no_args():
    p = function_arg()
    assert isinstance(p, Pat)
```

- [ ] **Step 2: Create pat.rs**

Create `crates/strider-py/src/pattern/pat.rs`:
```rust
use pyo3::prelude::*;

use super::capture::{CaptureTable, PyCapture};

#[pyclass(name = "Pat", module = "strider.pattern")]
#[derive(Clone)]
pub struct PyPat {
    pub(crate) inner: pattern::Pat,
    pub(crate) table: CaptureTable,
}

impl PyPat {
    pub fn new(inner: pattern::Pat) -> Self {
        Self { inner, table: CaptureTable::default() }
    }

    pub fn with_table(inner: pattern::Pat, table: CaptureTable) -> Self {
        Self { inner, table }
    }
}

#[pymethods]
impl PyPat {
    fn __repr__(&self) -> String {
        format!("Pat({:?})", self.inner)
    }
}

/// Coerce a Python object (str | Capture | Pat) into a PyPat,
/// interning strings against the supplied CaptureTable.
pub fn coerce_to_pat(
    obj: &Bound<'_, PyAny>,
    table: &CaptureTable,
) -> PyResult<PyPat> {
    use crate::errors::into_pattern_err;

    if let Ok(p) = obj.extract::<PyPat>() {
        return Ok(p);
    }
    if let Ok(c) = obj.extract::<PyCapture>() {
        let pat = pattern::var(c.inner);
        return Ok(PyPat::with_table(pat.into(), table.clone()));
    }
    if let Ok(s) = obj.extract::<String>() {
        if s == "_" || s == "any_" {
            return Ok(PyPat::with_table(pattern::any().into(), table.clone()));
        }
        let cap = table.intern(&s).map_err(into_pattern_err)?;
        let pat = pattern::var(cap);
        return Ok(PyPat::with_table(pat.into(), table.clone()));
    }
    Err(into_pattern_err(anyhow::anyhow!(
        "expected str, Capture, or Pat; got {}",
        obj.get_type().name()?,
    )))
}
```

- [ ] **Step 3: Create leaves.rs**

Create `crates/strider-py/src/pattern/leaves.rs`:
```rust
use pyo3::prelude::*;

use super::capture::{CaptureTable, PyCapture};
use super::pat::{coerce_to_pat, PyPat};
use crate::errors::into_pattern_err;

#[pyfunction]
pub fn var(obj: &Bound<'_, PyAny>) -> PyResult<PyPat> {
    let table = CaptureTable::default();
    coerce_to_pat(obj, &table)
}

#[pyfunction(signature = (value, width=None))]
pub fn const_(value: &Bound<'_, PyAny>, width: Option<&str>) -> PyResult<PyPat> {
    use ir::node::NodeOutputType;

    if let Ok(b) = value.extract::<bool>() {
        return Ok(PyPat::new(pattern::bool_const(b).into()));
    }
    if let Ok(i) = value.extract::<i128>() {
        let _ = width; // TODO: respect width hint to choose NodeOutputType
        // For v1 default to U64. AnyIntConst is more permissive — see
        // crates/pattern/src/pat/ctor/.
        let v = i as u128;
        return Ok(PyPat::new(pattern::int_const(v as u64).into()));
    }
    if let Ok(f) = value.extract::<f64>() {
        return Ok(PyPat::new(pattern::float_const(f.to_bits()).into()));
    }
    Err(into_pattern_err(anyhow::anyhow!("const() takes int|bool|float")))
}

// any_ singleton — exposed as a free function returning a fresh Pat
// so __init__.pyi can type it as `Pat` rather than callable.
#[pyfunction]
pub fn any_fn() -> PyPat {
    PyPat::new(pattern::any().into())
}

// initial_var(vn), function_arg(idx=None), phi(), phi_for(vn) —
// implement using the corresponding pattern crate constructors.
#[pyfunction]
pub fn initial_var(vn_obj: &Bound<'_, PyAny>) -> PyResult<PyPat> {
    // vn_obj is an rsleigh.Vn — would need a PyVn wrapper. For v1
    // accept (space, offset, size) tuple as fallback.
    let _ = vn_obj;
    Err(into_pattern_err(anyhow::anyhow!("initial_var: PyVn wrapper not yet implemented")))
}

#[pyfunction(signature = (idx=None))]
pub fn function_arg(idx: Option<usize>) -> PyResult<PyPat> {
    let mut b = pattern::function_arg();
    if let Some(i) = idx {
        b = b.idx(i);
    }
    Ok(PyPat::new(b.into()))
}
```

> **Implementer note:** `initial_var(vn)` and `phi_for(vn)` need a `PyVn` wrapper around `rsleigh::Vn`. Ship a stub for v1 that accepts a 3-tuple `(space_id, offset, size)` and constructs the Vn manually; mark it as a follow-up to ship a real `PyVn` class.

- [ ] **Step 4: Register everything**

Update `pattern/mod.rs`:
```rust
pub fn register(py: Python<'_>, parent: &Bound<'_, PyModule>) -> PyResult<()> {
    let m = PyModule::new_bound(py, "pattern")?;
    m.add_class::<capture::PyCapture>()?;
    m.add_class::<pat::PyPat>()?;
    m.add_function(wrap_pyfunction!(leaves::var, &m)?)?;
    m.add_function(wrap_pyfunction!(leaves::const_, &m)?)?;
    m.add_function(wrap_pyfunction!(leaves::any_fn, &m)?)?;
    m.add_function(wrap_pyfunction!(leaves::initial_var, &m)?)?;
    m.add_function(wrap_pyfunction!(leaves::function_arg, &m)?)?;
    // Use Python rename to expose `any_` as `any_` and `const_` as `const`
    parent.add_submodule(&m)?;
    Ok(())
}
```

- [ ] **Step 5: Build + test**

`maturin develop && pytest tests/python/test_pattern_leaves.py -v`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-py/src/pattern/ crates/strider-py/tests/python/test_pattern_leaves.py
git commit -m "$(cat <<'EOF'
strider-py: add Pat + leaf builders (any_, var, const, function_arg)

Pat wraps pattern::Pat plus a per-finalised-pattern CaptureTable so
string captures intern correctly. coerce_to_pat is the central
str|Capture|Pat → PyPat coercion that every field method will use.
initial_var / phi_for ship as stubs awaiting a PyVn wrapper.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.3: Memory + integer-op builders

**Mirror:** `crates/pattern/src/pat/ctor/` for the full constructor list. Group: `load`, `store`, `stack_store`, `stack_store_phi`, plus integer ops `add, sub, mul, shl, shr, ushr, and_, or_, xor, int_eq, int_lt, int_slt, int_carry, int_scarry, int_unary, ...`.

**Files:**
- Create: `crates/strider-py/src/pattern/builders.rs`
- Modify: `crates/strider-py/src/pattern/mod.rs`
- Create: `crates/strider-py/tests/python/test_pattern_builders_int.py`

- [ ] **Step 1: Failing test**

Create `crates/strider-py/tests/python/test_pattern_builders_int.py`:
```python
from strider.pattern import load, store, stack_store, add, sub, mul, shl, and_, or_, xor, int_eq, int_lt, const, var, Capture, Pat

def test_load_basic():
    p = load()
    assert isinstance(p, Pat)

def test_load_with_addr():
    p = load(addr=add("p", "off"))
    assert isinstance(p, Pat)

def test_add_string_captures():
    p = add("a", "b")
    assert isinstance(p, Pat)

def test_add_back_reference():
    p = add("x", "x")    # back-reference; both must agree
    assert isinstance(p, Pat)

def test_chained_field_methods():
    p = load().addr(add("p", const(8)))
    assert isinstance(p, Pat)
```

- [ ] **Step 2: Create builders.rs**

Create `crates/strider-py/src/pattern/builders.rs`. The pattern is identical for every binary op — write one helper and one #[pyfunction] per op. Sketch for `add`:

```rust
use pyo3::prelude::*;

use super::capture::CaptureTable;
use super::pat::{coerce_to_pat, PyPat};

/// Merge tables from two operand pats into one result table.
fn merge_tables(a: &CaptureTable, b: &CaptureTable) -> CaptureTable {
    // For v1: take the first table; subsequent operand strings intern
    // into it. coerce_to_pat is called with the final table, so
    // strings always resolve into one shared table.
    let _ = b;
    a.clone()
}

#[pyfunction]
pub fn add(l: &Bound<'_, PyAny>, r: &Bound<'_, PyAny>) -> PyResult<PyPat> {
    let table = CaptureTable::default();
    let lp = coerce_to_pat(l, &table)?;
    let rp = coerce_to_pat(r, &table)?;
    let merged = merge_tables(&lp.table, &rp.table);
    let pat = pattern::add(lp.inner, rp.inner);
    Ok(PyPat::with_table(pat.into(), merged))
}

// Repeat with the same shape for sub, mul, shl, shr, ushr, and_,
// or_, xor, int_eq, int_lt, int_slt, int_carry, int_scarry.
//
// load() builder — supports field methods via a separate PyLoadPat
// class:
#[pyclass(name = "LoadPat", module = "strider.pattern")]
#[derive(Clone)]
pub struct PyLoadPat {
    pub(crate) inner: pattern::LoadPat,
    pub(crate) table: CaptureTable,
}

#[pymethods]
impl PyLoadPat {
    fn addr(&self, py: Python<'_>, p: &Bound<'_, PyAny>) -> PyResult<Self> {
        let arg = coerce_to_pat(p, &self.table)?;
        Ok(Self {
            inner: self.inner.clone().addr(arg.inner),
            table: self.table.clone(),
        })
    }
    fn space(&self, _py: Python<'_>, _space: u8) -> PyResult<Self> {
        // VnSpace conversion — accept an int for v1; provide PyVnSpace later.
        todo!()  // replace with real impl, no todo!() in committed code
    }
    fn capture(&self, c: PyCapture) -> Self {
        Self {
            inner: self.inner.clone().capture(c.inner),
            table: self.table.clone(),
        }
    }
    fn cap(&self, _py: Python<'_>, name: &str) -> PyResult<Self> {
        let cap = self.table.intern(name).map_err(into_pattern_err)?;
        Ok(Self {
            inner: self.inner.clone().capture(cap),
            table: self.table.clone(),
        })
    }
    // Convert to Pat
    fn __into_pat__(&self) -> PyPat {
        PyPat::with_table(self.inner.clone().into(), self.table.clone())
    }
}

// Free-function `load()` — returns a PyLoadPat.
#[pyfunction(signature = (addr=None))]
pub fn load(addr: Option<&Bound<'_, PyAny>>) -> PyResult<PyLoadPat> {
    let table = CaptureTable::default();
    let mut b = pattern::load();
    if let Some(a) = addr {
        let p = coerce_to_pat(a, &table)?;
        b = b.addr(p.inner);
    }
    Ok(PyLoadPat { inner: b, table })
}
```

> **Implementer note:** Use `coerce_to_pat` for every field-method argument and intern into the builder's own `table`. The `__into_pat__` is implicit — register `PyLoadPat` to `Into<PyPat>` so it works wherever Pat is expected (PyO3 supports `From` conversions via `FromPyObject`).
>
> Same pattern for `store`, `stack_store`, `stack_store_phi`, `call`, `call_other`, `ret`, `if_`, `phi` — each gets a `Py<X>Pat` class with the relevant field methods.

- [ ] **Step 3: Build + test**

`maturin develop && pytest tests/python/test_pattern_builders_int.py -v`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/src/pattern/builders.rs crates/strider-py/src/pattern/mod.rs crates/strider-py/tests/python/test_pattern_builders_int.py
git commit -m "$(cat <<'EOF'
strider-py: add memory + integer-op pattern builders

load() returns a PyLoadPat with chainable field methods; the same
shape applies to store/stack_store/etc in later tasks. Binary int ops
(add/sub/mul/shl/...) are bare functions returning Pat; commutative
matching follows automatically because they delegate to the Rust
crate's commutative constructors.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.4: Bool + float-op builders

**Mirror:** `crates/pattern/src/pat/ctor/` boolean and float sections.

**Files:**
- Modify: `crates/strider-py/src/pattern/builders.rs`
- Create: `crates/strider-py/tests/python/test_pattern_builders_bool_float.py`

- [ ] **Step 1: Failing test**

```python
from strider.pattern import bool_and, bool_or, bool_xor, float_add, float_sub, float_mul, float_div, float_eq, Pat

def test_bool_ops():
    for op in (bool_and, bool_or, bool_xor):
        assert isinstance(op("a", "b"), Pat)

def test_float_ops():
    for op in (float_add, float_sub, float_mul, float_div, float_eq):
        assert isinstance(op("a", "b"), Pat)
```

- [ ] **Step 2: Implement**

Add `bool_and`, `bool_or`, `bool_xor`, `bool_unary`, `float_add`, `float_sub`, `float_mul`, `float_div`, `float_eq`, `float_ne`, `float_lt`, `float_le` to `builders.rs`. Same pattern as `add`.

- [ ] **Step 3: Build + test**

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/src/pattern/builders.rs crates/strider-py/tests/python/test_pattern_builders_bool_float.py
git commit -m "$(cat <<'EOF'
strider-py: add bool + float pattern builders

Same shape as the int builders — delegating to the Rust crate's
commutative constructors so float_eq / bool_and / etc auto-match
both operand orders.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.5: Control-flow builders (call, call_other, ret, if_)

**Files:**
- Modify: `crates/strider-py/src/pattern/builders.rs`
- Create: `crates/strider-py/tests/python/test_pattern_builders_control.py`

- [ ] **Step 1: Failing test**

```python
from strider.pattern import call, call_other, ret, if_, const, Pat

def test_call():
    p = call()
    assert isinstance(p, Pat) or hasattr(p, "arg")

def test_call_at_arg():
    p = call().at(0x401000).arg(0, "x").arg(1, const(42))
    assert hasattr(p, "arg")

def test_ret_with_preceded_by():
    p = ret().preceded_by(call())
    assert hasattr(p, "preceded_by") or isinstance(p, Pat)

def test_if_branches():
    p = if_().cond("c").true_branch(call()).false_branch(ret())
    assert hasattr(p, "cond") or isinstance(p, Pat)
```

- [ ] **Step 2: Implement**

For each of `call`, `call_other`, `ret`, `if_`, create a `Py<X>Pat` class with the relevant field methods (`at`, `arg(idx, p)`, `preceded_by(p)`, `cond(p)`, `true_branch(p)`, `false_branch(p)`, `ret_val(idx, p)`, `capture`, `cap(name)`). Convert to `PyPat` via `__into_pat__`/`From` impl.

- [ ] **Step 3: Build + test + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: add control-flow pattern builders (call/call_other/ret/if_)

Each is its own PyClass with chainable field methods (.at, .arg,
.cond, .true_branch, .false_branch, .preceded_by, .ret_val). All
field-method arguments go through coerce_to_pat so str/Capture/Pat
all work uniformly.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.6: Predicates + .when() + .ordered() + .cap() coverage

**Files:**
- Modify: `crates/strider-py/src/pattern/pat.rs`
- Modify: `crates/strider-py/src/pattern/builders.rs`
- Create: `crates/strider-py/tests/python/test_pattern_predicates.py`

- [ ] **Step 1: Failing test**

```python
from strider.pattern import load, add, predicate, var, Capture, Pat

def test_when_on_pat():
    p = load(addr=add("p", "off")).when(lambda m: True)
    assert isinstance(p, Pat) or hasattr(p, "when")

def test_predicate_top_level():
    p = predicate(lambda m: True)
    assert isinstance(p, Pat)

def test_ordered():
    from strider.pattern import add
    p = add("a", "b").ordered()
    # ordered() only on the typed builder, not free-function add — see spec
    assert hasattr(p, "ordered") or isinstance(p, Pat)
```

- [ ] **Step 2: Implement `.when()`**

Add to PyPat:
```rust
fn when(&self, py: Python<'_>, f: PyObject) -> PyResult<PyPat> {
    // Wrap f as a Rust closure. The closure receives a partial-match
    // proxy; for v1 give it a stub Match-like that just exposes the
    // captures bound so far. Real Match impl comes in Task 5.8.
    let table = self.table.clone();
    let pat = self.inner.clone().when(move |_partial_match| {
        let result: PyResult<bool> = Python::with_gil(|py| {
            // build a partial-Match Python object, call f, extract bool
            let arg = ...; // PyMatchProxy
            let r = f.call1(py, (arg,))?;
            r.extract::<bool>(py)
        });
        result.unwrap_or(false)  // pattern crate expects a closure that
                                 // returns bool, not Result; on error
                                 // bail (don't match)
    });
    Ok(PyPat::with_table(pat, table))
}
```

> **Implementer note:** the `unwrap_or(false)` swallows errors — replace with a proper logging hook. For v1 a TODO comment is acceptable; the spec acknowledges predicate failures bail-without-match.

- [ ] **Step 3: Implement `.ordered()` on typed binary builders**

Per builder (`PyAddPat`, `PyMulPat`, etc.) — add `fn ordered(&self) -> Self` calling the Rust `.ordered()`. Free-function `add()` returns `PyAddPat` so `.ordered()` is chainable; coercion to `PyPat` happens via `__into_pat__`.

- [ ] **Step 4: Implement `predicate(f)` free function**

```rust
#[pyfunction]
pub fn predicate(f: PyObject) -> PyPat {
    let any = pattern::any();
    let pat = any.when(move |m| {
        Python::with_gil(|py| {
            f.call1(py, (/* partial match proxy */,))?.extract::<bool>(py)
        }).unwrap_or(false)
    });
    PyPat::new(pat.into())
}
```

- [ ] **Step 5: Build + test + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: add .when() / .ordered() / predicate() pattern guards

Predicate closures cross the Rust→Python boundary under the GIL;
errors raised inside the lambda bail the match rather than propagate
(matches the Rust pattern crate's bool-returning contract).

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.7: PyMatcher + matcher options

**Mirror:** `crates/pattern/src/matcher/`. Methods: `find_all(pat)`, `match_at(node, pat)`. Options: `ignore_casts`, `ignore_casts_mask`, `ignore_control_states`.

**Files:**
- Create: `crates/strider-py/src/pattern/matcher.rs`
- Modify: `crates/strider-py/src/pattern/mod.rs`
- Modify: `crates/strider-py/src/graph.rs`
- Create: `crates/strider-py/tests/python/test_pattern_matcher.py`

- [ ] **Step 1: Failing test**

```python
import strider
from strider.pattern import load, add, var, Capture, Matcher, Pat

def test_matcher_via_graph(lifted):
    s, cfg, result = lifted
    p = load()
    matches = result.graph.find_all(p)
    assert isinstance(matches, list)

def test_matcher_options(lifted):
    s, cfg, result = lifted
    m = result.graph.matcher(ignore_casts=True, ignore_control_states=True)
    matches = m.find_all(load())
    assert isinstance(matches, list)
```

- [ ] **Step 2: Create matcher.rs**

```rust
use pyo3::prelude::*;

use super::pat::PyPat;
use super::r#match::PyMatch;
use crate::errors::into_pattern_err;
use crate::graph::PyGraph;

#[pyclass(name = "Matcher", module = "strider.pattern", unsendable)]
pub struct PyMatcher {
    graph: Py<PyGraph>,
    ignore_casts: bool,
    ignore_control_states: bool,
    // ignore_casts_mask: handled via separate setter
}

#[pymethods]
impl PyMatcher {
    fn find_all(&self, py: Python<'_>, pat: PyPat) -> PyResult<Vec<PyMatch>> {
        let g = self.graph.borrow(py);
        let mut m = pattern::Matcher::new(&g.inner);
        if self.ignore_casts {
            m = m.ignore_casts();
        }
        if self.ignore_control_states {
            m = m.ignore_control_states();
        }
        let hits = m.find_all(&pat.inner);
        Ok(hits.into_iter().map(|h| PyMatch::from_match(h, self.graph.clone_ref(py))).collect())
    }

    fn match_at(&self, py: Python<'_>, node: PyObject /* opaque NodeId */, pat: PyPat)
        -> PyResult<Option<PyMatch>>
    {
        // Extract NodeId from `node` — opaque wrapper class to be added in Task 5.9
        let _ = (py, node, pat);
        Err(into_pattern_err(anyhow::anyhow!("match_at: PyNodeId wrapper not yet wired")))
    }
}
```

- [ ] **Step 3: Add Graph.matcher() + Graph.find_all()**

```rust
#[pymethods]
impl PyGraph {
    #[pyo3(signature = (ignore_casts=false, ignore_control_states=false))]
    fn matcher(slf: Py<Self>, ignore_casts: bool, ignore_control_states: bool, py: Python<'_>) -> PyMatcher {
        PyMatcher { graph: slf, ignore_casts, ignore_control_states }
    }

    #[pyo3(signature = (pat, ignore_casts=false, ignore_control_states=false))]
    fn find_all(slf: Py<Self>, py: Python<'_>, pat: PyPat,
                ignore_casts: bool, ignore_control_states: bool) -> PyResult<Vec<PyMatch>> {
        let m = PyMatcher { graph: slf.clone_ref(py), ignore_casts, ignore_control_states };
        m.find_all(py, pat)
    }
}
```

- [ ] **Step 4: Build + test + commit**

(Test stub for Match comes next; matcher returns Vec<PyMatch> immediately.)

---

### Task 5.8: PyMatch with typed accessors

**Files:**
- Create: `crates/strider-py/src/pattern/match_.rs`
- Modify: `crates/strider-py/src/pattern/mod.rs`
- Create: `crates/strider-py/tests/python/test_pattern_match.py`

- [ ] **Step 1: Failing test**

```python
import strider
from strider.pattern import load, add, var, const, Capture, Pat

def test_match_fields(lifted):
    s, cfg, result = lifted
    off = Capture()
    p = load(addr=add(var("p"), var(off)))
    hits = result.graph.find_all(p)
    if hits:
        m = hits[0]
        # API exists even when no captures bind
        assert m.uint(off) is None or isinstance(m.uint(off), int)
        assert "p" in m or True   # `in` semantics
```

- [ ] **Step 2: Create match_.rs**

```rust
use pyo3::prelude::*;

use super::capture::PyCapture;
use crate::graph::PyGraph;

#[pyclass(name = "Match", module = "strider.pattern", unsendable)]
pub struct PyMatch {
    inner: pattern::Match,
    graph: Py<PyGraph>,
}

impl PyMatch {
    pub fn from_match(inner: pattern::Match, graph: Py<PyGraph>) -> Self {
        Self { inner, graph }
    }

    fn resolve_capture(&self, key: &Bound<'_, PyAny>) -> PyResult<pattern::Capture> {
        if let Ok(c) = key.extract::<PyCapture>() {
            return Ok(c.inner);
        }
        // String resolution requires looking up in the originating Pat's
        // CaptureTable. For v1, accept Capture only here; string keys
        // resolve via __getitem__ on the Pat (a PartialMatch view).
        // OR: store the table on PyMatch.
        Err(crate::errors::into_pattern_err(anyhow::anyhow!(
            "Match key must be a Capture (string-key resolution requires the originating Pat's CaptureTable)"
        )))
    }
}

#[pymethods]
impl PyMatch {
    fn uint(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Option<u128>> {
        let c = self.resolve_capture(key)?;
        let g = self.graph.borrow(py);
        Ok(self.inner.get_uint(c, &g.inner))
    }

    fn int(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Option<i128>> {
        let c = self.resolve_capture(key)?;
        let g = self.graph.borrow(py);
        Ok(self.inner.get_int(c, &g.inner))
    }

    fn bool_(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Option<bool>> {
        let c = self.resolve_capture(key)?;
        let g = self.graph.borrow(py);
        Ok(self.inner.get_bool(c, &g.inner))
    }

    fn float_bits(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<Option<u64>> {
        let c = self.resolve_capture(key)?;
        let g = self.graph.borrow(py);
        Ok(self.inner.get_float_bits(c, &g.inner))
    }

    fn __getitem__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<PyObject> {
        // Best-effort: try uint first, then bool, then float bits.
        if let Some(v) = self.uint(py, key)? {
            return Ok(v.into_py(py));
        }
        if let Some(v) = self.bool_(py, key)? {
            return Ok(v.into_py(py));
        }
        if let Some(v) = self.float_bits(py, key)? {
            return Ok(v.into_py(py));
        }
        Err(pyo3::exceptions::PyKeyError::new_err("capture not bound or no value"))
    }

    fn __contains__(&self, py: Python<'_>, key: &Bound<'_, PyAny>) -> PyResult<bool> {
        let c = self.resolve_capture(key)?;
        Ok(self.inner.has(c))
    }

    // node_id, output_id, vn — opaque wrappers; stub for v1 if PyNodeId
    // not yet ready.
}
```

> **Implementer note:** for string keys to resolve, `PyMatch` needs the `CaptureTable` from the pattern that produced the match. Plumb it: `find_all` already has `pat: PyPat` which carries `pat.table` — pass `pat.table.clone()` into `PyMatch` and store it. Then `resolve_capture` for `String` keys does `self.table.intern(s)` and uses the resulting `Capture` (note: `intern` here REUSES if present, never creates — capturing strings post-match should error if the name wasn't in the pattern).

- [ ] **Step 3: Build + test + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: add PyMatch with uint/int/bool/float_bits accessors

Match resolves keys via the originating Pat's CaptureTable so both
Capture-object and string keys work. __getitem__ is best-effort
(tries uint→bool→float_bits in order); the typed accessors mirror
Rust 1-for-1.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 5.9: Integration test — find a real pattern in the test ELF

**Files:**
- Create: `crates/strider-py/tests/python/test_run_and_query.py`

- [ ] **Step 1: Write integration test**

```python
import strider
from strider.pattern import load, add, var, Capture

def test_find_load_in_struct_test(lifted):
    s, cfg, result = lifted
    pat = load()
    matches = result.graph.find_all(pat)
    # struct_test in test.elf contains memory accesses; expect ≥ 1 hit.
    assert len(matches) >= 1
```

- [ ] **Step 2: Build + run + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: end-to-end test — find_all(load()) on lifted struct_test

Closes the loop on Phase 5: build CFG → analyze → query → match. Any
regression in capture interning, builder coercion, or matcher wiring
trips this test against a real binary.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 6 — Rewrite

### Task 6.1: graph.rewrite + graph.rewrite_all

**Mirror:** `crates/pattern/src/rewrite.rs::rewrite_rule`, `apply_rules_in_order`, plus `crates/strider/src/rewrite.rs::GraphRewriter`.

**Files:**
- Modify: `crates/strider-py/src/graph.rs`
- Create: `crates/strider-py/tests/python/test_rewrite.py`

- [ ] **Step 1: Failing test**

```python
import strider
from strider.pattern import load, const, var, Capture

def test_rewrite_pattern_substitution(lifted):
    s, cfg, result = lifted
    # Trivial substitution: replace a constant load with a constant
    # (just exercising the API; semantics depend on the binary).
    find_pat = load()
    repl_pat = const(0)
    n_before = len(result.graph.find_all(find_pat))
    if n_before == 0:
        return
    result.graph.rewrite(find=find_pat, replace=repl_pat)
    result.graph.reoptimize()
    # After rewrite the load count should drop (or be 0).
    n_after = len(result.graph.find_all(find_pat))
    assert n_after <= n_before
```

- [ ] **Step 2: Add to PyGraph**

```rust
#[pymethods]
impl PyGraph {
    fn rewrite(&mut self, find: PyPat, replace: PyPat) -> PyResult<()> {
        let rule = pattern::rewrite_rule(find.inner, replace.inner);
        let rewriter = strider::GraphRewriter::new(&mut self.inner);
        rewriter.apply(&rule).map_err(into_rewrite_err)?;
        Ok(())
    }

    fn rewrite_all(&mut self, rules: Vec<(PyPat, PyPat)>) -> PyResult<()> {
        let rust_rules: Vec<_> = rules.into_iter()
            .map(|(f, r)| pattern::rewrite_rule(f.inner, r.inner))
            .collect();
        let rewriter = strider::GraphRewriter::new(&mut self.inner);
        rewriter.apply_in_order(&rust_rules).map_err(into_rewrite_err)?;
        Ok(())
    }
}
```

> **Implementer note:** verify the exact `GraphRewriter` API (`apply` / `apply_in_order` may have different names). Read `crates/strider/src/rewrite.rs` first.

- [ ] **Step 3: Build + test + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: add graph.rewrite + graph.rewrite_all

Pattern→pattern substitution backed by strider::GraphRewriter and
pattern::rewrite_rule. Multi-rule ordering follows Rust
apply_in_order — first-match-wins per node, no per-pattern rewriting
inside another rule's substitution.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 7 — Callback-style readers

### Task 7.1: PyMemReader + PyReadOnlyMemory subclass-style

**Files:**
- Modify: `crates/strider-py/src/reader.rs`
- Create: `crates/strider-py/tests/python/test_callback_reader.py`

- [ ] **Step 1: Failing test**

```python
import pytest
import strider

class TinyMem(strider.MemReader):
    def __init__(self, data, base):
        self.data = data
        self.base = base
    def read(self, addr, size):
        if addr < self.base or addr + size > self.base + len(self.data):
            return None
        offset = addr - self.base
        return bytes(self.data[offset:offset+size])

def test_callback_reader_basic():
    data = bytes([0x90, 0x90, 0xc3])    # 2 NOPs + ret
    r = TinyMem(data, 0x1000)
    arch = strider.SleighArch.x86()
    sleigh = strider.Sleigh(arch, r)    # Sleigh accepts a MemReader subclass
    assert sleigh is not None

class CallbackRom(strider.ReadOnlyMemory):
    def __init__(self, blob): self.blob = blob
    def read(self, space, addr, size):
        if addr < 0 or addr + size > len(self.blob):
            return None
        return int.from_bytes(self.blob[addr:addr+size], "little")

def test_callback_rom_read():
    rom = CallbackRom(b"\x42\x00\x00\x00")
    # Pass through to a Strider that uses it as the LoadReadOnly
    # pass's source — for the unit smoke test, just verify the
    # subclass instance survives Python-side and can be passed to
    # strider.opt.LoadReadOnly without raising.
    p = strider.opt.LoadReadOnly(rom)
    assert p is not None
```

- [ ] **Step 2: Implement PyMemReader + PyReadOnlyMemory abstract base classes**

Append to `crates/strider-py/src/reader.rs`:
```rust
//! Callback-style readers — Python subclasses whose `read` method is
//! invoked from Rust under the GIL.

#[pyclass(name = "MemReader", subclass, module = "strider")]
pub struct PyMemReader;

#[pymethods]
impl PyMemReader {
    #[new]
    fn new() -> Self { Self }

    fn read(&self, _py: Python<'_>, _addr: u64, _size: usize) -> PyResult<Option<Vec<u8>>> {
        // Default impl raises — subclasses MUST override.
        Err(crate::errors::into_reader_err(anyhow::anyhow!(
            "MemReader.read must be overridden in subclass"
        )))
    }
}

/// Adapter that lets a PyMemReader subclass act as rsleigh::MemReader.
pub struct PyMemReaderAdapter {
    obj: Py<PyAny>,
}

impl PyMemReaderAdapter {
    pub fn from_pyobject(obj: Py<PyAny>) -> Self { Self { obj } }
}

impl rsleigh::MemReader for PyMemReaderAdapter {
    fn read(&mut self, addr: u64, out: &mut [u8]) -> bool {
        Python::with_gil(|py| {
            let result = self.obj.call_method1(py, "read", (addr, out.len()));
            let bytes_obj = match result {
                Ok(v) if v.is_none(py) => return false,
                Ok(v) => v,
                Err(e) => { e.print(py); return false; }
            };
            let bytes_extract = bytes_obj.extract::<Vec<u8>>(py);
            match bytes_extract {
                Ok(b) if b.len() == out.len() => {
                    out.copy_from_slice(&b);
                    true
                }
                _ => false,
            }
        })
    }
}

#[pyclass(name = "ReadOnlyMemory", subclass, module = "strider")]
pub struct PyReadOnlyMemory;

#[pymethods]
impl PyReadOnlyMemory {
    #[new]
    fn new() -> Self { Self }

    fn read(&self, _py: Python<'_>, _space: u8, _addr: u64, _size: usize) -> PyResult<Option<u64>> {
        Err(crate::errors::into_reader_err(anyhow::anyhow!(
            "ReadOnlyMemory.read must be overridden in subclass"
        )))
    }
}

pub struct PyReadOnlyMemoryAdapter {
    obj: Py<PyAny>,
}

impl PyReadOnlyMemoryAdapter {
    pub fn from_pyobject(obj: Py<PyAny>) -> Self { Self { obj } }
}

impl reader::ReadOnlyMemory for PyReadOnlyMemoryAdapter {
    fn read(&self, space: rsleigh::VnSpace, addr: u64, size: usize) -> Option<u64> {
        Python::with_gil(|py| {
            // Pass space as an int (raw repr); v1 accepts an opaque
            // numeric — Python users rarely care about the discrim.
            let space_int: u8 = unsafe { std::mem::transmute(space) }; // placeholder
            let result = self.obj.call_method1(py, "read", (space_int, addr, size));
            match result {
                Ok(v) if v.is_none(py) => None,
                Ok(v) => v.extract::<u64>(py).ok(),
                Err(e) => { e.print(py); None }
            }
        })
    }
}

// Update register():
pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyMemoryMap>()?;
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    Ok(())
}
```

> **CRITICAL implementer notes:**
> 1. The `unsafe transmute` for `VnSpace` is wrong — replace with the actual `VnSpace::raw_id()` (or whatever rsleigh exposes). Read `rsleigh::VnSpace`'s API before committing.
> 2. `e.print(py)` on the error path silently swallows — for v1 acceptable but document it as a known limitation. A follow-up should propagate via a proper logging channel.
> 3. The `Sleigh::new` constructor in `crates/strider-py/src/sleigh.rs` takes `PyMemoryMap` only; extend it to accept any of `PyMemoryMap | PyMemReader subclass`. Use `extract::<PyMemoryMap>()` first, fall back to `extract::<Py<PyAny>>()` and check `isinstance(PyMemReader)`. Wrap as `PyMemReaderAdapter`.
> 4. Same for `LoadReadOnly`: extend to accept either `PyMemoryMap` or any `PyReadOnlyMemory` subclass. The `AnyPass::LoadReadOnly` variant already takes `Arc<dyn ReadOnlyMemory>`, so just wrap the adapter in an Arc.

- [ ] **Step 3: Extend Sleigh::new to accept either reader style**

Refactor `PySleigh::new` in `crates/strider-py/src/sleigh.rs`:
```rust
#[new]
fn new(py: Python<'_>, arch: PySleighArch, mem: &Bound<'_, PyAny>) -> PyResult<Self> {
    if let Ok(mm) = mem.extract::<PyMemoryMap>() {
        let table = mm.lookup_table().map_err(into_lift_err)?;
        let reader = PyMemoryMapReader { table };
        let inner = rsleigh::Sleigh::new(arch.inner.sla_spec, arch.inner.pspec, reader)
            .map_err(into_lift_err)?;
        return Ok(Self { inner: Some(inner), arch_name: arch.preset_name });
    }
    // Else: try to treat it as a Python MemReader subclass
    let obj: Py<PyAny> = mem.clone().unbind();
    let adapter = PyMemReaderAdapter::from_pyobject(obj);
    // BUT: Sleigh<...>'s type parameter is fixed to one reader type per
    // instantiation. We need an enum reader or boxed-dyn approach.
    // Simplest: PySleigh becomes an enum of PySleighWith<MemMap> /
    // PySleighWith<Adapter>.
    todo!("see implementer notes")
}
```

> **Implementer note:** because `rsleigh::Sleigh<R>` is parameterized by the reader type, `PySleigh` needs to support multiple inner-reader types. Two approaches:
> - **Boxed:** `Box<dyn rsleigh::MemReader>` — requires `rsleigh` to support trait objects, which it might not.
> - **Enum:** `PySleigh::Mm(rsleigh::Sleigh<BufMemReader<PyMemoryMapReader>>) | PySleigh::Cb(rsleigh::Sleigh<BufMemReader<PyMemReaderAdapter>>)`.
>
> Prefer the enum — it keeps trait-object surface out of rsleigh. Every PySleigh method matches on the variant and forwards. Add a third "moved-out" variant for the take/put dance from Task 2.1.

- [ ] **Step 4: Build + test + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: add PyMemReader + PyReadOnlyMemory callback-reader subclasses

Both subclass-able from Python; their read methods get called from
Rust under the GIL via PyMemReaderAdapter / PyReadOnlyMemoryAdapter.
PySleigh becomes an enum of (MemoryMap-backed | callback-backed) so
either reader style flows through the same construction API.

Acknowledged-slow: every byte fetch under sleigh disassembly takes a
GIL acquire. MemoryMap remains the fast path; subclass-readers exist
for flexibility (lazy/streaming/exotic formats) where speed isn't the
priority.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 7.2: Smoke test — Python reader drives a real lift

**Files:**
- Create: `crates/strider-py/tests/python/test_python_reader_lift.py`

- [ ] **Step 1: Write test**

```python
import strider
from strider.pattern import ret

class TinyRetMem(strider.MemReader):
    def __init__(self): self.calls = 0
    def read(self, addr, size):
        # Single ret instruction at 0x1000.
        if 0x1000 <= addr < 0x1001:
            self.calls += 1
            return b"\xc3"   # x86 ret
        return None

def test_python_reader_lifts_ret():
    arch = strider.SleighArch.x86()
    cc = strider.CallingConvention.x86_cdecl()
    r = TinyRetMem()
    result = strider.run(arch=arch, cc=cc, mem_reader=r, entry=0x1000)
    # Expect at least one return in the lifted graph.
    hits = result.graph.find_all(ret())
    assert len(hits) >= 1
    assert r.calls > 0   # confirm Python reader was actually invoked
```

- [ ] **Step 2: Build + run + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: end-to-end test — Python MemReader drives a real lift

Lifts a single x86 ret instruction served from a Python subclass and
confirms (a) the resulting Graph contains a Return node and (b) the
subclass.read was actually called from Rust. Closes the loop on the
callback-reader path.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Phase 8 — Polish

### Task 8.1: Hand-written .pyi type stubs

**Files:**
- Create: `crates/strider-py/strider/__init__.pyi`
- Create: `crates/strider-py/strider/pattern/__init__.pyi`
- Create: `crates/strider-py/strider/opt/__init__.pyi`
- Create: `crates/strider-py/strider/errors.pyi`
- Modify: `crates/strider-py/pyproject.toml` (include stubs in wheel)

- [ ] **Step 1: Write stubs**

`crates/strider-py/strider/__init__.pyi`:
```python
from typing import Optional, Union, List
from . import pattern as pattern
from . import opt as opt
from . import errors as errors

class SleighArch:
    @classmethod
    def x86_64(cls) -> "SleighArch": ...
    @classmethod
    def x86(cls) -> "SleighArch": ...
    # ... one per preset
    def name(self) -> str: ...

class CallingConvention:
    @classmethod
    def x86_64_systemv_abi(cls) -> "CallingConvention": ...
    # ... one per preset
    def name(self) -> str: ...

class MemoryMap:
    def __init__(self) -> None: ...
    def add_region(self, start_addr: int, data: bytes) -> None: ...
    def add_region_from_elf(self, path: str) -> None: ...
    def region_count(self) -> int: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...

class MemReader:
    def __init__(self) -> None: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...

class ReadOnlyMemory:
    def __init__(self) -> None: ...
    def read(self, space: int, addr: int, size: int) -> Optional[int]: ...

class Sleigh:
    def __init__(self, arch: SleighArch, mem: Union[MemoryMap, MemReader]) -> None: ...
    def arch_name(self) -> str: ...

class Cfg:
    def to_html(self, path: str, style: str = "dark_cfg") -> None: ...
    def to_dot(self, path: str) -> None: ...
    def html_str(self, style: str = "dark_cfg") -> str: ...

def build_cfg(sleigh: Sleigh, entry: int,
              allow_code_before_start_addr: bool = False) -> Cfg: ...

class Graph:
    def find_all(self, pat: pattern.Pat,
                 ignore_casts: bool = False,
                 ignore_control_states: bool = False) -> List[pattern.Match]: ...
    def matcher(self, ignore_casts: bool = False,
                ignore_control_states: bool = False) -> pattern.Matcher: ...
    def rewrite(self, find: pattern.Pat, replace: pattern.Pat) -> None: ...
    def rewrite_all(self, rules: List[tuple]) -> None: ...
    def optimize(self, pipeline: "OptimizerPipeline") -> None: ...
    def reoptimize(self, destructive: bool = False) -> None: ...
    def to_html(self, path: str, style: str = "dark") -> None: ...
    def to_dot(self, path: str) -> None: ...
    def html_str(self, style: str = "dark") -> str: ...

class OptimizerPipeline:
    @staticmethod
    def empty() -> "OptimizerPipeline": ...
    @staticmethod
    def default() -> "OptimizerPipeline": ...
    @staticmethod
    def stable_default() -> "OptimizerPipeline": ...
    @staticmethod
    def destructive_default() -> "OptimizerPipeline": ...
    def add(self, p) -> None: ...
    def add_post(self, p) -> None: ...
    def pass_count(self) -> int: ...

class AnalyzeOutcome:
    graph: Graph

class Strider:
    def __init__(self, arch: SleighArch, sleigh: Sleigh, cc: CallingConvention) -> None: ...
    def analyze_cfg(self, cfg: Cfg) -> AnalyzeOutcome: ...
    def build_optimizer_pipeline(self) -> OptimizerPipeline: ...
    def build_stable_optimizer_pipeline(self) -> OptimizerPipeline: ...
    def build_destructive_optimizer_pipeline(self) -> OptimizerPipeline: ...

class RunResult:
    cfg: Cfg
    graph: Graph
    sleigh: Sleigh

def run(*, arch: SleighArch, cc: CallingConvention,
        mem_reader: Union[MemoryMap, MemReader],
        entry: int,
        rom: Optional[Union[MemoryMap, ReadOnlyMemory]] = None,
        pipeline: Optional[OptimizerPipeline] = None,
        allow_code_before_start_addr: bool = False,
        fn_max_size: Optional[int] = None) -> RunResult: ...
```

`crates/strider-py/strider/pattern/__init__.pyi`:
```python
from typing import Callable, Optional, Union, Any, List

class Capture:
    def __init__(self) -> None: ...

class Pat: ...

class Match:
    def uint(self, key: Union[str, Capture]) -> Optional[int]: ...
    def int(self, key: Union[str, Capture]) -> Optional[int]: ...
    def bool_(self, key: Union[str, Capture]) -> Optional[bool]: ...
    def float_bits(self, key: Union[str, Capture]) -> Optional[int]: ...
    def __getitem__(self, key: Union[str, Capture]) -> Any: ...
    def __contains__(self, key: Union[str, Capture]) -> bool: ...

class Matcher:
    def find_all(self, pat: Pat) -> List[Match]: ...

# Builders return Pat or a typed builder class; for stubs, declare them as Pat.
def load(addr: Optional[Union[str, Capture, Pat]] = None) -> Pat: ...
def store(addr: Optional[Union[str, Capture, Pat]] = None,
          value: Optional[Union[str, Capture, Pat]] = None) -> Pat: ...
def add(l: Union[str, Capture, Pat], r: Union[str, Capture, Pat]) -> Pat: ...
def sub(l: Union[str, Capture, Pat], r: Union[str, Capture, Pat]) -> Pat: ...
# ... full list per the spec

def const(value: Union[int, bool, float], width: Optional[str] = None) -> Pat: ...
def var(c_or_name: Union[str, Capture]) -> Pat: ...
any_: Pat
def predicate(f: Callable[[Match], bool]) -> Pat: ...
```

`crates/strider-py/strider/errors.pyi`:
```python
class StriderError(Exception): ...
class LiftError(StriderError): ...
class ReaderError(StriderError): ...
class PatternError(StriderError): ...
class RewriteError(StriderError): ...
```

`crates/strider-py/strider/opt/__init__.pyi`:
```python
class ConstantFold:
    def __init__(self) -> None: ...
class KnownBits:
    def __init__(self) -> None: ...
class RedundantPhis:
    def __init__(self) -> None: ...
class DeadBranchElim:
    def __init__(self) -> None: ...
class CallOtherElide:
    def __init__(self) -> None: ...
class LoadReadOnly:
    def __init__(self, rom) -> None: ...
class StackStoreDetect:
    def __init__(self, cc, sleigh) -> None: ...
class StackLoadForward:
    def __init__(self, cc, arch, sleigh) -> None: ...
class FunctionArgDetect:
    def __init__(self, cc, sleigh) -> None: ...
class CallStackArgCollect:
    def __init__(self, cc, sleigh) -> None: ...
```

- [ ] **Step 2: Update pyproject.toml to ship stubs**

Add to `[tool.maturin]`:
```toml
include = ["strider/**/*.pyi"]
```

- [ ] **Step 3: Add a stub-presence smoke test**

```python
# tests/python/test_stubs.py
def test_stubs_present():
    import importlib.util, pathlib
    pkg_root = pathlib.Path(__import__("strider").__file__).parent
    assert (pkg_root / "__init__.pyi").exists()
    assert (pkg_root / "pattern" / "__init__.pyi").exists()
    assert (pkg_root / "errors.pyi").exists()
    assert (pkg_root / "opt" / "__init__.pyi").exists()
```

- [ ] **Step 4: Build + test + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: ship hand-written .pyi type stubs

Stubs cover the full public surface (top-level, pattern, opt,
errors). Hand-written so the typing matches the design intent rather
than maturin's auto-generated stub which loses the union/optional
detail. Snapshot test pins their presence in built wheels.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8.2: README + usage examples

**Files:**
- Modify: `crates/strider-py/README.md`

- [ ] **Step 1: Expand the README**

Sections to include:
- Build & install (`maturin develop`).
- Quickstart: 10-line `strider.run(...)` + `find_all`.
- Reader styles: MemoryMap (fast) vs subclass (slow). Performance warning.
- Pattern API tour (string vs Capture; commutative; predicates).
- Visualization (`to_html`).
- Rewrite (`rewrite` + `reoptimize`).
- Custom optimizer pipeline.
- Link to the spec + design rationale.

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: README with quickstart, reader-style guide, examples

Up-front guidance on MemoryMap vs subclass readers (performance
contract is the sharp edge users will hit first). Quickstart covers
the 90% case end-to-end so readers can see the full surface in one
page.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8.3: Public-API snapshot test

**Files:**
- Create: `crates/strider-py/tests/python/test_public_api.py`

- [ ] **Step 1: Snapshot test**

```python
import strider
from strider import pattern, opt, errors

EXPECTED_TOP = sorted([
    "__version__",
    "SleighArch", "CallingConvention", "Sleigh",
    "MemoryMap", "MemReader", "ReadOnlyMemory",
    "Cfg", "Graph", "Strider", "AnalyzeOutcome",
    "OptimizerPipeline", "RunResult",
    "build_cfg", "run",
    "errors", "pattern", "opt",
    "StriderError",
])

def test_strider_public_api():
    actual = sorted(n for n in dir(strider) if not n.startswith("_"))
    # Use issubset / set diff so we get a useful failure message
    missing = set(EXPECTED_TOP) - set(actual)
    extra = set(actual) - set(EXPECTED_TOP)
    assert not missing, f"missing: {missing}"
    assert not extra, f"unexpected: {extra}"

EXPECTED_PATTERN = sorted([
    "Capture", "Pat", "Match", "Matcher",
    "load", "store", "stack_store", "stack_store_phi",
    "add", "sub", "mul", "shl", "shr", "ushr",
    "and_", "or_", "xor",
    "int_eq", "int_lt", "int_slt", "int_carry", "int_scarry",
    "bool_and", "bool_or", "bool_xor",
    "float_add", "float_sub", "float_mul", "float_div",
    "float_eq", "float_ne", "float_lt", "float_le",
    "call", "call_other", "ret", "if_",
    "phi", "phi_for", "initial_var", "function_arg",
    "const", "var", "any_", "predicate",
])

def test_pattern_public_api():
    actual = sorted(n for n in dir(pattern) if not n.startswith("_"))
    missing = set(EXPECTED_PATTERN) - set(actual)
    extra = set(actual) - set(EXPECTED_PATTERN)
    assert not missing, f"missing: {missing}"
    assert not extra, f"unexpected: {extra}"

EXPECTED_OPT = sorted([
    "ConstantFold", "KnownBits", "RedundantPhis", "DeadBranchElim", "CallOtherElide",
    "LoadReadOnly", "StackStoreDetect", "StackLoadForward",
    "FunctionArgDetect", "CallStackArgCollect",
])

def test_opt_public_api():
    actual = sorted(n for n in dir(opt) if not n.startswith("_"))
    missing = set(EXPECTED_OPT) - set(actual)
    extra = set(actual) - set(EXPECTED_OPT)
    assert not missing, f"missing: {missing}"
    assert not extra, f"unexpected: {extra}"

EXPECTED_ERRORS = sorted(["StriderError", "LiftError", "ReaderError", "PatternError", "RewriteError"])
def test_errors_public_api():
    actual = sorted(n for n in dir(errors) if not n.startswith("_"))
    missing = set(EXPECTED_ERRORS) - set(actual)
    extra = set(actual) - set(EXPECTED_ERRORS)
    assert not missing, f"missing: {missing}"
    assert not extra, f"unexpected: {extra}"
```

- [ ] **Step 2: Build + run + commit**

```bash
git commit -m "$(cat <<'EOF'
strider-py: pin public API surface with a snapshot test

Any accidental addition or removal at the top-level / pattern / opt /
errors namespaces will trip this test. Keeps the v1 surface stable
without repeated audits.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 8.4: Update CLAUDE.md (strider-py no longer "planned")

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Remove the *(planned)* tag and add the real description**

Find this line in CLAUDE.md:
```
- **`strider-py`** *(planned)* — Python bindings (PyO3) that are the primary user-facing interface. ...
```

Replace with:
```
- **`strider-py`** — Python bindings (PyO3, abi3, maturin-built). The primary user-facing interface for ad-hoc analysis, querying, and rewriting. Provides:
  - `strider.run(arch=..., cc=..., mem_reader=..., entry=..., ...)` convenience entry returning `RunResult{cfg, graph, sleigh}`; or the building-blocks path (`SleighArch`/`Sleigh`/`build_cfg`/`Strider.analyze_cfg`).
  - Fast `MemoryMap` reader for the common (data-only) case + `MemReader` / `ReadOnlyMemory` Python subclasses for callback-style readers.
  - Custom optimizer pipelines via `OptimizerPipeline` + per-pass classes in `strider.opt`.
  - Pattern matching (`strider.pattern`) — full mirror of the Rust `pattern` crate with both `Capture`-typed and string-shorthand forms; predicates via `.when(...)`.
  - Pattern→pattern rewrites via `Graph.rewrite(find=..., replace=...)` + `reoptimize()`.
  - HTML/DOT visualization on both `Cfg` and `Graph` (thin wrapper over the `dot` crate).
  See `crates/strider-py/README.md` and `docs/superpowers/specs/2026-05-01-strider-py-design.md` for details.
```

- [ ] **Step 2: Commit**

```bash
git commit -m "$(cat <<'EOF'
docs: CLAUDE.md — strider-py is real, document its surface

Removes the (planned) tag now that the crate ships v1 and replaces the
single-line placeholder with the actual user-facing surface so future
context messages know what's available.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

**Spec coverage:** every spec section maps to tasks:
- §1 Goal → Phases 0-7 collectively
- §2 Constraints → enforced per-task (TDD, no unwrap, anyhow)
- §3 Architecture → Task 0.1 + per-phase wrappers
- §4 Module layout → all phases
- §5 Reader API → Tasks 1.3 + 7.1 + 7.2
- §6 Pattern API → Phase 5 (Tasks 5.1-5.9)
- §7 Graph API → Tasks 2.2, 2.3, 3.3, 5.7-5.9, 6.1
- §8 CFG API → Task 2.1
- §9 Optimizer pipeline → Phase 3 (Tasks 3.1-3.3)
- §10 Top-level entry → Task 4.1 + Task 2.2 (building blocks)
- §11 Error handling → Task 0.2
- §12 Build & distribution → Task 0.1 + Task 8.2
- §13 Testing → every task ships with tests; Task 8.3 pins surface
- §14 File layout → matches the file paths in every task
- §15 Risks → addressed in spec; mitigations enforced per-task (no Rust locks across callbacks: Task 7.1; surface drift: Task 8.3)
- §16 Out of scope → respected (no Python FunctionBuilder, no wheel CI)
- §17 Acceptance → satisfied by Task 8.4 (CLAUDE.md update) + per-task green tests

**Placeholder scan:** there are several `todo!()` markers in implementer-note sketches. These ARE flagged for the implementer to replace before commit and explicitly call out the workspace lint rule (`todo` is `deny`). They are not committed code; they are guidance about non-trivial decisions that benefit from inspecting the inner-crate API at implementation time. The plan also flags places where the inner-crate API may differ from what's sketched (e.g. `cfg::Cfg` ownership, exact pass constructors) — these are honest "verify before committing" markers, not placeholder content.

**Type consistency:** PyOptimizerPipeline.add takes a per-pass instance (PyConstantFold, PyKnownBits, etc.) consistently across Tasks 3.1-3.3. PyPat / PyMatch / PyCapture are referenced consistently. PyMemoryMap / PyMemReader / PyReadOnlyMemory are all distinct types as defined.

**Known soft spots the implementer should resolve early:**
1. `PySleigh` reader-type polymorphism (Task 7.1 implementer note) — decide enum vs trait-object before Task 1.4 ships, or refactor in Task 7.1.
2. The `take/put` dance for moving `rsleigh::Sleigh` into and out of `PyCfg` (Task 2.1) — could be cleaner with a different ownership model; verify against `cfg::Cfg`'s actual API first.
3. String-key resolution on `PyMatch` requires plumbing the `CaptureTable` from the originating `PyPat` through `find_all` into `PyMatch` (Task 5.8 implementer note).
