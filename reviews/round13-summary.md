# Round 13 — Final summary

**Branch:** `review/ai7` (HEAD `fa4d671`, forked from `review/ai6` HEAD `54f00bb`).
**Date:** 2026-05-11.

## Verdict

Workspace is in **good shape**.  Round 12 W1-W5 landed substantial correctness + type-design + simplification work; round 13 finds the residue is small and largely cosmetic.  No HIGH-severity correctness regressions surfaced.  The biggest concrete bug-class is a narrow MED finding (`DeadBranchElimination` corrupts `StackStorePhi` in the SP-divergent-branches scenario — INV13-1) which the round-12 audit dismissed but round-13 re-derived as real under the specific precondition.

## Counts

| Severity | Count |
|----------|-------|
| HIGH | 1 |
| MED | 4 |
| LOW | ~30 |

| Axis | Findings (≥ MED) |
|------|------------------|
| Code-vs-code (self-vs-self) | 0 |
| IR-vs-pcode | 0 |
| IR-vs-assembly | 0 (1 LOW sound over-approximation; 1 documented `#[ignore]`) |

## HIGH-severity findings (1)

### H1 — `strider-py` `test_read_only_memory_kbd_interrupt.py` doesn't actually exercise the Rust adapter (F1 in 1F)
- **Where:** `crates/strider-py/tests/python/test_read_only_memory_kbd_interrupt.py:27-35`
- **What:** The test calls `rom.read(0x1000, 8)` directly on a Python `_KbdRom` subclass.  Python MRO dispatches to the subclass override; the Rust `PyReadOnlyMemoryAdapter::read` re-raise guard at `reader.rs:597-601` is never invoked.  The test asserts only that a Python method can raise `KeyboardInterrupt`, not that the guard is in place.  A regression deleting the guard would not be caught.
- **Fix:** Pass `_KbdRom()` as `rom=` to `strider.run` with a binary that contains a constant-address `Load` so `LoadReadOnly` actually invokes `PyReadOnlyMemoryAdapter::read`.  Contrast `test_mem_reader_kbd_interrupt.py` which correctly forces the adapter path.

## MED-severity findings (4)

### M1 — EC-1 release-mode behaviour and test gap on `set_function_boundary`
- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/options.rs:194-227`
- **What:** `set_function_max_size(0)` and `set_function_boundary(Bounded { max_size: 0 })` use `debug_assert!(false, ...)`.  In release, the assertion is compiled out but the corrective `self.options.fn_max_size = None` still runs (behaviour is correct).  Only `set_function_max_size` has a release-mode pin test (`#[cfg_attr(debug_assertions, ignore)]`); `set_function_boundary` does not.  Per the project's own `strider-public-api-encapsulation` skill, `debug_assert!` in error paths is the anti-pattern.
- **Fix:** Either (a) convert both setters to `Result<Self, BuilderError>`-returning; or (b) replace `debug_assert!(false, …)` with hard `panic!`; or (c) add a release-mode pin test for `set_function_boundary` and document the silent-fallback as the intentional contract.

### M2 — `DeadBranchElimination` can corrupt `StackStorePhi` arity when SP diverges across branches (INV13-1)
- **Severity:** MED (narrow trigger; Round 12 dismissed but round 13 re-derived as real)
- **Where:** `crates/opt/src/dead_branch/mod.rs:146-165`
- **What:** Round 12 W1 verification concluded "dead-ctrl from const-If never directly feeds the join CS where StackStorePhi lives".  That holds for typical ABI-compliant functions.  When a function has conditional `alloca` / inline-asm modifying SP, SP diverges across the branches → `VarPhi(sp)` at the join → `StackStorePhi` consumes the same `PhiToken`.  Then if a const-If above the join produces a dead branch, DBE's `phi_nodes` loop calls `remove_node_input(phi_node, dead_idx+1)` on the StackStorePhi, removing input[1]=Memory or input[2]=Data and violating the fixed-arity-3 invariant.  Layer A catches via `SlotCountMismatch` at the next validate() call.
- **Fix:** Add a kind-guard in the phi-node loop:
  ```rust
  if matches!(*ctx.node_kind(phi_node), NodeKind::StackStorePhi { .. }) {
      let mut offsets = ctx.stack_phi_offsets(phi_node).to_vec();
      if (dead_idx as usize) < offsets.len() {
          offsets.remove(dead_idx as usize);
          ctx.set_stack_phi_offsets(phi_node, offsets);
      }
      continue;
  }
  ```
- **Regression test:** Function with `if cond { sp -= 16 } else { sp -= 32 }; *sp = …; if (const true) {…} else {…}`.

