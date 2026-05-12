# Round 9 / 1F — `strider-py` + small crates audit

**Branch:** `review/ai3`. Independent audit; round-7 / round-8 reports not consulted.

**Reviewing:** `strider-py` (all `src/*.rs`, all `tests/python/**/*.py`), `dot/src/lib.rs`, `graphwalk/src/lib.rs`, `entity-utils/src/{lib,set,worklist}.rs`.

## Findings

### IMPORTANT — R9-1F-01: `test_int_cmp_op_recovery` asserts non-existent op-name strings, making it useless as a correctness pin

- **Confidence:** 95.
- **Where:** `crates/strider-py/tests/python/test_pattern_full_builders.py:351-355` and `crates/strider-py/src/matcher.rs:274-285`.
- **What's wrong:** Test asserts:
  ```python
  assert op_name in {
      "Equal", "Less", "LessEqual",
      "Sless", "SlessEqual",
      "Carry", "Scarry", "Sborrow", "Borrow",
  }
  ```
  `int_cmp_op_name` only handles six variants of `ir::IntCmpOp` — `Equal`, `Less`, `Sless`, `Carry`, `Scarry`, `Sborrow`. `LessEqual`, `SlessEqual`, `Borrow` do **not** exist in `ir::IntCmpOp` — they are lift-time-lowered shapes (CLAUDE.md: `IntLessEqual → BoolNeg(IntLess(b, a))`). The strings can never be returned. Test passes for the wrong reason — actual values always fall in the (six-name) subset. A future change returning `"LessEqual"` would still pass. A reader could be misled into writing `m.int_cmp_op(c) == "LessEqual"`.
- **Fix:** Narrow the allowed set to the actual six names:
  ```python
  assert op_name in {"Equal", "Less", "Sless", "Carry", "Scarry", "Sborrow"}
  ```

### IMPORTANT — R9-1F-02: Stale doc comment on `PyPartialMatch.graph_ptr` claims `Arc<Mutex<...>>` where the field is `Mutex<Option<*const ...>>`

- **Confidence:** 90.
- **Where:** `crates/strider-py/src/pattern.rs:302-306`.
- **What's wrong:** Comment says the pointer is "Boxed in `Arc<Mutex<...>>`" but the field is `Mutex<Option<*const ir::Graph>>` — no `Arc`. The struct is `unsendable` so reference-counted sharing isn't needed. The SAFETY argument in `with_graph` is still correct; only the comment describing the mechanism is stale. Reader searching for the `Arc` will not find one.
- **Fix:** Replace the comment:
  ```rust
  /// Raw pointer to the graph the matcher is operating on.  Protected
  /// by a `Mutex` so re-entrant access from a Python callback would
  /// deadlock visibly rather than reading a stale pointer.  Cleared
  /// to `None` by `clear_graph_ptr` immediately after the predicate
  /// returns; stale use returns `None` rather than UB.
  ```

### IMPORTANT — R9-1F-03: `Graph.optimize(pipeline)` silently becomes a no-op if called twice; no error or warning

- **Confidence:** 82.
- **Where:** `crates/strider-py/src/graph.rs:302-308` and `crates/strider-py/src/opt.rs:165-178`.
- **What's wrong:** `Graph.optimize(pipeline)` calls `pipeline.drain_into_pipeline()`, which moves all passes out of `state.passes` and `state.post_passes`. After the first call, both lists are empty. A second call creates an empty `opt::OptimizerPipeline`, runs the validator only, and returns `Ok(())` silently. Similarly, `strider.run(pipeline=p)` drains; subsequent `result.graph.optimize(p)` is a no-op with no indication. Docstring in `graph.rs` says "Drains the pipeline" but `strider.run`'s docstring does not mention the drain, and no test calls `optimize` twice on the same pipeline.
- **Fix:** Return an error when the pipeline is already empty at the start of `drain_into_pipeline`:
  ```rust
  if state.passes.is_empty() && state.post_passes.is_empty() {
      return Err(into_strider_err(anyhow::anyhow!(
          "OptimizerPipeline is empty (already drained); \
           rebuild via OptimizerPipeline.default() or a fresh constructor"
      )));
  }
  ```

### IMPORTANT — R9-1F-04: `wrap_when` swallows `KeyboardInterrupt` and `SystemExit` from predicate callbacks

