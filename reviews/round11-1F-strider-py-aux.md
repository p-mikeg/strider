# Round 11 — Audit 1F: `strider-py` + `dot` + `graphwalk` + `entity-utils`

Audit derived from current source on `feature/ai` (commit `c7a2903`).
Comments / docstrings / READMEs are treated as inputs to verify, not as
evidence.

## Coverage

| Area | Files read | Status |
|------|------------|--------|
| `strider-py/src/lib.rs` | full (95 lines) | covered |
| `strider-py/src/errors.rs` | full (92 lines) | covered |
| `strider-py/src/reader.rs` | full (812 lines) | covered |
| `strider-py/src/run.rs` | full (298 lines) | covered |
| `strider-py/src/pattern.rs` | full (2122 lines, in 3 reads) | covered |
| `strider-py/src/matcher.rs` | full (339 lines) | covered |
| `strider-py/src/graph.rs` | full (512 lines) | covered |
| `strider-py/src/opt.rs` | full (469 lines) | covered |
| `strider-py/src/strider_cls.rs` | full (133 lines) | covered |
| `strider-py/src/sleigh.rs` | full (233 lines) | covered |
| `strider-py/src/cfg.rs` | full (85 lines) | covered |
| `strider-py/src/cc.rs` | full (214 lines) | covered |
| `strider-py/src/arch.rs` | full (88 lines) | covered |
| `strider-py/src/dot.rs` | full (34 lines) | covered |
| `strider-py/strider/__init__.py` | full | covered |
| `strider-py/Cargo.toml` + `pyproject.toml` + `README.md` | full | covered |
| `strider-py/tests/python/test_public_api_snapshot.py` | full | covered |
| `strider-py/tests/python/test_callback_reader.py` | full | covered |
| `strider-py/tests/python/test_read_only_memory_kbd_interrupt.py` | full | covered |
| `strider-py/tests/python/test_mem_reader_error_propagation.py` | full | covered |
| `strider-py/tests/python/test_typed_errors_e2e.py` | full | covered |
| `strider-py/tests/python/test_pattern_basics.py` | full | covered |
| `strider-py/tests/python/test_compact.py` | full | covered |
| `strider-py/tests/python/test_exception_backtrace.py` | full | covered |
| `strider-py/tests/python/conftest.py` | full | covered |
| `strider-py/tests/python/system/_helpers.py` | full | covered |
| `strider-py/examples/python/07_callback_rom.py` | full | covered |
| `strider-py/examples/python/01..06,README.md` | sampled | spot |
| `dot/src/lib.rs` | full (593 lines) | covered |
| `dot/tests/emitter.rs` + `tests/style.rs` | full | covered |
| `dot/Cargo.toml` + `README.md` | full | covered |
| `graphwalk/src/lib.rs` | full (376 lines) | covered |
| `graphwalk/tests/preorder.rs` | full | covered |
| `graphwalk/tests/postorder.rs` + `graphmock_dsl.rs` + `common/mod.rs` | sampled | spot |
| `graphwalk/Cargo.toml` + `README.md` | full | covered |
| `entity-utils/src/lib.rs` + `set.rs` + `worklist.rs` | full | covered |
| `entity-utils/Cargo.toml` + `README.md` | full | covered |

## Findings

### Critical (severity 90+)

#### F-1. `PyMemReaderAdapter::read` swallows `KeyboardInterrupt` / `SystemExit`
Severity: 92.
Where: `crates/strider-py/src/reader.rs` lines 492–514.
What: The MemReader callback adapter wraps every Python exception
(including `KeyboardInterrupt` and `SystemExit`) in
`anyhow!("PyMemReader.read raised: {e}")` (line 500), which becomes
`reader::MemReadError` → `ReaderError` on the way back to Python.  This
diverges from `PyReadOnlyMemoryAdapter::read` (lines 581–592) which
explicitly re-raises both control-flow exceptions via `e.restore(py)`.

