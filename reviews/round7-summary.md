# Round 7 — Final Consolidated Review Summary

This is the executive summary covering all 11 user asks for the strider workspace pre-deployment review. Each finding is independent (no prior reviews trusted) and verified against the source — and where the topic is correctness-critical, against the rsleigh dependency at `../rsleigh` and the published ABI / Sleigh pcode specs.

The full per-area reports live alongside this file:

| File | Topic |
|------|-------|
| `round7-ir.md` | ir crate audit |
| `round7-pcode-lift-cfg.md` | pcode-lift + cfg audit |
| `round7-opt.md` | opt crate audit |
| `round7-pattern.md` | pattern crate audit |
| `round7-strider-target-reader.md` | strider + target + reader audit |
| `round7-py-support.md` | strider-py + dot/graphwalk/entity-utils/graphmock |
| `round7-claudemd-verify.md` | CLAUDE.md verification (22 claims, 6 false) |
| `round7-comments.md` | Stale-comment / TODO sweep |
| `round7-naming.md` | "tier 1/2" + name cleanup |
| `round7-types.md` | Type-design audit |
| `round7-panics.md` | Production-panic hunt |
| `round7-silent-failures.md` | Silent-failure / fallback audit |
| `round7-scale.md` | Scalability (1000s of nodes) |
| `round7-correctness.md` | Deep correctness vs ABI / Sleigh / rsleigh |
| `round7-simplifications.md` | Simplification roll-up |
| `round7-flows.md` | Logic-flow consolidation |
| `round7-generalize.md` | Generalization opportunities |
| `round7-test-plan.md` | Concrete missing tests |
| `round7-skills.md` | 8-skill bundle design |

---

## CRITICAL — fix before deploy (silent data-flow corruption)

These are silent-correctness bugs that produce wrong analysis on real binaries with no error indication. Each has a corresponding regression test in `round7-test-plan.md` Priority 1.

### 1. `IfCondInversion` corrupts VarPhi values at merge points (NEW)
- **Where:** `crates/opt/src/if_cond_inversion/mod.rs:94-130`
- The pass swaps `If`'s control-output consumers via `update_input`, but does NOT swap the corresponding `VarPhi`/`MemPhi` value inputs. After inversion, a phi at the merge slot `j+1` still holds the *true-branch* value while ControlState slot `j` now carries the *false-branch* control. **Every function with phi nodes downstream of an inverted conditional has wrong phi values.** This was missed by 6 prior crate audits because none read the rewrite logic alongside the phi-arity invariant.
- **Fix:** After swapping If's control outputs, walk every consumer ControlState's phi consumers and swap their value inputs at the same slot indices.

### 2. CFG silently drops a CondBranch's in-range edge
- **Where:** `crates/cfg/src/cfg/builder/region_builder.rs:374-385`
- When `self.insns.len() == 1` (the conditional is the sole instruction in the region) and exactly one successor is OOB, the code emits `RegionTerminator::TailCall { target: in_range_addr }` and **never enqueues the in-range successor in `work_queue`**. The in-range edge is permanently lost. Comment says "essentially unobserved in real binaries" — but trampolines, hot-patching thunks, and tail-call stubs hit this exactly.
- **Fix:** When `insns.len() == 1`, emit a one-instruction region terminating with `Branch` and push `in_range` to `work_queue`.

### 3. `x86_64_all_preserving` cannot express memory preservation
- **Where:** `crates/target/src/calling_convention/mod.rs:185-203` + `crates/ir/src/builder/call.rs:124-127`
- The "all preserving" CC name promises zero observable side effects (e.g. `__fentry__` hooks). But `build_call_with_cc` always emits a `Memory` output on every Call regardless of CC. So `LoadReadOnly` and `StackLoadForward` cannot forward across these "preserving" calls. The user's deeper point: a "preserve all" CC should mean **every Vn — registers, memory, flags — is preserved**, not just registers.
- **Fix (incremental):** Add `no_memory_clobber: bool` to `BuiltCallingConvention` and conditionally skip emitting the Memory output. Long-term: lift the abstraction from "callee-saved registers" to "preserved Vn set" so memory and flags can be expressed.

