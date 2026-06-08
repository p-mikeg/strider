# BufferReader — replace Python `MemoryMap` with a simple single-region reader

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the public Python `MemoryMap` class with a `BufferReader(base_addr, data)` single-region reader, keeping the multi-region store internal for the ELF path, while leaving the Rust reader traits untouched.

**Architecture:** No new Rust traits. The two existing reader traits stay exactly as-is: `rsleigh::MemReader` (code/instruction fetch, partial `Result<usize>`) and `read_only_memory::ReadOnlyMemory` (rom/constant-fold, fill-or-error `Result<()>`). All change is in `strider-py`: the `PyMemoryMap` pyclass is renamed to `PyBufferReader` with a single-region constructor; its existing dual-trait snapshot (`PyMemoryMapReader`) and `MemInput`/`AnyMemReader` plumbing are reused verbatim (only internal variant names change), so the consumers (`run.rs`, `strider_cls.rs`) compile unchanged. The ELF loader keeps a multi-region instance via a new internal `from_regions` constructor. The two callback ABCs (`MemReader`, `ReadOnlyMemory`) are kept for explicitness.

**Tech Stack:** Rust + PyO3 0.22 (abi3), maturin, pytest, uv.

**Out of scope (separate follow-up):** the broader `strider-py` line-count simplification (`pattern.rs`, `opt.rs` macros, etc.). This plan only touches the memory-reader surface.

---

## Decisions (locked during brainstorming)

- **No new trait.** Code = `rsleigh::MemReader` (exists), rom = `ReadOnlyMemory` (exists). Kept separate.
- **Remove public `MemoryMap`.** Replace with `BufferReader(base_addr, data)` (single region). Multi-region stays internal (ELF only).
- **Keep both callback ABCs** (`MemReader`, `ReadOnlyMemory`) — the user wants explicit base classes.
- **`BufferReader` serves both roles** (`mem=` and `rom=`) by reusing the existing `PyMemoryMapReader` snapshot, which already implements both `rsleigh::MemReader` and `ReadOnlyMemory`.
- `_LoadedElf.memory_map()` → renamed `_LoadedElf.reader()` (returns a multi-region `BufferReader`); `ElfStrider.memory_map()` → `ElfStrider.reader()`.

## File map

- `crates/strider-py/src/reader.rs` — rename `PyMemoryMap` → `PyBufferReader`; single-region `#[new]`; add internal `from_regions`; drop `add_region`/`region_count`/`set_endianness` pymethods; rename internal helpers + `MemInput`/`AnyMemReader` variants; update `load_elf`/`add_elf`/`_LoadedElf`; update `register`.
- `crates/strider-py/strider/_api.py` — `_rebuild_strider` and the `memory_map()` facade → `reader()`.
- `crates/strider-py/strider/__init__.pyi` — replace `class MemoryMap` with `class BufferReader`; update type-hint mentions; `ElfStrider.memory_map` → `reader`.
- `crates/strider-py/tests/python/test_buffer_reader.py` — **new**, replaces `test_memory_map.py`.
- `crates/strider-py/tests/python/test_memory_map.py` — **deleted**.
- `crates/strider-py/tests/python/test_compact.py`, `test_custom_cc.py`, `test_callback_reader.py`, `test_cross_arch_e2e.py` — migrate `MemoryMap` → `BufferReader`, `.memory_map()` → `.reader()`.

---

## Task 0: Branch setup

**Files:** none (git only)

