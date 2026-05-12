# Round 10 — `strider-py` + `dot` + `graphwalk` + `entity-utils`

Reviewing all .rs/.py files in the four small auxiliary crates plus 46 Python test files.

---

## CRITICAL

### F-01: `PyMemPhiPat` and `PyValuePhiPat` missing from `PatLike` — implicit pass-through broken

- **Severity:** HIGH
- **Where:** `crates/strider-py/src/pattern.rs:237-255`
- **What's wrong:** `PatLike` lists 16 builder types as variants enabling them to be passed directly to `find_all`, any field setter (`.addr()`, `.arg()`, `.data()`, etc.), or `rewrite()` without a manual `.into_pat()` call. `PyMemPhiPat` (line 1970, registered) and `PyValuePhiPat` (line 1971, registered) are NOT variants in `PatLike`. Both have `pat_builder_finalise!` applied (lines 2103-2104), giving them `.capture()`, `.cap()`, `.when()`, `.into_pat()` methods — but they cannot be used as arguments to `find_all` or any `PatLike`-accepting builder method. `g.find_all(mem_phi())` will raise a Python `TypeError`, not a `PatternError`. CLAUDE.md states these node kinds exist and are matchable. All other typed builders (including `PyPhiPat`) are in `PatLike`.
- **Verified against:** `PatLike` enum (lines 237-255) vs `pat_builder_finalise!` invocations (lines 2103-2104) vs module registration (lines 1970-1971). No test in 46 test files exercises `mem_phi()` or `value_phi()` in `find_all`.
- **Fix:** Add `MemPhiPat(Bound<'py, PyMemPhiPat>)` and `ValuePhiPat(Bound<'py, PyValuePhiPat>)` to the `PatLike` enum, with matching arms in `PatLike::into_pat`.
- **Regression test:** `g.find_all(mem_phi())` should not raise `TypeError`.

---

### F-02: `PyCapture.__hash__` returns `u32 as isize` — silent collision on 32-bit platforms

- **Severity:** HIGH
- **Where:** `crates/strider-py/src/pattern.rs:99-104`
- **What's wrong:** `__hash__` returns `self.inner.id() as isize`. On 32-bit Python (abi3-py39 supports 32-bit targets), `isize` is 32 bits. The globally-unique capture counter is a `u32`; at 2³¹ captures the cast would produce a negative hash. Capture ids above `isize::MAX as u32` would silently hash-collide with ids in the lower half after sign-wrap. A Python `dict` keyed on `PyCapture` returns wrong bindings for those ids. In practice the per-process counter never reaches 2³¹, but the cast is unsound.
- **Fix:** Return `(self.inner.id() as u64 ^ ((self.inner.id() as u64) >> 32)) as isize`, or simply `self.inner.id() as i64 as isize` since `u32 → i64` is always positive.

---

## IMPORTANT

### F-03: `rewrite_all` calls `Python::with_gil` from inside a `#[pymethods]` — redundant re-acquisition

- **Severity:** MED
- **Where:** `crates/strider-py/src/graph.rs:489-503`
- **What's wrong:** The method is `#[pymethods]` so the GIL is already held. It internally calls `Python::with_gil(|py| { … })`. Functionally correct but signals to readers that GIL acquisition is needed here, masking the calling convention. Compare with sibling method `rewrite` (line 472) which takes `py: Python<'_>` directly.
- **Fix:** Add `py: Python<'_>` to the method signature and remove the `with_gil` wrapper.

### F-04: `test_public_api_snapshot.py` `EXPECTED_PATTERN` missing ~15 exported symbols

- **Severity:** MED
- **Where:** `crates/strider-py/tests/python/test_public_api_snapshot.py:73-118`
- **What's wrong:** `EXPECTED_PATTERN` is missing `function_arg_reg`, `function_arg_stack`, `initial_var_for`, `phi_for`, `mem_phi`, `value_phi`, `bit_not`, `int_cmp`, `CastMask`, `PhiPat`, `MemPhiPat`, `ValuePhiPat`, `LoadPat`, `StorePat`, `StackStorePat`, `StackStorePhiPat`, `CallPat`, `CallOtherPat`, `RetPat`, `IfPat`, `FunctionArgPat`. The snapshot test only checks for missing items (`assert not missing`), never unexpected additions. Future removal of `mem_phi` from the public API would not fail the test.
- **Fix:** Add all missing names; add `assert not extras` (or print-warning) so additions are visible.

