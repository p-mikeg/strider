# Round 13 — 3A: doc verification (trust-only-the-code)

Branch: `review/ai7` · Scope: CLAUDE.md, root `README.md`, every `crates/*/README.md`.  Trust model: code is the oracle.

## Verdict

27 claims sampled.  **25 confirmed, 2 new doc drifts found.**

## Round-12 fix verification

All four R12 doc fixes still hold:

| Fix | Location | Status |
|-----|----------|--------|
| E1 — `Optimizer`/`OptimizerRaw` direction | `CLAUDE.md:89` | Still fixed |
| E4 — `IndirectBranchResolve` as free functions | `CLAUDE.md:99` | Still fixed |
| R-1 — root README `float_is_nan` alias list | `README.md:231` | Still fixed |
| R-6/R-7 — strider-py README `float_is_nan` | `strider-py/README.md:203-206` | Still fixed |

## New findings — DOC DRIFT

### Finding 1 — Root `README.md:221`: `IndirectBranchResolve` struct claim is stale
- **Severity:** MED (confidence 100)
- **Doc text:** "`IndirectBranchResolve` is a producer-shape classifier … implements the `Optimizer` trait but is instantiated *directly* by the strider orchestrator …"
- **Code reality:** `grep -rn "struct IndirectBranchResolve" crates/opt/src/` → zero hits.  Struct deleted in Round 11 W9 S1.1.  Module `opt::indirect_branch_resolve` now exposes only free functions (`classify_anchor_with_rom_and_sp`, `apply_link_register`, `apply_tail_call`, `classify_jump_table`, `classify_stack_array`).
- **Fix:** Replace the line with: *"`opt::indirect_branch_resolve` is a module of free-function classifiers (link-register, tail call, jump table, stack-array dispatch) and in-place IR editors — there is no `Optimizer`-implementing struct.  The strider orchestrator calls them directly, outside any pipeline."*

### Finding 2 — `crates/strider/README.md:14`: `start_addr: u64` type is stale
- **Severity:** LOW (confidence 100)
- **Doc text:** "`RunConfig<'a, R>` — input bundle: `strider: &Strider`, `start_addr: u64`, …"
- **Code reality:** `crates/strider/src/orchestrator.rs:70` reads `pub start_addr: cfg::MachineInsnAddr`.  Newtype added in Round 12 R12-T-H to prevent accidental `u64` swap with `fn_max_size`.
- **Fix:** Change `start_addr: u64` → `start_addr: cfg::MachineInsnAddr` (construct via `cfg::MachineInsnAddr::new(addr)` or `addr.into()`).

## Sampled claims (all 27, verdict only — full evidence in agent transcript)

| # | Claim | Source | Verdict |
|---|---|---|---|
| 1 | `Optimizer` trait signature | CLAUDE.md:89, opt/README:9-13 | Confirmed |
| 2 | `IndirectBranchResolve` implements `Optimizer` | root README:221 | **Refuted (Finding 1)** |
| 3 | `RunConfig.start_addr` is `u64` | strider/README:14 | **Refuted (Finding 2)** |
| 4 | `default_pipeline()` has 6 passes | CLAUDE.md, opt/lib.rs:194 | Confirmed |
| 5 | `stable_default_pipeline()` has 4 passes | CLAUDE.md, opt/lib.rs:123 | Confirmed |
| 6 | `destructive_default_pipeline()` has 2 passes | CLAUDE.md, opt/lib.rs:165 | Confirmed |
| 7 | `Strider::build_stable_optimizer_pipeline()` adds SSD+SLF+FAD post | CLAUDE.md, pipeline.rs:216 | Confirmed |
| 8 | `Strider::build_destructive_optimizer_pipeline()` adds CSAC post | CLAUDE.md, pipeline.rs:240 | Confirmed |
| 9 | `Decision { FixedPoint, StableOnly, Rebuild }` | CLAUDE.md, orchestrator.rs:190 | Confirmed |
| 10 | `opt::indirect_branch_resolve` public surface | CLAUDE.md, mod.rs:49 | Confirmed |
| 11 | `float_is_nan` Python-only | README:231, strider-py/pattern.rs:1060 | Confirmed |
| 12 | `IntSub`/`FloatSub` lowering to `Add(_, Neg(_))` | CLAUDE.md, arithmetic.rs | Confirmed |
| 13 | `FunctionArgDetect` is post-pass | opt/README, pipeline.rs:186 | Confirmed |
| 14 | `GraphRewriter` in `crates/strider/src/rewrite.rs` | CLAUDE.md, rewrite.rs:1 | Confirmed |
| 15 | `vn_sort_key` exposed from pcode-lift | pcode-lift/README, lib.rs:106 | Confirmed |
| 16 | `ValueLifter::lift` returns `Result<bool>` | pcode-lift/README, lib.rs:86 | Confirmed |
| 17 | `apply_elf_relocations_autoload` exists | reader/README, elf.rs:768 | Confirmed |
| 18 | `phi()` matches VarPhi only; mem_phi/value_phi separate | README:235, control.rs:51 | Confirmed |
| 19 | `StackStorePhi` arity-3 exempt from phi arity check | CLAUDE.md, layer_c.rs | Confirmed |
| 20 | No `Opaque` variant in `CallOtherClass` | target/README:88, call_other_abi.rs:44 | Confirmed |
| 21 | `Builder::for_arch` preferred over deprecated ctors | cfg/README:18 | Confirmed |
| 22 | `find_all_requirements` shared-capture-agreement | CLAUDE.md, pattern matcher | Confirmed |
| 23 | 15 `SleighArch` presets | target/arch.rs:159-345 | Confirmed |
| 24 | `strider::indirect_resolve` re-exports from opt | strider/indirect_resolve/mod.rs:17 | Confirmed |
| 25 | `AnchorCallingContext` shape | opt/README, indirect_branch_resolve/mod.rs:119 | Confirmed |
| 26 | `Capture` atomic-counter uniqueness | pattern/README, capture.rs | Confirmed |
| 27 | `FunctionArgDetect` + `CallStackArgCollect` are post-passes | opt/README:38-41 | Confirmed |

## Summary

| Verdict | Count |
|---------|-------|
| Confirmed | 25 |
| Partially confirmed | 0 |
| Refuted / new drift | 2 |

All core architectural claims hold.  Two stale doc references (root README's deleted-struct mention; strider-README's stale `start_addr` type).  Both are documentation-only fixes.