- [ ] **Step 1: Create and push a feature branch off develop**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
git checkout develop && git pull --ff-only origin develop
git checkout -b feature/buffer-reader
git push -u origin feature/buffer-reader
```
Expected: new branch `feature/buffer-reader` tracking origin.

---

## Task 1: Rust — replace `PyMemoryMap` with `PyBufferReader`

**Files:**
- Modify: `crates/strider-py/src/reader.rs`

This task only edits `reader.rs`. The `MemInput` / `AnyMemReader` *type names* are preserved (only their internal variant `Map` → `Buffer` is renamed), so `run.rs` and `strider_cls.rs` continue to compile without edits.

- [ ] **Step 1: Rename the inner state struct and its doc**

In `reader.rs`, rename `PyMemoryMapInner` → `PyBufferReaderInner` (struct + the two field-doc comments stay). Replace the struct definition (currently around lines 33–43):

```rust
/// Plain-data inner state shared by every clone of a `PyBufferReader`.
/// Held behind a single `Rc<RefCell<...>>` on the surface pyclass.
pub(crate) struct PyBufferReaderInner {
    pub(crate) regions: Vec<MemRegion>,
    /// Lazily-rebuilt lookup table; cleared whenever `regions` changes.
    pub(crate) table: Option<Arc<MemRegionsLookupTable>>,
    /// Byte order recorded for the ELF-backed path (read by
    /// `_LoadedElf.endianness()`).  The reader fills caller buffers with
    /// RAW bytes regardless — integer decode happens in the optimizer per
    /// the run's `SleighArch` endianness — so this never affects reads.
    pub(crate) endianness: strider_target::Endianness,
}
```

- [ ] **Step 2: Rename the surface pyclass to `BufferReader` with a single-region constructor**

Replace the `PyMemoryMap` struct + its inherent impl + `#[pymethods]` block (currently lines 45–175) with:

```rust
/// Single-region raw-byte reader.  Implements `rsleigh::MemReader`
/// (instruction fetch) and `read_only_memory::ReadOnlyMemory` (rodata
/// constant folding) indirectly through the `PyBufferReader::reader_view`
/// snapshot, so the same `BufferReader` can serve both the `mem=` and
/// `rom=` arguments of `strider.run` / `strider.strider` / `strider.Lifter`.
///
/// For an ELF, prefer `strider.load_elf(path)` (yields an `ElfStrider`),
/// which builds a multi-region reader from the ELF sections and adds
/// symbol lookups.
///
/// `unsendable`: only ever touched from the GIL-holding Python thread.
/// Downstream consumers that need a `Send + Sync` reader take a
/// `PyBufferReaderView` snapshot (see `reader_view`).
#[pyclass(name = "BufferReader", module = "strider", unsendable)]
#[derive(Clone)]
pub struct PyBufferReader {
    /// `Rc` so a clone shares state with the original (the ELF path holds
    /// one handle and `add_elf` mutates the shared regions in place).
    pub(crate) inner: Rc<RefCell<PyBufferReaderInner>>,
}

impl PyBufferReader {
    fn rebuild_table(&self) -> Arc<MemRegionsLookupTable> {
        let mut inner = self.inner.borrow_mut();
        let t = Arc::new(MemRegionsLookupTable::new(inner.regions.clone()));
        inner.table = Some(Arc::clone(&t));
        t
    }

    /// Current lookup table, built on demand (or returned from cache).
    pub(crate) fn lookup_table(&self) -> Arc<MemRegionsLookupTable> {
        if let Some(t) = self.inner.borrow().table.as_ref() {
            return Arc::clone(t);
        }
        self.rebuild_table()
    }

    /// Mint a `Send + Sync` snapshot implementing both reader traits.
    pub(crate) fn reader_view(&self) -> PyBufferReaderView {
        PyBufferReaderView { table: self.lookup_table() }
    }

    /// Internal multi-region constructor used by the ELF loader.
    pub(crate) fn from_regions(
        regions: Vec<MemRegion>,
        endianness: strider_target::Endianness,
    ) -> Self {
        Self {
            inner: Rc::new(RefCell::new(PyBufferReaderInner {
                regions,
                table: None,
                endianness,
            })),
        }
    }
}

#[pymethods]
impl PyBufferReader {
    /// Create a reader serving `data` mapped at `base_addr`.
    ///
    /// # Errors
    /// Raises `StriderError` if `base_addr + data.len()` overflows `u64`.
    #[new]
    fn new(base_addr: u64, data: Vec<u8>) -> PyResult<Self> {
        let region = MemRegion::new(base_addr, data).map_err(into_strider_err)?;
        Ok(Self::from_regions(vec![region], strider_target::Endianness::Little))
    }

    /// Read up to `size` bytes at `addr`.  Returns the bytes (possibly
    /// fewer than `size` near the region edge) or `None` when `addr` is
    /// unmapped.
    fn read<'py>(
        &self,
        py: Python<'py>,
        addr: u64,
        size: usize,
    ) -> PyResult<Option<Bound<'py, PyBytes>>> {
        let table = self.lookup_table();
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
```

