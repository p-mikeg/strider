# strider-py PyO3 Generalization Audit

## Summary

The strider-py bindings (5.5k LOC) exhibit significant boilerplate patterns that _could_ be partially unified via proc-macros or trait abstractions, but the Rust/PyO3 ecosystem constraints are limiting. This audit identifies 5 high-ROI opportunities and 5 patterns that are genuinely single-use or load-bearing-distinct.

## Findings

### 1. Wrapper-Class Boilerplate: Presets + Frozen Classes (Arch + CC)

**Pattern:** `arch.rs` (88 LOC) and `cc.rs` (214 LOC) are structurally identical: ~16–18 `#[classmethod]`s, each `Self { inner: T::method(), preset_name: "..." }`.

**File citations:**
- `arch.rs:7–84`: 18 preset classmethods on `PySleighArch`
- `cc.rs:15–191`: 22 preset classmethods on `PyCallingConvention`

**Shared shape:**
```rust
#[pyclass(name = "X", module = "strider", frozen)]
pub struct PyX { inner: T, preset_name: &'static str }
#[classmethod]
fn preset_name(_cls: &Bound<PyType>) -> Self {
    Self { inner: T::preset_name(), preset_name: "preset_name" }
}
```

**Merge proposal:**
A proc-macro `#[pyo3_preset_wrapper(T, module)]` could eliminate the repetition:
```rust
#[pyo3_preset_wrapper(target::SleighArch, "strider")]
pub struct PySleighArch;
// Macro generates classmethods for every `T::*()` classmethod
```

**Difficulty:** Moderate — requires introspection of the wrapped type's static methods.
**LOC delta:** -150 lines (eliminate ~18 × 8 LOC methods per wrapper).
**Migration risk:** None — Python API unchanged (classmethods remain identical).

---

### 2. Pattern-Builder Finaliser Macro: 18 Per-Class Repetitions

**Pattern:** `pattern.rs:47–76` defines `pat_builder_finalise!(BuilderTy)` to inject four identical methods (`.capture()`, `.cap()`, `.when()`, `.into_pat()`) into every pattern builder class.

**File citations:**
- `pattern.rs:47–76`: macro definition
- `pattern.rs:686–1890`: 18 macro invocations (`pat_builder_finalise!(PyPhiPat);`, etc.)

**Boilerplate coverage:**
The macro is _already_ minimizing repetition here — each builder would otherwise repeat ~20 LOC of finalisers. The macro itself is working as designed.

**Assessment:** Already optimized via existing macro. No further abstraction needed.

---

### 3. Error Translation: Typed Downcast Dispatch (into_strider_err + into_lift_err)

**Pattern:** `errors.rs:38–79` contains two inline downcast chains:
- `into_strider_err`: checks `e.downcast_ref::<UnresolvedIndirectBranch>()`, then `e.downcast_ref::<ir::error::UnknownCallOtherError>()`, then string-match heuristic for lift.
- `into_lift_err`: checks `UnknownCallOtherError`, then unconditionally routes to `LiftError`.

**File citations:**
- `errors.rs:38–69`: `into_strider_err` (30 LOC)
- `errors.rs:71–79`: `into_lift_err` (9 LOC)
- `errors.rs:87–98`: `plain_converter!` macro (12 LOC)

**Dispatch table size:** 2 typed errors (UnresolvedIndirectBranch, UnknownCallOtherError) + 1 string-match heuristic.

**Merge proposal:**
Define a `register_typed_error_mapping!` macro to avoid repetition across converters if new typed errors are added. Current usage is sparse (only 2 types), so the upside is limited.

```rust
macro_rules! register_error_mapping {
    ($($ty:ty => $err_class:ident),+) => { /* auto-generate dispatch */ }
}
```

**Difficulty:** Trivial if needed — but with only 2 typed errors today, the manual dispatch is arguably more readable.
**LOC delta:** -5 lines if macro; +3 lines if new types are added later.
**Migration risk:** None — error conversion boundary is internal.

**Assessment:** Current code is acceptable; revisit if >5 typed errors emerge.

---

### 4. Reader Wrapper Proliferation: 6+ Strata

