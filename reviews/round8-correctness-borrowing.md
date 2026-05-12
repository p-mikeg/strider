# Round 8 / Ask 18-3 — Borrowing / aliasing / concurrency

**Branch:** `review/ai2`.  Independent audit.

## Findings

### HIGH: Re-entrant `RwLock` deadlock in `Graph::find_all` with `.when()` predicates

- **Severity:** HIGH (deterministic deadlock from reachable Python API usage).
- **Where:** `crates/strider-py/src/graph.rs:349-361` (deadlock site); `crates/strider-py/src/pattern.rs:385-423` (callback path).
- **What's wrong:** `Graph::find_all` acquires a read-lock on `self.inner` (`Arc<RwLock<BuiltFunctionGraph>>`) at line 350, then calls `matcher.find_all(&pat)` at line 360 while still holding the read-lock.  If the pattern contains a `.when()` predicate (constructed via `wrap_when`), the predicate calls `Python::with_gil` and invokes the user-supplied Python callable.  The Python callable can reach the outer `PyGraph` (via closure capture or globals).  Calling `graph.rewrite(...)`, `graph.optimize(...)`, `graph.compact()`, or `graph.reoptimize(...)` from inside the predicate tries to acquire a write-lock on the same `RwLock` already read-locked by the same OS thread.  On Linux pthreads this deadlocks (`pthread_rwlock_wrlock` blocks waiting for the reader on the same thread).  Same issue in `find_all_requirements` at lines 398-441.
- **Reproducer:**
  ```python
  g = strider.run(...)
  p = pattern.call().when(lambda m: (g.rewrite(...), True)[1])
  g.find_all(p)  # deadlock
  ```
- **Fix:** Document the constraint that `.when()` predicates must not call back into mutating graph methods on the same `Graph`.  As a runtime guard, replace `RwLock::write()` with `RwLock::try_write().map_err(...)` so re-entrant attempts return a typed error instead of blocking.

### MED: `std::env::set_var` SAFETY comment understates risk

- **Severity:** MED (theoretical glibc memory unsafety; practical risk low).
- **Where:** `crates/strider-py/src/lib.rs:63-73`.
- **What's wrong:** SAFETY comment states "not memory unsafety."  This is false on glibc — concurrent `getenv` while `setenv` runs can cause use-after-free in glibc's `environ` array, which is unprotected.  Rust 1.80 stabilised the `unsafe` requirement specifically because of this.  Practical risk at Python import time is low (Python's import lock serialises module init), but the comment misleads future maintainers.
- **Fix:** Rewrite the comment to acknowledge the real-but-bounded risk.

### MED: `PyPartialMatch::with_graph` SAFETY comment has subtle reasoning gap

- **Severity:** MED (currently non-exploitable; reasoning gap).
- **Where:** `crates/strider-py/src/pattern.rs:291-299`.
- **What's wrong:** SAFETY comment says "the outer Mutex guard prevents the cleanup from racing this call."  True for the cleanup path.  But `with_graph` holds the `Mutex<Option<*const ir::Graph>>` lock for the entire duration of `f(graph_ref)`.  If `f` ever called back into Python and that Python code re-entered `with_graph` on the same proxy, the second `lock()` would deadlock on `std::sync::Mutex` (non-reentrant).  Current callers (`bindings.get_uint`, `get_int`, etc.) are pure-Rust so the deadlock cannot fire — but it is an undocumented constraint.
- **Fix:** Update the SAFETY comment to require that `f` not re-enter `with_graph` (i.e., must not call back into Python or acquire the same mutex).

### LOW: `PyPartialMatch` field doc says "Boxed in `Arc<Mutex<...>>`"

- **Severity:** LOW (documentation only).
- **Where:** `crates/strider-py/src/pattern.rs:255-259`.
- **What's wrong:** Field is `Mutex<Option<*const ir::Graph>>`, not `Arc<Mutex<...>>`.  The Arc is implicit via PyO3's `Py<PyPartialMatch>`.
- **Fix:** Rewrite field doc to reflect actual type.

### LOW: `lookup_table` TOCTOU double-build race

- **Severity:** LOW (redundant work only; no safety issue).
- **Where:** `crates/strider-py/src/reader.rs:134-144`.
- **What's wrong:** `lookup_table` drops the read-lock before calling `rebuild_table`.  Two concurrent callers can both observe `None` and both rebuild.  Benign — second write overwrites first with equivalent data.  Python's GIL makes this unreachable in pure Python; native threads sharing the `Arc<PyMemoryMap>` could see it.
- **Fix:** Double-checked locking pattern: take write-lock, re-check, rebuild if still `None`.

## Areas verified correct

- **No `transmute`, `from_raw_parts`, manual `unsafe impl Send/Sync`, or `Rc` usage** anywhere in production code.
- **No `Arc` reference cycles** constructible through the public pattern API (all `Pat` chains are trees).
- **No recursive `Mutex::lock`** at any site (all locked regions are short and don't re-acquire).
- **`PyOptimizerPipeline::drain_into_pipeline`**: locks `Mutex<PipelineState>` once, doesn't hold across Python calls.
- **`DecodeCache` `Mutex<FxHashMap>`**: locked only for HashMap get/insert; no cross-lock ordering issues.

## Summary of unsafe blocks in production code

| File:line | Purpose | SAFETY | Sound? |
|-----------|---------|--------|--------|
| `pcode-lift/src/lib.rs:151` | `rsleigh::VnSpace::by_id` on pcode LOAD/STORE space pointer | Adequate | Yes |
| `strider-py/src/pattern.rs:297` | `&*ptr` on raw graph pointer in `with_graph` | Adequate; minor reasoning gap (MED) | Yes |
| `strider-py/src/lib.rs:72` | `std::env::set_var` for backtrace var | Comment factually wrong (MED) | Yes (in practice) |

## Summary

- **1 HIGH** — re-entrant RwLock deadlock in `Graph::find_all`/`find_all_requirements` with `.when()` predicates.
- **2 MED** — `set_var` SAFETY comment; `with_graph` re-entrancy reasoning.
- **2 LOW** — `PyPartialMatch` field doc; `lookup_table` TOCTOU double-build.
