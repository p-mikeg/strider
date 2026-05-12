# Round 7 — strider-py + dot/graphwalk/entity-utils/graphmock Audit

Independent review (code-only) of the Python bindings and small support crates.

---

## CRITICAL / HIGH

### C1 — `PyCapture::__hash__` is broken — HIGH (conf 95)
- **Where:** `crates/strider-py/src/pattern.rs:52-55`
- **Evidence:** Hash is computed as `format!("{:?}", self.inner).len() as isize`. `Capture` debug-formats as `Capture(N)` so the hash is the *string length of the decimal id*. All ids 10–99 hash to 11, ids 100–999 hash to 12. Python `dict`/`set` keyed on `Capture` degrade to O(n) and produce false-positive equality buckets.
- **Fix:** Hash the underlying `u32` id: `self.inner.as_u32() as isize` (or expose a stable id getter on `pattern::Capture`).

### C2 — `PyPat::ordered()` silent no-op — HIGH (conf 85)
- **Where:** `crates/strider-py/src/pattern.rs:432-438`
- **Evidence:** Method body `self.clone()`. Free-fn ctors (`pattern.add(x,y)`) return finalized `PyPat`; commutativity baked in at construction. Calling `.ordered()` after that has no effect, contrary to user expectation.
- **Fix:** Raise `PatternError("ordered() is only supported on typed builders (int_binary, bool_binary, float_binary)")`. Same finding surfaced in pattern audit.

---

## MEDIUM

### M1 — GIL not released during `strider.run` — MED (conf 80)
- **Where:** `crates/strider-py/src/run.rs:57-177`
- **Evidence:** No `py.allow_threads(...)` call anywhere in strider-py. Whole CFG/lift/optimizer chain holds the GIL. Other Python threads block for seconds on large functions.
- **Fix:** Wrap pure-Rust path (`MemoryMap` fast path) with `py.allow_threads`. Callback `MemReader` path must keep GIL.

