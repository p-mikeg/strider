# Round 12 audit 1F — strider-py + dot + graphwalk + entity-utils

Scope reviewed:

- `crates/strider-py/src/*.rs` (14 files, ~5,560 LOC)
- `crates/strider-py/strider/__init__.py`
- `crates/strider-py/tests/python/*.py` (sampled core + snapshot)
- `crates/strider-py/examples/python/*.py` (7 examples)
- `crates/strider-py/Cargo.toml` + `pyproject.toml` (via Cargo.toml)
- `crates/strider-py/README.md`
- `crates/dot/src/lib.rs` (592 LOC)
- `crates/graphwalk/src/lib.rs` (375 LOC)
- `crates/entity-utils/src/{lib.rs,set.rs,worklist.rs}` (~529 LOC)

Confidence threshold: ≥ 80. Findings grouped by severity.

---

## Critical (90–100)

None.

## Important (80–89)

### F-1 — README "What's NOT in v1" still lists `float_is_nan` as missing, but the constructor is fully wired

Confidence: **85**.

- **Sites:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/README.md:203` ("`float_is_nan` is registered but raises `PatternError` until the IR gains a `FloatIsNan` node kind.")
  - `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/README.md:310` ("`float_is_nan` constructor (no `FloatIsNan` IR node yet).")
  - `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:1059-1063` — the real implementation:
    ```rust
    #[pyfunction]
    pub fn float_is_nan(operand: PatLike<'_>) -> PyResult<PyPat> {
        let op = operand.into_pat()?;
        Ok(PyPat::from_pat(pattern::float_ne(op.clone(), op)))
    }
    ```
  - `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:2051` — registered as a normal pyfunction (`add_fn!(float_is_nan)`).
  - `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/tests/python/test_public_api_snapshot.py:115` — listed in `EXPECTED_PATTERN` ("`float_is_nan`").
- **Issue:** Two README statements (line 203 and the "What's NOT in v1" bullet at line 310) tell users the constructor is a stub that raises `PatternError`. The actual implementation desugars to the lift-time-lowered `BoolNeg(FloatEqual(x, x))` shape — see the doc-comment in `pattern.rs:1051-1057` which explicitly says this matches both the FLOAT_NAN-lifted shape and any `x != x` source. The constructor is a fully functional, non-raising pattern.
- **Impact:** Users reading "Pattern coverage" / "What's NOT in v1" will work around a feature that already exists, or will avoid testing it and never discover that it works. The bullet at line 310 also misadvertises the v1 surface.
- **Fix:** Replace lines 203-204 and the v1-gap bullet at line 310 with the correct description: `float_is_nan(x)` matches the lifter's lowered FLOAT_NAN shape (`BoolNeg(FloatEqual(x, x))`) and any explicit source-level `x != x`. The `CLAUDE.md` pattern-crate section already documents the lowering at the IR level.

---

## Other observations (not reported as findings)

These are notes from the audit; none rise to confidence ≥ 80.

- **`errors.rs:58-67` LIFT_MARKERS heuristic** — the substring scan ("lift", "sleigh", "decode", "pcode", "unsupported instruction") is documented as a temporary heuristic. It does correctly catch the bulk of lift failures, but a future error message containing "decode" in an unrelated context (e.g. base64 decoding inside a relocation parser) would be mis-classified as `LiftError`. The comment acknowledges this; not worth flagging at HIGH confidence. Mitigation: introduce a typed `LiftError` in `pcode-lift` (already noted in the comment).
- **`PyMemReaderAdapter::read` / `PyReadOnlyMemoryAdapter::read` KbdInterrupt/SysExit re-raise** (`reader.rs:507-515`, `597-607`) — both adapters correctly call `e.restore(py)` and signal failure to the caller, matching the documented contract. The `PyPartialMatch::clear_graph_ptr` + `find_all`/`find_all_requirements` `PyErr::take(py)` re-propagation in `graph.rs:381-383` and `453-455` closes the loop. Verified working.
- **`PyOptimizerPipeline::drain_into_pipeline`** (`opt.rs:178-199`) — empty-after-drain guard reports a typed error rather than silently running an empty pipeline. Good defensive choice; matches `CLAUDE.md`'s emphasis on detecting masked caller bugs.
- **`PyCapture.__hash__`** (`pattern.rs:99-106`) — uses `Capture::id() -> u32` (verified via `pattern/src/var.rs:50`) and casts via `i64 → isize` to dodge 32-bit-isize sign-wrap. Documented in the comment. Correct.
- **`unsafe` blocks** — two found, both with full SAFETY comments:
  - `lib.rs:71` (env var set during pymodule init) — comment notes the worst case is a missing backtrace.
  - `pattern.rs:367` (raw-pointer deref inside `PyPartialMatch::with_graph`) — comment ties the validity window to the matcher's set-clear lifecycle, with the `Mutex` preventing races.
- **`build_cfg` → `Builder::for_arch`** (`cfg.rs:53`) — explicitly chosen over the deprecated `Builder::new` with rationale comment. W2 fix verified intact.
- **`ReadOnlyMemory.read` signature** — README quote (`README.md:280`) is `def read(self, addr: int, size: int) -> Optional[int]`, matching `reader.rs:559`. W11 doc fix verified.
- **`PatLike` enum coverage** (`pattern.rs:239-259`) — 15 typed builder variants enumerated, matching the 15 `pat_builder_finalise!` macro invocations at the bottom of the file (lines 2108-2122). Coverage is exact; no `#[pyclass]` builder is missing from `PatLike`.
- **`py.allow_threads` for `strider::run`** (`run.rs:166-178`) — Strider is `Clone` (verified at `strider/pipeline.rs:130-140`), cheap fields. Snapshot taken outside the closure to avoid holding a `PyRef` across GIL release. Callback readers reacquire via `Python::with_gil` per `read`. Sound.
- **`dot::escape_dot_label` / `json_quote`** (`lib.rs:152-209`) — handle the full set of edge cases (literal newlines, backslash followed by recognised DOT escapes vs anything else, low control chars `\uXXXX`, `<` → `<` for script-tag escape). Test coverage is comprehensive (lines 479-591); clippy-clean.
- **`DenseEntitySet::insert`** (`set.rs:65-67`) — single-pass delegation to `CompoundBitSet::insert`. `test_and_set` removed (verified absent via grep). Tests pin the single-pass shape (`insert_returns_true_on_first_insert_false_on_repeat`).
- **`Worklist::enqueue`** (`worklist.rs:55-59`) — single-pass `if workset.insert(entity) { worklist.push_back }`. Test `enqueue_dedup_at_ten_thousand_scale` (`worklist.rs:221-238`) pins the shape at 10k scale.
- **`Worklist::dequeue`** (`worklist.rs:63-67`) — pops from `worklist`, removes from `workset`. Workset-mirror invariant preserved; `re_enqueue_after_dequeue_is_allowed` test pins it.
- **`PreOrder` / `PostOrder` termination** (`graphwalk/src/lib.rs:157-181`, `270-314`) — `VisitTracker` short-circuits via `is_visited`. `DenseEntitySet` is the default tracker for cyclic graphs; `NopTracker` for trees. Cycle handling correct.
- **`try_successors` with `ControlFlow<()>`** — the public trait shape is `ControlFlow<()>`, with a non-short-circuit `successors` convenience wrapper. None of the in-tree traversals exercise the short-circuit path. Not a bug (the API is conservative for external callers); rewriting to a plain `FnMut` would be a minor simplification but no current caller would benefit.
- **`test_public_api_snapshot.py`** verifies every name listed in EXPECTED_* exists (asserts `missing` is empty; extras only print). I spot-checked: every `EXPECTED_TOP` symbol has a matching `add_class`/`m.add` in the registration functions; every `EXPECTED_OPT` symbol is in `opt.rs:457-471`; every `EXPECTED_PATTERN` symbol is wired via `add_fn!` or `add_class`. No drift.

---

## Verdict

One important documentation drift (F-1: README incorrectly claims `float_is_nan` is missing/stubbed when it works). Otherwise the four crates are clean at the HIGH-confidence bar: error-mapping is consistent and typed, FFI lifetime boundaries are guarded, unsafe blocks carry full SAFETY rationale, and the public-API snapshot matches the registered surface exactly.
