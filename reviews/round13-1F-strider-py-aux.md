# Round 13 — 1F: `strider-py` + `dot` + `graphwalk` + `entity-utils` audit

Branch: `review/ai7` · Scope: `crates/strider-py/{src,tests/python}/**`, `crates/dot/src/**`, `crates/graphwalk/src/**`, `crates/entity-utils/src/**`.

## Verdict

**2 findings: 1 HIGH (test gap), 1 MED (direct tuple-struct construction missing migration to `::from`).**

## Findings

### F1 — `test_read_only_memory_kbd_interrupt.py` does NOT exercise the Rust adapter
- **Severity:** HIGH (confidence 90) — test pretends to verify a guard that isn't on the code path under test
- **Where:** `crates/strider-py/tests/python/test_read_only_memory_kbd_interrupt.py:27-35`
- **What's wrong:** `test_keyboard_interrupt_in_rom_read_propagates` creates `_KbdRom()` and calls `rom.read(0x1000, 8)` directly.  Python MRO dispatches to `_KbdRom.read` (the subclass override) which raises `KeyboardInterrupt` — the test never crosses the Rust boundary.  The Rust `PyReadOnlyMemoryAdapter::read` re-raise guard at `reader.rs:597-601` is never invoked.  The test proves only that a Python method can raise `KeyboardInterrupt`, not that the guard exists or is correct.  Same issue with `test_system_exit_in_rom_read_propagates`.  Contrast with `test_mem_reader_kbd_interrupt.py`, which correctly forces the adapter path by passing the reader as `mem=` to `strider.run`.
- **Fix:** Pass `_KbdRom()` as `rom=` to `strider.run` with a binary that contains a constant-address `Load` so `LoadReadOnly` fires and actually calls into `PyReadOnlyMemoryAdapter::read`.  Without this, a regression deleting the guard would go undetected.
- **Regression test:** the fixed test itself.

### F2 — `MemReadError` constructed via tuple-struct literal in `reader/src/elf.rs:312`
- **Severity:** MED (confidence 80)
- **Where:** `crates/reader/src/elf.rs:312`
- **What's wrong:** Round 12 R12-T-P tightened `MemReadError`'s inner field to `pub(crate)` so external callers must go through `From<anyhow::Error>`.  The `elf.rs` site uses the in-crate tuple-struct literal `crates/reader::MemReadError(anyhow::anyhow!(...))` — still valid since it's the same crate, but inconsistent with `strider-py/src/reader.rs:528, 663` which correctly use `reader::MemReadError::from(...)`.  Round 12's CLAUDE.md / commit message states the intent was to funnel all construction through `::from`.
- **Fix:** `crate::MemReadError::from(anyhow::anyhow!("address {:#x} is not mapped", addr.off))`.

## Categories verified clean

✓ **PyO3 boundary error mapping** — all `#[pymethods]` entry points convert `anyhow::Error` to typed Python exceptions (`StriderError`, `LiftError`, `ReaderError`, `PatternError`, `RewriteError`).  Pending-PyErr passthrough in `into_strider_err`/`into_lift_err` (errors.rs:39-42, 73-74) handles `KeyboardInterrupt`/`SystemExit` that a callback already `restore`d.

✓ **`unsafe` blocks with SAFETY comments** — three sites in `strider-py/src/`:
- `lib.rs:71` (`std::env::set_var`): SAFETY comment lines 63-70.
- `pattern.rs:367` (raw pointer deref in `PyPartialMatch::with_graph`): SAFETY comment 319-322 + 364-366.

✓ **GIL release in `strider.run`** — `run.rs:166` wraps the entire `strider::run(config)` call in `py.allow_threads(|| { … })`.  Callback readers reacquire GIL per-call via `Python::with_gil`.

✓ **KeyboardInterrupt / SystemExit propagation** — guards in place at `reader.rs:507-514`, `reader.rs:597-601`, `pattern.rs:511-514`, `graph.rs:381, 453` (`PyErr::take(py)` after `find_all`/`find_all_requirements`).

✓ **Str-keyed capture interning** — `intern_str` (pattern.rs:127-138) uses `OnceLock<Mutex<HashMap<String, Capture>>>`; same string always maps to same Capture id within a process.  `CaptureKey::Str` in `matcher.rs:40` calls the same `intern_str`.

✓ **`multiple-pymethods` feature** — enabled in `Cargo.toml:27`.  `pat_builder_finalise!` macro emits a second `#[pymethods]` block per builder type; 12 invocations at `pattern.rs:2108-2119` cover every typed builder.

✓ **`graphwalk` termination** — `PreOrderContext::next` and `PostOrderContext::next_event` track visited nodes with `DenseEntitySet`.  No node yielded twice.  `ControlFlow<()>` in `try_successors` / `try_predecessors` correct; convenience wrappers discard return intentionally.

✓ **`entity-utils` invariants** — `DenseEntitySet::insert` returns `bool` (matches `HashSet::insert`).  `Worklist::enqueue` is single-pass `if workset.insert(entity) { push_back }` — atomic dedup+push.  `Worklist::dequeue` removes from workset before returning.

✓ **`float_is_nan` Python binding** — `pattern.rs:1060-1063` implements `pattern::float_ne(op.clone(), op)`.  Registered at `pattern.rs:2051`.  In `EXPECTED_PATTERN` (`test_public_api_snapshot.py:115`).

✓ **Public API snapshot completeness** — `test_public_api_snapshot.py` covers all four namespaces (`strider`, `errors`, `opt`, `pattern`).  Every `EXPECTED_PATTERN` symbol matches `add_fn!` calls in `pattern.rs:1981-2092`.

✓ **`dot` crate** — no `unsafe`.  `escape_dot_label` / `json_quote` pure string transformers.  `as_svg` drops stdin before `wait_with_output` (avoids deadlock).  `<` → `<` JSON escaping prevents script-tag injection.

## Coverage table

| Area | Files | Result |
|---|---|---|
| PyO3 boundary error mapping | `errors.rs`, `run.rs`, `graph.rs`, `reader.rs` | clean |
| Str-keyed capture interning | `pattern.rs:121-138`, `matcher.rs:36-43` | clean |
| `unsafe` + SAFETY comments | `lib.rs:71`, `pattern.rs:319-367` | clean |
| Python tests calling unsupported features | all 51 `.py` | clean |
| `graphwalk` termination | `graphwalk/src/lib.rs` | clean |
| `entity-utils` invariants | `set.rs`, `worklist.rs` | clean |
| `multiple-pymethods` | `Cargo.toml:27`, `pattern.rs:2108-2119` | clean |
| GIL release in `strider.run` | `run.rs:166` | clean |
| KeyboardInterrupt/SystemExit propagation | reader.rs / pattern.rs / graph.rs | **F1** (test gap) |
| Public API snapshot completeness | `test_public_api_snapshot.py` | clean |
| `MemReadError` construction sites | `reader/src/lib.rs:41`, `elf.rs:312`, `reader.rs:528, 663` | **F2** (direct literal) |
| `float_is_nan` Python binding | `pattern.rs:1060-1063` | clean |