### M2 — `float_is_nan` always raises despite being in public-API snapshot — MED (conf 90)
- **Where:** `crates/strider-py/src/pattern.rs:881-885`
- **Evidence:** Registered + listed in `EXPECTED_PATTERN` (`tests/python/test_public_api_snapshot.py:113`); body raises `PatternError("not yet implemented")`. Snapshot passes; functional use fails.
- **Fix:** Either implement (an `IntCmpOp::Equal(x,x).bool_neg()` rewrite is essentially the IR shape; the IR has no `FloatIsNan` node, but the lifter lowers `FloatNan(x) → BoolNeg(FloatEqual(x,x))` per pcode-lift's `float.rs:78-90`), or remove from the public surface. Recommend implementing as `bool_neg(float_eq(x, x))`.

### M3 — `Graph.node_kind()` / per-node introspection absent — MED (conf 85)
- **Where:** `crates/strider-py/src/graph.rs`
- **Evidence:** No `node_kind(id)`, `node_outputs(id)`, `node_inputs(id)`, `asm_fingerprint(node_id)` on `PyGraph`.
- **Fix:** Add these accessors. Return enum variants as Python strings or as a typed `NodeKindRepr` class.

### M4 — `validate_with_options` not exposed — MED (conf 85)
- (Cross-listed with ir audit PY-1.)

### M5 — `GraphRewriter` not exposed as Python class — LOW (conf 80)
- Existing `Graph.rewrite()` / `Graph.rewrite_all()` is sufficient for common cases; gap is for advanced use.

### M6 — `Cfg` region introspection absent — LOW (conf 80)
- No region iteration / per-region instruction inspection from Python.

---

## LOW

### L1 — Stale TODO in `pattern.rs` module comment — LOW (conf 90)
- **Where:** `crates/strider-py/src/pattern.rs:20-21`
- **Evidence:** Comment says "TODO: op-variant accessors are not yet exposed". They ARE implemented at `matcher.rs:119-181` (`int_binary_op`, `int_unary_op`, `int_cmp_op`, `bool_binary_op`, `bool_unary_op`, `float_binary_op`, `float_unary_op`, `float_cmp_op`).
- **Fix:** Remove the stale TODO.

### L2 — `_unused_marker` dummy + unused `Mutex` import — LOW (conf 90)
- **Where:** `crates/strider-py/src/reader.rs:686-689`
- **Fix:** Delete dummy function and any unused `use std::sync::Mutex;` in reader.rs.

### L3 — `PyCfg::inner()` is dead code — LOW (conf 85)
- **Where:** `crates/strider-py/src/cfg.rs:26-29`
- **Fix:** Remove (no callers; `#[allow(dead_code)]` doesn't make it useful).

### L4 — `CastMask::empty()` not registered as classmethod — LOW (conf 80)
- Only `none()` exposed; CLAUDE.md mentions both. Add `empty` as alias.

### L5 — README minor inaccuracy — LOW
- `BuildingBlocks` section ordering of `Strider`/`build_cfg` doesn't warn about Sleigh consumption.

---

## Verified-Correct (no issues found)

### A. errors.rs converters
- All 5 converters (`into_strider_err`, `into_lift_err`, `into_reader_err`, `into_pattern_err`, `into_rewrite_err`) used at 109 call sites across 11 files. `#[allow(dead_code)]` is a valid lint-suppression artifact (lint fires inside private modules even when used through `crate::errors::into_*`).
- `format!("{e:?}")` correctly captures the anyhow caused-by chain.
- Typed subtypes `UnresolvedIndirectBranchError` / `UnknownCallOtherError` correctly downcast before fallback.
- All public PyO3 functions route errors through one of the 5 converters; no panic-to-abort path.

### B. unsafe blocks
- `unsafe { std::env::set_var(...) }` at `lib.rs:59-75` — single-threaded under Python's import lock; documented; theoretical race only with other native extensions.
- `unsafe { &*ptr }` at `pattern.rs:278-286` — null-checked; pointer cleared after each predicate call; `unsendable` marker prevents cross-thread UB.

### C. PyO3 boundary
- `PyPartialMatch` `unsendable` + `Mutex<Option<*const ...>>` correct.
- Capture intern table is global-per-process. Documented; intentional for shared captures across `find_all_requirements`.

### D. dot crate
- `json_quote` escapes `<` to `<`, preventing `</script>`-style XSS in HTML output.
- `escape_dot_label` escapes `"` and `\`.
- `DotEmitter::extra` attribute caller-responsibility documented; current callers all use fixed strings.

### E. graphwalk
- `GraphRef::try_successors` / `successors` clean; `&G` blanket impl.
- Cycle handling via `DenseEntitySet`/`CompoundBitSet` visited tracking.
- `entity_preorder` / `entity_postorder` re-entrancy safe.
- No production panics (`#![no_std]`, no unwrap/expect).

### F. entity-utils
- `DenseEntitySet` capacity grows automatically via `CompoundBitSet`.
- `Worklist` FIFO + dedup semantics correct.
- No issues.

### G. graphmock
- Used only by `graphwalk/tests/{preorder,postorder}.rs`.
- Panics on malformed input — documented test-only DSL.
- **Recommendation:** Move into `graphwalk/tests/common/graph.rs` as inline module and remove the standalone `graphmock` crate (saves a Cargo.toml entry + crate-boundary indirection).

### H. Python tests
- 47 test files; substantive (asm-fingerprint assertions, backtrace child-process tests, callback invocation counts, public-API snapshot).
- **Gap:** No tests deliberately trigger `PatternError`, `RewriteError`, `UnknownCallOtherError` to verify correct typed exception is raised (vs falling back to generic `StriderError`). If `into_*_err` downcast is broken, current tests miss it.

---

## Top 5 Findings

1. **C1 (HIGH)** `PyCapture::__hash__` collides catastrophically — `dict`/`set` keyed on Capture is broken.
2. **C2/M2 (HIGH)** `PyPat::ordered()` and `float_is_nan` are silent traps — exposed but non-functional.
3. **M1 (MED)** `strider.run` holds GIL for entire analysis — blocks other Python threads for seconds.
4. **M3/M4 (MED)** Per-node IR introspection (`node_kind`, `node_outputs`, `asm_fingerprint(node_id)`, `validate_with_options`) entirely absent from Python.
5. **L1/L2/L3 (LOW)** Three dead-code / stale-TODO / unused-import items in strider-py — easy removals.
