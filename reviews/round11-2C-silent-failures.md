# Round 11 — 2C: silent-failure audit

## Scope and methodology

Cataloged every `unwrap_or` / `unwrap_or_default` / `unwrap_or_else` /
`.ok()?` / `if let Ok(...)` (with implicit-`Err` discard) / `let _ = ...`
across `crates/*/src/`.  Test-only sites (under `mod tests` or `tests.rs`),
documented mutex-poisoning recovery, and Python optional-kwarg defaults
were classified as INTENTIONAL on visual inspection and grouped to the
totals but generally elided from the per-site discussion.  Each suspicious
production site was opened and its discarded error type and caller-default
semantics traced from the code shape alone.

## Summary

| Crate         | unwrap_or | .ok()? | if-let-Ok-ignore | let _ =  | Total |
|---------------|-----------|--------|------------------|----------|-------|
| cfg           | 2         | 0      | 0                | 0        | 2     |
| dot           | 0         | 0      | 0                | 1        | 1     |
| entity-utils  | 1         | 0      | 0                | 2 (test) | 3     |
| graphwalk     | 0         | 0      | 0                | 2        | 2     |
| ir            | 4 (1 dot, 1 panic, 2 dot) | 2 | 0 | 1 (test) | 7 |
| opt           | 3         | 7      | 2                | 2        | 14    |
| pattern       | 0         | 0      | 0                | 2 (mac.) | 2     |
| pcode-lift    | 0         | 0      | 0                | 0        | 0     |
| reader        | 0         | 1      | 0                | 0        | 1     |
| strider       | 4         | 0      | 1                | 1 (test) | 6     |
| strider-py    | 9         | 1      | 4                | 0        | 14    |
| target        | 9 (panic) | 0      | 0                | 0        | 9     |

(`(panic)` = the `unwrap_or_else` is `|| panic!(…)` — assertive, not
silent; counted but not flagged.)

## Findings

### F1. `mem_chain_is_dirty` returns "clean" for malformed `Call` / `MemPhi`

- **Severity:** MED
- **Where:** `crates/opt/src/function_args/mod.rs:491-510, 511-518`
- **Code:**
  ```rust
  NodeKind::MemPhi => {
      // ...
      if pred_count == 0 {
          // Empty phi (malformed) — clean.
          results.push(false);
      } else { ... }
  }
  NodeKind::Call | NodeKind::CallOther { .. } => {
      let inputs = fg.node_inputs(node);
      if inputs.len() < 2 {
          results.push(false);
      } else {
          work.push(Frame::Visit(inputs[1]));
      }
  }
  ```
- **Discarded error type:** none (no `Err` is being thrown — but a real
  IR-shape invariant violation is being papered over).  By signature
  `Call` has `[CTRL, MEM, TARGET]; in_tail: ARG` (≥ 3 inputs;
  `node_signature.rs:331-334`) and `MemPhi` has at least one
  predecessor.  An `inputs.len() < 2` `Call` or empty `MemPhi` is
  always graph corruption.
- **What the caller does with the default:** treats the chain as
  "clean" — meaning the load can be safely forwarded.  That is the
  *unsafe* direction for a memory-aliasing analysis: if a malformed
  Call is mistakenly classified as not clobbering memory, the
  function-args pass forwards a stale value past it.
- **Verdict:** SWALLOWED-BUG (bounded — only fires on already-malformed
  IR, but if it does fire the fallback masks the bug *and* picks the
  unsound direction).
- **Proposed change:** return `Err(anyhow!(...))` — the same direction
  this file already takes for the result-stack invariant violation at
  `mod.rs:533-545` ("walker bug" Err).  Alternatively, push `true`
  (conservative-correct) so a malformed Call is treated as "may
  clobber" — but Err is preferred because we *know* the graph is
  corrupt and want validation to surface that, not silently produce
  one safe result.

### F2. `apply_in_place_edits` silently skips `InitialVar` with non-1 outputs