### M3 — Stale `IndirectBranchResolve` claim in root README:221 (3A finding 1)
- **Severity:** MED (actively-misleading doc)
- **Where:** `README.md:221`
- **What:** Root README still says "`IndirectBranchResolve` is a producer-shape classifier … implements the `Optimizer` trait but is instantiated *directly* by the strider orchestrator".  The struct was deleted in Round 11 W9 S1.1.  CLAUDE.md was fixed in Round 12 W2 (E4); the root README was not.
- **Fix:** Replace with: "`opt::indirect_branch_resolve` is a module of free-function classifiers (link-register-return, tail call, jump table, stack-array dispatch) and in-place IR editors.  There is no `Optimizer`-implementing struct.  The strider orchestrator calls them directly, outside any pipeline."

### M4 — Stale `Builder::with_endianness` references in strider source comments (3B)
- **Severity:** MED (multiple sites referring to a deleted symbol)
- **Where:** `crates/strider/src/orchestrator.rs:264` ("consumed by `Builder::with_endianness`"), `orchestrator.rs:954`, `benches/scaling.rs:90`, `tests/common/mod.rs:216`; `strider-py/src/cfg.rs:50`; `strider-py/src/sleigh.rs:25`; `opt/src/indirect_branch_resolve/stack_array.rs:125-126,149` (citing a `classify.rs:233-269` mirror that no longer exists); `reader/tests/elf_smoke.rs:15` (cites `strider::analyze_binary` which doesn't exist; entry is `strider::run`).
- **Fix:** Strip / replace each reference.  Mechanical sweep.

## LOW-severity (~30 across all reports)

Listed in their respective per-area reports.  Highlights:

- **B-1/B-2/B-3 borrowing** (carry-forward unchanged) — SAFETY comment naming wrong type; Mutex-held-across-closure; `env::set_var` data-race acknowledgement.
- **R13-T-1 to R13-T-18 type-design** — 18 items (3 MED carry-forwards: `ResolvedTargets::Multiple` tuple-public, `cfg::Region` pub fields, `AnchorCallingContext` push-fill partial-state).
- **TR13-1 / TR13-2 strider+target+reader** — `arch_independent_call_entries_have_empty_register_channels` test stale; `locate_spliced_call` multi-CS-pred ambiguity.
- **IRA13-1 / IRA13-2 ir-vs-assembly** — `mwait` `EAX` over-approximates to RAX via aliasing; aarch64-be `Or(SP,K)` `#[ignore]` is real.
- **TY13-1 / TY13-2 types** — spurious `Result<_>` in `Graph::make_value_node` + `make_bool_const`.
- **OPT-FCC-1** — production `expect()` in `FlagCmpCanonicalize::try_apply_rule`.
- **3A doc verify** — 2 new doc drifts (M3 above + `strider/README.md:14` `start_addr: u64` should be `MachineInsnAddr`).
- **3B comments** — 26 findings + 14 terminology touch-ups (round-12 breadcrumb accumulation, "analyzer" terminology, stale paths).
- **2B naming** — 22 round-12-style breadcrumb sites in `.rs` source (mechanical strip).
- **5 simplifications** — 12 items, ~−85 LOC: dead `pattern::Capture::from_id` + 4 `BuiltFunctionGraph` test setters; `RewriteCtxView::new` ctor; "Graph lock poisoned" 14× repeat in `strider-py/src/graph.rs` (route through `read_inner()` helper, saves ~30 LOC); 4 visibility tightenings (`ir::walk` helpers, `opt::sp_expr` module, `opt::stack_load_forward` module, `opt::indirect_branch_resolve` module — all zero external callers).

## Categories verified consistent

✓ **2A panics** — 0 unjustified.  12 sites all annotated.  Δ vs R12: −2 (EC-3 hardened to runtime `Err`).
✓ **2C silent failures** — 0 SWALLOWED-BUG.  All intentional fallbacks documented.
✓ **Self-vs-self correctness** — 0 findings across 9 categories.
✓ **IR-vs-pcode** — 0 findings.  All opcode families verified consistent with rsleigh.
✓ **3 categories of borrowing/aliasing/concurrency** — only the 3 R12 LOW notes carry forward; no new issues.

## Cross-finding signals

- 1B and 2D both flagged `R13-T-1` (`ResolvedTargets::Multiple` non-empty invariant via tuple-construct still allowed).
- 1F and 2D both flagged the `MemReadError` direct-tuple-construction in `reader/src/elf.rs:312` (R12-T-P followup).
- 3A and 3B both surfaced `Builder::with_endianness` / `Builder::new` stale references — consolidated as M4.

## Round-12 carry-forward status

| R12 item | Status |
|---|---|
| R12-T-A (RewriteCtx{,View} fields) | LANDED W5a |
| R12-T-Q (Cfg accessors) | LANDED W5b (accessors added; fields kept pub due to partial-move pattern) |
| R12-T-C (SleighArch fields) | LANDED W2-W3 |
| R12-T-N (Binding fields) | LANDED W2-W3 |
| R12-T-P (MemReadError inner) | LANDED W2-W3 |
| R12-T-G (set_function_boundary) | LANDED W5e |
| R12-T-H (RunConfig::start_addr → MachineInsnAddr) | LANDED W5f |
| R12-T-B (Region pub fields) | DEFERRED (still applicable as R13-T-3) |
| R12-T-D (FunctionGraph partial-state) | DEFERRED (still applicable as R13-T-4) |
| R12-T-E (BuiltCallingConventionParts non_exhaustive) | DEFERRED (R13-T-2) |
| R12-T-F (ResolvedTargets::Multiple non-empty) | DEFERRED (R13-T-1) |
| R12-T-I (AnalyzeOptions enum) | DEFERRED (R13-T-5) |
| R12-T-J/K (RegionLiftHandles / AnalyzeOutcome) | DEFERRED (R13-T-6) |
| R12-T-L (type_name capture) | DEFERRED (R13-T-7) |
| R12-T-M (ProcessInsnRes) | REFUTED (R12 W4) — used externally via cfg::test_api |
| R12-T-O (is_little_endian: bool) | DEFERRED (R13-T-8) |

## Recommended next-round implementation order

**Quick wins (correctness-critical, < 1 hour each):**
1. **M3** — Update root README:221 `IndirectBranchResolve` claim.
2. **M4** — Strip `Builder::with_endianness` / deleted-API references (mechanical sweep, ~9 sites).
3. **2B breadcrumb strip** — 22 round-12-style mentions in `.rs` source (sed-style).

**Medium effort (1-3 hours):**
4. **H1** — Fix `test_read_only_memory_kbd_interrupt.py` to actually exercise the Rust adapter.
5. **M2** — Add `StackStorePhi` skip-guard in DBE (INV13-1); regression test for SP-divergent branches.
6. **M1** — Convert `OptionsBuilder::set_function_max_size` + `set_function_boundary` to either `Result`-returning OR hard `panic!`, with release-mode pin test for the latter.
7. **3A finding 2** — Update `strider/README.md:14` `start_addr: u64` → `MachineInsnAddr`.

**Simplifications batch (mechanical):**
8. Delete unused `pattern::Capture::from_id`, `BuiltFunctionGraph::{set_call_clobbered_for_test, set_ret_val_regs_for_test, ret_val_regs_as_slice, no_memory_clobber}`, `RewriteCtxView::new` (5 sites).
9. Route 14 "Graph lock poisoned" guards in `strider-py/src/graph.rs` through `read_inner()`.
10. Tighten `ir::walk::{cfg_outputs, cfg_succs, ...}`, `opt::sp_expr`, `opt::stack_load_forward`, `opt::indirect_branch_resolve` modules to `pub(crate)` where zero external callers.

**Test scaffolding (T-1..T-7):**
11. 7 new tests from `round13-test-plan.md`.

## Files produced (23)

| File | Status |
|------|--------|
| reviews/round13-coverage-manifest.md | Round 0 |
| reviews/round13-1A-ir.md | 1 LOW finding (Layer B doc drift) |
| reviews/round13-1B-pcode-lift-cfg.md | 1 MED + 2 LOW (EC-1 followup + 2 cosmetic) |
| reviews/round13-1C-opt.md | 1 MED (FCC expect) |
| reviews/round13-1D-pattern.md | 0 findings |
| reviews/round13-1E-strider-target-reader.md | 2 LOW |
| reviews/round13-1F-strider-py-aux.md | 1 HIGH + 1 MED (test gap + tuple-struct) |
| reviews/round13-2A-panics.md | 0 unjustified |
| reviews/round13-2B-naming.md | 22 breadcrumbs + 2 minor naming |
| reviews/round13-2C-silent-failures.md | 0 |
| reviews/round13-2D-types.md | 18 (3 MED carry-forwards + 15 LOW) |
| reviews/round13-3A-doc-verify.md | 25 confirmed + 2 refuted |
| reviews/round13-3B-comments.md | 26 findings + 14 terminology |
| reviews/round13-correctness-self-vs-self.md | 0 findings |
| reviews/round13-correctness-ir-vs-pcode.md | 0 findings |
| reviews/round13-correctness-ir-vs-assembly.md | 2 LOW (mwait alias, aarch64-be ignore) |
| reviews/round13-correctness-types.md | 2 LOW (spurious Results) |
| reviews/round13-correctness-invariants.md | 2 MED (INV13-1 + INV13-2) |
| reviews/round13-correctness-borrowing.md | 0 new (3 R12 LOW carry-forward) |
| reviews/round13-correctness-edge-cases.md | 1 LOW (set_function_boundary test gap) |
| reviews/round13-correctness-cross-arch.md | 0 findings |
| reviews/round13-test-plan.md | 7 new tests proposed |
| reviews/round13-simplifications.md | 12 items, ~−85 LOC |
| reviews/round13-summary.md | (this file) |
| reviews/round13-claudemd-diff.md | (this round) |
| reviews/round13-readme-diffs.md | (this round) |