### 4. `sysret` classified as `NoReturn`
- **Where:** `crates/target/src/call_other_abi.rs:248`
- Intel SDM Vol.2B: `SYSRET` is the architectural return path of a syscall handler. Marking it `NoReturn` corrupts the CFG of every Linux kernel function whose fast path ends in `sysret` (e.g. `do_syscall_64`).
- **Fix:** Reclassify as `Call(CallOtherAbi { implicit_reads: &["RCX","R11"], implicit_writes: &[], memory_edge: true })`.

### 5. `swapgs` classified `PURE` (no memory edge)
- **Where:** `crates/target/src/call_other_abi.rs:299`
- Intel SDM: SWAPGS exchanges `IA32_GS_BASE` ↔ `IA32_KERNEL_GS_BASE`. Subsequent `%gs:`-relative loads/stores depend on the new base. Without `memory_edge: true`, forwarding passes incorrectly bypass `swapgs` in kernel entry/exit code.
- **Fix:** Reclassify as `PURE_WITH_MEM_EDGE` (matching `wrgsbase`/`wrfsbase`).

### 6. `PyMemoryMap::ReadOnlyMemory::read` always little-endian
- **Where:** `crates/strider-py/src/reader.rs:567-579`
- Hardcoded `u64::from_le_bytes(buf)` regardless of arch endianness. `ElfFileMemReader` correctly switches via `is_little_endian` (`reader/src/elf.rs:328-342`). Big-endian Python pipelines (MIPS BE, AArch64 BE, ARM BE, PowerPC BE) silently byte-swap `LoadReadOnly` constants.
- **Fix:** Carry endianness on `PyMemoryMap` and switch byte-order parsing accordingly. See `round7-generalize.md` Category 10 for a unified `target::Endianness::read_u64` helper that fixes both `reader/src/elf.rs` and the Python wrapper at the same time.

### 7. `PyCapture::__hash__` catastrophically collides
- **Where:** `crates/strider-py/src/pattern.rs:52-55`
- Hash is `format!("{:?}", self.inner).len() as isize`. Captures debug-format as `Capture(N)`; the hash is the **string length of the decimal id**. Ids 10–99 all hash to 11. Python `dict`/`set` keyed on `Capture` degrade to O(n) and produce false equality buckets.
- **Fix:** Hash the underlying `u32` id directly.

### 8. Unbounded recursion in `find_stack_stored_value_at_offset` + `mem_chain_is_dirty`
- **Where:** `crates/opt/src/stack_load_forward/mod.rs:489-549`; `crates/opt/src/function_args/mod.rs:393`
- Both recurse through memory chains without depth bound or `visited` set. Real binaries with deep stack-store prologues (~10k stores) overflow the 8 MB Rust stack.
- **Fix:** Convert to iterative `Vec`-worklist form mirroring the already-iterative `probe` (`stack_load_forward/mod.rs:205`) and `walk_control_for_if_bound_iter`.

### 9. `PyReadOnlyMemoryAdapter::read` swallows Python exceptions
- **Where:** `crates/strider-py/src/reader.rs:499-518`
- Two `.ok()?` calls + a silent `extract::<u64>().ok()`. Compare the symmetric `PyMemReaderAdapter::read` at `:436-458` which surfaces errors properly. User-side `ReadOnlyMemory.read` exceptions are converted to "no fold" silently.
- **Fix:** Mirror the `PyMemReaderAdapter::read` error-wrapping path.

### 10. `run.rs:197 let _ = rom;` discards user ROM in custom-pipeline path
- **Where:** `crates/strider-py/src/run.rs:197`
- Custom-pipeline runs throw away the user-supplied ROM. `LoadReadOnly` in the custom pipeline never sees it; loads that should fold to constants don't.
- **Fix:** Thread `rom` into the cfg builder (which needs it for tier-1 jump-table resolution) and into the custom pipeline's `LoadReadOnly`.