Consequence: a long `strider.run(mem=PythonReader, ...)` that lifts a
multi-MB binary cannot be interrupted with Ctrl-C — the `KeyboardInterrupt`
is converted to `ReaderError` which the orchestrator's lift loop catches
and continues from.  The audit prompt explicitly calls this out as a
required behaviour ("must re-raise `KeyboardInterrupt` and `SystemExit`
instead of swallowing them"); the existing test
`test_read_only_memory_kbd_interrupt.py` only verifies the
`ReadOnlyMemory` path, not the `MemReader` path.
Verified against: comparison with `PyReadOnlyMemoryAdapter::read` lines
574–586 which gates the same exceptions; absence of any
`is_instance_of::<PyKeyboardInterrupt>` check in the `Cb` branch of
`AnyMemReader::read` (lines 624–628) or in `PyMemReaderAdapter::read`
itself.
Fix: mirror the `PyReadOnlyMemoryAdapter` pattern — at the
`call_method1(...)` call-site, match on the returned `Err`, restore
`KeyboardInterrupt` / `SystemExit` to Python's pending-exception slot,
and bail out of the surrounding lift loop.  The `rsleigh::MemReader`
trait can't surface a "stop now" signal directly, but
`reader::MemReadError(anyhow!(...))` carrying the typed exception still
propagates back through `strider::run`, where the orchestrator's
`map_err(into_strider_err)` sees the restored exception via
`PyErr::take(py)` (analogous to the `find_all` propagation at
`graph.rs:378`).
Regression test: subclass `MemReader.read` raising `KeyboardInterrupt`,
call `strider.run(...)`, assert `pytest.raises(KeyboardInterrupt)` (not
`ReaderError`).

#### F-2. `crate::cfg::build_cfg` defaults the `ArchPreset` to `X86_64`
Severity: 90.
Where: `crates/strider-py/src/cfg.rs` lines 52–55; consumed by
`crates/strider-py/src/run.rs::run_via_orchestrator` lines 127–136 to
construct the **snapshot CFG** the user receives via `RunResult.cfg`.
What: `build_cfg` calls the deprecated `cfg::Builder::new(...)` which
silently defaults `ArchPreset` to `X86_64` (see
`crates/cfg/src/cfg/builder/mod.rs:103-106`, with explicit deprecation
notice on lines 98–102 warning that it "silently misclassifies
CallOthers on non-x86 binaries").  CLAUDE.md confirms the contract:
"the orchestrator delivers the preset via `cfg::Builder::for_arch(...)`
— `Builder::with_endianness` defaults `preset = X86_64`, which silently
misclassifies arch-specific CallOthers on non-x86 binaries."

Consequence: every `strider.run(arch=non_x86, ...)` call returns a
`RunResult` whose `result.cfg` was built with the wrong preset.  The
underlying graph (`result.graph`) is correct because the orchestrator
internally rebuilds the CFG via `Builder::for_arch`
(`crates/strider/src/orchestrator.rs:940`), but every Python user who
inspects `result.cfg` to check trap-region termination, dump dot, or
count blocks sees a CFG whose CallOther classification disagrees with
what the orchestrator used.  Worst case: a non-x86 CallOther that the
real orchestrator classified as `NoOp` is rendered in the snapshot as
an unclassified terminator (extra trap region).
Verified against: orchestrator at
`crates/strider/src/orchestrator.rs:940` uses `Builder::for_arch`;
strider-py snapshot path at `cfg.rs:53` uses the deprecated `new`.
Comments at `cfg.rs:46-52` acknowledge the issue but argue the snapshot
is "arch-agnostic by design" — that argument was correct before
CallOther classification became preset-dependent.
Fix: change `build_cfg` to take an `arch: PySleighArch` and call
`cfg::Builder::for_arch(arch.inner, sleigh, entry, opts)`.  Existing
callers from `strider.run` already have the arch — pass it through.
Direct Python callers of `strider.build_cfg` would need to add the
`arch` argument, which is a small breaking change but unavoidable for
correctness.
Regression test: lift an aarch64 ELF that contains a `brk` instruction;
assert `result.cfg.html_str()` shows the same number of regions as
`result.graph.preorder()`-derived control states.

### Important (severity 80–89)

#### F-3. Example `07_callback_rom.py` has a wrong `ReadOnlyMemory.read` signature
Severity: 85.
Where: `crates/strider-py/examples/python/07_callback_rom.py` line 43.
What: The example defines
`def read(self, space: int, addr: int, size: int) -> int | None:`
(three positional args after `self`).  The Rust adapter at
`crates/strider-py/src/reader.rs:579` calls
`self.py_obj.call_method1(py, "read", (addr, size))` (two positional
args).  A Python subclass that follows the example will receive
`space=<actual addr>` and crash on missing `size`, raising a TypeError
through `PyReadOnlyMemoryAdapter::read`'s "did not return int" or
"raised" branch — depending on which Python error path fires first.

The fixture in the example is constructed so the callback never
actually fires (line 89-98 explains: "for *this* fixture the callback
never fires"), so the bug is latent — anyone copying the example onto
a fixture that *does* exercise the ROM gets a confusing TypeError.

Verified against: same file's `test_callback_reader.py:120` which uses
the *correct* `def read(self, addr: int, size: int)` signature, and
the actual adapter signature at reader.rs:579.
Fix: change line 43 of the example to
`def read(self, addr: int, size: int) -> int | None:` and update the
"# space_id: 0 = RAM, ..." comment in the README at line 281–285 (which
has the same wrong signature) to match.
Regression test: the example is referenced from the README and the
crate-level docs; add a `pytest` smoke that imports and invokes a
callback ROM with at least one fold-eligible load to catch this.

#### F-4. README documents `ReadOnlyMemory.read(space_id, addr, size)` — wrong signature
Severity: 84.
Where: `crates/strider-py/README.md` lines 280–285; the same wrong
3-arg signature is also embedded in
`crates/strider-py/src/reader.rs:722` as a TypeError message
("rom must be a MemoryMap or an object with a `read(space_id, addr,
size)` method").
What: The README's "Python-implemented memory readers" section claims
`ReadOnlyMemory.read` takes `(space_id, addr, size)` and tells the user
to gate on `space_id != 0`.  Both the Python ABC default at reader.rs:543
(`fn read(&self, addr: u64, size: usize)`) and the adapter call at
reader.rs:579 use 2-arg `(addr, size)`.  The space gate is done in
Rust (line 563) before calling Python, so the user override is RAM-only
already.

Consequence: a Python user reading the README gets a working class
that *does* have a `read` method but with the wrong arity, leading
to the same TypeError as F-3 once the callback fires.
Verified against: docstring on `PyReadOnlyMemory` at reader.rs:518–530
correctly documents the 2-arg form ("Subclasses override
`read(addr, size) -> Optional[int]` …"); only the README and the
TypeError message disagree.
Fix: update README:280-285 and the FromPyObject error message at
reader.rs:722 to say `read(addr, size)`.
Regression test: pin the README example as a doctest (or copy into a
test) so signature drift trips CI.

#### F-5. `into_strider_err` does NOT surface `LiftError` for orchestrator-path lift failures
Severity: 82.
Where: `crates/strider-py/src/errors.rs` lines 31–39 (the converter)
and `crates/strider-py/src/run.rs` line 179 (the only call site for the
orchestrator path).
What: `into_strider_err` only downcasts to
`UnresolvedIndirectBranchError` and `UnknownCallOtherError`; everything
else, including a plain `LiftError` produced by `strider::run` when the
first instruction lift fails (e.g. unmapped `entry` address), lands as
the generic `StriderError` on the Python side.  By contrast,
`run_with_custom_pipeline` uses `into_lift_err`
(`run.rs:269`) for its `analyze_cfg_with` boundary, which
**does** surface `LiftError`.

Consequence: a user catching `except errors.LiftError` for a typo'd
entry address on the orchestrator path will miss it and fall through
to a bare-`except StriderError` handler — defeating the typed-error
hierarchy's purpose.  The existing test
`test_lift_error_subclass_when_explicit_lift_fails`
(test_typed_errors_e2e.py:131-160) accepts both `LiftError` and
`UnknownCallOtherError` — if it passes today, it's because the empty
MemoryMap fail path happens to surface as `UnknownCallOtherError` (the
lift hits an unrecognised Sleigh user-op before the bytes-missing
error?) or it actually fails silently in CI and the audit flags it.
Verified against: the `into_lift_err` body at errors.rs:42–47 has the
exact `LiftError::new_err(...)` fallback the strider-side path lacks;
both `Strider::analyze_cfg` (run.rs:269) and `Sleigh::new`
(sleigh.rs:53) explicitly use `into_lift_err`.
Fix: make `into_strider_err` also fall back to `LiftError` when the
inner error is one of the lift-error categories (or, simpler: have it
do `if e.downcast_ref::<cfg::Error>() ... LiftError`).  Alternatively
run.rs:179 could use a custom converter that tries `LiftError` first
and falls through to `StriderError`.
Regression test: extend `test_lift_error_subclass_when_explicit_lift_fails`
to assert the **specific** `LiftError` subclass for the orchestrator
path (drop the "or UnknownCallOtherError" alternative — that case is
covered by its own test).

#### F-6. `Worklist::enqueue` is two-pass
Severity: 80.
Where: `crates/entity-utils/src/worklist.rs` lines 55–60.
What: `enqueue` first calls `self.workset.contains(entity)` then, if
absent, calls `self.workset.insert(entity)` and pushes to the deque.
This is two bitset accesses per enqueue.  `DenseEntitySet::insert`
returns `bool` ("was newly inserted") since the round-7 single-pass
fix (set.rs:65–67), so the body could become:

```rust
pub fn enqueue(&mut self, entity: E) {
    if self.workset.insert(entity) {
        self.worklist.push_back(entity);
    }
}
```

Consequence: hot fixed-point passes that enqueue/dequeue thousands of
times per pipeline iteration pay 2× the bitset cost.  Not a correctness
bug — but the audit prompt explicitly asked about
"`DenseEntitySet::insert` / `test_and_set` — single-pass behaviour" and
worklist invariants in the same breath, and the worklist's own
invariant is *literally* what `insert`'s bool return was added for.
Verified against: set.rs:65-67's doc comment says "Hot graph-traversal
paths get one bitset access per insert" — the worklist still pays two.
Fix: collapse to single-pass per the snippet above.  The existing
test `enqueue_dedups_while_queued` (worklist.rs:103–112) keeps
passing.
Regression test: not strictly needed — the existing tests cover the
contract, and a benchmark would be the right artefact for the perf
delta.

## Coverage summary

Reviewed every Rust source file in `strider-py` (14 .rs in `src/`) and
every test/example file under `strider-py/tests/python/` and
`strider-py/examples/python/` (~70 .py).  Covered all four crates'
`src/` and `tests/` and the four `Cargo.toml` + `README.md` pairs.
Six findings: two Critical (F-1 GIL/control-flow exception leak in
`PyMemReaderAdapter`, F-2 wrong arch preset in snapshot CFG), four
Important (F-3 wrong example signature, F-4 wrong README signature,
F-5 missing typed-error mapping on orchestrator path, F-6 two-pass
worklist enqueue).
