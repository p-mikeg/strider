# Round 12 — 2C: silent-failure audit (post-W6/W2 verification)

## Scope and methodology

Re-walked every `unwrap_or` / `unwrap_or_default` / `unwrap_or_else` /
`.ok()?` / `.ok()` / `if let Ok(...)` (with implicit-`Err` discard) /
`let Ok/Some(...) else` / `Err(_) =>` / `let _ = ...` occurrence across
`crates/*/src/` outside of tests.

Then verified the **round-11 W6** (mem-chain malformed shape `Err`)
and **round-11 W2** (`KeyboardInterrupt` / `SystemExit` re-raise in
`PyMemReaderAdapter::read` and `PyReadOnlyMemoryAdapter::read`)
fixes are in place at the sites round 11 flagged as SWALLOWED-BUG
(F1 and F2) and at the Python callback boundary respectively.

## Headline

**All round-11 SWALLOWED-BUGs are fixed.  No new silent failures
introduced between c4127ca and c7a2903 (round-11 W6 → round-12 head).
Every remaining `unwrap_or` / `.ok()?` / `if let Ok` / `let _ =` site
is a documented INTENTIONAL-FALLBACK with a justification already
inline.**

## Fix verification

### W6.F1 — `mem_chain_is_dirty` malformed-shape Err propagation

- `crates/opt/src/function_args/mod.rs:501-509` — empty `MemPhi` now
  returns `Err` with a descriptive message instead of pushing `false`
  to the result stack.
- `crates/opt/src/function_args/mod.rs:515-528` — `Call` /
  `CallOther` with fewer than 2 inputs now returns `Err` with a
  descriptive message naming the node kind.
- Both messages prefix the error with `mem_chain_is_dirty:` so a
  caller scanning stderr can locate the failing site immediately.
- The result-stack-length and result-stack-pop walker-bug `Err`s at
  lines 545-558 are also in place (round 11 already had these,
  re-verified here for completeness).
- **Verdict:** F1 is fully resolved.

### W6.F2 — `apply_in_place_edits` InitialVar arity propagation

- `crates/strider/src/orchestrator.rs:581-597` — replaced
  `if let Ok([out]) = …` with `let [out] = …?` plus `map_err` that
  decorates the error as
  `"apply_in_place_edits: InitialVar({existing:?}) has wrong output arity (expected 1): …"`.