### 11. `analyze_known_bits.ok()?` swallows Kb::merge contradictions
- **Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:54,78`; `strider/src/indirect_resolve/classify.rs:49`
- `analyze_known_bits` returns a typed contradiction error (per `known_bits/mod.rs:50-62`) — a real bug indicator. The call-sites `.ok()?` discard it, silently returning `None` (no classification). The parallel call at `mod.rs:261` correctly uses `?` — inconsistent policy.
- **Fix:** Propagate the error: `analyze_known_bits(fg)?`.

### 12. `build_call_other_terminal` doesn't close the region — MED
- **Where:** `crates/ir/src/builder/call.rs:185-207`
- Subsequent `build_*` calls into the same region after the NoReturn terminal silently succeed instead of failing with `NoCurrentRegion`. Producing IR inconsistency.
- **Fix:** Call `terminate_cur_region()` inside `build_call_other_terminal`.

---

## Retraction (corrected after re-checking the Sleigh spec)

**AArch64 `x30` and ARM `lr` in `callee_saved_regs` is CORRECT** — this was originally flagged as HIGH but is actually right:

- Sleigh's AArch64 `bl` semantic at `../rsleigh/sleigh/processors/AARCH64/data/languages/AARCH64base.sinc:876-880` is `x30 = inst_start + 4; call Addr26;` — the LR assignment is an explicit pcode COPY emitted *before* the `call`. Sleigh's ARM `bl` at `ARMinstructions.sinc:2291` is similarly `lr = inst_next; call Addr24;`.
- The strider lifter therefore produces `LR := IntConst(inst_next); Call(target)`. With LR in `callee_saved_regs` (excluded from Call's clobber set), post-call LR holds the IntConst — **exactly the return-address value the AAPCS callee preserves**.
- If LR were clobbered by Call, the IR would lose the "LR equals next-PC" information and link-register-return classification would break.
- "Callee preserves LR" means the callee preserves the post-`bl` value (the return address `bl` itself wrote), not the pre-`bl` value. The strider model captures this correctly.
- Non-AAPCS callees (rare; e.g. exception-handling thunks) are out of scope for the AAPCS preset — they'd be modelled by a different `CallingConvention` preset or a `per_address_ccs` override.

The previous HIGH finding A2/A3 was based on a wrong mental model of "callee-saved" — retracted. The ABI specs (AAPCS64 §6.1.1, AAPCS §5.1.1) and the strider model agree under the correct interpretation.

---

## (1) Correctness — graph faithfulness

Beyond the 12 critical issues above, the deep-correctness audit (`round7-correctness.md`) confirmed:

- **Lift-time canonicalization** of `IntSub`, `IntLessEqual`, `IntSlessEqual`, `IntNotEqual`, `FloatSub`, `FloatNotEqual`, `FloatLessEqual`, `FloatNan` is **all correct** (verified at `pcode-lift/src/value/{arithmetic,float}.rs` against pattern aliases at `pattern/src/pat/ctor/{int,float}.rs`).
- **Pcode opcode dispatch** is exhaustive; `Indirect` and `MultiEqual` correctly bail (rsleigh's `lift_one` doesn't emit them).
- **`is_addr_tail_call`** bounded vs unbounded logic is correct (off-by-one verified).
- **Asm-fingerprint funnel** via `set_lift_addr` wrapper at `strider/src/strider/insn/mod.rs:35-39` correctly attributes every node-creation in the lifter.
- **Asm-fingerprint dedup-cache hit** correctly unions contributors (`builder/mod.rs:401-403`).
- **CallOther classify** sample-correct for `cpuid`, `rdtsc`, `rdmsr`/`wrmsr`, `wrfsbase`/`wrgsbase`, `mfence`/`sfence`/`lfence`.

Additional medium-severity correctness items:
- `FlagCmpCanonicalize` Rule 2 (HI) shared-capture brittleness: may silently miss matches if ConstantFold collapsed shared sub-expressions before the rule runs.
- `FlagCmpCanonicalize::rhs_thumb_b` returns the captured node directly without `extend_asm_fingerprint_from(a, root)` — proof-of-correctness gap (root's contributing addr is lost).
- `KnownBits` does not propagate through `SignExtend` (sound but loses precision; affects MIPS / ARM Thumb jump-table classifier).
- `rdtscp` may not be distinguished from `rdtsc` (missing `ECX = TSC_AUX` clobber if Sleigh reuses the name).
- MIPS `n64` `arg_passing_regs` lists `t0..t3` instead of `a4..a7` — confusing if Sleigh's mips64 spec uses different physical regs (verify with the `#[ignore]`-d diagnostic test).
- `Region.variables: SecondaryMap<VarId, NodeOutputId>` returns `NodeOutputId(0)` (Entry control output) for unset entries — silent wrong-edge risk if regions are constructed sparsely.