- **Severity:** MED
- **Where:** `crates/strider/src/orchestrator.rs:582-588`
- **Code:**
  ```rust
  for nid in graph.graph.preorder(graph.entry) {
      if let ir::node::NodeKind::InitialVar(existing) = graph.graph.node_kind(nid)
          && let Ok([out]) = graph.graph.node_outputs_exact::<1>(nid)
      {
          initial_var_index.insert(*existing, out);
      }
  }
  ```
- **Discarded error type:** `ir::error::IrError` (mismatched output
  count from `node_outputs_exact::<1>`).  By signature `InitialVar`
  has exactly one output (`node_signature.rs:307`).
- **What the caller does with the default:** silently skips this
  varnode.  Subsequent `read_or_init_var` calls for the same `Vn`
  cannot find it in `initial_var_index`, fall through to a fresh
  `InitialVar` allocation, and may resurrect a zombie (precisely the
  invariant the comment at lines 574-580 says we're trying to avoid).
- **Verdict:** SWALLOWED-BUG (bounded — only fires on a malformed
  `InitialVar`; but if it does fire, the orchestrator's whole
  edit-iteration silently desyncs from the IR).
- **Proposed change:** propagate Err — change the `if let Ok` to `?`
  using `let [out] = graph.graph.node_outputs_exact::<1>(nid)?;`
  guarded by the existing `InitialVar` `if let` (split into two
  statements so `?` can short-circuit the function).

### F3. `LookupTable` poisoning silently returned to Sleigh as "address not mapped"

- **Severity:** LOW
- **Where:** `crates/strider-py/src/reader.rs:673`
- **Code:**
  ```rust
  let table = self.lookup_table().ok()?;
  ```
- **Discarded error type:** `anyhow::Error` carrying RwLock-poisoning
  message from `rebuild_table` (`reader.rs:116-129`).
- **What the caller does with the default:** the trait API is
  `ReadOnlyMemory::read -> Option<u64>`.  Returning None tells Sleigh
  the address has no backing — Sleigh then proceeds, eventually
  failing with a confusing downstream "byte missing" error.  The user
  has no signal that the *real* cause was a panicked
  region-rebuilding writer.
- **Verdict:** INTENTIONAL-FALLBACK (trait API constrains us; cannot
  propagate `Result`).
- **Annotation suggestion:** add a comment naming the trait-API
  constraint and (optionally) `eprintln!` the poisoning event for
  visibility — analogous to the `eprintln!` reaction in
  `PyReadOnlyMemoryAdapter::read` for Python exceptions
  (`reader.rs:579-606`).

### F4. ELF section data parse failures silently bucketed (only counter)

- **Severity:** LOW
- **Where:** `crates/reader/src/elf.rs:806-810` (autoload), and
  `crates/reader/src/elf.rs:588-599` (relocation target).
- **Code:**
  ```rust
  Err(_) => {
      *parse_failure_count += 1;
      return false;
  }
  // ...
  Err(_) => {
      stats.skipped_malformed_target += 1;
      continue;
  }
  ```
- **Discarded error type:** `object::Error` — the actual parse-error
  detail (truncated section, bad flags, out-of-range offset, …).
- **What the caller does with the default:** the section is treated
  as "no usable bytes" or the target as "malformed", and the relocation
  is dropped or skipped.  The parse-failure count is exposed via
  `RelocationStats::autoload_section_parse_failures` /
  `skipped_malformed_target`; programmatic callers can detect that
  *something* went wrong but cannot diagnose *what*.
- **Verdict:** INTENTIONAL-FALLBACK (counter mechanism is documented
  in the comment block at lines 791-799).
- **Annotation suggestion:** consider stashing the first N
  `object::Error` strings into the stats so callers reading
  `autoload_section_parse_failures > 0` have something to print
  beyond a count — without resorting to library-side `eprintln!`.

### F5. `find_placeholder_return_for_anchor` swallows `IndirectBranch` shape errors

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/mod.rs:431-437`
- **Code:**
  ```rust
  if let Ok([_, _, val]) = graph.node_inputs_exact::<3>(consumer)
      && val == anchor_output
  {
      return Some(consumer);
  }
  ```
- **Discarded error type:** `IrError` (input-arity mismatch).  Signature
  for `IndirectBranch` mandates `[CTRL, MEM, TARGET]` (3 inputs;
  `node_signature.rs:344`).
- **What the caller does with the default:** treats this consumer as
  "not the right placeholder Return" and continues the search loop.
  Worst case: orchestrator concludes there's no placeholder Return
  for this anchor and skips its in-place edit, falling through to a
  CFG rebuild — slower but not incorrect.
- **Verdict:** INTENTIONAL-FALLBACK (signature-guaranteed; bounded
  cost of misclassifying as "no placeholder").
- **Annotation suggestion:** add a one-line comment: "`Ok([_, _, val])`
  is total by IndirectBranch signature; the `if let Ok` is shape
  defensiveness in case validation hasn't run yet."

### F6. `same_value`'s phi-walk truncates on non-trivial phis

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:710-721`
- **Code:**
  ```rust
  NodeKind::VarPhi(_) | NodeKind::ValuePhi => {
      if let Ok([_token, val]) = graph.node_inputs_exact::<2>(node) {
          out = val;
          ...
      }
      return out;
  }
  ```
- **Discarded error type:** `IrError` (a phi with a different number of
  inputs).  Documented intent: only chase *trivial* single-value phis;
  multi-input phis stop the walk.
- **What the caller does with the default:** treats phi root as the
  identity end-point.  This is the *correct* semantics for
  multi-pred phis (their value isn't equal to a single predecessor's).
- **Verdict:** INTENTIONAL-FALLBACK — the `Ok([_, _])` is doing
  shape-classification ("trivial" vs "not"), not error recovery.

### F7. `KnownBits` shift rhs falls back to `u64::MAX` mask

- **Severity:** LOW
- **Where:** `crates/opt/src/known_bits/mod.rs:204-209, 245-250`
- **Code:**
  ```rust
  let rhs_mask = fg.graph.output_kind(rhs)
      .as_value()
      .and_then(u64_type_mask)
      .unwrap_or(u64::MAX);
  ```
- **Discarded error type:** none — `Option`-default.  The `None` arises
  when rhs is non-int / wider-than-u64.
- **What the caller does with the default:** `rhs_kb.all_known(u64::MAX)`
  becomes the gate for the literal-shift branch.  A non-integer rhs
  has `Kb::default()` (all-unknown) so `all_known` fails naturally and
  the unwrap_or default doesn't bite.  But the choice silently masks
  the upstream invariant violation: `IntBinaryOp::ShiftLeft` rhs is
  always an integer by signature — a non-int rhs is graph corruption.
- **Verdict:** INTENTIONAL-FALLBACK (no observable effect; defensive
  default).  Comment at line 312 contrasts it with a buggier earlier
  formulation (`unwrap_or(0)` → silent corruption), so the choice is
  thought-through.

### F8. `int_const_signed` returns `None` on i128→i64 overflow

- **Severity:** LOW
- **Where:** `crates/opt/src/sp_expr.rs:215, 243`
- **Code:**
  ```rust
  return i64::try_from(signed).ok();
  ```
- **Discarded error type:** `TryFromIntError` (the constant doesn't fit
  in `i64`).
- **What the caller does with the default:** treats the value as "not
  an SP-trackable offset".  Stack offsets must fit in `i64` to be
  representable in `Graph::stack_phi_offsets`.  Oversize offsets
  necessarily fall outside the tracked range.
- **Verdict:** INTENTIONAL-FALLBACK (signature/storage constraint).

### F9. `node_inputs_exact::<2>(load_node).ok()?` in jump-table classifier

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/stack_array.rs:322`
- **Code:**
  ```rust
  let [mem_input, addr_output] = graph.node_inputs_exact::<2>(load_node).ok()?;
  ```
- **Discarded error type:** `IrError` (Load with non-2 inputs).  By
  signature Load has exactly 2 inputs (`node_signature.rs:347`).
- **What the caller does with the default:** classify the candidate
  as "not a jump table".  Worst case: orchestrator falls through to a
  CFG rebuild iteration that has to redo the work.
- **Verdict:** INTENTIONAL-FALLBACK (signature-guaranteed shape).
- **Annotation suggestion:** the comment "Load has exactly 2 inputs by
  signature" already fits in one line — add it.

### F10. Type-mask narrowing in `bound_via_known_bits`

- **Severity:** LOW
- **Where:** `crates/opt/src/indirect_branch_resolve/jump_table.rs:309`
- **Code:**
  ```rust
  let type_mask = u64::try_from(ty.get_unsigned_int(u128::from(u64::MAX))?).ok()?;
  ```
- **Discarded error type:** `TryFromIntError` (mask exceeds `u64`).
  Fires for `U80` / `U128` / `U256` / `U512` integer types.
- **What the caller does with the default:** classify the index as
  "not bounded enough to enumerate" — falls back to the predecessor-If
  walk via the surrounding `or_else` at `stack_array.rs:95-96`.
- **Verdict:** INTENTIONAL-FALLBACK — the function's return type is
  `Option<u64>`, so > u64 bounds are inexpressible.

### F11. ELF-loader `read` boundary on negative offsets / overflow

- **Severity:** LOW
- **Where:** `crates/reader/src/lib.rs:188`
- **Code:**
  ```rust
  let offset = usize::try_from(addr.checked_sub(self.start_addr)?).ok()?;
  ```
- **Discarded error type:** `TryFromIntError` (32-bit `usize` host,
  region offset > 4 GiB).
- **What the caller does with the default:** None signals "address
  out of bounds for this region" — same as the documented `addr <
  start_addr` case.
- **Verdict:** INTENTIONAL-FALLBACK (the function's API is
  `Option<usize>` and out-of-range is a valid None case).

### F12. ELF region builder `Err(_) => DidntFinishProcessing`

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:405-407`
- **Code:**
  ```rust
  let id_u32 = match u32::try_from(id_vn.addr_off) {
      Ok(v) => v,
      Err(_) => return Ok(ProcessInsnRes::DidntFinishProcessing),
  };
  ```
- **Discarded error type:** `TryFromIntError` (user-op id > u32::MAX).
- **What the caller does with the default:** routes the malformed
  CallOther through the standard processing path; the IR layer's
  strict-on-emission check raises the real error with full context
  (per the comment at lines 393-397).
- **Verdict:** INTENTIONAL-FALLBACK (documented; deferral pattern).

### F13. `dot_style_for` silent fallback to `dark`

- **Severity:** LOW
- **Where:** `crates/strider-py/src/dot.rs:15-22`
- **Code:**
  ```rust
  pub fn dot_style_for(name: Option<&str>) -> dot::DotStyle {
      match name.unwrap_or("dark") {
          "dark_cfg" => ...,
          "empty" => ...,
          _ => dot::DotStyle::dark(),
      }
  }
  ```
- **Discarded error:** none — but an unknown style name (typo,
  outdated tutorial) silently falls through to `dark` instead of
  raising a Python `ValueError`.
- **What the caller does with the default:** receives a dark-styled
  graph where they may have asked for an as-yet-unimplemented style.
- **Verdict:** INTENTIONAL-FALLBACK (documented in the comment, line
  19: "any unknown name fall through to dark").
- **Annotation suggestion:** consider tightening to a typed enum so
  unknown names raise a Rust-side typed error → Python TypeError.

### F14. CallOther `let _ = build_call_other_terminal(...)?`

- **Severity:** LOW
- **Where:** `crates/strider/src/strider/insn/mod.rs:141`
- **Code:**
  ```rust
  let _ = self.builder.build_call_other_terminal(user_op_id, name)?;
  ```
- **Discarded value:** the returned `NodeId`.  Errors propagate via
  `?`.
- **Verdict:** INTENTIONAL-FALLBACK (terminal builder result not used;
  side effect is the real intent).

### F15. `detach_unreachable_nodes` results discarded after passes

- **Severity:** LOW
- **Where:** `crates/opt/src/function_args/mod.rs:108`,
  `crates/opt/src/redundant_phis/mod.rs:206`
- **Discarded value:** `OptimizationResult` (Changed/NoChange) — *not*
  a `Result`.
- **Verdict:** INTENTIONAL-FALLBACK (documented at `worklist.rs:81-85`:
  Changed verdict is bookkeeping-only here).

### F16. PyO3 `FromPyObject` polymorphic dispatch

- **Severity:** LOW
- **Where:** `crates/strider-py/src/reader.rs:713, 748`,
  `crates/strider-py/src/pattern.rs:388, 391`
- **Discarded error type:** PyO3 `PyDowncastError` from each candidate
  type extraction.  Standard PyO3 idiom for "try several types, raise
  TypeError when none match".
- **Verdict:** INTENTIONAL-FALLBACK (idiomatic; the final `Err(...)`
  is the user-visible message).

### F17. Mutex-poisoning recovery via `into_inner`

- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/decode_cache.rs:60, 66`,
  `crates/strider-py/src/pattern.rs:342, 362`,
  `crates/strider-py/src/reader.rs:143, 688`
- **Discarded error type:** `PoisonError<MutexGuard<...>>` /
  `PoisonError<RwLockReadGuard<...>>`.
- **What the caller does:** recovers via `into_inner` — every site
  has an in-source comment justifying it (the inner is `Copy` /
  `Option<*const T>` / single-slot, so a panicking writer cannot leave
  half-initialised data).
- **Verdict:** INTENTIONAL-FALLBACK (well-documented at every site).

## Test-only sites (not flagged)

The following are intentionally silent in test code:

- `crates/ir/src/dot/tests.rs:166`
- `crates/ir/src/validate/tests.rs:685`
- `crates/opt/src/stack_load_forward/tests.rs:1208`
- `crates/opt/src/flag_cmp_canonicalize/tests.rs:379-380`
- `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:497`
- `crates/strider/src/strider/insn/control.rs:526` (inside a test)
- `crates/entity-utils/src/worklist.rs:205, 215`
- `crates/target/src/call_other_abi.rs:399, 418, 495, 544, 558, 604,
  612, 732, 774, 832` (`unwrap_or_else(|| panic!(…))` in tests is
  assertive, not silent)
- `crates/target/src/calling_convention/tests.rs:311, 422, 636-637,
  665, 695, 698`

## Recommendations summary

| Severity | Site | Fix |
|----------|------|-----|
| MED | `mem_chain_is_dirty` (F1) | Replace malformed-shape `false` returns with `Err`, mirroring the existing walker-bug `Err` at lines 533-545. |
| MED | `apply_in_place_edits` (F2) | Replace `if let Ok([out]) = ...` with `?`-propagating destructure. |
| LOW | F3 — `lookup_table().ok()?` | Document trait-API constraint inline; consider `eprintln!` on poisoning. |
| LOW | F4 — ELF parse failures | Stash first N error strings into stats. |
| LOW | F5, F9 | Add one-line "signature-guaranteed" comments. |
| LOW | F13 — unknown dot style | Tighten to typed enum so typos raise. |

No HIGH-severity findings.  The hot orchestrator paths
(`classify_anchor`-on-Ok-None, `apply_link_register`, `apply_tail_call`)
correctly propagate Err and use `Ok(None)` only for the documented
"can't resolve this iteration; defer" semantics that the fixed-point
loop is built around.  The `pattern` crate is silent-failure-clean.
The `pcode-lift` crate is silent-failure-clean.