- The comment block above (lines 574-580 then 584-588) now spells out
  why a non-1 arity is a shape bug and what would go wrong if it
  silently fell through (zombie resurrection breaking
  `FunctionArgDetect`'s post-detection invariant).
- **Verdict:** F2 is fully resolved.

### W2 — Python control-flow exception re-raise

- `crates/strider-py/src/reader.rs:496-518` — `PyMemReaderAdapter::read`
  Python-callback error path now matches `e.is_instance_of::<PyKeyboardInterrupt>()`
  and `PySystemExit>()` and re-raises (`return Err(e)`) instead of
  converting to a soft `None`.  Ctrl-C during a long lift now exits
  cleanly.
- `crates/strider-py/src/reader.rs:589-614` — same shape applied to
  `PyReadOnlyMemoryAdapter::read`.
- **Verdict:** W2 is fully resolved.  Other Python exceptions still
  convert to `None` per the trait API (`read -> Option<u64>` /
  `Option<&[u8]>`), with an `eprintln!` recording the exception for
  visibility.

## Summary table

Counts are over `crates/*/src/` (test-only files excluded).

| Crate         | unwrap_or | .ok()? | if-let-Ok | let _ =  | Verdict       |
|---------------|-----------|--------|-----------|----------|---------------|
| cfg           | 2 (mtx)   | 0      | 0         | 0        | all-intent.   |
| dot           | 0         | 0      | 0         | 1 (doc)  | all-intent.   |
| entity-utils  | 1         | 0      | 0         | 0        | all-intent.   |
| graphwalk     | 0         | 0      | 0         | 2 (CF)   | all-intent.   |
| ir            | 4 (1 dot lab, 2 dot dump, 1 sig-test-panic) | 0 | 0 | 0 | all-intent. |
| opt           | 2 (KB)    | 7      | 2         | 2 (rslt) | all-intent.   |
| pattern       | 0         | 0      | 0         | 2 (mac.) | all-intent.   |
| pcode-lift    | 0         | 0      | 0         | 0        | clean         |
| reader        | 0         | 1      | 0         | 0        | all-intent.   |
| strider       | 3         | 0      | 0         | 1 (term) | all-intent.   |
| strider-py    | 9         | 1      | 4         | 0        | all-intent.   |
| target        | 9 (panic) | 0      | 0         | 0        | assertive     |

Legend: `mtx` = mutex-poison `into_inner` recovery, `CF` = discarding
`ControlFlow::Break` in non-short-circuiting wrapper, `mac.` = macro
hygiene `let _ = &$hy`, `term` = terminal-builder return-value
discard (errors propagate through `?`), `KB` = KnownBits u64
mask-narrowing, `rslt` = `OptimizationResult` (not a `Result`)
intentionally dropped (documented in `worklist.rs:81`), `sig-test-panic` =
`unwrap_or_else(|| panic!(…))` is assertive, not silent.

## Findings (verdict-only, with code references)

### Resolved since round 11

| ID | Where | Verdict | Status |
|----|-------|---------|--------|
| F1 | `opt/function_args/mod.rs:501-528` | was SWALLOWED-BUG | **FIXED in W6** (Err with descriptive message) |
| F2 | `strider/orchestrator.rs:581-597`  | was SWALLOWED-BUG | **FIXED in W6** (`?` + `map_err`) |
| W2 | `strider-py/reader.rs:496-518, 589-614` | was SWALLOWED-BUG (Ctrl-C) | **FIXED in W2** (instance-of dispatch + re-raise) |

### Still present, classified INTENTIONAL-FALLBACK (no action needed)

All sites below carry inline documentation or a comment block
explaining the fallback.  Re-flagging or reclassifying any of these
would just churn the same conclusions.  Listed for completeness so
the next round can sanity-check the set hasn't grown.

| ID | Where | Discarded type | Why intentional |
|----|-------|----------------|-----------------|
| K1 | `cfg/decode_cache.rs:60, 66` | `PoisonError<MutexGuard<HashMap>>` | `into_inner` recovery — single-slot map, panic mid-write can't tear an `Arc<LiftRes>` (doc comment lines 54-57). |
| K2 | `dot/lib.rs:202` | `fmt::Result` | Documented infallibility of `String::write_str` (lines 197-201). |
| K3 | `entity-utils/set.rs:126` | none — `size_hint` upper-bound `None` | `FromIterator` standard idiom: when upper is `None`, fall back to lower bound. |
| K4 | `graphwalk/lib.rs:48, 79` | `ControlFlow<()>` | Wrapper deliberately ignores `Break` since callers passed `Continue(())`-only closures. |
| K5 | `ir/dot/label.rs:279` | `Option<&'static str>` (call-other name unset) | Synthetic graphs (tests / third-party builders) omit the name side-table; renderer must remain total (comment lines 271-274). |
| K6 | `ir/dot/label.rs:304, 322` | `io::Error` from `pretty_label` | Dot rendering must never fail; if label generation fails, fall back to `{kind:?}`.  Render is non-functional debug output. |
| K7 | `ir/node_signature.rs:811` | `unwrap_or_else(|| panic!(…))` | Test asserting every variadic-tail signature is set; the panic is the assertion. |
| K8 | `opt/known_bits/mod.rs:88, 223, 264` | `TryFromIntError` (>u64 type masks) | KB tracker is fundamentally `u64`; > u64 types are out of model (doc lines 80-84, 312). |
| K9 | `opt/indirect_branch_resolve/mod.rs:166` | `IrError` (IndirectBranch arity) | Signature-guaranteed shape; comment lines 162-165 names the invariant. |
| K10 | `opt/indirect_branch_resolve/jump_table.rs:309, 641, 665, 713` | `TryFromIntError` / `IrError` | Bound-fits-in-u64 / trivial-phi-classification (doc inline). |
| K11 | `opt/indirect_branch_resolve/stack_array.rs:104-105, 322, 517` | `TryFromIntError` / `IrError` | Stride/offset width and Load arity (signature-guaranteed). |
| K12 | `opt/sp_expr.rs:215, 243` | `TryFromIntError` (>i64 stack offset) | Stack offsets must fit in `i64` to be storable in `Graph::stack_phi_offsets`. |
| K13 | `opt/worklist.rs:81-85` (consumed at `function_args/mod.rs:108`, `redundant_phis/mod.rs:196`) | `OptimizationResult::Changed` | Bookkeeping discard, doc-commented. |
| K14 | `reader/lib.rs:188` | `TryFromIntError` (32-bit `usize`) | API is `Option<usize>`; out-of-range is the documented `None` case. |
| K15 | `reader/elf.rs:520-524, 567-571, 590, 823` | `object::Error` | Bucketed into typed `RelocationStats::skipped_*` counters; documented at the call sites (e.g. lines 555-560 for the symtab fallback, 810-816 for the autoload parse-failure counter). |
| K16 | `strider/orchestrator.rs:785` | `Option<&BuiltCallingConvention>` (no override) | `or_else` to function-default CC; documented at lines 781-783. |
| K17 | `strider/pipeline.rs:324, 332` | `Option<…>` (no all_vns; empty CFG → max idx 0) | Defaults to a fresh CFG scan / a 1-slot region map. |
| K18 | `strider/strider/insn/mod.rs:141` | `NodeId` from `build_call_other_terminal` | Terminal builder; side effect is the intent.  Error propagates via `?`. |
| K19 | `cfg/builder/region_builder.rs:407` | `TryFromIntError` (user-op id > u32) | Defers to IR-layer strict-on-emission check (comment lines 393-397). |
| K20 | `cfg/builder/indirect_resolve.rs:243` | none — classifier `Ok(None)` for unclassifiable producers | Documented at lines 241-243: not an error, just "defer to strider's outer loop." |
| K21 | `strider-py/dot.rs:9`, `strider-py/cfg.rs:64, 75`, `strider-py/graph.rs:95, 122` | unknown style name → "dark" | Documented at `dot.rs:6-7, 12`.  Acceptable for a debug-only renderer; could be tightened to a typed enum if Python users typo style names often (round-11 F13). |
| K22 | `strider-py/reader.rs:143, 703` | `PoisonError<RwLockReadGuard<Arc<…> / Endianness>>` | `into_inner` recovery — slot is atomic-pointer / `Copy`, doc lines 135-141 and 697-702 respectively. |
| K23 | `strider-py/reader.rs:688` | `anyhow::Error` from `lookup_table()` | Trait API is `ReadOnlyMemory::read -> Option<u64>`; cannot propagate `Result`.  Round-11 F3, still constrained by the trait. |
| K24 | `strider-py/reader.rs:728, 763` | `PyDowncastError` | Standard PyO3 polymorphic-dispatch try-then-fallback (`PyMemoryMap` first, then `hasattr("read")`-based duck-type). |
| K25 | `strider-py/pattern.rs:342, 362` | `PoisonError<MutexGuard<Option<*const Graph>>>` | `into_inner` recovery — single-slot `Option<*const _>`, doc lines 333-360. |
| K26 | `strider-py/pattern.rs:388, 391` | `PyDowncastError` | Standard PyO3 polymorphic dispatch (`Capture` then `String`). |
| K27 | `strider-py/run.rs:70` | `Option<HashMap<u64, PyCallingConvention>>` (kwarg) | Python optional-kwarg default to empty map. |
| K28 | `target/call_other_abi.rs:399, 418, 495, 544, 558, 604, 612, 732, 774, 832` | `unwrap_or_else(\|\| panic!(…))` | All in tests; the panics are the assertion. |
| K29 | `pattern/macros.rs:177, 181` | none — macro-hygiene `let _ = &$hy` | Suppresses unused-binding warning in macro-expanded code. |

## Recommendations

**No HIGH or MED severity findings remain.**

LOW-severity polish that could be done if and only if a future
iteration genuinely needs the headroom:

- **K21** — tighten `dot_style_for` to a typed enum so typos raise
  TypeError on the Python side rather than silently defaulting to
  `dark`.  Not load-bearing — the rendering is debug-only.
- **K15** — stash the first N `object::Error` strings into
  `RelocationStats` so a caller seeing `autoload_section_parse_failures
  > 0` has something to print.  Round-11 F4 suggestion; still
  beneficial-but-not-needed.

The hot orchestrator paths (`classify_anchor`, `apply_link_register`,
`apply_tail_call`, `mem_chain_is_dirty`, `apply_in_place_edits`)
all propagate `Err` for graph-shape violations and use `Ok(None)`
exclusively for the documented "this iteration cannot decide; defer"
semantics that the fixed-point loop is built around.  The
`pattern`, `pcode-lift`, and `target` (non-test) crates remain
silent-failure-clean.
