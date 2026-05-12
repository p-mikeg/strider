# Round 9 — Silent-failure audit (Cat 2C)

Branch: `review/ai3` @ `6b358ea`. Hunt every site that swallows real errors via
`unwrap_or` / `unwrap_or_default` / `unwrap_or_else(|_| default)` / `.ok()?` /
`if let Ok(_) … { … } else { default }` / bare `let _ = result`.

Rules of evidence: I read the surrounding code at every flagged site rather
than relying on prior-round notes. Findings are split into three buckets:
**Real bug**, **Documented optional fallback / OK**, **Border**.

## Round-8 cleanup verification (no regressions)

Confirmed all four are still clean at HEAD:

| Round-8 site                                                            | HEAD state                                                         |
|-------------------------------------------------------------------------|--------------------------------------------------------------------|
| `flag_cmp_canonicalize` had `.unwrap_or(a)` masking a width mismatch    | `grep unwrap_or crates/opt/src/flag_cmp_canonicalize/` returns nothing |
| `apply_tail_call` had `.unwrap_or(NodeOutputType::U64)` on Vn-size fail | `grep unwrap_or crates/strider/src/indirect_resolve/inplace.rs` returns nothing |
| `IndirectBranchResolve::optimize` had `anchor_contexts.get().unwrap_or(&empty_ctx)` | `crates/opt/src/indirect_branch_resolve/mod.rs:287` now `ok_or_else(...)` propagating a typed error |
| 3 strider-py mutex-poison `.ok()?` sites                                | Now use `unwrap_or_else(\|p\| p.into_inner())` with full poison-recovery rationale at every callsite (`pattern.rs:332`, `pattern.rs:352`, `reader.rs:143`, `reader.rs:676`) |

No regression observed. Each round-8 fix is structurally still in place.

## HIGH — Real bugs (silent unsoundness)

