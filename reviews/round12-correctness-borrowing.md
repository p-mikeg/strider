# Round 12 — Ask 8 pass 3: concurrency / aliasing / borrowing audit

Branch: `review/ai6` · Trust model: strict (no prior-round reviews consulted; no other round-12 reports read).

## Verdict

**No HIGH-confidence borrowing/concurrency bugs.** Three LOW findings (one comment, one fragile-but-documented reentrancy contract, one acknowledged-data-race).

## Findings

### B-1 — SAFETY comment names wrong type in `PyPartialMatch::with_graph`
- **Severity:** LOW
- **Where:** `crates/strider-py/src/pattern.rs:364-367`
- **Borrow shape:** `Mutex<Option<*const ir::Graph>>` is dereferenced as `&ir::Graph`; safety comment says `&BuiltFunctionGraph`.
- **Trigger scenario:** Not a runtime issue — the invariant is upheld correctly. The pointer is set by `PyPartialMatch::new` which receives `graph: &ir::Graph`, and cleared by `clear_graph_ptr` before the predicate returns. The `Mutex` guard remains live while `f(graph_ref)` runs, preventing cleanup from racing. The only problem is the SAFETY comment's type name (`BuiltFunctionGraph`) does not match the actual pointer type (`ir::Graph`). A future reader relying on the comment to reason about the invariant might be confused.
- **Fix:** Change `&BuiltFunctionGraph` to `&ir::Graph` in the comment.
- **Regression test:** N/A (comment-only).

### B-2 — `PyPartialMatch::with_graph` holds `Mutex` guard across closure call
- **Severity:** LOW
- **Where:** `crates/strider-py/src/pattern.rs:361-369`
- **Borrow shape:** `let guard = self.graph_ptr.lock()…` remains live while `f(graph_ref)` executes. If any future caller passes a closure that calls back into Python (which could then call `uint()`/`int_()` on the same proxy), `std::sync::Mutex`'s non-reentrancy would deadlock the thread.
- **Trigger scenario:** Currently impossible because all callers of `with_graph` pass pure-Rust closures (`|g| self.bindings.get_uint(cap, g)` etc.). The contract is only enforced by documentation, not the type system. A future caller adding a Python callback would deadlock silently.
- **Fix:** Replace the `Mutex<Option<*const _>>` with `Cell<Option<*const _>>` (soundness-equivalent since `unsendable` prevents cross-thread sharing). `Cell::get` is not reentrancy-unsafe.
- **Regression test:** N/A (deadlock is not triggered by any existing call site).

### B-3 — `std::env::set_var` in `force_anyhow_backtrace_capture` has acknowledged data race
- **Severity:** LOW
- **Where:** `crates/strider-py/src/lib.rs:71-73`
- **Borrow shape:** `unsafe { std::env::set_var("RUST_LIB_BACKTRACE", "1") }` called during module init. A concurrent Rust thread from another already-loaded native extension that calls `std::env::var` simultaneously triggers UB on platforms where `setenv`/`getenv` are not thread-safe (glibc).
- **Trigger scenario:** Two native extensions loaded simultaneously from separate Python threads. Python's import lock serialises across CPython threads, but Rust threads spawned by another extension bypass the GIL entirely.
- **Fix:** Move the env-var write to process startup (not import time), or wrap in `OnceLock` with `AtomicBool` write-once. Alternatively accept the risk as documented (matches the convention used by many native Python extensions).
- **Regression test:** N/A without a multi-threaded native-extension harness.

## Categories verified clean

### ✓ 1. `&mut Graph` held across cache-mutating calls
`retain_reachable` (compact) pre-collects `reachable: Vec<NodeId>` before any arena mutation; all later loops iterate the pre-collected snapshot. The dedup-cache rebuild similarly pre-collects `all_node_ids: Vec<NodeId>` before inserting into `node_to_id`. `detach_unreachable_nodes` pre-collects `to_detach: Vec<NodeId>` before the mutation loop. No streaming iterator + concurrent insertion found.