**Wrappers identified:**
1. `PyMemoryMap` (data-only fast path, holds Arc<RwLock<Vec<MemRegion>>>)
2. `PyMemReader` (callback ABC, subclass-able from Python)
3. `PyMemReaderAdapter` (internal, holds Py<PyAny>, impl rsleigh::MemReader)
4. `PyReadOnlyMemory` (callback ABC for optimizer ROM)
5. `PyReadOnlyMemoryAdapter` (internal, holds Py<PyAny>, impl opt::ReadOnlyMemory)
6. `PyMemoryMapReader` (internal, derived from PyMemoryMap, impl rsleigh::MemReader)

**File citations:**
- `reader.rs:90–450`: PyMemoryMap
- `reader.rs:459–484`: PyMemReader (ABC)
- `reader.rs:488–545`: PyMemReaderAdapter
- `reader.rs:547–650`: PyReadOnlyMemory (ABC)
- `reader.rs:567–650`: PyReadOnlyMemoryAdapter
- `reader.rs:652–725`: PyMemoryMapReader

**Assessment — Load-bearing distinct?**
- **PyMemoryMap:** Yes — fast path, holds owned data, cheap to clone.
- **PyMemReader + PyMemReaderAdapter:** Yes — ABC + adapter boundary is necessary for Python subclassing.
- **PyReadOnlyMemory + PyReadOnlyMemoryAdapter:** Yes — separate from MemReader (different trait impl, different purpose).
- **PyMemoryMapReader:** Questionable — internal only, converts PyMemoryMap → rsleigh::MemReader. Could be `impl rsleigh::MemReader for PyMemoryMap` directly, but `for<'b>` trait-object rules may block it.

**Merge proposal:**
Inline `PyMemoryMapReader` into `PyMemoryMap` by implementing `rsleigh::MemReader` directly. Saves ~75 LOC of wrapping.

**Difficulty:** Mechanical — move trait impl, delete wrapper struct.
**LOC delta:** -75 lines.
**Migration risk:** Very low — PyMemoryMapReader is internal only.

---

### 5. Match Accessor Methods: Uniform `.get_*(c)` Dispatch

**Pattern:** `matcher.rs` (339 LOC) exposes:
- `Match.get_int(c)` → `Option<i128>`
- `Match.get_uint(c)` → `Option<u128>`
- `Match.get_bool(c)` → `Option<bool>`
- `Match.get_float_bits(c)` → `Option<u64>`
- `Match.get_vn(c)` → `Option<Vn>`
- `Match.stack_offset(c)` → `Option<i64>`
- `Match.stack_phi_offsets(c)` → `Option<&[i64]>`
- `Match.asm_fingerprint(c)` → `&[u64]`

**File citations:**
- `matcher.rs:100–310`: PyMatch impl, all getters follow identical pattern:
  - Resolve capture → binding
  - Call `self.bindings.get_*(cap, &graph)` (or `graph.method()` for stack/fingerprint)
  - Map Ok/None appropriately

**Boilerplate uniformity:** Very high — each method is 4–6 LOC of identical dispatch.

**Assessment:** Already optimized for readability; further macro reduction would obscure the per-type safety. Acceptable as-is.

---

### 6. Arc<RwLock<…>> Pattern + Lock Helpers

**Usage sites:**
- `PyGraph` (line 25): `inner: Arc<RwLock<ir::BuiltFunctionGraph>>`
- `PyMemoryMap` (line 97): `inner: Arc<RwLock<Vec<MemRegion>>>`
- `PyMemoryMap` (line 99): `table: Arc<RwLock<Option<Arc<MemRegionsLookupTable>>>>`
- `PyMemoryMap` (line 105): `elfs: Arc<RwLock<Vec<object::File<'static>>>>`
- `PyMemoryMap` (line 112): `endianness: Arc<RwLock<target::Endianness>>`
- `PyOptimizerPipeline` (line 106): `state: Mutex<PipelineState>`

**Lock helpers:**
- `graph.rs:55–68`: `read_inner()` + `write_inner()` + `try_write_inner()` (three methods per type that mutates; only PyGraph uses all three)

