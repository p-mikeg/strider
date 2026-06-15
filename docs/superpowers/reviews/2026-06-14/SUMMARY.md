# Deep review 2026-06-14 — consolidated findings

14 read-only audits (per-crate deep + 3 cross-cutting). Baseline: `cargo test
--workspace` green, compile clean. **0 confirmed live miscompiles.** Findings
below are verified against real code (comments/CLAUDE.md/memory NOT trusted —
several doc claims found stale, e.g. `match_at_any`, `Program`/`Analyzer`,
`validate(fn,entry)` all gone).

Per-crate reports: `strider-ir.md`, `strider-opt.md`, `strider-lift.md`,
`strider-cfg.md`, `strider-graph.md`, `strider-pattern.md`,
`strider-orchestrator.md`, `strider-reader.md`, `strider-target.md`,
`strider-py.md`, `generic-utils.md`, `dim1-dedup-generalize.md`,
`dim6-dead-code.md`, `dim7-ssot.md`.

---

## Group A — clear wins (low-risk, clearly correct, high value)

| ID | Dim | What | Fix |
|----|-----|------|-----|
| D1-01 | 1/7 | `int_for_byte_size(vn.size)` derivable-arg: 49 sites, 34 pass `.size` | add `VnTypeExt::{int_type,float_type}`, fold `float_type_from_vn` too |
| D1-02(min) | 1 | `handle_popcount`/`handle_lzcount` bodies character-identical | `lift_int_unary(insn, build)` helper |
| IR-1 | 5 | `dedup_overlapping_largest` O(n²) over tracked vns (sibling step is O(n log n)) | sweep mirroring `build_largest_container_map` |
| PY-2 | 2/4 | `PyCallOtherPat::ctrl/mem` route to value slot → never match | dedicated ctrl/mem via control/mem compiler + regression test |
| LIFT LOW-1/2/3 | 1 | `handle_cond_branch` open-codes read_input; `handle_piece` double-convert; 3 open-coded `IntConst(1):I1` | use `read_input`/drop intermediates/`build_boolean_const` |
| D1-07 | 1 | `Xor(_,IntConst(1)):I1` logical-NOT built by hand in 2 lift helpers | `build_logical_not` primitive |
| dim6 | 6 | 6 dead `Tb` test-builder methods; `Interval::upper_exclusive`, `Graph::value_has_one_use`, `PositionalArgLayout::stack_offset_of`, `EntityInterner::{contains,is_empty,keys,values}`, `Worklist::contains` test-only-pub | delete / `#[cfg(test)]` / make private |
| GEN MED-1 | 4 | dot template `innerHTML` w/ `${err}` (only unescaped DOM sink) | `textContent` |
| dim8 | 8 | ~25 named missing edge-case tests across crates | add tests (TDD) |

## Group B — judgment calls (real, but tradeoff / behavior-change / possibly intentional)

| ID | Dim | What | Note |
|----|-----|------|------|
| OPT M1 | 5 | jump-table clone+`default_pipeline()` per (candidate×index) inside rebuild loop | memory says clone design is intentional; O(N) cone-extraction "pending" |
| OPT M2/M3/M7 | 2/3 | recovered `Multiple` targets get no exec/mapped-memory validation; `enumerate_targets` may accept index-independent constant / entry under unrelated guard | over-approximation is partly by-design; M3 wrong-edge needs verification |
| OPT M4/M5 | 3 | ConstantFold int-binary/cmp mask to one width; validator allows LHS≠RHS≠out | latent (lifter keeps widths equal); add guard or document |
| OPT M6 | 5 | memory-SSA walk O(loads×chain) per fixed-point iter, no cross-load memo | bounded in practice by narrowing |
| LIFT MED-1 | 2 | `int_for_byte_size` hard-errors on odd widths (3/5/6/7/12) real Sleigh emits | behavior change; at min attach asm context to error |
| LIFT MED-2 | 2 | signed ops (`Sdiv`/`Srem`/`SShiftRight`) silently truncate wider operand, no guard | add width-equality guard (cheap) |
| LIFT MED-3 | 2 | `handle_subpiece` wide (YMM/ZMM) nonzero-offset aborts lift | wide-const shift path or documented error |
| LIFT MED-4 | 4 | `decode_space_id` `pub`+`unsafe` trusts CONST tag, not safety precondition | make `pub(crate)`; tighten |
| IR-2/IR-3 | 4 | resurrection doesn't re-canon; `canonicalize_node` silent escape hatches (`let _=`, single-output `return`) | enqueue recanon; `expect` invariant |
| IR-5 | 4 | `initial_var_index`/`value_vn` can desync after payload rewrite; no validator check | add validator invariant |
| OR-1/OR-2 | 2 | `unresolved_indirect_branches` unfiltered for liveness; `Multiple` out-of-range re-deferred → loop to 256-cap | liveness filter; surface non-convergence |
| PAT MED-1/2/3 | 3 | `find_joined` cartesian when no shared captures; no tuple dedup; `IfPat` swallows `.root()` errors | guard/dedup; propagate errors |
| R-1 | 7 | `ReadOnlyMemory: Send+Sync` + `Arc` blanket impl vs no-Arc rule | known deviation — decide keep/remove |
| R-2/R-4 | 2/5 | autoload coverage single-byte (field straddle dropped); reloc write O(relocs×regions) | full-width coverage; index pass-2 |
| CFG-01/CFG-04 | 4/2 | zero-length insn → infinite loop; switch target not instruction-boundary validated | bail on len 0; diagnostic |
| TGT-1 | 4 | `StackArgs` slot math unchecked i64 on binary-derived offsets → panic/wrap | checked/saturating |
| PY-1 | 2 | `PyMemReaderAdapter::read` `restore` vs stash → can lose KeyboardInterrupt | mirror sibling stash |
| PY-3/4/5/6 | 2/4 | depth-guard bypass via typed builders; over-long read truncate; pipeline mutate; optimize-fail no gen bump | per report |

## Group C — cosmetic / micro / doc-drift

D1-03..D1-11 (further dedup), IR-4/6/7/8/9, GR-1/3/4/5, OR-3/4/5, R-3/5/7,
TGT-2/3/4/5, PY-7/8/9/10, CFG-02/03/05/06, dim7 (value_vn dual-key,
retain_reachable self-arg, EditFunction function_mut), CLAUDE.md staleness.
