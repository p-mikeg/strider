# Round 10 — Pre-fix Verification of HIGH Findings

Each round-10 HIGH finding re-verified against current source. Mirrors the round-9 verification methodology (which caught the H-1 sysret false positive).

**Verdict legend:**
- **CONFIRMED** — claim matches code; fix
- **CONFIRMED-LOW** — bug is real but very low impact; fix
- **PARTIAL** — claim has merit but design has justification
- **BY-DESIGN** — explicit doc in source justifies; skip
- **FALSE** — agent misread the code; skip
- **DEBATABLE** — current behavior arguably correct; skip

---

## R10-1C C-1 — `IndirectBranchResolve` KB cache invalidated by `apply_tail_call`

**Verdict:** **FALSE.**

The cached `known: KnownBitsMap` is keyed on `NodeOutputId`. `analyze_known_bits` propagates KB values forward from inputs via the `output_uses` use-list during the worklist propagation. Once the analysis converges, KB values are STORED in the SecondaryMap, independent of use-lists.

`apply_tail_call::detach_node_inputs(placeholder)` removes the placeholder from its operand outputs' use-lists. This DOES NOT change any operand output's KB value (KB is computed from inputs going up, not consumers). It also creates fresh nodes (IntConst, Call, Return) with NEW NodeOutputIds — the cached map has no entries for these, so classifier lookups return default `Kb::default()` (no info), which is conservative-safe.

`apply_link_register` mutates the placeholder's NodeKind from `IndirectBranch` to `Return`, but the placeholder has no integer value outputs (it's terminal); no KB entries are affected.

**Action:** No fix.

---

## R10-1C C-2 — `FunctionArgDetect` exact-width register-arg path drops `InitialVar` fingerprint

**Verdict:** **CONFIRMED-LOW.**

`detect_register_args` at `crates/opt/src/function_args/mod.rs:189-198` creates a `FunctionArg` node and immediately calls `replace_all_uses(old_out, new_out)` without `extend_asm_fingerprint_from(new_node, initial_var)`. The stack-args path at line 331-334 has an explicit comment justifying the skip ("FunctionArg is exempt from the fingerprint check; doing so would couple FunctionArg's identity to the loads it happens to subsume").

The justification applies to the STACK-args path (multiple Loads → one FunctionArg, coupling concern). For the REGISTER-args path, only ONE InitialVar is replaced — no coupling concern. Round-9 H-3 verdict was "PARTIAL/not a bug" because it conflated the two paths; round-10 audit correctly distinguishes them.

**Action:** Add `fg.extend_asm_fingerprint_from(new_node, initial_var)` to the register-arg path before `replace_all_uses`.

---

## R10-1D C-1 — `GuardPat` zero-output silent-fail

**Verdict:** **BY-DESIGN.**

`crates/pattern/src/pat/guards.rs:30-46` has an explicit doc comment naming this limitation as STRUCTURAL: "Both [`GuardFn`] variants require a [`NodeOutputType`] (sourced from `target`'s output kind) which a zero-output node cannot supply, so the limitation is structural rather than something this combinator can bridge transparently."

The doc tells callers to use `ret().preceded_by(<witness>.when(p))` instead. The proposed round-10 fix (delegate `try_match_node` for zero-output bases, skipping the guard) would silently bypass the predicate, which is silently incorrect (the user thinks the guard fired). The current silent-fail is no worse.

**Action:** No code fix. Doc could be made more prominent at the public `.when()` constructor — minor.

---

## R10-1D C-2 — `*_any` variant-agnostic captures bind `output: None`

**Verdict:** **PARTIAL** — by-design but enhanceable.

The `impl_variant_any!` macro post-match closure intentionally binds only the `NodeId` (line 69, 107, 139): `b.bind_capture(c, Binding::new(node, None))`. Documented as "After the match, callers recover the concrete op variant via the matching `Match::get_*_op` helper" — node-only, no output.

The round-10 finding wants `int_binary_any(c, _, _)` to also populate the value output so `match.output(c)` works. Populating the value output is a strict superset of current behavior and harmless (no doc lies, just enables `.output(c)`). Fix is one-liner per macro arm.

**Action:** Fix — populate `Some(value_output)` in each `bind_capture` call.

---

## R10-1F F-01 — `PyMemPhiPat`/`PyValuePhiPat` missing from `PatLike`

**Verdict:** **CONFIRMED.**

`PatLike` enum at `crates/strider-py/src/pattern.rs:237-255` lists 16 variants. `PyMemPhiPat` (line 734) and `PyValuePhiPat` (line 767) are registered as Python classes (lines 1970-1971) and have `pat_builder_finalise!` applied (lines 2103-2104), but are NOT in `PatLike`. `g.find_all(mem_phi())` raises `TypeError`.

**Action:** Add 2 variants + 2 dispatch arms in `PatLike::into_pat`.