### 1. `read_or_init_var`: silent drop on byte-size→`NodeOutputType` mismatch
- **File:** `crates/strider/src/orchestrator.rs:786`
- **Code:** `let ty: ir::node::NodeOutputType = vn.size.try_into().ok()?;`
- **Swallowed error:** the `anyhow::Error("unsupported node output size: N bytes")` from `TryFrom<u32> for NodeOutputType` (rejects 3, 5, 6, 7, 9, 11–15, 17+, …) — see `crates/ir/src/node/output_type.rs:233`.
- **Why it's a real bug:** This helper is called from `build_anchor_calling_context` for every register in `arg_passing_regs` and `ret_val_regs` on every indirect-branch resolution. A CC whose convention preset declares an arg/ret register with an unsupported byte size is silently dropped — the resulting `Call` node's footprint *under-models* the call. Users defining a per-address override CC (e.g. `mips_n64`'s 16-byte-aligned vector slots, or BCD `pack8`) lose their argument plumbing without any diagnostic. The pipeline produces wrong analysis, not an error.
- **Recommendation:** propagate. Convert `read_or_init_var` to `Result<Option<NodeOutputId>>` and bubble a typed `UnsupportedConventionRegSize { vn, size }` error up to `build_anchor_calling_context` → `apply_in_place_edits` → `LoopState::run`. Strider's `run` already returns `anyhow::Result`, so adding a typed error doesn't disturb the outer surface.

### 2. `build_anchor_calling_context`: silent drop on clobbered-vn size mismatch
- **File:** `crates/strider/src/orchestrator.rs:729-731`
- **Code:** `let Ok(ty) = ir::node::NodeOutputType::try_from(vn.size) else { continue; };`
- **Swallowed error:** same `TryFrom<u32>` error as #1.
- **Why it's a real bug:** Same severity, same root cause as #1, but applied to the **clobber list**. A clobber dropped silently means the post-call IR will believe a register's pre-call value still holds — under-modelled call effect, false-positive forward-substitution by `StackLoadForward` and friends.
- **Recommendation:** identical to #1 — propagate as typed error rather than silently drop.

### 3. `classify_anchor_with_rom_and_sp`: known-bits-analysis error eprintln'd then converted to None
- **File:** `crates/strider/src/indirect_resolve/classify.rs:49-57`
- **Code:**
  ```rust
  let known = match opt::analyze_known_bits(graph) {
      Ok(k) => k,
      Err(e) => {
          eprintln!("strider: classify_anchor_with_rom_and_sp: analyze_known_bits failed: {e:?}");
          return None;
      }
  };
  ```
- **Swallowed error:** the *contradiction* path of `Kb::merge` (see `crates/opt/src/known_bits/mod.rs:71-77`), which the comment explicitly calls out as "real bugs we want to surface rather than silently let `ones` win." The wrapper here re-buries the very error that `merge` was designed to surface.
- **Why it's a real bug:** A KB contradiction means *the analyzer is contradictory* (same output proven both 1 and 0). Logging to stderr and returning None makes the orchestrator silently treat the indirect anchor as unresolved → the loop emits a generic `UnresolvedIndirectBranch` error from a totally different anchor. Root-cause is thrown away.
- **Recommendation:** change the signature to `Result<Option<ResolvedTargets>>` and propagate. The two callers (`crates/strider/src/orchestrator.rs:482`) already live inside `Result`-returning code paths.

### 4. `wrap_when` predicate: skipped `clear_graph_ptr` on `try_borrow` failure (raw-pointer hazard)
- **File:** `crates/strider-py/src/pattern.rs:462-464`
- **Code:**
  ```rust
  // Always invalidate the proxy's graph pointer so any
  // subsequent use from Python doesn't deref a stale ptr.
  if let Ok(b) = py_proxy.try_borrow(py) {
      b.clear_graph_ptr();
  }
  ```
- **Swallowed error:** `pyo3::PyBorrowError` from `try_borrow`.
- **Why it's a real bug:** the comment says "Always invalidate" but the code only invalidates **when the borrow succeeds**. If a Python callback inside the `.when` predicate captured `py_proxy` and is still re-entrantly borrowing it, `try_borrow` returns Err, the inner `clear_graph_ptr` is skipped, and the proxy's `graph_ptr` keeps pointing at a now-soon-to-be-freed `&BuiltFunctionGraph`. The next pure-Rust `with_graph` call on that same proxy will SAFETY-violate via `unsafe { &*ptr }` at `pattern.rs:357`.
- **Recommendation:** the proxy's `graph_ptr` is a `Mutex<Option<*const _>>`; the `clear_graph_ptr` impl already recovers from poisoning via `into_inner`. Switch to `unsafe { (*py_proxy.as_ptr()).clear_graph_ptr() }` or restructure so the matcher (which sets the pointer) is also responsible for clearing it on every exit path. At minimum, panic loudly when invalidation can't happen — never silently leave a dangling pointer alive.

### 5. `wrap_when` predicate: Python exception in user callback printed and discarded
- **File:** `crates/strider-py/src/pattern.rs:475-480`
- **Code:**
  ```rust
  Err(e) => {
      // Surface the predicate's exception to stderr but
      // treat it as "no match" to avoid aborting find_all in the middle of a walk.
      e.print(py);
      false
  }
  ```
- **Swallowed error:** every `PyErr` from the user's `.when()` callback.
- **Why it's a real bug:** the pre-existing comment is honest about the trade-off, but the trade-off is wrong. A buggy predicate that always raises produces zero matches across the whole walk; in non-interactive contexts (Jupyter capture, pytest, server logs) `e.print` lands in places the caller never sees, so the user observes "find_all returned []" and has no programmatic signal. That's the textbook silent-failure pattern.
- **Recommendation:** capture the first exception in a `RefCell<Option<PyErr>>` carried by the closure, treat the match as "no match" for that one node so the walk doesn't deadlock half-state, then re-raise after `find_all` returns. Alternative: short-circuit the entire walk (`Matcher::find_all` already returns a `Vec`; switching its public signature to `PyResult<Vec<Match>>` is a minor breakage worth taking).

## MEDIUM — Border / structurally questionable

### 6. `apply_elf_relocations_autoload` data-parse fallback uses `eprintln!` + skip without bumping a stats counter
- **File:** `crates/reader/src/elf.rs:766-779` (in `find_loadable_section_containing`).
- **Swallowed error:** `object::Error` from `sec.data()`.
- **Why it's MEDIUM:** the `RelocationStats` struct (lines 379-402) has dedicated counters for every other skip class (`skipped_unresolved_target`, `skipped_malformed_target`, `skipped_unsupported_kind`, `skipped_no_region`); section-data parse failures *during autoload search* are the only case that goes only to stderr. A malformed `.got.plt` data section silently disappears from the autoload pool → relocs that target it bucket as `skipped_no_region` instead of `skipped_malformed_target`, mis-attributing the failure mode. The `unwrap_or("?")` on `sec.name()` at line 775 is itself harmless (cosmetic label fallback) but is a symptom of "we already gave up" thinking around this branch.
- **Recommendation:** add `RelocationStats::skipped_malformed_section` (or reuse `skipped_malformed_target`) and increment it instead of (or alongside) the eprintln. The autoload site has access to `&mut stats` via the closure boundary — currently the closure is `Fn` not `FnMut` because of the `obj.sections().find(...)` predicate; restructure as a `for sec in obj.sections() { … }` loop to allow stats mutation.

### 7. `PyMemoryMap::read` (`MemReader::read`) silently swallows lookup-table-rebuild error
- **File:** `crates/strider-py/src/reader.rs:661`
- **Code:** `let table = self.lookup_table().ok()?;`
- **Swallowed error:** `anyhow::Error("MemoryMap regions lock poisoned")` / `"MemoryMap table lock poisoned"` from `rebuild_table` (lines 116-128).
- **Why it's MEDIUM:** Sleigh's `MemReader::read` returns `Option<u64>`, so the trait can't propagate. But the rebuild errors are RwLock-poisoning errors that are *recoverable* with the same `into_inner` strategy already used elsewhere in this file (`reader.rs:143`, `676`). Using `.read().map_err(|_| ...)?` instead of `.read().unwrap_or_else(|p| p.into_inner())` defeats the consistent recovery story already built around the file. A poisoned regions-lock makes every Sleigh decode return None silently → Sleigh complains "no bytes at addr X" far from the actual cause.
- **Recommendation:** rebuild_table should use `unwrap_or_else(|p| p.into_inner())` for its two RwLock acquires (matching the rest of this file), and `lookup_table` becomes infallible.

### 8. `PyReadOnlyMemoryAdapter::read` eprintln-on-Python-exception pattern
- **File:** `crates/strider-py/src/reader.rs:573-580` and `585-593`.
- **Swallowed error:** every `PyErr` from the user's `ReadOnlyMemory.read` callback.
- **Why it's MEDIUM:** comment at 567-572 explicitly justifies the design ("contract is still `Option<u64>` (we can't propagate through this trait) but the user gets a visible warning"). Honest about the trade-off, but the same critique as #5 applies: stderr gets eaten in non-interactive contexts.
- **Recommendation:** stash the first `PyErr` on the adapter's struct (it's already a `&self`); after the orchestrator's `run` returns, raise the saved exception in `run_via_orchestrator`. No trait-signature change needed.

