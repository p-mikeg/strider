# Round 9 — Ask-8 R3: Correctness / Borrowing pass

## Coverage

All Rust source files in `crates/strider-py/src/`, `crates/pcode-lift/src/lib.rs`, `crates/ir/src/graph/uses.rs`, `crates/ir/src/ops/rewrite.rs`, `crates/opt/src/pipeline.rs`, `crates/opt/src/redundant_phis/mod.rs`, `crates/opt/src/dead_branch/mod.rs`, `crates/opt/src/stack_store/detect.rs`, `crates/reader/src/lib.rs`, `crates/reader/src/elf.rs`, `crates/pattern/src/pat/mod.rs`.

## Critical

### ISSUE-1: `wrap_when` — `try_borrow` failure silently leaves raw pointer alive

**Confidence:** 88.

**Where:** `crates/strider-py/src/pattern.rs:459-464`.

```rust
let result = py_func.call_bound(py, args, None);
if let Ok(b) = py_proxy.try_borrow(py) {
    b.clear_graph_ptr();
}
```

`try_borrow` returns `Err` when PyO3's borrow check detects the object is already mutably borrowed. The `if let` arm is silently skipped and the raw `*const ir::Graph` is **not cleared**. The proxy outlives `find_all`'s `RwLockReadGuard`; later `with_graph` on the proxy dereferences an invalid pointer. `unsendable` prevents cross-thread access but the proxy can be stored in Python and accessed after `find_all` returns.

**Fix:** Replace the `try_borrow`-guarded clear with an unconditional clear via `Arc<Mutex<Option<*const _>>>` so the clear can be invoked without a PyO3 borrow check.

### ISSUE-2: `PyPartialMatch::with_graph` — Mutex held across closure call containing `&*ptr` dereference + misleading SAFETY comment

**Confidence:** 85.

**Where:** `crates/strider-py/src/pattern.rs:351-359`.

The `Mutex` guard is held for the entire closure. The SAFETY comment says "the outer Mutex guard prevents the cleanup from racing this call." Correct for the non-reentrant case, but: (a) if the closure ever calls back into Python and re-enters another `with_graph`, the non-reentrant `std::sync::Mutex` would deadlock; (b) the SAFETY comment misattributes the soundness to "Mutex prevents concurrent access" when in fact the soundness comes from `unsendable` (no concurrent access exists).

**Fix:** Replace `Mutex` with `Cell` or `RefCell` (consistent with `unsendable`) and update the SAFETY comment to state correctly that the raw pointer is sound because the type is `unsendable`.

## Important

### ISSUE-3: `decode_space_id` SAFETY comment understates preconditions

**Confidence:** 80.

**Where:** `crates/pcode-lift/src/lib.rs:143-152`.

The SAFETY comment says the CONST-space tag check "is not the safety condition itself." In fact the CONST-space check is the only in-process guard preventing a garbage offset reaching `by_id`. Comment also doesn't state who keeps the pointer valid (the `Sleigh` must outlive the dereference).

**Fix:** Extend SAFETY: "(a) the Sleigh object must remain alive for the duration of the returned reference; (b) the CONST-space check above is a necessary structural precondition for (a) — this function must not be called with pcode from any source other than `rsleigh::Sleigh::lift_one`."

### ISSUE-4: `force_anyhow_backtrace_capture` SAFETY comment incorrectly claims `set_var` cannot cause memory unsafety

**Confidence:** 80.

**Where:** `crates/strider-py/src/lib.rs:60-74`.

The comment says "the worst case here is a missing backtrace on a racing reader, not memory unsafety." Incorrect on Linux glibc and macOS: `setenv`/`getenv` are not thread-safe in POSIX. Concurrent reader calls to `std::env::var_os(...)` while `set_var` mutates `environ` is a data race with UB in libc's hash table. Rust 2024 marks `set_var` `unsafe` for exactly this reason.

**Fix:** Either acknowledge the race risk and explain why this call site is safe (e.g. "module is imported before any Rust thread is spawned"), or use a `OnceLock<()>` compare-and-set approach.

## Verified Correct

- **`unsafe impl Send` / `Sync`**: none in workspace.
- **`Rc`**: none in production.
- **`Arc` reference cycles**: none.
- **Iterator invalidation in opt passes**: all pass `Vec`-collected candidates before mutation. `InputCursor` advances before replacing.
- **`PyGraph::find_all` re-entrancy**: `compact`/`optimize`/`rewrite`/`reoptimize`/`rewrite_all` all use `try_write_inner` (non-blocking).
- **`with_built` in `opt/src/pipeline.rs`**: `mem::take` swap sound; panic worse-case leaves empty graph.
- **`object::File<'static>` in `PyMemoryMap`**: deliberate process-lifetime leak, documented.
- **`PyPartialMatch` `unsendable` + Mutex**: `Mutex` is for re-entrancy detection, not thread safety. Sound for current callers.

## Summary

| # | File | Lines | Category | Confidence |
|---|------|-------|----------|-----------|
| 1 | `strider-py/src/pattern.rs` | 459-464 | Raw ptr not cleared when `try_borrow` fails | 88 |
| 2 | `strider-py/src/pattern.rs` | 351-359 | Misleading SAFETY + latent re-entrancy trap | 85 |
| 3 | `pcode-lift/src/lib.rs` | 143-152 | SAFETY understates preconditions | 80 |
| 4 | `strider-py/src/lib.rs` | 60-74 | SAFETY incorrectly claims set_var safe | 80 |