## (2) Simplicity — what to delete or merge

Total deletable LOC: **~195** (from `round7-simplifications.md` + `round7-flows.md` + `round7-generalize.md`).

### Direct deletions
- **`graphmock` standalone crate** (~283 LOC) — only consumed by `graphwalk` dev-tests. Move into `graphwalk/tests/common/graph.rs` and remove the crate.
- **`_unused_marker` + unused `Mutex` import** in `strider-py/src/reader.rs:686-689`.
- **`PyCfg::inner()`** dead code at `strider-py/src/cfg.rs:23-30`.
- **`ValidationError::InputPointsToMissingOutput`** declared but never emitted (`ir/src/validate/mod.rs:177-182`).
- **Stale TODO** at `strider-py/src/pattern.rs:20-21` (op-variant accessors are implemented).
- **`PyPat::ordered()`** silent no-op (`strider-py/src/pattern.rs:432-438`) — remove or convert to `PatternError`.

### Logic-flow folds (top 5 from `round7-flows.md`)
1. **Extract `first_output_matching` helper** — `crates/ir/src/region.rs:294-351` (`region_entry_control` and `region_entry_memory` differ only by predicate). −30 LOC, LOW risk.
2. **Extract `locate_and_write` in `apply_elf_relocations`** — `crates/reader/src/elf.rs:485-546,622-636`. −15 LOC, LOW risk.
3. **Unify `RedundantPhis` VarPhi/MemPhi vs ControlState single-pred collapse** — `crates/opt/src/redundant_phis/mod.rs:32-162`. −15-20 LOC, MED risk.
4. **`apply_elf_relocations_with_extender(..., F)`** — collapse autoload + non-autoload variants behind a closure. −30-40 LOC, MED risk.
5. **`is_addr_tail_call` half-open form** — `crates/cfg/src/cfg/query.rs:25-45`: `(target < lower) || (target >= upper)`. −5 LOC, LOW risk.

### Generalization opportunities (top 3 from `round7-generalize.md`)
1. **`Graph::create_node_attributed(kind, inputs, contributors: &[NodeId])`** helper — replaces ~15 manual `extend_asm_fingerprint_from` sites in opt passes. ~25 LOC saved + compile-time correctness for the superset contract.
2. **`target::Endianness::read_u64` / `read_u32` helpers** — fixes the strider-py always-LE bug at the same time as removing duplication. ~20 LOC + a real bug fix.
3. **`WorkSet::seeded_kind` + `collect_kind`** — directly responds to the user's "lots of places walk the graph" example. ~50 LOC across 12+ opt-pass sites.

## (3) Renaming — "tier 1 / tier 2" cleanup

`round7-naming.md` enumerates 121 occurrences across 37 files. Verified meanings:
- "tier 1" = **cfg-time mini-graph resolver** (`crates/cfg/src/cfg/builder/indirect_resolve.rs`).
- "tier 2" = **IR-level indirect-branch resolver** (`crates/strider/src/indirect_resolve/` + `crates/opt/src/indirect_branch_resolve/`).

Both run optimization passes; the split is **scope** (single-region vs whole-function) and **timing** (cfg-build vs post-lift), not a real "tier".

### Concrete rename mapping
- File renames: `tier2_orchestrator.rs` → `orchestrator.rs`, `tier2_optimizer_tiers.rs` → `optimizer_pipeline_subsets.rs`.
- `SpecialTerm::Unresolved(Vn, addr)` → `SpecialTerm::PendingIndirect { target_vn, addr }`.
- 5 `tier_1_*` / `tier_2_*` test-fn names → cfg-time / ir-level prefixes.
- ~80 comment-only rewrites of "tier 1" / "tier 2" terminology.

