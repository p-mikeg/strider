# Deep-audit remediation

> Source: 8 parallel read-only review subagents (one per crate cluster), 4 axes
> each + panic audit. Every item below must be **independently re-verified
> against the code before fixing** (don't trust the subagent or comments).
> Status legend: ☐ to-verify · ✅ verified+fixed · ❎ rejected/intentional (no change).

## Panic audit — global result

All 8 clusters report **zero production NEEDS-FIX panics**: every reachable
`unwrap`/`expect`/`panic!`/`unreachable!`/index/div in production is either
test-only or guarded by a documented invariant. The "production returns errors,
not panics" rule already holds. Remaining work is only to *document* a couple of
sound-but-implicit invariants (see DOC items). Re-confirm with a global grep.

## P0 — correctness bugs (real wrong output)

- ☐ **B1 [reader] `R_MIPS_REL32` drops symbol value `S`** — `crates/strider-reader/src/elf/relocations.rs:607-628`. Handled in `image_relative_reloc` returning `(addend, size)` only; comment says semantics are `S + A`. Defined-symbol REL32 (MIPS GOT/func-ptr slots) gets patched with `addend` (usually 0) instead of `symbol+addend`. Fix: route defined-symbol REL32 through the symbol-resolving path, or restrict image-relative arm to symbol-index-0.
- ☐ **B2 [target] PPC float-return regs list `f2`** — `crates/strider-target/src/calling_convention/mod.rs:614` (`powerpc_sysv32`), `:658` (v1), `:692` (v2). PPC SysV returns floats only in `f1`; `f2` is an arg reg. Cross-check rsleigh `ppc_*.cspec` output blocks. Fix: `ret_val_regs_float: &["f1"]`. (MIPS `f0,f2` is correct — leave.)
- ☐ **B3 [py] ARM Thumb detection broken in `load()`** — `crates/strider-py/strider/_api.py:155,284-287`. `load()` calls `_arch_and_cc_for_elf(header)` with no entry; defaults to non-Thumb `arm()`. `analyze(entry)` strips the `&1` interworking bit but never switches `self._arch` to `arm_thumb()`. Thumb fn → wrong `.sla` → wrong pcode, silent. Fix: detect Thumb in `analyze()` (re-derive arch from entry) or thread entry into arch selection.
- ☐ **B4 [py] `.pyi` stubs badly out of sync** — `crates/strider-py/strider/{pattern.pyi,__init__.pyi}`. Ghost fns `cast_to_int/cast_to_bool/cast_to_float` (don't exist — matches "no Cast* nodes" invariant), fictional `BoolBinaryPat` class, wrong return types (`ret`/`if_`/`bool_binary`), ~30 missing methods/classmethods/exports. Root cause = hand-maintained stubs (see S7). Overlaps Python-docs deliverable.
- ☐ **B5 [ir] non-saturating `addr_off + size`** — `crates/strider-ir/src/builder/mod.rs:61` (`upgrade_to_tracked_for`), `:117` (`dedup_overlapping_largest`). Sibling `largest_container_for:590` uses `saturating_add` for high-offset ppc64/aarch64be CR slices; these two use plain `+` → debug overflow-panic / release wrap → misclassify containment. Fix: `saturating_add` in all three (single convention).

## P1 — soundness gaps (conditional / lower severity)

- ☐ **S1 [pattern] `find_all_requirements` ignores shared `OffsetCapture`** — `crates/strider-analyze/src/pattern/matcher/mod.rs:650-661` (`prefix_agrees`) + `matcher/bindings.rs:197-199` (`iter`). Cross-pattern join compares only `Capture` entries, not `offset_entries`; re-introduces offset-without-base unsoundness at the cross-pattern level. Fix: add offset-aware agreement to the join, or document that OffsetCaptures are match-local + reject reuse across patterns.
- ☐ **S2 [pattern] `bool_binary_any`/`bool_unary_any` match any width** — `crates/strider-analyze/src/pattern/pat/ctor/variant_agnostic.rs:186-196`. No `I1`-output guard (unlike `bool_and`/`bool_binary` which call `require_i1_output()`); over-fires on wide `And`/`Or`. Fix: append I1-output check to the `bool_*_any` post-match.
- ☐ **S3 [opt] `find_stack_stored_value_at_offset` walks past any non-SP store** — `crates/strider-analyze/src/opt/load_forward/mod.rs:727-730`. No `AliasMode` gate, accepts opaque `Anchor` addresses, unlike SSoT `step_through_store` (sp_expr/walk.rs:44-61). Aliasing opaque store between a stack-array label store and the dispatch load → stale (wrong) jump target. Fix: delegate per-store verdict to `step_through_store` (also resolves S5 dup).
- ☐ **S4 [orchestrator] `handle_switch` `None` arm re-reads dispatch index** — `crates/strider-analyze/src/strider/insn/control.rs:172-176`. Fresh pre-optimization `read_vn` may differ from the SSA value the classifier proved exhaustive; if `Switch.target_value` can be `None` on a resolved jump table, the unconditional final-else routes OOB index to last target. Fix: verify `target_value` is always `Some` on the orchestrator rebuild path (check strider-lift Switch construction); if reachable, `bail!`.
- ❎ **S5 [ir] `update_input` leaves dedup cache incomplete** — `crates/strider-ir/src/graph/uses.rs:152`. Test `update_input_on_cacheable_evicts_stale_cache_entry` asserts the orphaning is intended. Decision: leave behavior; consider doc note on `create_node` re dedup-completeness after in-place edits. Re-verify the scalability claim is bounded.
- ☐ **S6 [ir] `largest_container_for` may return non-largest on partial overlap** — `crates/strider-ir/src/builder/mod.rs:594`. Pop test `end < v_end` discards an enclosure that still contains later narrows. HW regs nest (never partial-overlap) so low impact. Fix: pop only when `end < v_start`. Re-verify with a constructed overlap case.
- ☐ **S7 [lift] one-OOB `CondBranch` rewritten to unconditional `Branch`** — `crates/strider-lift/src/cfg/builder/region_builder.rs:352-383`. Drops the predicate (over-approx edge presented as faithful). Likely intentional under `fn_max_size`. Decision: judge — at minimum record the dropped predicate / document; probably leave behavior.
- ☐ **S8 [lift] `process_float_cmp_op` derives width from SSA read, not varnode** — `crates/strider-lift/src/pcode_lift/value/float.rs:69-95`. Sound today (Sleigh emits at true width); asymmetric with binary-op path. Fix (optional): use `float_type_from_vn(input_vn)`.
- ☐ **S9 [orchestrator] `VnCache::scan_new` monotone-region invariant undocumented** — `crates/strider-analyze/src/orchestrator/mod.rs:300-312`. Sound only if regions never shrink across rebuilds. Fix: document, or fall back to full re-scan when region count drops.

## P2 — simplification / SSoT / optimization

- ☐ **S5dup/SSoT [opt] store-alias verdict duplicated** — `load_forward/mod.rs:669-744`, `call_stack_args/mod.rs:159-194` vs `sp_expr/walk.rs` `step_through_store`. Factor one shared helper (resolves S3).
- ☐ **C1 [pattern] `LoadPat`/`StorePat` ~95% duplicated scaffolding** — `pattern/pat/builders/memory.rs`. Extract shared `StackAccessSpec` (fields + stack post-match fragment).
- ☐ **C2 [lift] cmp-negate / float-eq-negate helpers duplicated** — `pcode_lift/value/integer.rs:169-238`, `float.rs:97-145`. Extract `lower_cmp_negate` / `build_float_eq_negated`.
- ☐ **C3 [orchestrator] `build_{,stable_,destructive_}optimizer_pipeline` SP-pass wiring copy-pasted** — `strider/pipeline.rs:217-282`. Extract `add_stable_sp_passes`/`add_destructive_sp_passes`.
- ☐ **C4 [reader/target] `Endianness` SSoT not threaded into reader** — `strider-target/src/arch.rs:11-24` (`read_u64` claims to consolidate) vs `strider-reader/src/elf/reader.rs:88-102` + `relocations.rs` (raw `bool`, hand-rolled branch). Fix: store `Endianness`, decode via `read_u64`; or delete the misleading doc.
- ☐ **C5 [py] migrate hand-written `.pyi` to `pyo3-stub-gen`** — root cause of B4. Migrate `PyFunction`/`PyMatch`/`PyCallingConvention`/pattern module to `#[gen_stub_pymethods]`.
- ☐ **C6 [py] `forall_preset!`/`forall_castmask!` re-enumerate presets** — `strider-py/src/{cc.rs,arch.rs}`. 3 edits to add a preset. Lower priority.
- ☐ **C7 [py] `pure_pass_class!`/`cc_aware_pass_class!` near-duplicate** — `strider-py/src/opt.rs`. Unify with optional strider param.
- ☐ **C8 [orchestrator] `dump_per_region`/`dump_neighborhood` shared tail** — `orchestrator/mod.rs:1148-1233`. Extract `render_filtered_html`.
- ☐ **C9 [generic] graphwalk: add `PostOrder::into_visited`; make `graph`/`visited` private; dot `dark_cfg` use `.expect`; `GraphDot::with_name`; test-utils `FxHashMap`** — small SSoT/ergonomic fixes.
- ☐ **O1 [orchestrator] wasted re-lift when in-place edits + Rebuild coincide** — `orchestrator/mod.rs:518-550`. Skip `apply_in_place_edits` when decision will be `Rebuild`.
- ☐ **O2 [reader] reloc dedup O(sites×regions)** — `relocations.rs:319-322`. Build a lookup table once, query per site.
- ☐ **O3 [generic] dot 4× full-string `replace`; `DenseEntitySet` len/iter O(N/64) doc notes** — `dot/src/lib.rs:396-400`, `entity-utils/src/set.rs`. Low urgency.

## P3 — docs (stale/wrong) + Python binding docstrings

- ☐ **D1 [ir] stale `Bool` doc references** — `output_kind.rs:56-58` (`as_integer_or_err` says errors on `Bool`; I1 is integer so it doesn't), `lib.rs:43` (lists `Bool`), `node_signature.rs:9-11` ("Bool remains distinct"). Reword to I1.
- ☐ **D2 [py] `ReadOnlyMemory.read` "little-endian-decoded" wrong** — `__init__.pyi:124`. Contract is target-endian (impl's responsibility).
- ☐ **D3 [generic] `EntityInterner::Index` `# Panics` + `#[track_caller]`; `DenseEntitySet` complexity notes** — `entity-utils/src/{interner.rs:99,set.rs}`.
- ☐ **D4 [py] add docstrings to ALL bindings** — extensive list from py review (Function/Match/CallingConvention methods, pattern module fns, opt pass ctors). Mostly resolved by C5 (gen_stub) + source docstrings.
- ☐ **D5 fix CLAUDE.md + READMEs** — reconcile with post-remediation code; iterate until every claim verified.
- ☐ **D6 [ir] `make_float_const` doesn't mask bits** — `ops/consts.rs:90-102`. Latent F32 dedup gap. Document zero-high-bits precondition or mask.

## Dead code
- ☐ Run `cargo +nightly udeps` is unavailable; use compiler `dead_code` + grep for unused pub items confirmed dead. Remove (no compat shims).

## Final
Full gate (cargo test --workspace + clippy + doc + maturin + pytest) green →
final code-review subagents → open PR rewrite/deep-audit → rewrite/strider via
GitHub compare URL (no gh CLI).