- **Confidence:** 80.
- **Where:** `crates/strider-py/src/pattern.rs:465-484`.
- **What's wrong:** The `Err(e)` arm calls `e.print(py)` (prints traceback) and returns `false`, discarding the exception:
  ```rust
  Err(e) => {
      e.print(py);
      false
  }
  ```
  A `KeyboardInterrupt` or `SystemExit` raised inside a `.when(f)` predicate is printed and swallowed. `find_all` continues walking. `Ctrl-C` cannot interrupt a slow pattern walk once inside a predicate, violating user expectations for interactive Python sessions. No test exercises this scenario.
- **Fix:** Check exception type before printing and discarding:
  ```rust
  Err(e) => {
      if e.is_instance_of::<pyo3::exceptions::PyKeyboardInterrupt>(py)
          || e.is_instance_of::<pyo3::exceptions::PySystemExit>(py)
      {
          e.restore(py);
      } else {
          e.print(py);
      }
      false
  }
  ```

## Verified Correct — Round-8 Follow-Up Items

- **`[lib] test = false` rationale** (`Cargo.toml:8-19`): comment is accurate. `cdylib` with `multiple-pymethods` requires Python symbols at link time the Rust test harness can't provide.
- **`multiple-pymethods` macro behaviour**: 15 `pat_builder_finalise!(PyXxx)` invocations at `pattern.rs:2068-2082`, each emitting a separate `#[pymethods]` block. End-to-end exercised by `test_pattern_full_builders.py`.
- **`build_cc_for_sleigh` helper — 4 call sites**: `PyStackStoreDetect::new` (opt.rs:302), `PyStackLoadForward::new` (opt.rs:323), `PyFunctionArgDetect::new` (opt.rs:343), `PyCallStackArgCollect::new` (opt.rs:362). All follow the same correct borrow/clone/drop/build pattern with `into_lift_err` mapping.
- **`pure_pass_class!` macro** (opt.rs:265-281): all six emitted correctly. The intentional name divergence `PyDeadBranchElim → opt::DeadBranchElimination` is correct in `into_erased`.
- **`PyMatch::with_graph` helper — 11 typed accessors** (matcher.rs:50-58): all use the helper correctly. `stack_phi_offsets` and `asm_fingerprint` bypass it consistent with their non-`Option<R>` returns.
- **GIL release in `strider.run`** (run.rs:157-178): `strider_owned: strider::Strider` is cloned plain-Rust value; `Py<>` borrows dropped before `allow_threads`. Callback-reader path re-acquires GIL per-call via `Python::with_gil`.
- **`PyVnSpace::__hash__`** (sleigh.rs:151): hashes `self.inner.shortcut_raw()`, matching `__eq__`. `a == b ⇒ hash(a) == hash(b)` holds.
- **`PyVn::__hash__`** (sleigh.rs:216-225): mixes all three Vn fields. `__eq__` compares same. Contract holds.
- **`PyMatch::__getitem__` and `PyPartialMatch::__getitem__` u128**: PyO3 0.22 converts u128 to Python's arbitrary-precision int. No truncation.
- **Mutex poison recovery** (four sites in reader.rs and pattern.rs): all use `unwrap_or_else(|p| p.into_inner())` to recover from poison rather than `.ok()?` swallowing the error.
- **`DenseEntitySet`/`SecondaryMap` migration** in `entity-utils` and `graphwalk`: zero `FxHashSet`/`FxHashMap` uses. `graphwalk` uses `DenseEntitySet<G::NodeId>` as `VisitTracker`; `entity-utils` implements both `DenseEntitySet` and `Worklist`.
- **Typed exception coverage**: all fallible Python entry points map errors through six typed converters. `StriderError` subclass hierarchy correct. `test_typed_errors_e2e.py` covers static + most dynamic paths. Documented skips for `RewriteError`, `UnknownCallOtherError`, `UnresolvedIndirectBranchError`.
- **Dead public items in `dot`, `graphwalk`, `entity-utils`**: all exported items used by downstream crates. No dead `pub`.

## Coverage

All 14 `crates/strider-py/src/*.rs` files read in full. Selected representative Python tests (`test_pattern_full_builders.py`, `test_optimizer_pipeline.py`, `test_typed_errors_e2e.py`, partial `test_pattern_full_coverage.py`, partial `test_pattern_basics.py`). `crates/dot/src/lib.rs`, `crates/graphwalk/src/lib.rs`, `crates/entity-utils/src/{lib,set,worklist}.rs` read in full.

## Summary

- **0 HIGH**
- **4 IMPORTANT** — R9-1F-01 (test asserts non-existent op-name strings), R9-1F-02 (stale Arc<Mutex<>> doc), R9-1F-03 (Graph.optimize silent no-op on second call), R9-1F-04 (KeyboardInterrupt swallowed in predicate).
- **13/13** round-8 follow-up items verified correct.