### ✓ 2. `SecondaryMap` keyed on stale `NodeId` after compact/detach
`retain_reachable` explicitly remaps all four side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`) through the translation table at step 8 (`crates/ir/src/graph/compact.rs:194-228`). Entries for dropped nodes are dropped. After `detach_node_inputs` the node remains in the arena as a zombie — `NodeId` is still a valid `SecondaryMap` key — so stale side-table data is accessible but the validator scopes Layer A/C to reachable nodes only.

### ✓ 3. `PrimaryMap` iteration while inserting
`FunctionBuilder::set_entry_region` and `create_region` both pre-collect `let var_ids: Vec<_> = self.variables.keys().collect()` before the loop that calls `build_*` (which may insert into `self.graph.nodes`). `self.variables` itself is not modified inside the loop.

### ✓ 4. PyO3 callback re-entry during `&mut Graph` lift
`PyMemReaderAdapter::read` acquires GIL via `Python::with_gil` and calls `py_obj.call_method1`. This occurs during `rsleigh::Sleigh::lift_one`, called within `IrStrider`'s region loop. The `FunctionBuilder` is not reachable through the callback — only `addr`/`size` scalars are passed. No aliasing path back to the `Graph`. Same for `PyReadOnlyMemoryAdapter::read`.

### ✓ 5. `RefCell`/`Cell`/`OnceCell` re-entrant borrow
No `RefCell` usage anywhere in the crate tree. The only `OnceLock` is `intern_table()` for the string-capture intern map, accessed via a `Mutex` lock released before returning. No Python callbacks invoked while the intern table lock is held.

### ✓ 6. `Arc<RefCell<_>>` / `Arc` cycles in `Pat`
`Pat` is backed by `Arc<dyn Pattern>`. All construction is compositional and acyclic: `GuardPat` contains `inner: Pat` (child `Arc`) and `func: GuardFn`. No back-references. Pattern builder types contain `Pat` children built by-value before boxing.

### ✓ 7. `unsafe` blocks without safety comments
Three `unsafe` blocks; all three have SAFETY comments:
1. `crates/strider-py/src/pattern.rs:367` — wrong-type comment (B-1) but invariant stated.
2. `crates/strider-py/src/lib.rs:71` — data-race acknowledged (B-3).
3. `crates/pcode-lift/src/lib.rs:152` — states the precondition (`VnSpace::by_id` requires a valid Sleigh `AddrSpace` pointer, guaranteed by rsleigh for LOAD/STORE opcodes).

### ✓ 8. Iterator invalidation in `OptimizerPipeline::run`
The pipeline iterates `&self.optimizers` (immutable `Vec<Box<dyn OptimizerRaw>>`). Each pass receives `&mut ir::Graph` and processes nodes via the `WorkSet` pattern (pre-seeds from `preorder`, then drains). The `optimizers` Vec is never mutated during the loop. Individual passes use `WorkSet::seeded_kind` which calls `preorder_kind` to consume the iterator into the worklist before any mutation.

### ✓ 9. `SecondaryMap` keyed on stale `NodeId` after detach
`detach_node_inputs` removes all inputs but does not remove the node from `asm_fingerprints` / `call_other_names` / `stack_phi_offsets` / `call_clobbered_overrides`. Since zombie nodes remain in the `PrimaryMap` arena (their `NodeId` is still valid), the `SecondaryMap` key is not stale — it simply holds data for an unreachable node. `validate_with_options(check_asm_fingerprints: true)` correctly limits the Layer-C fingerprint check to reachable nodes.

### ✓ 10. `PyAny` lifetimes across `Python::with_gil` boundaries
`PyMemReaderAdapter` and `PyReadOnlyMemoryAdapter` both store `Py<PyAny>` (owned, GIL-independent strong reference). Both upgrade to `Bound<'_, PyAny>` inside `Python::with_gil` before calling methods. Correct PyO3 pattern. No `&'py PyAny` borrowed across `with_gil` boundaries.

## Files reviewed

- `crates/ir/src/graph/{store.rs,access.rs,compact.rs,uses.rs}`
- `crates/ir/src/builder/{mod.rs,nodes.rs,vars.rs}`
- `crates/ir/src/{function.rs,validate/*.rs}`
- `crates/opt/src/{pipeline.rs,worklist.rs,sp_expr.rs,**/mod.rs}`
- `crates/pattern/src/{matcher/*.rs,rewrite.rs,pat/*.rs,var.rs}`
- `crates/strider/src/{orchestrator.rs,strider/**/*.rs}`
- `crates/strider-py/src/{pattern.rs,reader.rs,run.rs,lib.rs,graph.rs}`
- `crates/pcode-lift/src/{lib.rs,vn_io.rs}`
- `crates/cfg/src/cfg/decode_cache.rs`

Cross-checked with `grep -rn 'unsafe' crates/*/src/` and `grep -rn 'RefCell\|OnceLock\|Mutex' crates/*/src/`.