- [ ] **Step 3: Point `_LoadedElf` at `PyBufferReader` and rename `memory_map()` → `reader()`**

In `PyLoadedElf` (currently line 215), change the field type:

```rust
    /// Raw-region reader assembled from the ELF sections.
    mem: PyBufferReader,
```

Replace the `memory_map()` pymethod (currently lines 247–255) with:

```rust
    /// The multi-region `BufferReader` assembled from this ELF's sections.
    /// Hand it to `strider.run(mem=…, rom=…)`; `add_elf` mutations are
    /// visible through the same handle.
    fn reader(&self) -> PyBufferReader {
        self.mem.clone()
    }
```

In `PyLoadedElf::read` (currently line 326) the body `self.mem.read(py, addr, size)` is unchanged.

- [ ] **Step 4: Build the ELF reader via `from_regions`**

Replace the body of `load_elf` that builds the map (currently lines 372–379) with:

```rust
    let regions = elf_to_regions(&obj, apply_relocations)?;
    let mem = PyBufferReader::from_regions(regions, endianness);
```

In `add_elf` (currently lines 344–354) the in-place mutation of `self.mem.inner` is unchanged — it still does:

```rust
        let regions = elf_to_regions(&obj, apply_relocations)?;
        {
            let mut inner = self.mem.inner.borrow_mut();
            inner.regions.extend(regions);
            inner.table = None;
        }
```

- [ ] **Step 5: Rename the snapshot type and update both trait impls**

Rename `PyMemoryMapReader` → `PyBufferReaderView` (currently the struct at line 599 and the two `impl` blocks at 603 and 621). Only the struct name changes; the field and bodies are identical:

```rust
/// `Send + Sync` snapshot over a `PyBufferReader`'s region table.
/// Implements both `rsleigh::MemReader` (partial fetch) and
/// `read_only_memory::ReadOnlyMemory` (fill-or-error rodata) so one
/// reader can drive both the Sleigh path and the `LoadReadOnly` pass.
pub struct PyBufferReaderView {
    pub table: Arc<MemRegionsLookupTable>,
}

impl rsleigh::MemReader for PyBufferReaderView {
    type Err = strider_reader::MemReadError;
    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        self.table.read(addr.off, out_buf).ok_or_else(|| {
            strider_reader::MemReadError::from(anyhow::anyhow!("address {:#x} is not mapped", addr.off))
        })
    }
}

impl ReadOnlyMemory for PyBufferReaderView {
    fn read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        let want = buf.len();
        let got = self
            .table
            .read(addr, buf)
            .ok_or_else(|| anyhow::anyhow!("address {addr:#x} is not mapped"))?;
        if got != want {
            anyhow::bail!("read at {addr:#x} spans past mapped memory: got {got} of {want} bytes");
        }
        Ok(())
    }
}
```

- [ ] **Step 6: Rename the `AnyMemReader::Map` variant → `Buffer`**

In the `AnyMemReader` enum (currently lines 570–584) rename the `Map` variant and its view type:

```rust
pub enum AnyMemReader {
    Buffer(PyBufferReaderView),
    Cb(PyMemReaderAdapter),
}

impl rsleigh::MemReader for AnyMemReader {
    type Err = strider_reader::MemReadError;
    fn read(&self, addr: rsleigh::VnAddr, out_buf: &mut [u8]) -> Result<usize, Self::Err> {
        match self {
            AnyMemReader::Buffer(m) => rsleigh::MemReader::read(m, addr, out_buf),
            AnyMemReader::Cb(c) => rsleigh::MemReader::read(c, addr, out_buf),
        }
    }
}
```

- [ ] **Step 7: Rename the `MemInput::Map` variant → `Buffer` and update its three consumers**