### Other unclear names
- `Rule.cap_a` / `cap_b` → `lhs_capture` / `rhs_capture` in `crates/opt/src/flag_cmp_canonicalize/mod.rs`.
- Half-rename leftovers mentioning `Var`/`NodeVar` (deleted) at `pattern/src/matcher/bindings.rs:136`, `pattern/src/pat/mod.rs:43-44`.

### Recommended order (CI-green between commits)
1. Comment-only rewrites (~80 lines, no consumer impact).
2. `Rule.cap_a/cap_b` rename (single private file).
3. Test-fn renames (Cargo auto-discovers).
4. `SpecialTerm::Unresolved` → `PendingIndirect` (private enum, 4 sites).
5. File renames via `git mv`.

## (4) Unused features

### Confirmed unused (delete candidates)
- `graphmock` standalone crate (only graphwalk dev-tests consume it).
- `ValidationError::InputPointsToMissingOutput` (declared, never emitted).
- `PyCfg::inner()`, `_unused_marker` in strider-py.
- `PyPat::ordered()` (silent no-op trap).
- `pattern::float_is_nan` Python wrapper (registered but raises `PatternError` unconditionally — implement as `bool_neg(float_eq(x, x))` per pcode-lift's lowering, or remove).

### Confirmed in-use (keep)
- All 8 registered opt passes are wired and tested.
- All `SleighArch` presets (15 — 11 enumerated in CLAUDE.md + 4 PowerPC) are exposed.
- All `CallingConvention` presets (~22 — 6 enumerated in CLAUDE.md + 16 PowerPC/Linux kernel/Linux syscall variants) are exposed.
- All pattern free constructors are exercised.
- All NodeKind variants are produced and consumed.

## (5) Missing Python bindings

| Missing | Where | Impact |
|---------|-------|--------|
| `Graph::node_kind(id)` / `node_outputs(id)` / `node_inputs(id)` | `strider-py/src/graph.rs` | Cannot inspect IR nodes by id; locked to pattern queries |
| `Graph::asm_fingerprint(node_id)` (per-node, not just per-Match) | same | Cannot read fingerprints from non-pattern code |
| `Graph::call_other_name(id)` | same | Cannot query CallOther op names |
| `validate_with_options { check_asm_fingerprints: bool }` | same | Cannot enable Layer-C check from Python |
| `Graph::compact()` / `BuiltFunctionGraph::compact()` | same | Zombies accumulate in long-running scripts |
| `GraphRewriter` as Python class | `strider-py/src/lib.rs` | Available indirectly via `Graph.rewrite{,_all}` only |
| `Cfg` region introspection | `strider-py/src/cfg.rs` | Only dot rendering exposed |
| `pattern::float_is_nan` (currently raises) | `strider-py/src/pattern.rs:881` | Snapshot test passes; runtime fails |
| Op-variant accessors on Match (the TODO at `pattern.rs:20-21`) | `strider-py/src/matcher.rs` | Partially landed; remove stale TODO |

GIL handling: `strider.run` holds the GIL for the entire analysis (`run.rs:57-177`). Wrap the pure-Rust `MemoryMap` fast path with `py.allow_threads`. Callback `MemReader` path must keep the GIL.

## (6) Multi-round audits — what each round caught (and the prior rounds missed)

| Round | Most impactful unique find |
|-------|---------------------------|
| 1A (ir) | `IfCase` ghost in fingerprint exemption doc |
| 1B (cfg + pcode-lift) | CondBranch sole-insn OOB silent edge loss; verified all 6 lift-time canonicalizations |
| 1C (opt) | `find_stack_stored_value_at_offset` unbounded recursion |
| 1D (pattern) | `phi()` only matches `VarPhi`; `if_node()` doc lies about symmetric matching |
| 1E (strider+target+reader) | `swapgs` PURE; `sysret` NoReturn; AArch64 `x30` callee-saved; `PyMemoryMap` always-LE |
| 1F (py + support) | **`PyCapture::__hash__` collides catastrophically**; `pattern.rs` op-variant TODO is stale |
| 2A (panics) | Verified the `type_info_table_matches_variants` test exists at `node/tests.rs:305` (correcting Round 1A) |
| 2B (naming) | 121 "tier 1/2" sites; rename plan |
| 2C (silent failures) | `analyze_known_bits.ok()?` swallows real errors; `let _ = rom;` discard; `PyReadOnlyMemoryAdapter` swallows Python exceptions; ELF malformed-index bucketed as weak-extern |
| 2D (types) | `BuiltFunctionGraph::from_graph_and_entry` empty-fields contract leak; phantom-typed `OptimizerPipeline<Stable|Destructive|Full>`; scope-guard for `set_lift_addr` |
| 2E (scale) | **`mem_chain_is_dirty` unbounded recursion** (NEW vs Round 1C); `build_anchor_calling_context` rebuilds HashMap per in-place edit |
| 3A (CLAUDE.md) | 5 ghost NodeKind variants; `IntConst(u64)` should be `u128`; missing PPC presets; missing `FlagCmpCanonicalize` in stable pipeline doc |
| 3B (comments) | 6 doc-string lies on `pub` items; 4 `TODO(Task17)` markers tracked; `PyPat::ordered()` doc honestly says "no-op" (the method itself is the trap, not the doc) |
| 5C (flows) | 8 real fold opportunities, 6 verified-needed-bifurcation |
| 5B (generalize) | `Graph::create_node_attributed`; `Endianness::read_u64`; `WorkSet::seeded_kind` |
| 6B (correctness) | **`IfCondInversion` corrupts VarPhi values** (NEW — silent data-flow corruption); `x86_64_all_preserving` cannot model memory preservation |

The multi-round approach proved its value: 6B caught the IfCondInversion bug after 1C, 1E, 2D had all read the if_cond_inversion code without spotting it. The Round 1A claim that `type_info_table_matches_variants` didn't exist was corrected by Round 2A. **Trust-only-code, multiple agents, no shared review state — all three together moved the needle.**

## (7) Test plan

See `round7-test-plan.md`. **28 new tests** across 4 priority tiers:
- **P1 (12 tests):** regression for each HIGH/CRITICAL bug above.
- **P2 (8 tests):** asm-fingerprint dedup-union, shrink-prevention; vn_io partial-write; AArch64 e2e lift; `phi()` matching MemPhi; typed-Python-error coverage; KnownBits SignExtend.
- **P3 (4 tests):** pattern-alias round-trip; stack-array indirect-branch shape; `StackLoadForward+StackStoreDetect` ≤ 2 iters; pipeline idempotency.
- **P4 (4 benchmarks):** chain-of-1000-stores; diamond CFG 1000 regions; wide jump-table 256 targets; `find_all_requirements` shared-capture join.

Effort: ~6 engineer-days end-to-end.

The plan also records 6 already-covered items (don't re-add): `IfCondInversion` unit tests, `find_all_requirements` disagreement filter (Python), `at_any([])` / `offset_any([])` vacuous failure, `validate_with_options` Layer-C, Python pipeline count sync, `DuplicateFunctionArg`.

## (8) Stale comments

Highest-impact docstring lies on `pub` items (per `round7-comments.md`):
1. `pattern::if_node()` doc claims symmetric matching — code is direct-only.
2. `pattern::phi()` doc claims "any phi" — code matches only `VarPhi`.
3. `pattern::float_cmp_any` mentions nonexistent `FloatCmpOp::NotEqual`.
4. `Graph::asm_fingerprints` doc lists ghost `IfCase` and omits actual exempt members.
5. `strider::pipeline::build_stable_optimizer_pipeline` doc lists 5 passes, code composes 7 (omits FlagCmpCanonicalize + IfCondInversion).
6. `PyPhiPat` doc at `strider-py/src/pattern.rs:550` makes false claims.

Plus the `cfg/types.rs:103-105` "legacy mapping retained until indirect-branch resolver lands" — resolver landed.

`TODO(Task17)` markers in `cfg/decode_cache.rs:35`, `strider/orchestrator.rs:251`, `strider/strider/pipeline.rs:43` are linked to the existing plan `docs/superpowers/plans/2026-05-01-incremental-indirect-resolve.md` — keep as-is.

## (9) No `panic` / `unwrap` / `expect` in production

`round7-panics.md`: **8 of 13 crates have zero production panics**. Four unjustified items, all in `ir`:
1. `ir/src/graph/compact.rs:127-129` `expect` on cross-reachability invariant — convert `retain_reachable` to `Result`.
2. `ir/src/node/output_type.rs:69` `&TYPE_INFO[self as usize]` — runtime test exists at `node/tests.rs:305` (so this is MED not HIGH); replace with explicit `match` for compile-time guarantee.
3. `ir/src/iterators.rs:37-40` `Index<usize> for Outputs` — by-design panicking footgun.
4. `ir/src/iterators.rs:91-97` `Index<usize> for Inputs` — same.

`#[allow(clippy::expect_used)]`-annotated by-construction invariants (justified): `compact.rs:118`, `function.rs:150`, `flag_cmp_canonicalize/mod.rs:128,161,175`.

## (10) CLAUDE.md / README / per-crate docs

### CLAUDE.md corrections (per `round7-claudemd-verify.md`)
1. **Remove ghost NodeKind variants:** `FloatIsNan`, `Piece`, `Extract { lsb, len }`, `Insert { lsb, len }`, `IfCase(bool)`. None exist in `crates/ir/src/node/kind.rs`.
2. **`IntConst(u64)` → `IntConst(u128)`** (`crates/ir/src/node/kind.rs:132`).
3. **Add 4 PowerPC SleighArch presets:** `ppc32be`, `ppc32le`, `ppc64be`, `ppc64le` (in `target/src/arch.rs`).
4. **Add 16 CallingConvention presets:** `x86_64_all_preserving` + 4 PowerPC + 6 Linux kernel + 6 Linux syscall variants.
5. **`stable_default_pipeline()` description:** add `FlagCmpCanonicalize`. Currently CLAUDE.md says `ConstantFold + KnownBits + IfCondInversion`; code adds `FlagCmpCanonicalize` between (`opt/src/lib.rs:106-126`).
6. **Clarify `IndirectBranchResolve`:** it IS an `Optimizer` impl but is NOT in any of the 3 named pipelines — it's instantiated directly by the strider orchestrator.

### README rewrite plan
Per the user request, the root README should focus on the Python API. Concrete plan:
- Trim Rust-API sections (lines ~193-416) and move them to crate-level docs.
- Add: bounded-lift semantics + `function_max_size`; asm-fingerprint quickstart with `Match.asm_fingerprint(c)` from Python; `find_all_requirements` examples beyond the single field-offset example; troubleshooting "why did my pattern not match?" referencing `IfCondInversion` canonical layout and lift-time canonicalisation aliases.
- Reconcile `LoadReadOnly` placement (CLAUDE.md says it's NOT in `default_pipeline()`; the README table currently presents it as a default pass).

### Per-crate READMEs (12 missing)
Every crate except `strider-py` lacks a README. Outline (per crate, ~100-200 LOC):
1. Purpose
2. Public surface (top-level types and functions)
3. Internal architecture (modules, key invariants)
4. Tests (where they live, how to run)
5. Gotchas / non-obvious behaviour

Crates needing one: `cfg`, `dot`, `entity-utils`, `graphmock` (or de-crated), `graphwalk`, `ir`, `opt`, `pattern`, `pcode-lift`, `reader`, `strider`, `target`.

## (11) Skill bundle design

Per `round7-skills.md`, **8 skills** designed:
1. **`strider-pattern-author`** — guides writing a `pattern::Pat` for a given asm shape (commutativity, lift-time aliases, IfCondInversion canonical layout).
2. **`strider-debug-pattern`** — diagnoses "why doesn't my pattern match?" (IR layer, fingerprints, canonicalisation).
3. **`strider-opt-pass-author`** — scaffolds new `opt` pass: pipeline registration, fingerprint-extension contract, idempotency, four-shape test pattern.
4. **`strider-fingerprint-audit`** — verifies asm-fingerprint propagation through new code.
5. **`strider-indirect-shape-author`** — adds new shape to indirect-branch resolver.
6. **`strider-callother-abi`** — extends `target::call_other_abi::classify` for unhandled user-ops.
7. **`strider-target-arch`** — adds new `SleighArch` + `CallingConvention` preset.
8. **`strider-py-binding`** — exposes Rust API to Python via PyO3 with typed exception mapping.

Each has 8 fields filled (name, triggers, when-not, inputs, procedure, verify command, exit criteria, pitfalls). Suggested install location: `crates/strider/.claude/skills/<name>/SKILL.md` (in-repo, project-scoped).

---

## Prioritized action backlog

Group 1 — critical correctness (ship before deploy):
1. `IfCondInversion` swap also swaps VarPhi value inputs.
2. CFG single-insn CondBranch OOB emits Branch + enqueues in-range.
3. `sysret` reclassified Call(abi); `swapgs` reclassified PURE_WITH_MEM_EDGE.
4. `x86_64_all_preserving` truly preserves memory (add `no_memory_clobber` field).
5. `PyCapture::__hash__` use raw u32 id.
6. `PyMemoryMap::ReadOnlyMemory::read` honors arch endianness.
7. `find_stack_stored_value_at_offset` + `mem_chain_is_dirty` iterative form.
8. `PyReadOnlyMemoryAdapter::read` propagates Python exceptions.
9. `analyze_known_bits` errors propagated with `?`.
10. `run.rs` custom-pipeline path threads `rom`.
11. `build_call_other_terminal` closes the region.

Group 2 — type-design + dead-code (low risk):
11. Delete `graphmock` standalone crate; move to `graphwalk/tests/`.
12. Delete `_unused_marker`, `PyCfg::inner()`, `ValidationError::InputPointsToMissingOutput`, stale TODO at `pattern.rs:20-21`.
13. Demote `BuiltFunctionGraph::from_graph_and_entry` to `pub(crate)`; introduce `RewriteCtx` newtype.
14. `BuiltCallingConvention` field privacy + add accessor methods.
15. Remove `PyPat::ordered()` (or convert to PatternError).
16. Convert `pattern::float_is_nan` to `bool_neg(float_eq(x, x))` (or remove from public surface).

Group 3 — generalization & flows (medium risk):
17. `Graph::create_node_attributed(kind, inputs, contributors)` helper.
18. `target::Endianness::read_u64` / `read_u32` helpers (also fixes #6 above structurally).
19. Logic-flow folds top 5 from `round7-flows.md`.
20. `WorkSet::seeded_kind` / `collect_kind` to consolidate opt-pass walks.

Group 4 — naming / docs / Python parity:
21. Tier 1/2 rename batch (4 commits, comment-only first).
22. CLAUDE.md correctness diff (6 corrections).
23. Per-crate READMEs (12).
24. Root README Python-focused rewrite.
25. Add Python bindings: `Graph.node_kind/_outputs/_inputs/_asm_fingerprint(id)`, `validate_with_options`, op-variant accessors completion.

Group 5 — tests & benchmarks (per `round7-test-plan.md`):
26. P1 (12 regression tests).
27. P2 (8 coverage tests).
28. P3 (4 property tests).
29. P4 (4 benchmarks).

Group 6 — skills:
30. Author the 8 skills designed in `round7-skills.md`.

Total estimated effort: ~3-4 engineer-weeks for Groups 1–3 (the correctness-critical and structural fixes), plus ~2 weeks for Groups 4–6 (docs, tests, skills).

---

## Counts

- Reports written this round: **19** (this file + 18 in-depth audits).
- HIGH-severity findings: **~25** across all reports.
- MED-severity findings: **~40**.
- LOW-severity / cosmetic: **~30**.
- Stale-comment / docstring-lie items: **~12**.
- Dead-code deletions ready to ship: ~195 LOC.
- Logic-flow consolidations ready to ship: ~80-100 LOC.
- Generalization opportunities: ~95 LOC.
- New tests planned: **28** across 4 priority tiers.
- Skills designed: **8**.
- CLAUDE.md corrections: **6** factual + ~16 missing presets to enumerate.
- Per-crate READMEs to author: **12**.
