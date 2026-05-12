# Round 10 — Silent Failure Hunt (2C)

Workspace: `/mnt/c/Users/mikeg/Documents/strider`. Branch: `feature/ai`. Pre-fixes from
rounds 7–9 (KB diagnostic propagation, `wrap_when` re-raising
`KeyboardInterrupt`/`SystemExit`, rewrite-rule fingerprint propagation, etc.) are
verified against the current source — no findings below re-flag those.

The audit covers every site that swallows or down-converts an error / unexpected
state. For each finding, I distinguish:

- **intentional-fallback** — documented optional path; sound and surfaceable
  via separate channels.
- **silent-bug** — propagates a default that masks a real problem.
- **debate** — reasonable people could disagree; depends on contract intent.

Findings are sorted by severity. Code is verified at the listed line numbers
in this branch's HEAD as of the date in the harness.

---

## HIGH

### H10-S1: `LoopState::recompute_unresolved` returns empty Vec when graph is missing
- **Severity:** HIGH
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider/src/orchestrator.rs:607-609`
- **Pattern:** `let Some(graph) = self.graph.as_ref() else { return Vec::new(); };`
- **What's swallowed:** The orchestrator is between `apply_in_place_edits` (which
  itself errors out when `self.graph` is `None`) and the next loop tick. If
  some refactor ever moves the graph into a different slot mid-iteration,
  `recompute_unresolved` would silently return an empty unresolved-list, the
  outer loop would treat this as "fixed point", and any genuinely unresolved
  branch in the prior list is dropped without diagnostic.
- **Verdict:** debate (defensive code; reachable only via state-machine bug).
  The two surrounding sites — `classify_and_partition` and `apply_in_place_edits`
  at the same level — already use `.ok_or_else(|| anyhow!("orchestrator: graph
  not initialised"))`. This single helper diverges from that pattern.
- **Fix:** Promote to `Result<Vec<…>>`:
  ```rust
  let graph = self
      .graph
      .as_ref()
      .ok_or_else(|| anyhow!("orchestrator recompute_unresolved: graph not initialised"))?;
  ```
  Caller is `LoopState::step`, which already returns `Result<Decision>`, so
  propagation is one `?`.

### H10-S2: `KnownBits::ZeroExtend` `u64_type_mask(input_ty).unwrap_or(0)` masks unsupported input width
- **Severity:** HIGH
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs:279`
- **Pattern:** `.unwrap_or(0)`
- **What's swallowed:** When `input_ty` is non-integer or > u64 (`U128`,
  `U256`, `U512`, `U80`), `u64_type_mask` returns `None`. The fallback `0`
  feeds `zeros: kb.zeros | (type_mask ^ 0) = type_mask`, asserting
  "all upper bits of the output are zero." For `ZeroExtend(U128 → U256)`
  this is *technically* sound (zero-extension always zeroes the upper
  bits), but the analysis is supposed to be u64-bound (see the doc-comment
  on `u64_type_mask`). The result is a `Kb` claiming knowledge of bits
  outside the analysis's actual coverage. Subsequent merges via
  `Kb::merge` could reject genuine values as contradictions, OR the
  `KnownBits → ConstantFold` interaction could produce incorrect masking.
  Sister arms (`ShiftLeft`, `ShiftRight` at lines 179, 220) hit the same
  `.unwrap_or(u64::MAX)` pattern with similar consequences for the
  `rhs_mask` path — there `u64::MAX` over-permits where exact width was
  intended.
- **Verdict:** silent-bug — the analysis is documented u64-bound, but
  the fallback silently extends it to undefined territory. Either the
  arm should bail (return `Ok(None)` for the propagation) or the `Kb`
  output should truncate to u64. Currently it does neither.
- **Fix:** Make the unsupported width an explicit early-return:
  ```rust
  let Some(input_mask) = u64_type_mask(input_ty) else {
      return Ok(None); // out-of-scope width
  };
  ```
  The `SignExtend` arm directly below (line 294) already does this
  pattern, so this is just consistency.

### H10-S3: `apply_elf_relocations_autoload` SHF_ALLOC parse failure logs to stderr only
- **Severity:** HIGH
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/reader/src/elf.rs:778-784`
- **Pattern:** `eprintln!(...)` then `return false`
- **What's swallowed:** A malformed ELF section whose `data()` fails to parse
  is logged to `stderr` and silently treated as "no site coverage." The site
  *does* eventually get bucketed as `skipped_no_region` by the inner
  `apply_elf_relocations` call, but the *cause* (malformed section data)
  is invisible to anyone not watching `stderr`. `RelocationStats` cannot
  represent "section X failed to parse"; the caller learns only that some
  reloc went unapplied without knowing the section was unreadable.
- **Verdict:** silent-bug. Comment on line 769-771 explicitly acknowledges
  the goal is "so a malformed ELF doesn't hide silently inside autoload
  search" — but `eprintln!` IS hiding it from any caller that doesn't
  capture stderr (which is most of them: Python's pyo3 callers,
  long-running daemons, any test runner that swallows stderr).
- **Fix:** Add a `RelocationStats` field
  `malformed_section_names: Vec<(String, String /* error */)>` and record
  the section here. Or convert to `Result<…>` returning a typed
  `MalformedElfSection` error so callers can see and handle it.

### H10-S4: `PyMemReader.read` `None` return collapses to `MemReadError` without distinguishing modes
- **Severity:** HIGH
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/reader.rs:496-513`
- **Pattern:** `bail!` after `is_none(py)` check, plus
  `extract::<Vec<u8>>` failure converts to anyhow.
- **What's swallowed:** Three failure modes share one error path:
  1. Python callback raises an exception (caught at line 500).
  2. Python callback returns `None` ("not mapped" — line 502-504).
  3. Python callback returns wrong type (line 505-507).
  All three become `MemReadError`. (1) and (3) are user-code bugs that
  *should* be loud; (2) is a normal "address not in this region" signal
  that the decoder treats as "try the next region" or "end of code."
- **Verdict:** debate. The Sleigh `MemReader` trait has a single Err arm
  for "read failed" — there's no way to distinguish "buggy" from
  "not mapped" in the trait. But on the Python side, exceptions in a
  user `MemReader.read` (e.g. a `KeyError` because the user typed the
  wrong dict key) get collapsed to `MemReadError(addr {...} not mapped)`,
  which mis-attributes the bug to data when it's code.
- **Fix:** Mirror the `PyReadOnlyMemoryAdapter` pattern (lines 575-593)
  that prints the Python exception to `stderr` before downgrading. At
  minimum, include the Python exception message verbatim in the
  `MemReadError` so downstream diagnosis isn't truncated. Even better:
  re-raise `KeyboardInterrupt`/`SystemExit` like `wrap_when` does in
  `pattern.rs`.

### H10-S5: `PyReadOnlyMemoryAdapter.read` collapses Python exceptions to `eprintln!` + `None`
- **Severity:** HIGH
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/reader.rs:575-593`
- **Pattern:** `eprintln!(...)` followed by `return None`
- **What's swallowed:** Two paths:
  1. `call_method1` raises a Python exception (line 575) → stderr + None.
  2. Result extracts to non-`u64` (line 587) → stderr + None.
  Both cases emit a `LoadReadOnly`-pass "no fold" silently. The user's
  buggy `read` override never surfaces in Python; they see only that
  pattern queries miss matches that should hit.
- **Verdict:** debate. The trait `ReadOnlyMemory::read` returns
  `Option<u64>` with no way to propagate. So the choice is:
  (a) `eprintln!` (current) — visible if stderr is attached.
  (b) Panic/`PyErr::restore` — propagate to next GIL re-entry.
  (c) Build error queue, surface at run boundary.
  The CLAUDE.md prohibition on silent failures argues for (b) like
  `wrap_when` already does for control-flow exceptions (lines 505-508
  in `pattern.rs`).
- **Fix:** At minimum, mirror `wrap_when`'s `KeyboardInterrupt`/`SystemExit`
  re-raise. Better: extend `ReadOnlyMemory` trait with a `fn read(&self,
  …) -> Result<Option<u64>, BoxedError>` so Python exceptions surface
  through `LoadReadOnly`'s `Result` chain.

### H10-S6: `function_args::mem_chain_is_dirty` `unwrap_or(true)` after debug_assert
- **Severity:** HIGH
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/function_args/mod.rs:521-522`
- **Pattern:** `let result = results.pop().unwrap_or(true);`
- **What's swallowed:** Right above this line, `debug_assert_eq!(results.len(),
  1, "mem_chain_is_dirty: result-stack invariant")` enforces the invariant
  in debug. In release, a violation silently degrades to "dirty" (`true`),
  which is the conservative direction for the analysis (treats memory as
  clobbered) but masks a real walker bug. The `FunctionArgDetect` pass
  falls back to "I don't know what's stored," potentially missing
  argument-recovery opportunities for callers.
- **Verdict:** debate. The fallback is correctness-safe (over-conservative),
  but masks a real bug in the worklist walker. The contract should be:
  pop the answer, error out on stack-empty.
- **Fix:** Convert the function to `Result<bool>` and propagate:
  ```rust
  let result = results.pop()
      .ok_or_else(|| anyhow!("mem_chain_is_dirty: result stack empty"))?;
  ```
  Caller is in the post-pass; failures already need `Result` plumbing
  there.

---

## MEDIUM

### M10-S1: `RelocationKind::Unknown` symbol-lookup path drops `RelocationTarget` non-Symbol case as `malformed_target`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/reader/src/elf.rs:533-540`
- **Pattern:** `_ => { stats.skipped_malformed_target += 1; continue; }`
- **What's swallowed:** Any `RelocationTarget` variant we don't handle
  (`Section`, `Absolute`, future-added variants) is bucketed as
  "malformed" without recording which kind. The same pattern repeats
  at line 593-595 for the non-GLOB_DAT path. Diagnosing why a binary
  has high `skipped_malformed_target` requires source inspection.
- **Verdict:** debate. The bucketing is documented in the comment
  ("non-Symbol relocation target … bucket as malformed-from-our-
  perspective"). But if a future `object` crate adds a real variant
  we *should* handle, this silently masks it.
- **Fix:** Add `unhandled_target_variants: Vec<&'static str>` to
  `RelocationStats`, populated via `match-as-debug-name`. Costs nothing
  at runtime and exposes the gap.

### M10-S2: `region_builder::process_call_other` — unrecognised `id_vn.addr_off` returns `DidntFinishProcessing` silently
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/region_builder.rs:398-411`
- **Pattern:** `match … { None => return Ok(DidntFinishProcessing), Some(_) => … }`
- **What's swallowed:** Three early-returns silently fall through to
  "use today's behaviour":
  - `inputs.first()` is `None` (line 398-401)
  - `id_vn.addr_space != CONST` (line 402-404)
  - `u32::try_from(id_vn.addr_off)` fails (line 405-408)
  All three indicate a malformed lift (Sleigh contract violation), but the
  code proceeds to fall-through processing where the IR layer's
  `handle_call_other` *should* surface the same defect via
  `UnknownCallOtherError`. The comment on line 395-397 calls this out
  ("the IR layer's strict-on-emission check will surface any real problem
  with full context") which makes this an intentional-fallback.
- **Verdict:** intentional-fallback (documented). The IR layer always
  re-checks; the silent path here is a no-op handoff.
- **Fix:** None. Documented and tested-by-integration.

### M10-S3: `Match::stack_phi_offsets` collapses `Some(&[])` to `None`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/match_result.rs:288-291`
- **Pattern:** `if slice.is_empty() { None } else { Some(slice) }`
- **What's swallowed:** A `StackStorePhi` whose side-table entry is empty
  is reported as "not a stack-phi" via `None`, which a caller cannot
  distinguish from "captures isn't a `StackStorePhi` at all." The doc-comment
  at lines 271-275 spells this out as intentional ("collapses Some(&[])
  to None so an unpopulated side-table doesn't masquerade as a real phi
  shape").
- **Verdict:** intentional-fallback (documented). The "empty side-table"
  case represents a `StackStoreDetect` pass that didn't populate
  `Graph::stack_phi_offsets` — typically a synthetic/test graph. Real
  pipelines always populate.
- **Fix:** None. Behaviour pinned by tests in
  `crates/pattern/tests/matching/stack_store_phi.rs`.

### M10-S4: `Match::get_uint` returns `None` for non-IntConst, masking type errors
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:142-149`
- **Pattern:** Multiple `let X = …?;` early-returns to `None`
- **What's swallowed:** Three separate failure modes share one return:
  1. Capture isn't bound (`get_output(c) → None`).
  2. Bound node isn't an `IntConst`.
  3. Bound node's output type isn't a value (or is non-integer).
  Callers in `IndirectBranchResolve` (e.g. line 641, 665 of `jump_table.rs`
  use `m.get_uint(n_var, …)?`) can't distinguish "the pattern legitimately
  failed to bind" from "the bound type is not what I expected." Pattern
  shape errors silently degrade to "no resolution," visible only as
  `UnresolvedIndirectBranch`.
- **Verdict:** intentional-fallback. The pattern crate's contract
  (CLAUDE.md "the same 'wrong shape ⇒ None' contract the old typed-Var
  getters had") commits to this collapse. Fixing requires a richer
  `MatchOutcome { Bound(value), BoundWrongType, NotBound }`.
- **Fix:** None for the contract change. But: every consumer of `.get_uint(c, g)?`
  in the orchestrator path (`indirect_branch_resolve/jump_table.rs:309, 641,
  665`) should be reviewed to see whether the mask-fail and bound-fail
  cases need separate diagnostic.

### M10-S5: `Match::output(c)?` returns `None` for control-flow captures without distinguishing kind
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/matcher/bindings.rs:104-106`
- **Pattern:** `self.get_binding(c).and_then(|b| b.output)`
- **What's swallowed:** `get_output(c)` returns `None` for both
  (a) unbound capture and (b) captured but control-flow-only binding
  (e.g. `If` matched as a control node). Multiple call sites in
  `jump_table.rs` (lines 637, 661, 499, etc.) chain `m.output(idx_var)?`
  expecting binding, then silently treat unbound captures as match
  failures.
- **Verdict:** intentional-fallback (documented in line 102-103
  "or `None` if `c` was not captured or the binding was control-flow").
  But debate: a value-pattern capture that hits a control-only binding
  would be a pattern bug, not a runtime miss.
- **Fix:** Could split into `output_or_unbound()` (returns `None` only
  for unbound) vs `value_output()` (returns `Some(_)` only for value
  bindings). Low priority — pattern users can guard `get_node(c)` first
  to disambiguate.

### M10-S6: `IndirectBranchResolve::find_placeholder_return_for_anchor` silently returns `None` for malformed shapes
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/mod.rs:431-437`
- **Pattern:** `if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer) && val == anchor_output { … }`
- **What's swallowed:** Any `IndirectBranch` consumer with non-3-input
  shape silently doesn't match. The validator's Layer A enforces
  `IndirectBranch` arity = 3, so this should be unreachable, but if it
  were ever violated the consumer-walk just yields `None` instead of
  flagging the malformed `IndirectBranch`.
- **Verdict:** intentional-fallback. The validator catches the same
  defect upstream; this is defensive shape-checking.
- **Fix:** None.

### M10-S7: `classify_anchor_with_rom_and_sp` ValuePhi arm: empty target list returns `None`
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/classify.rs:188-192`
- **Pattern:** `if targets.is_empty() { None } else { Some(...) }`
- **What's swallowed:** A `ValuePhi` with zero value-input slots
  (i.e. only the phi-token, no per-pred values) returns `None`,
  defer-as-unresolved. Comment at lines 185-187 notes this is sound
  (an empty Multiple would silently advertise zero targets making
  the dispatch unreachable). But the underlying *cause* (malformed
  ValuePhi) isn't surfaced.
- **Verdict:** intentional-fallback (documented soundness). The
  validator's Layer C enforces phi arity, so this is double-defensive.
- **Fix:** None.

### M10-S8: `wrap_when` proxy alloc failure → eprintln + false
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:464-468`
- **Pattern:** `eprintln!(...)` then `return false`
- **What's swallowed:** `Py::new(py, proxy)` returns `Err` when Python
  is out of memory or a foreign type-error blocks construction. The
  predicate is treated as false, silently dropping every `find_all`
  match for that pattern. Comment at line 446-451 makes the design
  explicit ("aborting find_all mid-walk on a buggy predicate would be
  worse than continuing"). However, `KeyboardInterrupt`/`SystemExit`
  are re-raised by the parallel arm at lines 505-508 — so the door is
  open to extend that to other "must surface" errors (e.g. `MemoryError`).
- **Verdict:** debate. Round 9 H-8 fixed control-flow exceptions but
  this `Py::new` failure is still silently swallowed. Plausibly OOM
  during a heavy `find_all` is itself a control-flow event.
- **Fix:** Re-raise via `e.restore(py)` if `e.is_instance_of::<PyMemoryError>()`
  to mirror the H-8 spirit.

### M10-S9: `Match::output(c)?` chains in `jump_table::same_value` silently bail on non-VarPhi/ValuePhi
- **Severity:** MED
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/indirect_branch_resolve/jump_table.rs:709-720`
- **Pattern:** `match graph.node_kind(node) { VarPhi/ValuePhi if 2 inputs => continue, _ => return out }`
- **What's swallowed:** The `same_value` walker terminates on any
  non-phi or non-2-input phi by returning the current output.
  Comment at line 691 notes the conservative direction: "on cycle,
  return false rather than looping." That's documented, but the
  implicit termination for "phi shape we don't recognise" is
  undocumented.
- **Verdict:** intentional-fallback. The `same_value` contract is
  "best-effort identity through trivial phis" — failure to walk
  further is correctness-neutral (returns false agreement, leading
  to None/defer at the call site).
- **Fix:** Document the termination cases in the comment above
  `root` (line 695). Code itself is fine.

---

## LOW

### L10-S1: `RomInput::extract_bound` accepts duck-typed object via `hasattr("read")`
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/reader.rs:701-712`
- **Pattern:** `if let Ok(m) = ob.extract::<PyMemoryMap>()` then fall through to `hasattr` check
- **What's swallowed:** The `hasattr("read")` check accepts any Python
  object with a callable `read` attribute, regardless of arity. A wrong-
  arity callback (`def read(self, addr): ...` instead of
  `def read(self, space, addr, size): ...`) raises `TypeError` at the
  *first* call from inside `LoadReadOnly` — far from where the user
  registered the callback. The `eprintln!` at line 575-580 then logs
  it to stderr.
- **Verdict:** debate. The "duck typing then runtime fail" pattern is
  Pythonic. But the failure surfaces deep inside `LoadReadOnly` rather
  than at registration time.
- **Fix:** Probe `inspect.signature(ob.read).parameters` at registration
  to validate arity before accepting. Or commit to "PyO3 will
  raise on wrong arity, that's the contract" and document it.

### L10-S2: `pattern.rs` `CaptureKeyOwned::extract_bound` masks Capture extraction errors
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/pattern.rs:381-391`
- **Pattern:** `if let Ok(c) = ob.extract::<…>() { … }; if let Ok(s) = ob.extract::<String>() { … }; Err(...)`
- **What's swallowed:** Per-extract failures fall through to "try the
  next type." A `PyCapture` instance that's e.g. dead/poisoned would
  fall through to the `String` extract instead of surfacing.
- **Verdict:** intentional-fallback. The two `extract` calls are typed
  `Result<X, PyErr>` where `Err` means "wrong type" — the standard
  PyO3 pattern.
- **Fix:** None.

### L10-S3: `lookup_table().ok()?` in `PyMemoryMap::read` discards the rebuild error
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/strider-py/src/reader.rs:661`
- **Pattern:** `let table = self.lookup_table().ok()?;`
- **What's swallowed:** `lookup_table()` returns `Result<…>` whose `Err`
  comes from a poisoned `RwLock` (line 142, recovered via
  `unwrap_or_else(|p| p.into_inner())`) or an `Arc` reconstruction failure.
  In `ReadOnlyMemory::read`'s `Option<u64>` shape, the only sane
  fallback is `None`, but the *cause* of the failure is silently
  dropped.
- **Verdict:** debate. The trait's `Option<u64>` shape forces the
  collapse. But the inner errors here are infrastructure bugs that
  shouldn't be invisible.
- **Fix:** Add an `eprintln!` mirroring `PyReadOnlyMemoryAdapter` so
  the user can correlate. Or extend the trait. Low priority since
  `lookup_table()`'s failure mode is essentially unreachable
  (the only RwLock failures are poison, which are recovered earlier).

### L10-S4: `redundant_phis` and `function_args` `let _ = detach_unreachable_nodes(...)`
- **Severity:** LOW
- **Where:**
  - `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/redundant_phis/mod.rs:206`
  - `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/function_args/mod.rs:108`
- **Pattern:** `let _ = ...`
- **What's swallowed:** Verified: `detach_unreachable_nodes` returns
  `OptimizationResult` (not `Result`), so `let _` discards a verdict,
  not an error. Documented at `worklist.rs:81-85`.
- **Verdict:** intentional-fallback (documented).
- **Fix:** None.

### L10-S5: `CallOther` user-op-name `unwrap_or` into hash-id-only mismatch
- **Severity:** LOW
- **Where:** `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/src/cfg/builder/region_builder.rs:411`
- **Pattern:** `let class = name.and_then(|n| target::call_other_abi::classify(preset, n));`
- **What's swallowed:** When `name` is `None` (user_op_id not in Sleigh's
  table) OR `classify` returns `None` (unknown name for that preset),
  the cfg-time classifier treats it as "fall through to today's
  behaviour" without distinguishing the two. The IR layer's
  `handle_call_other` (line 132 in `crates/strider/src/strider/insn/mod.rs`)
  then errors out via `UnknownCallOtherError` so the user does see *a*
  diagnostic, but only the IR-level one — not "unknown user_op_id at
  pcode_addr X."
- **Verdict:** intentional-fallback (documented at line 395-397). IR
  layer surfaces the real defect.
- **Fix:** None.

### L10-S6: `CallStackArgCollect` and other post-passes' `unwrap_or` defaults
- **Severity:** LOW
- **Where:** Various — `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/known_bits/mod.rs:179, 220`
- **Pattern:** `.unwrap_or(u64::MAX)`
- **What's swallowed:** The `rhs_mask` for a non-u64-fitting type
  defaults to `u64::MAX`, which over-permits in the `all_known`
  check. The branch then either falls into the `all_known(rhs_mask)`
  path (rejecting if rhs_kb has any zero bit outside the type) or
  bails to `Ok(None)`. In practice rhs_mask is sound for
  `ShiftLeft`/`ShiftRight` because the shift amount fits in u64 for any
  realistic IR (shifts > 64 are ISA-undefined). Still, the silent
  default is brittle.
- **Verdict:** debate. Same as H10-S2 but lower stakes here because
  shift widths are bounded by `bit_width()` (line 182 / 223) which
  catches the over-permissive case.
- **Fix:** Drop the `unwrap_or` and propagate the bail like the
  `Extend(SignExtend)` arm does. One-line change per arm.

### L10-S7: `find_largest_fitting_register` `vn_mask` widths > 16 bytes degrade to `u128::MAX`
- **Severity:** LOW
- **Where:** Confirmed via CLAUDE.md note on `pcode-lift::vn_io.rs`:
  "Widths > 16 use a degraded `u128::MAX` mask; the wide-container
  guard rejects sub-register aliasing within > 16-byte containers
  with a clear error."
- **Pattern:** Documented degraded-mask + wall-of-text guard
- **What's swallowed:** Sub-register aliasing inside an XMM/YMM/ZMM
  container that doesn't align is rejected — but the guard exists.
- **Verdict:** intentional-fallback (documented).
- **Fix:** None.

---

## Counts

By pattern:
- `unwrap_or` / `unwrap_or_default` / `unwrap_or_else` (production): 13
  occurrences total — verified breakdown:
  - `crates/opt/src/known_bits/mod.rs`: 3 (H10-S2 + L10-S6)
  - `crates/opt/src/function_args/mod.rs`: 1 (H10-S6)
  - `crates/strider/src/orchestrator.rs`: 1 (intentional CC fallback)
  - `crates/strider/src/strider/pipeline.rs`: 2 (intentional caching/init)
  - `crates/strider-py/src/{cfg,graph,dot,pattern,reader}.rs`: 5
    (4 style defaults + 2 lock-poison recovery)
  - `crates/cfg/src/cfg/decode_cache.rs`: 2 (intentional poison recovery)
  - `crates/ir/src/dot/{render,label}.rs`: 3 (visualization fallbacks)
  - `crates/reader/src/elf.rs`: 1 (eprintln context — H10-S3)
  - `crates/strider-py/src/run.rs`: 1 (per_address_ccs default — intentional)
- `if let Ok(_) = …` (production): 6 occurrences — all
  type-extract or arity-check shape; classified as intentional except
  M10-S6 (low-confidence shape).
- `.ok()?` (production): 9 occurrences — all are
  `try_into`/`try_from` collapse-to-Option in functions whose
  return is already `Option`. Classified as intentional or contract-bound.
- `eprintln!` then return `None`/`continue` (production): 3 occurrences.
  - H10-S3 (`reader/src/elf.rs:779`)
  - H10-S5 (`strider-py/src/reader.rs:576`)
  - H10-S5 follow-up (`strider-py/src/reader.rs:588`)
  Plus M10-S8 (`strider-py/src/pattern.rs:466, 491`) — proxy alloc and
  non-bool predicate result.
- `let _ =` discarding `Result` (production): 0. All discarded
  expressions are non-Result types (`Value` from `build_call_other_terminal`
  with a separate `?`; `OptimizationResult` from `detach_unreachable_nodes`).
- `match … { _ => return None / Vec::new() }` masking (production):
  1 strong (H10-S1: `recompute_unresolved`). Several documented soundness-
  defers (M10-S6, M10-S7).

By crate:
- `crates/strider`: 1 HIGH (H10-S1).
- `crates/opt`: 2 HIGH (H10-S2, H10-S6), 4 MED (M10-S3 N/A, M10-S6,
  M10-S7, M10-S9), 1 LOW (L10-S6).
- `crates/reader`: 1 HIGH (H10-S3), 1 MED (M10-S1).
- `crates/strider-py`: 2 HIGH (H10-S4, H10-S5), 2 MED (M10-S8, partial L10-S2),
  3 LOW (L10-S1, L10-S2, L10-S3).
- `crates/cfg`: 1 MED (M10-S2 — intentional), 1 LOW (L10-S5 — intentional).
- `crates/pattern`: 2 MED (M10-S3, M10-S4), 1 MED (M10-S5).
- `crates/pcode-lift`: 0 (clean: production `unwrap` only inside tests
  with `clippy::unwrap_used` allowed).
- `crates/ir`: 0 (the few `.ok()?` patterns are all in `wide_const.rs`
  byte-conversion helpers with documented `Option<Self>` returns; the
  `unwrap_or_default()` / `unwrap_or_else(|_| format!(…))` cases at
  `dot/label.rs:279, 304, 322` are visualization-only fallbacks).
- `crates/target`, `crates/dot`, `crates/graphwalk`, `crates/entity-utils`:
  0 in production code (all hits are inside test files/test helpers).

Top fix priorities (by impact × tractability):
1. **H10-S2** — one-line bail in `KnownBits::ZeroExtend` arm (consistency
   with sister arm).
2. **H10-S6** — one-line `Result` propagation in `mem_chain_is_dirty`.
3. **H10-S1** — promote `recompute_unresolved` to `Result`.
4. **H10-S3** — extend `RelocationStats` with malformed-section field.
5. **H10-S4 / H10-S5** — coordinated trait extension for
   Python-callback diagnostics surfacing.

Round 10 net: no new "show-stopper" silent failures versus round 9, but
H10-S1, H10-S2, and H10-S6 are direct-line fixes that close real
diagnostic gaps. The Python boundary (H10-S4/S5/M10-S8) carries the
highest *user-visible* opacity — diagnosing a buggy `MemReader` /
`ReadOnlyMemory` / `.when()` predicate from inside `find_all` requires
attaching to stderr today.