In `MemInput` (currently lines 651–709) rename the variant and update `extract_bound` / `into_box` / `into_any` / `clone_one`:

```rust
pub enum MemInput {
    Buffer(PyBufferReader),
    Cb(Py<PyAny>),
}

impl<'py> FromPyObject<'py> for MemInput {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(m) = ob.extract::<PyBufferReader>() {
            return Ok(MemInput::Buffer(m));
        }
        if ob.hasattr("read")? {
            return Ok(MemInput::Cb(ob.clone().unbind()));
        }
        Err(pyo3::exceptions::PyTypeError::new_err(
            "expected a BufferReader or an object with a `read(...)` method",
        ))
    }
}

impl MemInput {
    pub fn into_box(self) -> Box<dyn ReadOnlyMemory> {
        match self {
            MemInput::Buffer(m) => Box::new(m.reader_view()),
            MemInput::Cb(obj) => Box::new(PyReadOnlyMemoryAdapter { py_obj: obj }),
        }
    }

    pub fn into_any(self) -> AnyMemReader {
        match self {
            MemInput::Buffer(m) => AnyMemReader::Buffer(m.reader_view()),
            MemInput::Cb(obj) => AnyMemReader::Cb(PyMemReaderAdapter { py_obj: obj }),
        }
    }

    pub fn clone_one(&self) -> PyResult<MemInput> {
        match self {
            MemInput::Buffer(m) => Ok(MemInput::Buffer(m.clone())),
            MemInput::Cb(obj) => Python::with_gil(|py| Ok(MemInput::Cb(obj.clone_ref(py)))),
        }
    }
}
```

Keep the existing doc-comments on these methods (they reference `Box<dyn ReadOnlyMemory>` / the `AnyMemReader` roles and remain accurate); only the `Map`→`Buffer` arm names change.

- [ ] **Step 8: Update the module-level doc + `register`**

Update the file header doc (lines 1–13): replace `PyMemoryMap` with `PyBufferReader` and `MemReader (subclass-able…)` text stays. Update `register` (currently 711–723):

```rust
pub fn register(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyBufferReader>()?;
    // `PyLoadedElf` (`_LoadedElf`) stays unregistered — internal ELF
    // parse/symbol backend reached only via the `load_elf` pyfunction,
    // which `_api.py` wraps inside an `ElfStrider`.
    m.add_class::<PyMemReader>()?;
    m.add_class::<PyReadOnlyMemory>()?;
    m.add_function(wrap_pyfunction!(load_elf, m)?)?;
    Ok(())
}
```

- [ ] **Step 9: Update the `PyMemReader` ABC doc that references `MemoryMap`**

In the `PyMemReader` doc (currently lines 392–393) replace "Use `MemoryMap` for the in-process fast path" with "Use `BufferReader` for the in-process fast path".

- [ ] **Step 10: Verify the crate compiles**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
cargo build -p strider-py 2>&1 | tail -20
```
Expected: `Finished` with no errors. (If `run.rs`/`strider_cls.rs` error on a missing `Map` variant, you missed a rename in Step 6/7 — they only reference the *type names* `MemInput`/`AnyMemReader`, which are unchanged.)

- [ ] **Step 11: Commit**

```bash
git add crates/strider-py/src/reader.rs
git commit -m "feat(strider-py): replace MemoryMap with single-region BufferReader"
git push origin feature/buffer-reader
```

---

## Task 2: New `test_buffer_reader.py` (replaces `test_memory_map.py`)

**Files:**
- Create: `crates/strider-py/tests/python/test_buffer_reader.py`
- Delete: `crates/strider-py/tests/python/test_memory_map.py`

- [ ] **Step 1: Write the new test file**

Create `crates/strider-py/tests/python/test_buffer_reader.py`:

```python
import pytest
import strider


def test_read_within_region():
    r = strider.BufferReader(0x1000, b"\x01\x02\x03\x04")
    assert r.read(0x1000, 4) == b"\x01\x02\x03\x04"
    assert r.read(0x1002, 2) == b"\x03\x04"


