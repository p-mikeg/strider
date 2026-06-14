# Deep audit — `strider-py` (PyO3 bindings)

Date: 2026-06-14
Scope: `crates/strider-py/src/*.rs` (Program/ElfStrider/Analysis high-level API,
`node_builder!`/`binary_op_builder!` macros generating `Py*Pat` builders in
`pattern.rs`, `opt.rs` per-pass classes, error mapping to `StriderError`,
BufferReader, `_LoadedElf`), plus the Python facade `strider/_api.py` /
`strider/__init__.py` and the test-suite shape under
`crates/strider-py/tests/python/`. READ-ONLY — no source/test edited.

Method: read the core files directly; dispatched three parallel read-only
sub-audits over `pattern.rs`, `reader.rs`+`run.rs`, and
`function.rs`+`matcher.rs`+`node.rs`; personally re-verified every HIGH/MED
finding against the actual code (and the Rust counterpart it mirrors) before
recording it.

Headline: the soundness-critical paths are in good shape. The stale-handle
contract (generation snapshot + `assert_generation` before any arena deref) is
sound and uniformly enforced; no reachable panic-across-FFI on Python-supplied
input was found in the handle/optimizer/error layers. The findings below are two
genuine wrong-binding / control-flow bugs, several mirror/doc-drift gaps, and
simplification opportunities.

---

## Severity counts

- HIGH: 1
- MED: 4
- LOW: 5

---

## HIGH

### PY-1 — `PyMemReaderAdapter::read` uses `PyErr::restore` for control-flow exceptions; can lose KeyboardInterrupt and/or trip CPython's "exception already set" guard
- Dimension: SOUNDNESS (binding correctness / silent failure of interrupt)
- Severity: HIGH · Confidence: HIGH
- Location: `crates/strider-py/src/reader.rs:441-450` (compare the correct sibling at `reader.rs:528-543`)
- What & why: The two reader callback adapters were meant to be mirror images
  (comment at `reader.rs:432-434` claims they mirror). They diverge.
  `PyReadOnlyMemoryAdapter::read` correctly **stashes** a KeyboardInterrupt /
  SystemExit into the thread-local `PENDING_CONTROL_FLOW` cell
  (`crate::pattern::stash_pending_control_flow(e)`, `reader.rs:539`), peeks the
  cell at the top of the next call (`peek_pending_control_flow()`,
  `reader.rs:520`), and lets the outer `strider.run` boundary drain + surface it.
  `PyMemReaderAdapter::read` (the Sleigh instruction-fetch path) instead calls
  `e.restore(py)` (`reader.rs:444`) and then `anyhow::bail!`s. The restored
  CPython error indicator is set while still inside `with_gil`, and the read
  error is wrapped as `strider_reader::MemReadError`
  (`reader.rs:466`) — which flows into rsleigh's lift machinery where instruction-
  fetch failures are routine and become lift diagnostics. Two consequences:
  (1) the next PyO3 call into Python during lifting can trip the "called a
  function with an exception already set" guard (the exact failure the
  ReadOnlyMemory path was rewritten at `reader.rs:534-538` to avoid);
  (2) if rsleigh converts the read error to a generic lift failure, the
  KeyboardInterrupt is downgraded to a `StriderError` message — the interrupt is
  lost and Ctrl-C during a long instruction-fetch loop does not interrupt.
- Proposed fix: make `PyMemReaderAdapter::read` mirror
  `PyReadOnlyMemoryAdapter::read` exactly — on a control-flow exception call
  `crate::pattern::stash_pending_control_flow(e)` (not `e.restore(py)`), and add
  a `peek_pending_control_flow()` short-circuit at the top. The `run` boundaries
  already drain via `check_pending_control_flow()` (`run.rs:370, 488`).

---

## MED

### PY-2 — `PyCallOtherPat.ctrl()` / `.mem()` route to the value-input slot, producing patterns that can never match
- Dimension: SOUNDNESS (macro/hand-written builder consistency; wrong binding)
- Severity: MED · Confidence: HIGH
- Location: `crates/strider-py/src/pattern.rs:2656-2665`; macro replay at
  `pattern.rs:2206-2217`; Rust counterpart `crates/strider-pattern/src/node_builders/flow.rs:159-175`
