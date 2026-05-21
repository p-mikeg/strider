# Round 13 — Ask-8 pass 3: concurrency / aliasing / borrowing audit

Branch: `review/ai7`.

## Verdict

**No HIGH findings.  3 R12 LOW notes (B-1, B-2, B-3) carry forward unchanged; zero new issues across all 10 categories.**

## R12 LOW notes — continuity check

**B-1 (carry-forward unchanged)** — `crates/strider-py/src/pattern.rs:364` SAFETY comment names `&BuiltFunctionGraph` but actual pointer is `*const ir::Graph`.  Runtime invariant correct; doc only.

**B-2 (carry-forward unchanged)** — `crates/strider-py/src/pattern.rs:361-369` `Mutex` guard held across closure call.  Currently unreachable (all callers pass pure-Rust closures); future Python-callback closure would deadlock the non-reentrant `std::sync::Mutex`.

**B-3 (carry-forward unchanged)** — `crates/strider-py/src/lib.rs:71-73` `unsafe { std::env::set_var("RUST_LIB_BACKTRACE", "1") }` at module init.  SAFETY comment acknowledges acknowledged-data-race risk on glibc with multi-threaded Rust extensions.

## Categories verified clean

✓ **`&mut Graph` across cache-mutating calls** — `retain_reachable` pre-collects `Vec<NodeId>` before any mutation.  `extend_asm_fingerprint_from` clones source before borrowing dst.

✓ **`&mut SecondaryMap` while Graph mutated** — all side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`) mutate only through `&mut self` on `Graph`.

✓ **PrimaryMap iteration while inserting** — `FunctionBuilder::set_entry_region`/`create_region` pre-collect `var_ids: Vec<_>` before loops.

✓ **PyO3 callback re-entry inside `&mut Graph` lift** — `PyMemReaderAdapter::read` / `PyReadOnlyMemoryAdapter::read` call Python via `with_gil` inside `rsleigh::Sleigh::lift_one`, BEFORE any `&mut Graph` is borrowed in `IrStrider`'s region loop.  Only `addr`/`size` scalars cross the boundary.

✓ **Interior mutability re-entry** — no `RefCell` in production source.  The only `OnceLock` is the string-capture intern table (`pattern.rs:122-124`) guarded by Mutex released before return.

✓ **Arc cycles** — `Pat` = `Arc<dyn Pattern>` composed top-down; `GuardPat`'s `inner: Pat` is a child, no back-reference.  `PyPat` wraps `Arc<pattern::Pat>` without cycles.  `DecodeCache` is `Arc<Mutex<FxHashMap<...>>>` (plain shared cache).

✓ **`unsafe` blocks needing SAFETY** — exactly three unsafe in production: `pattern.rs:367` (B-1 above), `pcode-lift/src/lib.rs:152` (correct SAFETY), `strider-py/src/lib.rs:71` (B-3 above).  All three commented.

✓ **Iterator invalidation in `OptimizerPipeline::run`** — iterates `&self.optimizers` (immutable Vec).  Passes pre-collect candidates before mutation (e.g., `RedundantPhis` collects `Vec<NodeId>` at `redundant_phis/mod.rs:176`).

✓ **`SecondaryMap` keyed on stale NodeId after detach** — zombies remain in arena; `NodeId` still valid as SecondaryMap key.  Reachability-scoped validator skips zombies.  `retain_reachable` remaps all four side-tables at `compact.rs:194-228`.

✓ **`PyAny` lifetimes across `Python::with_gil` boundaries** — `Py<PyAny>` (owned, GIL-independent) upgraded to `Bound<'_, PyAny>` inside `with_gil` before method calls.  In `run.rs:166`, GIL is released via `py.allow_threads` only after `Py<>` borrows are dropped.

## Files reviewed

- `crates/ir/src/graph/{store,compact,uses,mod}.rs`
- `crates/ir/src/builder/{mod,vars,call}.rs`, `crates/ir/src/walk.rs`
- `crates/opt/src/{pipeline,worklist,redundant_phis/mod}.rs`
- `crates/strider/src/orchestrator.rs`
- `crates/strider-py/src/{pattern,reader,run,lib,graph,matcher}.rs`
- `crates/pcode-lift/src/lib.rs`
- `crates/cfg/src/cfg/decode_cache.rs`