def test_read_unmapped_returns_none():
    r = strider.BufferReader(0x1000, b"\x00\x01\x02\x03")
    assert r.read(0x2000, 4) is None


def test_read_past_region_edge_truncates():
    r = strider.BufferReader(0x1000, b"\x00\x01\x02\x03")
    # Asking for more than is mapped returns only the mapped bytes.
    assert r.read(0x1002, 8) == b"\x02\x03"


def test_base_plus_len_overflow_rejected():
    with pytest.raises(strider.errors.StriderError):
        strider.BufferReader(0xFFFFFFFFFFFFFFFE, b"\x00\x00\x00\x00")
```

- [ ] **Step 2: Delete the old test**

```bash
git rm crates/strider-py/tests/python/test_memory_map.py
```

- [ ] **Step 3: Rebuild the extension and run the new test**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
uv run maturin develop 2>&1 | tail -3
uv run pytest crates/strider-py/tests/python/test_buffer_reader.py -q 2>&1 | tail -15
```
Expected: `Installed strider-...` then `4 passed`.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/tests/python/test_buffer_reader.py
git commit -m "test(strider-py): BufferReader single-region read/overflow tests"
git push origin feature/buffer-reader
```

---

## Task 3: Migrate `test_compact.py` and `test_custom_cc.py`

**Files:**
- Modify: `crates/strider-py/tests/python/test_compact.py`
- Modify: `crates/strider-py/tests/python/test_custom_cc.py`

- [ ] **Step 1: Update `test_compact.py`**

Change the import line 4 from `from strider import CallingConvention, MemoryMap, SleighArch` to `from strider import CallingConvention, BufferReader, SleighArch`. Replace the two `mem = MemoryMap(); mem.add_region(0x1000, _trivial_function_bytes())` blocks (lines 21–22 and 36–37) each with a single line:

```python
    mem = BufferReader(0x1000, _trivial_function_bytes())
```

- [ ] **Step 2: Update `test_custom_cc.py`**

Replace `_mem_with_func_bytes` (lines 23–28):

```python
def _mem_with_func_bytes() -> tuple[strider.BufferReader, int]:
    """Build a BufferReader with a tiny x86_64 function: `mov eax, 1; ret`."""
    # mov eax, 1 (b8 01 00 00 00) ; ret (c3)
    mem = strider.BufferReader(0x1000, b"\xb8\x01\x00\x00\x00\xc3")
    return mem, 0x1000
```

Replace the `test_custom_cc_rejects_invariant_violation_lr_not_in_callee_saved` mem-build (lines 90–91):

```python
    mem = strider.BufferReader(0x1000, b"\x00\x00\x00\xd6")  # arbitrary 4 bytes
```

- [ ] **Step 3: Run the migrated tests**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
uv run pytest crates/strider-py/tests/python/test_compact.py crates/strider-py/tests/python/test_custom_cc.py -q 2>&1 | tail -15
```
Expected: all pass (no rebuild needed — Rust unchanged since Task 1).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/tests/python/test_compact.py crates/strider-py/tests/python/test_custom_cc.py
git commit -m "test(strider-py): migrate compact/custom_cc tests to BufferReader"
git push origin feature/buffer-reader
```

---

## Task 4: Migrate `test_callback_reader.py` and `test_cross_arch_e2e.py`

**Files:**
- Modify: `crates/strider-py/tests/python/test_callback_reader.py`
- Modify: `crates/strider-py/tests/python/test_cross_arch_e2e.py`

- [ ] **Step 1: Update `test_callback_reader.py` imports and type hints**

Change line 21 from `from strider import MemReader, MemoryMap, ReadOnlyMemory, SleighArch, CallingConvention` to:

```python
from strider import MemReader, BufferReader, ReadOnlyMemory, SleighArch, CallingConvention
```

In `CountingReader` (lines 31–43), change the wrapped type hint and docstring:

```python
class CountingReader(MemReader):
    """Wraps a BufferReader, but counts every Python-side read."""

    def __init__(self, inner: BufferReader):
        super().__init__()
        self.inner = inner
        self.calls = 0
        self.lock = threading.Lock()

    def read(self, addr: int, size: int):
        with self.lock:
            self.calls += 1
        return self.inner.read(addr, size)