---

## R10-1F F-02 — `PyCapture.__hash__` truncation

**Verdict:** **CONFIRMED-LOW** — practically unreachable on 64-bit.

`crates/strider-py/src/pattern.rs:103` returns `self.inner.id() as isize`. On 32-bit platforms, isize is 32 bits, so capture ids ≥ 2³¹ would silently sign-wrap. Linux x86_64 (the deployment target) has 64-bit isize where `u32 as isize` is always non-negative. The per-process atomic counter would have to allocate 2 billion captures to hit the wrap on 32-bit.

**Action:** Fix — `(self.inner.id() as i64) as isize` is robust on both archs.

---

## R10-2C H10-S1 — `LoopState::recompute_unresolved` silent empty Vec on missing graph

**Verdict:** **CONFIRMED-LOW.**

`crates/strider/src/orchestrator.rs:607-609` returns empty Vec when `self.graph` is None. This is a state-machine bug masking — graph should always be Some at this point. Returning empty silently makes `apply_stall_guard` see no unresolved entries, hiding the real bug.

**Action:** Convert to `Result<Vec<...>>`, surface the inconsistency.

---

## R10-2C H10-S2 — `KnownBits::ZeroExtend` `unwrap_or(0)` poisons analysis

**Verdict:** **CONFIRMED.**

`crates/opt/src/known_bits/mod.rs:279`: `let input_mask = u64_type_mask(input_ty).unwrap_or(0);`. For unsupported widths (`U128`, `U256`), `u64_type_mask` returns `None`. Result: `input_mask = 0`, then line 283 sets `zeros: kb.zeros | (type_mask ^ 0) = kb.zeros | type_mask` — marks ALL bits of the output as known-zero, which is silently wrong. Sister `SignExtend` arm at lines 294-296 correctly bails with `let-else None`.

**Action:** Match `SignExtend`'s bail pattern.

---

## R10-2C H10-S3 — ELF autoload `eprintln!`-only diagnostic

**Verdict:** **CONFIRMED-LOW.**

`crates/reader/src/elf.rs:778-784` prints to stderr when an autoload section's `data()` parse fails. Caller sees only "no region returned" and counts as `skipped_no_region` — doesn't see the parse failure. Diagnostic is invisible to programmatic callers.

**Action:** Add a counter to `RelocationStats` (e.g., `autoload_section_parse_failures: u32`) so callers can detect.

---

## R10-2C H10-S4 — `PyMemReader.read` failure-mode collapse

**Verdict:** **FALSE.**

`crates/strider-py/src/reader.rs:495-513` actually preserves distinct error messages for each failure mode:
- Python exception → `"PyMemReader.read raised: {e}"`
- None return → `"address {:#x} is not mapped (Python read returned None)"`
- Wrong type → `"PyMemReader.read must return bytes: {e}"`

These are 3 distinct messages. The `MemReadError` type is one variant carrying a string, but the strings preserve the failure-mode distinction. The original Python exception text propagates via `{e}`. Not a collapse.

**Action:** No fix.

---

## R10-2C H10-S5 — `PyReadOnlyMemoryAdapter` doesn't re-raise `KeyboardInterrupt`/`SystemExit`

**Verdict:** **CONFIRMED.**

`crates/strider-py/src/reader.rs:566-595` catches all `Err(e)` paths into `eprintln!` + return `None`. No `is_instance_of::<PyKeyboardInterrupt>` or `<PySystemExit>` check. Round-9 H-8 added this for `wrap_when` in pattern.rs but the same fix wasn't applied here.

**Action:** Mirror wave-31's `wrap_when` pattern: detect base exceptions and call `e.restore(py)`.

---

## R10-2C H10-S6 — `mem_chain_is_dirty` `unwrap_or(true)` masks invariant violations in release

**Verdict:** **CONFIRMED-LOW.**

`crates/opt/src/function_args/mod.rs:521-522`: debug_assert in debug builds, `unwrap_or(true)` in release. If a future walker bug pushes 0 or 2 results onto the stack, release builds get conservative `true` and silently lose the optimization. Debug build catches it but CI needs to run debug to surface.

**Action:** Convert to `Result<bool>` so the invariant violation propagates as Err. Bigger refactor; defer to lower priority but worth doing.

---

## R10-2D — `opt::Kb` `pub` ones/zeros fields invariant unenforced

**Verdict:** **CONFIRMED-LOW.**

`crates/opt/src/known_bits/mod.rs:38-43`: doc says "must never overlap (`ones & zeros == 0`)". Internal ctors (`from_const`, `merge`) enforce, but external code can do `Kb { ones: 0xFF, zeros: 0xFF }` directly via struct literal.