### F-05: `test_lift_error_subclass_when_explicit_lift_fails` does not actually assert `LiftError`

- **Severity:** MED
- **Where:** `crates/strider-py/tests/python/test_typed_errors_e2e.py:131-155`
- **What's wrong:** Test documents itself as pinning that the raised exception "MUST be a `LiftError`", but `assert isinstance(raised, errors.StriderError)` only checks the base class. The test passes even if the code raised a plain `StriderError`.
- **Fix:** Change to `assert isinstance(raised, (errors.LiftError, errors.UnknownCallOtherError))`.

### F-06: ~~`PipelineState::from_default()` diverges from `opt::default_pipeline()`~~

- **Status:** Withdrawn after deeper analysis — pass counts match the base `opt::default_pipeline()` (CC-aware passes are layered on top by `Strider::build_optimizer_pipeline`). No bug.

### F-07: `PyReadOnlyMemoryAdapter` silently swallows `KeyboardInterrupt`/`SystemExit`

- **Severity:** MED
- **Where:** `crates/strider-py/src/reader.rs:556-595`
- **What's wrong:** Mirrors `wrap_when` but lacks the H-8 fix: a Python `KeyboardInterrupt` or `SystemExit` raised by `read` is caught into `eprintln!` + return `None`. Ctrl-C during a long `LoadReadOnly` pass is silently absorbed.
- **Fix:** Mirror `wrap_when`'s pattern: detect `PyKeyboardInterrupt`/`PySystemExit`, restore via `e.restore(py)`, return `None` (the exception propagates at the next PyO3 boundary).

---

## LOW

### F-08: `graphwalk::try_successors` `ControlFlow<()>` return type — known anti-pattern

- **Severity:** LOW
- **Where:** `crates/graphwalk/src/lib.rs:39-43`
- **What's wrong:** Every concrete impl always returns `Continue(())`. The `successors` wrapper discards the return with `let _ = …`. The `Break` arm is never used.
- **Fix:** Replace `try_successors`'s `ControlFlow<()>` with `successors` as the sole required method, OR make `try_successors` a default method calling `successors`.

### F-09: `DenseEntitySet::insert` does two bitset lookups (contains + insert)

- **Severity:** LOW
- **Where:** `crates/entity-utils/src/set.rs:60-64`
- **What's wrong:** `cranelift_bitset::CompoundBitSet::insert` returns `()`, so a second lookup is required to know whether the insertion was novel. Two index calculations + potential cache misses per call. `Worklist::enqueue` already does this at a higher level.
- **Fix:** No code change feasible until `cranelift_bitset` exposes a `test_and_set → bool` API. Document the limitation.

---

## Coverage

| File | Status |
|------|--------|
| `crates/strider-py/src/lib.rs` | Fully |
| `crates/strider-py/src/pattern.rs` | Fully |
| `crates/strider-py/src/matcher.rs` | Fully |
| `crates/strider-py/src/graph.rs` | Fully |
| `crates/strider-py/src/run.rs` | Fully |
| `crates/strider-py/src/reader.rs` | Fully |
| `crates/strider-py/src/errors.rs` | Fully |
| `crates/strider-py/src/opt.rs` | Fully |
| `crates/strider-py/src/cc.rs` | Fully |
| `crates/strider-py/src/arch.rs` | Not |
| `crates/strider-py/src/cfg.rs` | Not |
| `crates/strider-py/src/sleigh.rs` | Not |
| `crates/strider-py/src/strider_cls.rs` | Not |
| `crates/strider-py/src/dot.rs` | Not |
| `crates/strider-py/Cargo.toml` | Not |
| `crates/dot/src/lib.rs` | Partially (pre-existing `expect` at 203 noted) |
| `crates/graphwalk/src/lib.rs` | Fully |
| `crates/entity-utils/src/set.rs` | Fully |
| `crates/entity-utils/src/worklist.rs` | Fully |
| `crates/entity-utils/src/lib.rs` | Not (re-export only) |
| `crates/strider-py/tests/python/test_typed_errors_e2e.py` | Fully |
| `crates/strider-py/tests/python/test_optimizer_pipeline.py` | Fully |
| `crates/strider-py/tests/python/test_public_api_snapshot.py` | Fully |
| `crates/strider-py/tests/python/test_pattern_basics.py` | Partially |
| `crates/strider-py/tests/python/test_pattern_full_coverage.py` | Partially |
| Remaining 21 Python test files | Not (skim-level) |