```

Change `make_counting_reader` (lines 46–47) signature hint `inner: MemoryMap` → `inner: BufferReader`.

Replace each `strider.load_elf(str(x86_memory_elf)).memory_map()` (lines 57, 82, 147) and `strider.load_elf(...).memory_map()` with `.reader()`:

```python
    inner = strider.load_elf(str(x86_memory_elf)).reader()
```

- [ ] **Step 2: Update the `test_cross_arch_e2e.py` comment**

Line 5 mentions `ELF load → MemoryMap →`. Change it to `ELF load → BufferReader →`. (Grep first to confirm there are no functional `MemoryMap` uses:)

```bash
grep -n "MemoryMap\|memory_map" crates/strider-py/tests/python/test_cross_arch_e2e.py
```
If any functional `.memory_map()` call appears, replace it with `.reader()`; otherwise only the comment changes.

- [ ] **Step 3: Run the migrated tests**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
uv run pytest crates/strider-py/tests/python/test_callback_reader.py crates/strider-py/tests/python/test_cross_arch_e2e.py -q 2>&1 | tail -20
```
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/strider-py/tests/python/test_callback_reader.py crates/strider-py/tests/python/test_cross_arch_e2e.py
git commit -m "test(strider-py): migrate callback/cross-arch tests to BufferReader"
git push origin feature/buffer-reader
```

---

## Task 5: Update the Python API wrapper (`_api.py`)

**Files:**
- Modify: `crates/strider-py/strider/_api.py`

- [ ] **Step 1: Update `_rebuild_strider`**

Replace the body (currently lines 291–296):

```python
        self._strider = strider(
            self._arch,
            self._cc,
            self._elf.reader(),
            rom=self._elf.reader(),
        )
```

- [ ] **Step 2: Rename the `memory_map()` facade to `reader()`**

Replace the `memory_map` method (currently lines 360–366):

```python
    def reader(self) -> object:
        """The multi-region `BufferReader` assembled from the ELF's loaded
        sections — the low-level code reader you can hand to `strider.run`,
        `strider.strider`, `strider.Lifter`, or `strider.Sleigh` when
        dropping below the high-level `analyze` facade.  A `BufferReader`
        clone shares region state with this handle."""
        return self._elf.reader()
```

- [ ] **Step 3: Update the module docstring reference**

`_api.py` line 3 lists `(MemoryMap, Sleigh, Strider, ...)` as low-level blocks. Change `MemoryMap` → `BufferReader`.

- [ ] **Step 4: Grep for any remaining `memory_map` / `MemoryMap` in the package**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
grep -rn "memory_map\|MemoryMap" crates/strider-py/strider/*.py
```
Expected: no hits (all migrated). Fix any stragglers.

- [ ] **Step 5: Run the ELF-backed Python tests**

Run:
```bash
uv run pytest crates/strider-py/tests/python/ -q -k "elf or program or analyze or cross_arch" 2>&1 | tail -20
```
Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/strider-py/strider/_api.py
git commit -m "refactor(strider-py): ElfStrider.memory_map -> reader (BufferReader)"
git push origin feature/buffer-reader
```

---

## Task 6: Update the type stubs (`__init__.pyi`)

**Files:**
- Modify: `crates/strider-py/strider/__init__.pyi`

- [ ] **Step 1: Replace the `MemoryMap` class stub with `BufferReader`**

Replace the `class MemoryMap` block (currently lines 119–133) with:

```python
class BufferReader:
    """Single-region raw-byte reader for non-ELF / firmware-blob cases.
    Serves both the sleigh-fetch (`mem=`) and ReadOnlyMemory (`rom=`)
    roles, so one `BufferReader` can be passed as either argument to
    `strider.run` / `strider.strider` / `strider.Lifter` / `strider.Sleigh`.

    For an ELF, prefer `strider.load_elf(path)` → `ElfStrider`, which
    wires a multi-region reader up automatically and adds symbol/
    entry-point lookups.
    """
    def __init__(self, base_addr: int, data: bytes) -> None: ...
    def read(self, addr: int, size: int) -> Optional[bytes]: ...