- What & why: The Python convenience methods push onto the value-`args` vec:
  ```rust
  fn ctrl(...) { slf.inner.borrow_mut().args.push((0, p)); slf }  // slot 0
  fn mem(...)  { slf.inner.borrow_mut().args.push((1, p)); slf }  // slot 1
  ```
  Those `args` entries are replayed by the `multi_match args: arg` macro arm
  (`pattern.rs:2215`) as `b.arg(idx, compile_operand_match(...))`, i.e. the Rust
  `CallOtherPat::arg` → `self.inner.input(idx, p)` (a **value/match** input,
  `flow.rs:159-160`). But the IR signature is `CallOther: [control, memory,
  ...args]` (`crates/strider-ir/src/node_signature.rs`), and the Rust builder's
  own `ctrl`/`mem` deliberately use `input_control(0,p)` / `input_mem(1,p)`
  (`flow.rs:166-174`). `input_control` flips the operand's expected output to a
  Control edge so `output_ok` accepts it; the value path does not. A Python
  `call_other().ctrl(p)` / `.mem(p)` therefore fails `output_ok` and never
  matches; `.mem()` additionally uses the match compiler instead of the memory
  compiler. The dedicated methods are dead. (The test suite sidesteps them:
  `tests/python/test_pattern_full_builders.py:130` documents using `arg(0)`/
  `arg(1)` for ctrl/mem — so the broken methods are simply untested.)
- Proposed fix: give `PyCallOtherPat` dedicated ctrl/mem storage compiled via the
  control-relaxed match compiler and `compile_operand_mem`, calling the Rust
  `CallOtherPat::ctrl` / `::mem` (not `arg`). Add a regression test
  `test_call_other_ctrl_mem_methods_match` that builds a CallOther pattern with
  `.ctrl(...)` / `.mem(...)` and asserts it matches a real lifted CallOther.