**Action:** Add inline doc warning on each field (mirroring round 9's BFG-fields pattern). Tightening to `pub(crate)` would break callers iterating `KnownBitsMap`.

---

## R10-2D — `BuiltCallingConvention::from_parts` unvalidated `pub`

**Verdict:** **CONFIRMED.**

`try_from_parts` exists and validates. The unvalidated `from_parts` is also `pub`. A typo in CC parts (e.g., overlapping arg/callee-saved sets) silently miscompiles.

**Action:** Rename `from_parts` → `from_parts_unchecked` (with `#[doc(hidden)]` and a clear "test-only escape hatch" comment). Make `try_from_parts` the only validated public path.

---

## R10-2D — `BuiltFunctionGraph` 5 `pub` fields with hazard warnings

**Verdict:** **DEFER.**

Round 9 wave 31 added inline doc warnings on every CC field (`call_clobbered`, `ret_val_regs`, `call_other_clobbered`, `variables`). Round-10 audit calls this HIGH because the warnings are advisory only — fields are still `pub`. Tightening to `pub(crate)` blocks 4 pattern test scaffolds that mutate via `from_graph_and_entry_for_rewrite` + direct assignment.

**Action:** Defer (round 9's verdict). The migration path is to replace the test scaffolds with proper builder methods first; that's a separate, larger task.

---

## R10-1B I-1 — `Region.contains_addr` returns false on empty regions; downstream duplicate-region risk

**Verdict:** **CONFIRMED.**

`crates/cfg/src/cfg/types.rs:233-237`: `match self.insns.last() { Some(last) => ..., None => false }`. `add_region` allows empty Branch-terminated regions (verified at `crates/cfg/src/cfg/builder/mod.rs:184-192`). For empty regions, `contains_addr(start_addr)` returns false — but the region OWNS `start_addr`. Downstream `find_region_containing_addr` returns None for the start address; the work queue may push a duplicate region.

**Action:** Make `contains_addr` return `start_addr == addr` for empty regions. Update Region doc to drop the "Never empty" claim (matches the post-fix-impl reality).

---

## R10-1B I-3 — `handle_int_sub` neg width sized from `inputs[1].size`

**Verdict:** **DEBATABLE / by-design.**

Wave-31 IMP-4 changed `handle_int_sub`'s `Neg` width from `out_ty` to `inputs[1].size`. R10-1B I-3 says this is wrong because if Sleigh emits a width-mismatched `IntSub`, the IR builder's `build_int_binary_operation` would Err on width mismatch. But Sleigh's contract guarantees `input_size == output_size` for IntSub, so this case never happens in practice. The wave-31 fix is defensive, not buggy. The round-10 finding misanalyzes — both behaviors are sound for the current Sleigh contract.

**Action:** No fix.

---

## Summary

| # | Finding | Verdict | Action |
|---|---------|---------|--------|
| 1 | R10-1C C-1 KB cache | FALSE | Skip |
| 2 | R10-1C C-2 register-arg fingerprint | CONFIRMED-LOW | **Fix** |
| 3 | R10-1D C-1 GuardPat zero-output | BY-DESIGN | Skip |
| 4 | R10-1D C-2 *_any output binding | PARTIAL | **Fix** |
| 5 | R10-1F F-01 PatLike missing variants | CONFIRMED | **Fix** |
| 6 | R10-1F F-02 hash truncation | CONFIRMED-LOW | **Fix** |
| 7 | R10-2C H10-S1 silent empty Vec | CONFIRMED-LOW | **Fix** |
| 8 | R10-2C H10-S2 ZeroExtend unwrap_or | CONFIRMED | **Fix** |
| 9 | R10-2C H10-S3 ELF autoload eprintln | CONFIRMED-LOW | **Fix** |
| 10 | R10-2C H10-S4 PyMemReader collapse | FALSE | Skip |
| 11 | R10-2C H10-S5 ROM Kbd/SysExit | CONFIRMED | **Fix** |
| 12 | R10-2C H10-S6 mem_chain unwrap_or(true) | CONFIRMED-LOW | **Fix** |
| 13 | R10-2D Kb pub fields | CONFIRMED-LOW | **Fix (doc warnings)** |
| 14 | R10-2D BuiltCC::from_parts | CONFIRMED | **Fix** |
| 15 | R10-2D BFG pub fields | DEFER | Skip — blocked by tests |
| 16 | R10-1B I-1 contains_addr empty | CONFIRMED | **Fix** |
| 17 | R10-1B I-3 handle_int_sub width | DEBATABLE | Skip |

**Verified-actionable: 11 out of 17.** Skipped: 6 (3 FALSE, 1 BY-DESIGN, 1 DEBATABLE, 1 DEFER).

Plus doc fixes from R10-3A (HIGH refutations in `opt/README.md`) and R10-3B (HIGH stale-comment fixes including `with_built` ghosts and `pipeline.rs:137` typo) — all CONFIRMED, all easy edits.

Plus skill audit edits from R10-skill-audit — line-number refreshes and stale references.