```

- [ ] **Step 2: Update the `MemReader` ABC docstring reference**

In the `class MemReader` stub (currently line 138) change "prefer `MemoryMap` for in-process bulk data" → "prefer `BufferReader` for in-process bulk data".

- [ ] **Step 3: Update the remaining type-hint mentions**

Replace every remaining `MemoryMap` in the stub (the `run`/`strider`/`Lifter`/`Sleigh` signatures and the `mem`/`rom` comments at lines ~247, ~427, ~429, ~439, ~454) with `BufferReader`:

```bash
cd /mnt/c/Users/mikeg/Documents/strider
sed -i 's/MemoryMap/BufferReader/g' crates/strider-py/strider/__init__.pyi
```

Then change the `ElfStrider.memory_map` stub (currently lines 483–484) — rename the method:

```python
    def reader(self) -> BufferReader:
        """The multi-region `BufferReader` assembled from the ELF's loaded
        sections — the low-level reader for `strider.run` / `strider.strider`."""
```

- [ ] **Step 4: Confirm no `MemoryMap` remains in the stub**

```bash
grep -n "MemoryMap\|memory_map" crates/strider-py/strider/__init__.pyi
```
Expected: no hits.

- [ ] **Step 5: Commit**

```bash
git add crates/strider-py/strider/__init__.pyi
git commit -m "docs(strider-py): stub MemoryMap -> BufferReader"
git push origin feature/buffer-reader
```

---

## Task 7: Full verification gate

**Files:** none (verification only)

- [ ] **Step 1: Rebuild the extension**

Run:
```bash
cd /mnt/c/Users/mikeg/Documents/strider
uv run maturin develop 2>&1 | tail -3
```
Expected: `Installed strider-...`.

- [ ] **Step 2: Full Python test suite**

Run:
```bash
uv run pytest -q 2>&1 | tail -15
```
Expected: all pass (≈799, plus the 4 new `test_buffer_reader.py` minus the 5 deleted `test_memory_map.py` cases — confirm the total moved by the expected delta and there are **0 failures**).

- [ ] **Step 3: Workspace Rust gate**

Run:
```bash
cargo test -p strider-py 2>&1 | tail -15
cargo clippy -p strider-py 2>&1 | tail -15
```
Expected: tests pass; clippy clean (no new warnings).

- [ ] **Step 4: Stub-generation sanity (the stubs are hand-maintained but the gatherer must still build)**

Run:
```bash
cargo build -p strider-py --examples 2>&1 | tail -5
```
Expected: `Finished` (the `stub_gen` example compiles).

- [ ] **Step 5: Confirm no `MemoryMap` survives anywhere in strider-py**

Run:
```bash
grep -rn "MemoryMap\|memory_map\|PyMemoryMap" crates/strider-py/src crates/strider-py/strider crates/strider-py/tests | grep -v "\.pyc"
```
Expected: no hits.

- [ ] **Step 6: Final commit (if any verification fixups were made) and push**

```bash
git add -A
git commit -m "chore(strider-py): verification fixups for BufferReader" || echo "nothing to commit"
git push origin feature/buffer-reader
```

---

## Self-review checklist (run before execution)

1. **Spec coverage:** remove public `MemoryMap` (Task 1, 6), add `BufferReader` (Task 1), serves both roles via reused snapshot (Task 1 Step 5), keep both ABCs (untouched), ELF internal multi-region (Task 1 Steps 3–4), no new Rust trait (nothing in `read-only-memory`/`strider-reader` touched). ✓
2. **Type consistency:** `PyBufferReader` / `PyBufferReaderInner` / `PyBufferReaderView` used consistently; `MemInput::Buffer` / `AnyMemReader::Buffer` paired across Steps 6–7; `reader()` used in `_api.py` (Task 5) matches the Rust pymethod name (Task 1 Step 3) and the stub (Task 6 Step 3). ✓
3. **No placeholders:** every code step shows full code; every command has expected output. ✓