### PY-3 — Native-recursion depth guard is bypassed by pure typed-builder nesting (stack-overflow abort across FFI)
- Dimension: EDGE CASES / SOUNDNESS (process abort on adversarial input)
- Severity: MED · Confidence: HIGH (mitigated by CPython's construction-time recursion limit)
- Location: guard entered only at `crates/strider-py/src/pattern.rs:546`
  (`compile_repr_match`) and `:803` (`compile_repr_template`); the typed-builder
  compile path (`core_builder`/`compile_value`, e.g. `:2344`, `:2424`, `:2545`,
  `:2986`) never calls `DepthGuard::enter()`.
- What & why: The module doc (`pattern.rs:486-500`) claims the guard "covers both
  the direct recursion and the indirect recursion through nested `Pat` operands."
  It does not for patterns nested purely through typed builders —
  `load(addr=load(addr=load(...)))` or deeply chained `int_binary(...)` recurse
  `compile_operand_match` → `compile_value` → `core_builder` →
  `compile_operand_match` entirely outside `compile_repr_*`, so `COMPILE_DEPTH`
  is never incremented and a deep-enough chain overflows the Rust stack →
  `abort()`. Partially mitigated because CPython limits construction recursion,
  but each typed-builder level is only ~1-2 Python calls, so a moderate Python
  recursion budget can still build a chain deep enough to overflow during
  compile. Stale doc contradicting code = finding.
- Proposed fix: increment the depth counter inside `compile_operand_match` /
  `compile_operand_mem` (every recursion funnels through them), or enter a
  `DepthGuard` at the top of each `core_builder`/`compile_value`. Test:
  `test_deeply_nested_typed_builder_raises_not_aborts` building a 100k-deep
  `load(addr=...)` chain and asserting `StriderError`, not a crash.

### PY-4 — `PyMemReaderAdapter::read` silently truncates an over-long Python return; the ReadOnlyMemory sibling correctly errors
- Dimension: SOUNDNESS (silent wrong-length read)
- Severity: MED · Confidence: HIGH
- Location: `crates/strider-py/src/reader.rs:462-464` (vs strict check at `:551-556`)
- What & why: `let n = bytes.len().min(out_buf.len()); out_buf[..n].copy_from_slice(&bytes[..n]); Ok(n)`.
  A `MemReader.read` subclass returning *more* bytes than requested has the extra
  silently dropped; the `ReadOnlyMemory` adapter (`reader.rs:551`) rejects
  `bytes.len() != size`. The `MemReader` contract legitimately allows *short*
  reads near region edges, but an *over-long* return masking a Python bug gets no
  signal. Divergence between two sibling adapters.
- Proposed fix: reject `bytes.len() > out_buf.len()` with a clear error before
  copying; keep short-read tolerance.

### PY-5 — `strider.run(..., rom=..., pipeline=p)` mutates the caller's pipeline object via `prepend_load_read_only`
- Dimension: SOUNDNESS (state mutation of a Python-supplied handle / drain-on-use footgun)
- Severity: MED · Confidence: MED
- Location: `crates/strider-py/src/run.rs:408-410`; mutation impl
  `crates/strider-py/src/opt.rs:231-236`
- What & why: When `rom` is supplied, `pipeline.prepend_load_read_only()?`
  inserts a `LoadReadOnly` pass into the caller's borrowed `&PyOptimizerPipeline`
  before `drain_into_pipeline()`. Combined with the documented "drained on use"
  semantics, this mutates a user-visible object; calling `run` twice with the
  same `p` and a rom would prepend twice (or fail "already drained"). The
  orchestrator path (`run_via_orchestrator`, `run.rs:359`) passes `None` and
  builds `default_pipeline()` fresh, so it is unaffected — only the low-level
  `pipeline=` path mutates caller state.
- Proposed fix: snapshot the pipeline state into a fresh local `PipelineState`
  (the `snapshot_from`/clone machinery already exists in `opt.rs:130`) and
  prepend on the copy, leaving the caller's object untouched; or document the
  mutation explicitly.

---

## LOW

### PY-6 — Failed `optimize`/`reoptimize` leaves a half-mutated graph whose handles aren't invalidated
- Dimension: SOUNDNESS (stale read after surfaced error; not UB)
- Severity: LOW · Confidence: HIGH
- Location: `crates/strider-py/src/function.rs:383-401` (optimize), `:406-422` (reoptimize)
- What & why: `bump_generation()` runs only **after** a successful
  `pipeline.run(...)?`. If a pass mutates the arena in place (ids stay valid, no
  compaction) and a later pass errors, the method returns `StriderError` but the
  generation is unchanged, so outstanding `Match`/`Node` handles silently read
  the partially-rewritten graph instead of failing their staleness guard. Not
  UB/panic (ids still live) — a stale read of a partially-optimized graph after a
  surfaced error. Contrast `apply_rules_count_on` (`function.rs:709`) which bumps
  after the rewrite regardless.
- Proposed fix: bump the generation unconditionally (before the `?`) in
  `optimize`/`reoptimize`.

### PY-7 — Docstrings reference a non-existent `FunctionArg` node kind
- Dimension: SIMPLIFICATION / doc-drift (user-visible Python docstrings)
- Severity: LOW · Confidence: HIGH
- Location: `crates/strider-py/src/function.rs:310-311` (`asm_fingerprint` doc) and
  `crates/strider-py/src/matcher.rs:265` (`vn` doc)
- What & why: Both name a `FunctionArg` kind. Per the IR model there is no
  `FunctionArg` `NodeKind` variant — register args are `InitialVar`, stack args
  are `Load`, tracked in `arg_index_to_values`. These are user-facing docstrings
  naming a kind that can never appear. The `vn` accessor's real sources are
  `InitialVar` and tagged `Phi` (as `strider-pattern/src/match_result.rs:88-112`
  implements).
- Proposed fix: drop the `FunctionArg` mentions.

### PY-8 — High-level API doc drift: CLAUDE.md/spec describe `Program`/`Analyzer`/`pipeline_factory`; the actual facade is `ElfStrider`/`load_elf`/`strider`/`Analysis` with no per-call pipeline override
- Dimension: SOUNDNESS — code vs its documented contract
- Severity: LOW · Confidence: HIGH
- Location: `crates/strider-py/strider/__init__.py:46-52`, `strider/_api.py:411-490`
- What & why: The audit brief and CLAUDE.md describe `strider.load(path) ->
  Program`, an `Analyzer` "frozen configure-once handle", `.analyze(target,
  **override)` per-call option overrides, and a `pipeline_factory` invoked fresh
  per call to dodge drain-on-use. None of that exists in the code: the facade is
  `ElfStrider` (`_api.py:274`) which builds the inner native `Strider` **once** at
  construction (`_rebuild_strider`, `_api.py:299`), and `ElfStrider.analyze`
  (`_api.py:411`) exposes only `function_max_size` / `allow_code_before_start_addr`
  / `compact` / `per_address_ccs` — **no** `pipeline`/`pipeline_factory`/`cc`/`arch`
  override and no freeze semantics. Consequently the prompt's "Analyzer
  freeze/override" and "pipeline_factory drain-on-use" audit dimensions have no
  corresponding code to harden; the only pipeline-customisation seam is the
  low-level `strider.run(..., pipeline=)` (see PY-5). This is doc/spec drift, not
  a code defect, but worth recording so the gap isn't re-flagged as a missing
  feature.
- Proposed fix: reconcile CLAUDE.md / the `2026-05-01-strider-py-design.md` spec
  with the shipped `ElfStrider` surface (or, if `Analyzer`/`pipeline_factory` is
  still desired, that is a feature, not a binding fix).

### PY-9 — Mirror gaps: `int_ne` and `Load/Store.stack_offset` not exposed in Python
- Dimension: SIMPLIFICATION / mirror fidelity
- Severity: LOW · Confidence: HIGH
- Location: absent from `crates/strider-py/src/pattern.rs`; Rust has
  `int_ne` (`strider-pattern/src/typed/value_ops.rs:523` match, `:1147` template)
  alongside the mirrored `int_le`/`int_sle`/`float_ne`/`float_le`
  (`pattern.rs:1783,1789,1953,1959`), and `LoadPat/StorePat::stack_offset`
  (`strider-pattern/src/node_builders/memory.rs:159,277`) while the Python
  `PyLoadPat`/`PyStorePat` field specs (`pattern.rs:2452-2461`, `:2485-2494`)
  expose only the boolean `stack_only`.
- What & why: `int_ne` is the one lowered-shape lift-canonical builder
  (`Xor(IntEqual(a,b),1):I1`) asymmetrically missing from the Python mirror;
  `stack_offset` (exact SP offset filter) is likewise unmirrored.
- Proposed fix: add a `PatRepr::IntNe` + `int_ne` `#[pyfunction]` (buildable on
  both match and template sides), and a `{ scalar stack_offset(i64 => i64):
  stack_offset }` field to both `node_builder!` invocations.

### PY-10 — `PyBufferReader::read` allocates `vec![0u8; size]` from an unbounded Python-supplied `size`
- Dimension: EDGE CASES (user-triggerable abort/OOM; not UB)
- Severity: LOW · Confidence: MED
- Location: `crates/strider-py/src/reader.rs:129` (and the `_LoadedElf::read`
  delegate at `:318`)
- What & why: `size = 2**60` triggers a huge allocation (clean Rust abort/OOM)
  before any region bound is consulted; `table.read` would only fill the mapped
  portion, so the allocation is pure waste. Clean abort, not memory corruption.
- Proposed fix: clamp `size` to the remaining mapped-region length (or a ceiling)
  before allocating.

---

## Cross-cutting simplification (not numbered findings)

- The hand-written `PyCallPat` (`pattern.rs:2536-2617`) and `PyIfPat`
  (`:2714-2763`) duplicate the `core_builder`/`build_pattern_py`/capture/when/
  into_pat plumbing that `node_builder!` already generates. Their only real
  quirks are `at`/`at_any` precedence (Call) and finished-`Pattern` branches
  (If); extending the macro with an "extra hand-written setters" hook would fold
  ~120 lines back in. PY-2's fix for CallOther's ctrl/mem would dovetail with
  this. Lower priority than the bugs above.

## Items checked and found SOUND (recorded so they aren't re-audited)

- Stale-handle contract: every in-place mutation bumps the function generation
  (`optimize` `function.rs:399`, `reoptimize` `:420`, `rewrite*` via
  `apply_rules_count_on` `:709`, `compact` via `retain_reachable_roots`
  `strider-graph/src/graph.rs:518`); every graph-dereferencing accessor on
  `PyMatch`/`PyNode` gates on `assert_generation` (`matcher.rs:81-92`,
  `node.rs:81-89`) before touching a stored id, so a stale id is never
  dereferenced against a mutated arena. No panic-across-FFI; staleness → typed
  `StriderError`. (PY-6 is the one residual edge.)
- No reachable `.unwrap()`/`.expect()`/`panic!`/`unreachable!` on Python input
  across the crate. `opt.rs:528`'s `expect` is structurally unreachable (post-
  passes are matched first). Negative / out-of-range Python ints fail at the
  PyO3 `u32`/`u64`/`usize` coercion as `OverflowError`/`TypeError` before the
  function body (idiomatic; not a panic).