**Assessment — Single-threaded from Python?**
PyO3's GIL serializes Python-side calls. Rust threads spawned within callbacks are theoretically possible but rare. PyGraph alone justifies Arc<RwLock<…>> because pattern-matching can hold a read lock during predicates (need reentrancy guards). Other uses (PyMemoryMap, PyOptimizerPipeline) could downgrade to RefCell or Cell if they never expose mutable references.

**Merge proposal:** None immediately justified. Lock pattern is appropriate for the documented use case (pattern predicates holding read locks during mutations).

---

### 7. Module Registration Boilerplate

**Pattern:** Every module (`arch.rs`, `cc.rs`, `cfg.rs`, etc.) exports `pub fn register(py, m) -> PyResult<()>` that calls `m.add_class::<Py*>()`.

**File citations:**
- `lib.rs:77–94`: main module registers 12 submodules in sequence
- One `register` per file: arch, cc, reader, sleigh, cfg, graph, strider_cls, opt, run, pattern, matcher, errors

**Boilerplate uniformity:** Very high — each is 1–10 LOC:
```rust
pub fn register(_py: Python, m: &Bound<PyModule>) -> PyResult<()> {
    m.add_class::<PyX>()
}
```

**Merge proposal:** Attribute macro `#[pyo3_register]` on each Py* class to auto-register:
```rust
#[pyo3_register]
#[pyclass]
pub struct PySleighArch { … }
// Macro auto-exposes `pub fn register(…) { … }` at module scope
```

**Difficulty:** Moderate — requires proc-macro scope analysis to detect Py* classes and emit register functions.
**LOC delta:** -100 lines (eliminate most register functions; lib.rs becomes a single registration loop).
**Migration risk:** None — signature unchanged.

---

### 8. PyLike Enum Dispatch in Pattern Module

**Pattern:** `pattern.rs:240–293` defines `PatLike<'py>` enum with 17 variants (CallPat, LoadPat, IntBinaryPat, etc.) and a wide `match` arm in `into_pat()`.

**File citations:**
- `pattern.rs:240–293`: PatLike enum + impl
- `pattern.rs:262–292`: into_pat match with 17 arms

**Assessment:**
Each arm is load-bearing distinct — each pattern builder has a unique `.finalise()` signature. The enum is necessary for polymorphic builder-as-field-argument semantics. Further abstraction would obscure per-builder type safety.

---

### 9. Conversion Impls: Single From<…> Instance

**File citations:**
- `reader.rs:72–83`: `impl From<reader::elf::RelocationStats> for PyRelocationStats`

**Assessment:** Only one impl exists; no blanket-impl opportunities. Acceptable.

---

### 10. Python-Side Aliases vs. Rust-Side Names

**Scope:** `crates/strider-py/strider/__init__.py` and `pattern.py`.

**Assessment:** Cannot audit without reading Python files, but CLAUDE.md notes that Python provides aliases (`and_` / `or_` / `not_` for keyword collisions) and Python-only convenience functions. This is appropriate API design and not Rust-side boilerplate.

---

## ROI Prioritization

1. **Arch + CC wrapper presets** (opportunity #1): -150 LOC, moderate difficulty, zero migration risk. **Medium priority.**
2. **Inline PyMemoryMapReader** (opportunity #4): -75 LOC, trivial difficulty, very low risk. **High priority.**
3. **Module registration macro** (opportunity #7): -100 LOC, moderate difficulty, zero risk. **Medium priority.**
4. **Error dispatch macro** (opportunity #3): -5 LOC today, trivial difficulty. Revisit when >5 typed errors exist. **Low priority.**

## Honest Assessment

**80%+ of the remaining boilerplate is load-bearing:** PyO3 requires per-class `#[pyclass]` + `#[pymethods]` declarations (no way around it). Pattern builders genuinely have distinct `.finalise()` implementations. Match accessors are per-type-safe. Reader stratification is necessary for the ABC + adapter boundary.

The biggest win is **proc-macros for preset wrappers** — both SleighArch and CallingConvention repeat the same 200+ LOC of classmethod stubs. A shared macro would be the highest-ROI change (clean, isolated, zero risk).