### 9. `StackArrayClassify::shape_from_load`: `node_inputs_exact` shape-check silently drops malformed Loads
- **File:** `crates/opt/src/indirect_branch_resolve/stack_array.rs:250`
- **Code:** `let [mem_input, addr_output] = graph.node_inputs_exact::<2>(load_node).ok()?;`
- **Swallowed error:** `IrError::WrongInputArity` from `node_inputs_exact` when a `Load` doesn't have exactly 2 inputs.
- **Why it's MEDIUM:** The validator (Layer A in `crates/ir/src/validate/mod.rs`) enforces this signature, so the only path that reaches this code with the wrong shape is when validation has been disabled (e.g. mid-pipeline before the final validate). The classifier's contract is "best-effort match → return None on shape mismatch" so silently bailing is consistent with that contract — but it now masks a real invariant violation if it ever happens.
- **Recommendation:** swap to `let [mem_input, addr_output] = fg.node_inputs_exact::<2>(load_node)?;` (propagating the IrError via the function's `Result` return) — the function already returns Option; switching to Result here would force the caller to bubble. Lower-effort alternative: replace `.ok()?` with `.expect("Load has 2 inputs (validator-enforced)")`. Both options surface the invariant violation rather than masking it as "wrong shape".

### 10. `find_placeholder_return_for_anchor`: `if let Ok([_,_,val]) = ...` silently treats wrong-arity IndirectBranch as "not the placeholder"
- **File:** `crates/opt/src/indirect_branch_resolve/mod.rs:346-350`
- **Swallowed error:** `IrError::WrongInputArity` from `node_inputs_exact::<3>`.
- **Why it's MEDIUM:** Same critique as #9 — validator should catch wrong-arity IndirectBranch, but if it doesn't, the resolver silently doesn't find any placeholder for the anchor → the orchestrator emits a generic UnresolvedIndirectBranch with no hint about the actual mismatch.
- **Recommendation:** `.expect("IndirectBranch has 3 inputs (validator-enforced)")`.

### 11. `KnownBits` ShiftLeft/ShiftRight: `unwrap_or(u64::MAX)` masks rhs-type mismatch
- **File:** `crates/opt/src/known_bits/mod.rs:179` and `220`
- **Code:** `.as_value().and_then(u64_type_mask).unwrap_or(u64::MAX);`
- **Swallowed error:** "rhs of a Shift is not an integer-typed value-edge".
- **Why it's MEDIUM:** The validator forbids non-integer rhs to ShiftLeft/ShiftRight. The fallback `u64::MAX` here disables KB narrowing for the affected shift, which is harmless under a sound validator but turns a real invariant violation into a silent loss-of-precision. The `u64::MAX` choice is also slightly wrong if reached: it would treat every shift-amount bit as unknown, when the right answer for a non-integer rhs is "this isn't a Shift node, abort propagation."
- **Recommendation:** propagate via `?` from the surrounding `Result<…>` (the caller `analyze_known_bits` returns `anyhow::Result`, so threading through is mechanical).

### 12. `ir::Graph::get_as_unsigned_int` / `get_as_signed_int` collapse "doesn't fit u64/i64" with "isn't a constant"
- **Files:** `crates/ir/src/builder/coerce.rs:81` and `105`
- **Code:** `Ok(output_type.get_unsigned_int(*val).and_then(|v| u64::try_from(v).ok()))`
- **Swallowed error:** `TryFromIntError` when a U128/U256 IntConst's value exceeds u64.
- **Why it's MEDIUM:** the docstring at 95-96 says "Returns `Ok(None)` for non-constant nodes." But the code also returns `Ok(None)` for constants whose value doesn't fit u64. Two distinct conditions collapse to one return value — a caller that needs to know "this is a constant, but I asked for too-narrow a result type" can't tell.
- **Recommendation:** either widen the docstring to acknowledge the second case, or split the helper into `get_as_unsigned_int_u64` + `get_as_unsigned_int_u128` (the latter never overflows, since `get_unsigned_int` already returns u128 within type-mask).

### 13. `RegionIndex::region_for_placeholder`: silent `?` on missing control input
- **File:** `crates/strider/src/orchestrator.rs:154-155`
- **Code:** `let inputs: Vec<_> = graph.graph.node_inputs(placeholder).into_iter().collect(); let ctrl_in = *inputs.first()?;`
- **Swallowed signal:** "placeholder has zero inputs" — an IR-shape invariant violation.
- **Why it's MEDIUM:** A placeholder Return must have a control input (validator enforces). If the function's caller (`build_anchor_calling_context`) gets a `None` here, it falls through to using a fresh `InitialVar` for every CC arg/ret reg — silently reading the function's entry value rather than the placeholder-region's exit value. Validator catches this in normal flow, but a future refactor that loosens the validator on this slot would silently mis-classify.
- **Recommendation:** `.expect("placeholder has at least one input (control)")`.

## OK — Documented optional fallback / harmless

| File:Line | Pattern | Reason it's OK |
|-----------|---------|----------------|
| `crates/cfg/src/cfg/decode_cache.rs:60, 66` | `unwrap_or_else(\|e\| e.into_inner())` | Mutex-poison recovery with documented rationale (lines 53-57): inner is `FxHashMap`, prior-panic state is consistent, recovery is sound. |
| `crates/strider-py/src/pattern.rs:332, 352`; `crates/strider-py/src/reader.rs:143, 676` | Same `unwrap_or_else(\|p\| p.into_inner())` | Round-8 cleanup; each has a 4-6 line comment justifying poison recovery. |
| `crates/cfg/src/cfg/builder/region_builder.rs:405-407` | `match … Err(_) => return Ok(DidntFinishProcessing)` | Documented at lines 393-397: "fall through to today's behaviour if input shape is unexpected — the IR layer's strict-on-emission check will surface any real problem with full context". CallOther id-vn shape check; unmodelled inputs route through the strict path. |
| `crates/reader/src/elf.rs:513, 517, 560, 564, 583` | `.ok()` then bucket as `stats.skipped_*` | All five sites bump a typed counter (`skipped_malformed_target` / `skipped_unresolved_target`); the original `object::Error` is summarised, not lost. |
| `crates/reader/src/lib.rs:188` | `.ok()?` on `usize::try_from(addr.checked_sub(...))` | Documented `Option<usize>` contract at lines 175-186: returns `None` for out-of-region reads; the `try_from` here only fails on a 32-bit host with a >4 GiB `data.len()`-shaped offset which is impossible (slice indexing already requires `usize`). |
| `crates/ir/src/wide_const.rs:78, 92` | `try_into().ok()?` on slice-of-known-length | Bounds checked at lines 73, 87. The `?` is defensive boilerplate for an unreachable conversion. |
| `crates/opt/src/sp_expr.rs:215, 243` | `i64::try_from(signed).ok()` | Documented contract at lines 200-202: returns None when the constant doesn't fit i64. SP-expression decomposition is a structural classifier; None means "not an SP-relative constant we can fold". |
| `crates/opt/src/indirect_branch_resolve/jump_table.rs:309, 641, 665` | `u64::try_from(...).ok()?` | Pure classifier give-up; the surrounding function returns `Option<u64>` and the contract is "best-effort match, propagate None on mismatch". |
| `crates/opt/src/indirect_branch_resolve/stack_array.rs:104-105, 426` | `i64::try_from(i).ok()?` etc. | Same classifier-give-up contract; line 426 has `s_u128 < 64` checked at 422 so the `try_from` is unreachable — defensive `?`. |
| `crates/opt/src/load_readonly/mod.rs:85`; `crates/cfg/src/cfg/builder/indirect_resolve.rs:291` | `.and_then(|v| u64::try_from(v).ok())` | LoadReadOnly's per-Load classifier; for byte_size > 8 (U128/U256) the `read(space, addr, 16)` upstream returns None first, so the try_from never narrows real bits — defensive. |
| `crates/opt/src/known_bits/mod.rs:30, 56` | `u64::try_from(...).ok()?` | `bit_mask_u128` is gated by `fits_u64()` at line 27; the conversion is unreachable. |
| `crates/strider-py/src/run.rs:70` | `unwrap_or_default()` on `per_address_ccs` | Optional kwarg; default = empty HashMap; semantically "no per-address CCs supplied". |
| `crates/strider/src/strider/pipeline.rs:322, 330` | `unwrap_or_else(\|\| self.find_all_unique_vns(cfg))`; `unwrap_or(0)` | Caller-Option fallback; sizing default 0 (max of empty iterator). |
| `crates/ir/src/dot/label.rs:279, 304, 322` | `unwrap_or_default` / `unwrap_or_else(\|_\| format!("{kind:?}"))` | Visualization-only fallbacks; never affects analysis. |
| `crates/dot/src/lib.rs:196` | `let _ = std::fmt::write(&mut out, …)` | `String`'s `Write` impl is infallible (per std docs); `let _` discards an Err that can't occur. |
| `crates/graphwalk/src/lib.rs:48, 79` | `let _ = self.try_successors(...)` | Closure always returns `Continue(())`; `let _` discards a `ControlFlow` that's never `Break`. |
| `crates/opt/src/function_args/mod.rs:109`; `crates/opt/src/redundant_phis/mod.rs:206` | `let _ = detach_unreachable_nodes(...)` | Documented at the callsite (4-line comment): "post-pass return values are ignored by the pipeline; don't escalate it into Changed". Hygiene cleanup, not progress. |
| `crates/opt/src/function_args/mod.rs:523` | `results.pop().unwrap_or(true)` | Conservative fallback (true = dirty memory chain) when result-stack invariant violated; debug-asserted at 522. Conservative = analytically sound. |
| `crates/strider/src/strider/insn/mod.rs:143` | `let _ = self.builder.build_call_other_terminal(...)?;` | The `?` propagates Err; the `let _` only discards the success-NodeId return. |
| `crates/strider-py/src/pattern.rs:378-383` | `if let Ok(...) extract::<X>() { … }` two-type fallback | PyO3 type-coercion try-then-fallback; final TypeError raised at line 384 if both attempts fail. |
| `crates/strider-py/src/reader.rs:701-703, 736-738` | Same two-type extract pattern | Same justification. |
| `crates/strider-py/src/graph.rs:285` | `Err(e) => Ok(Some(format!("{e}")))` | Validator-error contract: validate returns `Option<String>` to Python, message is preserved. |
| `crates/opt/src/indirect_branch_resolve/jump_table.rs:713-717` | `if let Ok([_token, val]) = node_inputs_exact::<2>(node)` | Defensive shape match for "trivial phi has exactly 1 value input"; non-trivial phis correctly fall through. |
| `crates/strider/src/orchestrator.rs:540` | `if let Ok([out]) = node_outputs_exact::<1>(nid)` | InitialVar always has 1 output (signature-enforced); defensive shape match. |
| `crates/strider/src/orchestrator.rs:702-703` | `override_cc.unwrap_or_else(\|\| strider.calling_convention())` | Optional override fallback; documented at lines 699-701. |
| `crates/entity-utils/src/set.rs:123` | `upper.unwrap_or(lower)` | Iterator size-hint fallback for capacity allocation. |

## Coverage report

**Files inspected (non-test, non-bench, non-example):**

- `crates/reader/src/elf.rs` — 5 `.ok()` sites + 1 eprintln-on-skip + 1 cosmetic `unwrap_or("?")`
- `crates/reader/src/lib.rs` — 1 documented `.ok()?`
- `crates/strider-py/src/pattern.rs` — 4 mutex-poison recoveries (verified clean) + 1 raw-pointer hazard (#4) + 1 predicate eprintln (#5) + 2 type-coercion fallbacks
- `crates/strider-py/src/reader.rs` — 2 mutex-poison recoveries (verified clean) + 2 callback-eprintln paths + 1 lookup-table swallow (#7) + 2 type-coercion fallbacks
- `crates/strider-py/src/run.rs` — 1 `unwrap_or_default` (OK)
- `crates/strider-py/src/graph.rs` — 1 validator-error stringification (OK)
- `crates/strider-py/src/cfg.rs`, `dot.rs` — style-string defaults (cosmetic)
- `crates/strider/src/orchestrator.rs` — 2 size-conversion silent drops (#1, #2) + 1 input-shape defensive `?` (#13) + 1 override-CC fallback (OK)
- `crates/strider/src/indirect_resolve/classify.rs` — 1 known-bits-error eprintln-and-skip (#3)
- `crates/strider/src/indirect_resolve/inplace.rs` — round-8 cleanup verified
- `crates/strider/src/strider/pipeline.rs` — 2 fallback defaults (OK)
- `crates/strider/src/strider/insn/mod.rs` — `let _ = …?` (OK)
- `crates/strider/src/strider/insn/control.rs` — test-scope `let _` (OK)
- `crates/cfg/src/cfg/decode_cache.rs` — 2 mutex-poison recoveries (OK with rationale)
- `crates/cfg/src/cfg/builder/region_builder.rs` — 1 documented `Err(_) => …` fallback (OK)
- `crates/cfg/src/cfg/builder/indirect_resolve.rs` — 1 classifier conversion (OK)
- `crates/opt/src/known_bits/mod.rs` — 4 fallback masks (1 MEDIUM #11, 3 OK)
- `crates/opt/src/load_readonly/mod.rs` — 1 classifier conversion (OK)
- `crates/opt/src/sp_expr.rs` — 2 documented `i64::try_from(...).ok()` (OK)
- `crates/opt/src/indirect_branch_resolve/jump_table.rs` — 3 classifier `.ok()?` (OK) + 1 trivial-phi shape match (OK)
- `crates/opt/src/indirect_branch_resolve/stack_array.rs` — 1 invariant shape-check (#9 MEDIUM) + 4 classifier conversions (OK) + 1 unreachable defensive `?` (OK)
- `crates/opt/src/indirect_branch_resolve/mod.rs` — 1 invariant shape-check (#10 MEDIUM); round-8 anchor_contexts cleanup verified
- `crates/opt/src/function_args/mod.rs` — 1 documented post-pass discard (OK) + 1 conservative pop fallback (OK)
- `crates/opt/src/redundant_phis/mod.rs` — 1 documented post-pass discard (OK)
- `crates/opt/src/if_cond_inversion/mod.rs` — 1 defensive shape check (OK with caveat)
- `crates/opt/src/flag_cmp_canonicalize/` — round-8 cleanup verified
- `crates/ir/src/builder/coerce.rs` — 2 size-conversion fallbacks (#12 MEDIUM)
- `crates/ir/src/ops/consts.rs` — 1 size-conversion (OK; comment cross-references `int_const_val_u128`)
- `crates/ir/src/wide_const.rs` — 2 unreachable defensive `?` (OK)
- `crates/ir/src/dot/label.rs`, `render.rs` — visualization-only (OK)
- `crates/dot/src/lib.rs` — 1 unreachable `let _` (OK)
- `crates/graphwalk/src/lib.rs` — 2 ControlFlow-discard wrappers (OK)
- `crates/entity-utils/src/set.rs` — 1 capacity fallback (OK)
- `crates/target/src/call_other_abi.rs` — every `unwrap_or_else(...|| panic!(...))` is in a `#[cfg(test)]` module (verified by reading lines 399, 418, 495, 544, 558, 604, 612, 732, 774, 832 — all under `mod tests` blocks at the top of the file).
- `crates/ir/src/node_signature.rs:774` — `unwrap_or_else(|| panic!(...))` is in test-only path (verified the `cfg(test)` annotation upstream).

**Findings counts:**
- **HIGH (real bugs):** 5 (#1–#5)
- **MEDIUM (border / structurally questionable):** 8 (#6–#13)
- **OK (documented or harmless):** 27 sites tabulated above

**Round-8 cleanups verified intact:** 4/4 (no regressions).

**Suggested first cuts (highest value/effort):**
1. #1 + #2 together: one `Result`-flavoured signature change in `read_or_init_var` and `build_anchor_calling_context`'s clobber loop — both share root cause and fix.
2. #3: change one return type to `Result<Option<…>>` in `crates/strider/src/indirect_resolve/classify.rs` and wire through two callers.
3. #4: tighten `wrap_when` to never leave a dangling pointer alive — pure-Rust defensive change.
4. #6: add stats counter for autoload section-data parse failures.