- Poison handling: `opt.rs` lock → `StriderError`; `pattern.rs` intern lock →
  `StriderError` (`:100-102`); the two `function_ptr`/match locks use
  `unwrap_or_else(PoisonError::into_inner)` (poison-safe).
- Mutex reentrancy: no lock is held across a Python callback. The `.when()`
  predicate runs outside any lock (`pattern.rs:1255-1265`); the query path uses a
  shared `PyRef` + RwLock read guard (re-entrant for the read case), so a
  predicate re-entering `find_all` on the same function does not deadlock.
  Mutation uses RwLock `try_write` → typed error, never a borrow panic.
- ELF edge cases route through `load_elf(...).map_err(into_strider_err)` or
  explicit `Err(into_strider_err(...))`; empty/truncated/bad-`e_machine`/missing-
  symbol all surface as `StriderError`, not panic. `entry_point` uses
  `map_or(0, …)` (no `first().unwrap()`).
- Bridging cost is O(n) in pattern-tree size (children bridged by `Py<PyAny>`
  `clone_ref` / `Rc<PatRepr>` refcount, no per-level subtree deep-clone);
  `find_all`/`find_joined` match→Python conversion is O(matches) with O(1)
  per-match `clone_ref`. No O(n²).
- Lifetimes: `object::File<'static>` borrows a leaked slice (no use-after-free);
  `Py<>` clones use `clone_ref`; no `Rc` cycle in `PyBufferReaderInner`.

## Notable missing edge-case tests (names + scenario; not written)

- `test_call_other_ctrl_mem_methods_match` — `call_other().ctrl(p)` / `.mem(p)`
  must match a real lifted CallOther (currently would fail; pins PY-2).
- `test_deeply_nested_typed_builder_raises_not_aborts` — a ~100k-deep
  `load(addr=...)` chain raises `StriderError`, not a process abort (PY-3).
- `test_mem_reader_over_long_return_errors` — a `MemReader.read` returning more
  bytes than requested errors rather than silently truncating (PY-4).
- `test_mem_reader_keyboard_interrupt_during_lift` — Ctrl-C inside
  `MemReader.read` during instruction fetch surfaces as KeyboardInterrupt, not a
  downgraded `StriderError` (PY-1). (There is `test_mem_reader_kbd_interrupt.py`
  — verify it exercises the *instruction-fetch* path, not only the ROM path.)
- `test_run_twice_with_rom_and_same_pipeline` — reusing one pipeline object
  across two `run(rom=...)` calls behaves predictably (pins PY-5).
- `test_optimize_failure_invalidates_handles` — a pipeline that errors mid-run
  still invalidates outstanding handles (pins PY-6).
