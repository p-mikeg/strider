# Round 8 — Executive Summary

**Branch:** `review/ai2`.  Independent multi-pass review of the strider workspace.  All findings re-derived from current source (commit `c7a2903`); round-7 reviews not consulted.

## Coverage

22 per-ask reports across 7 rounds + 3 special asks.  394 in-scope `.rs` / `.py` / `Cargo.toml` files inspected (per `round8-coverage-manifest.md`).  17 parallel subagents + 3 sequential synthesis passes.

## HIGH-severity findings — prioritized fix backlog

| # | Theme | Where | Report | Fix |
|---|-------|-------|--------|-----|
| 1 | **CRITICAL: `strider::run` orchestrator hardcodes `ArchPreset::X86_64`** — every non-x86_64 binary's CallOther is misclassified.  ARM `swi` register channel silently lost; AArch64 `HVC`/`SMC` raise `UnknownCallOtherError`. | `crates/strider/src/orchestrator.rs:826` | `round8-correctness-cross-arch.md` §1 (conf 97) | Replace `Builder::with_endianness(...)` with `Builder::for_arch(opts.strider.arch(), ...)`.  Same fix at `crates/strider/tests/common/mod.rs:215` and `crates/strider/benches/scaling.rs:89`. |
| 2 | `PyVnSpace::__hash__` hashes the Rust field's stack address, violating Python hash/eq contract.  `dict[VnSpace.ram()]` raises `KeyError`. | `crates/strider-py/src/sleigh.rs:144-148` | `round8-1F-strider-py-aux.md` | Hash the inner `rsleigh::VnSpace` identity (variant discriminant). |
| 3 | `Match::get_vn` for `CallOther` with per-node clobber override returns `None` when override length differs from function-default length. | `crates/pattern/src/matcher/match_result.rs:204-226` | `round8-1D-pattern.md` | Use `call_clobbered_override(node).map_or(graph.call_other_clobbered.len(), ov.len)` for `clobber_start`. |
| 4 | `build_int_const` / `make_int_const` silently accept `U512`, producing type-confused `IntConst(u128)` claiming 512 bits.  Validator misses it. | `crates/ir/src/builder/nodes.rs:96-102`; `crates/ir/src/ops/consts.rs:87-97` | `round8-1A-ir.md` (conf 92) | Extend `U256` guard to `U256 \| U512`; direct callers to `build_int_const_wide`. |
| 5 | `decompose_sp` mutual recursion overflows the call stack on deep SP-expression chains (~4-8k nodes). | `crates/opt/src/sp_expr.rs:247, 277, 301, 303` | `round8-correctness-edge-cases.md` H1 (conf 87) | Convert to explicit worklist (mirror `probe` in `stack_load_forward/mod.rs`). |
| 6 | `IfCondInversion::invert` makes `BoolNeg` dead but never absorbs its fingerprint into the surviving inner-cond node — fingerprint contract violated. | `crates/opt/src/if_cond_inversion/mod.rs:101-105` | `round8-correctness-invariants.md` H-2 | Call `extend_asm_fingerprint_from(inner_node, bool_neg_node)` before `update_input`. |
| 7 | `StackLoadForward` BE narrow path emits `Truncate(ShiftRight(...))`; intermediate `ShiftRight` is reachable but has no fingerprint. | `crates/opt/src/stack_load_forward/mod.rs:360-364` | `round8-correctness-invariants.md` H-1 | Use `create_node_attributed(..., &[load])`. |
| 8 | Re-entrant `RwLock` deadlock in `Graph::find_all` when `.when()` predicate calls a mutating method on the same `PyGraph`. | `crates/strider-py/src/graph.rs:349-361, 398-441` | `round8-correctness-borrowing.md` | Use `try_write()` returning a typed error; document constraint in API. |
| 9 | AArch64 / ARM / PPC list link-register (`x30` / `lr` / `LR`) in `callee_saved_regs`, contradicting AAPCS64 §6.1, AAPCS §5.1.1, PPC SysV §3.4.  Tail-call shims via LR are mismodeled. | `crates/target/src/calling_convention/mod.rs:354-355, 496-579` | `round8-17-graph-soundness.md` B-1, B-2, B-3 | Document the deliberate deviation OR introduce a separate `link_register_preserved_by_convention` flag. |
| 10 | `mfence` / `sfence` / `lfence` missing from CallOther table → any binary using SSE memory fences raises `UnknownCallOtherError`. | `crates/target/src/call_other_abi.rs::classify_arch_specific` | `round8-17-graph-soundness.md` D-1 | Add `(X86 \| X86_64, "mfence" \| "sfence" \| "lfence") => PURE_WITH_MEM_EDGE`. |
| 11 | `step_through_stack_store_phi` returns `PassThrough` (sound: `MayAlias`) when `stack_phi_offsets` is empty.  Latent: `StackStoreDetect` always populates today. | `crates/opt/src/sp_expr.rs:131-152` | `round8-correctness-edge-cases.md` H2 | Early-return `MayAlias` on `offsets.is_empty()`. |
| 12 | `flag_cmp_canonicalize` `unwrap_or(a)` silently aliases rhs capture to lhs when binding missing.  | `crates/opt/src/flag_cmp_canonicalize/mod.rs:129` | `round8-2C-silent-failures.md` H1 | `.expect("rhs_capture must bind on successful match")`. |
| 13 | `apply_tail_call` `unwrap_or(NodeOutputType::U64)` defaults non-integer target type silently. | `crates/opt/src/indirect_branch_resolve/inplace.rs:129` | `round8-2C-silent-failures.md` H4 | Propagate `Err` via `as_integer_or_err()`. |
| 14 | `anchor_contexts.get(addr).unwrap_or(&empty_ctx)` silently splices an empty calling context for missing anchors. | `crates/opt/src/indirect_branch_resolve/mod.rs:281` | `round8-2C-silent-failures.md` H5 | Typed `Err(MissingAnchorContextError { addr })`. |
| 15 | `strider-py` reader / pattern paths swallow poisoned-mutex via `.ok()?` (`PyMemReaderAdapter`, `PyReadOnlyMemoryAdapter`, `with_graph`). | `crates/strider-py/src/reader.rs:656, 666`; `crates/strider-py/src/pattern.rs:292` | `round8-2C-silent-failures.md` H2, H3 | Map mutex poison to `LiftError` / `PatternError`. |
| 16 | `handle_insert` / `handle_extract` compute masks as `u64`; for `U128` destination/output with `len ≥ 64`, masks zero-extend losing upper bits, corrupting the destination. | `crates/pcode-lift/src/value/cast.rs:131-176` | `round8-1B-pcode-lift-cfg.md` | Compute masks in `u128`. |

## MED-severity (selected) — see per-ask reports

- `check_layer_c_function_arg_uniqueness` not reachability-scoped → false-positive `DuplicateFunctionArg` from zombies (`round8-1A-ir.md`).
- `apply_in_place_edits` scans `all_node_ids()` (zombie-inclusive); resurrects stale `InitialVar` nodes after `FunctionArgDetect` (`round8-correctness-invariants.md` M-1).
- `apply_elf_relocations` mislabels malformed-ELF as `skipped_unresolved_target` (`round8-1E-strider-target-reader.md`).
- `locate_and_write` unchecked `site_addr + size_bytes as u64` overflow (`round8-1E-strider-target-reader.md`).
- `PyMatch.__getitem__` `as i128` truncation for U128 with bit 127 set (`round8-1F-strider-py-aux.md`).
- `Vn.__hash__` omits `addr_space` → bucket collisions (`round8-1F-strider-py-aux.md`).
- `MemPhiPat` and `ValuePhiPat` not re-exported from `pattern::lib` (`round8-1D-pattern.md`).
- `GuardPat::try_match_node` default impl silently fails on zero-output nodes like `Return` (`round8-1D-pattern.md`).
- Type-design: `BuiltFunctionGraph` round-7 partial-state form still publicly constructible via `from_graph_and_entry_for_rewrite` (`round8-2D-types.md`).
- Type-design: `PcodeInsnAddr` / `MachineInsnAddr` newtypes-in-name-only (~30 leak sites) (`round8-2D-types.md`).
- PowerPC CR-bit canonicalisation gap (`round8-correctness-cross-arch.md` §2).
- `NodeOutputType` doc/CLAUDE.md missing `U80` / `U512` / `F80` variants (`round8-3A-doc-verify.md`).
- `vn_mask` accepts widths 32 / 64 (YMM/ZMM) — CLAUDE.md/README claim only 1/2/4/8/10/16 (`round8-3A-doc-verify.md`).
- `target/README.md` `ArchPreset` variant list broken: typos `X8664` / `Mipsbe32`, all `Ppc*` and Linux-kernel/syscall presets missing (`round8-3A-doc-verify.md`).

## Reports by user-ask number

| User ask | Status | Report file |
|----------|--------|-------------|
| 1. Correctness — graph faithful to assembly | covered | round8-17-graph-soundness.md, round8-correctness-{invariants,cross-arch,edge-cases,borrowing}.md, round8-1A-ir.md, round8-1B-pcode-lift-cfg.md |
| 2. Simplicity — duplicated patterns / dead code | covered | round8-simplifications.md (30 proposals) |
| 3. Tier-naming sweep | covered | round8-2B-naming.md (8 renames + 1 file rename) |
| 4. Unused features for deletion | covered | round8-simplifications.md §1 (5 deletion candidates) |
| 5. Missing Python-binding parity | covered | round8-1F-strider-py-aux.md, round8-3A-doc-verify.md |
| 6. Multiple rounds of review | done | 7 rounds × 22 reports |
| 7. Test-gap plan | covered | round8-test-plan.md (37 entries) |
| 8. Stale comments | covered | round8-3B-comments.md (8 HIGH + 6 MED) |
| 9. No `panic!`/`unwrap!`/`expect!` in production | clean | round8-2A-panics.md (0 unjustified, 5 justified) |
| 10. Verify CLAUDE.md against code; per-crate READMEs | covered | round8-3A-doc-verify.md (75/88 confirmed, 7 refuted) |
| 11. Skill design | covered | round8-skill-design.md (6 new skills) |

Special asks:
- 16 (perf at 10k+ nodes) — `round8-16-perf.md`
- 17 (graph soundness vs real asm) — `round8-17-graph-soundness.md`
- 18 (multi-correctness via parallel agents) — 4 reports under `round8-correctness-*.md`

## Recommended fix sequence

1. **First wave (HIGH, low-risk fixes):** #1 (orchestrator preset), #4 (U512 guard), #10 (mfence/sfence/lfence), #12-14 (silent-failure typed errors), #15 (mutex-poison mapping).  Estimated 1-2 days.
2. **Second wave (correctness invariants):** #5 (decompose_sp worklist), #6-7 (fingerprint absorption), #11 (StackStorePhi MayAlias default), #16 (Insert/Extract u128 masks).  Estimated 2-3 days.
3. **Third wave (PyO3 contract):** #2 (PyVnSpace hash), #3 (get_vn override-length), #8 (find_all re-entrancy guard).  Estimated 1 day.
4. **Fourth wave (CC tradeoffs):** #9 (LR-as-callee-saved documentation OR refactor).  Decide first; ~1 day either path.
5. **Test backfill:** Round 4 plan (37 entries; ~6 days total — 4 require new fixtures).
6. **Cleanup:** Round 5 simplifications (30 proposals; ~3 days).
7. **Skill bundle:** Round 6 skills (6 designs; ~2 days to author).

## Acceptance check

| Criterion | Status |
|-----------|--------|
| Every numbered user ask (1-11) addressed | ✓ |
| Every HIGH finding has a fix proposal with file:line | ✓ |
| CLAUDE.md correctness diff exists with ≥10 spot-checked claims | ✓ (`round8-claudemd-diff.md`, 38 sampled) |
| Tier-naming rename mapping concrete | ✓ (round8-2B, 8 mappings) |
| Test plan ≥12 gaps with file:line scaffolding | ✓ (37 entries) |
| Skill bundle ≥6 designs with triggers + verification | ✓ (round8-skill-design.md, 6 skills) |
| `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` continue to pass | ✓ at baseline; not re-verified after proposals (proposals only — no source edits in this round) |

## Non-findings worth recording

The following spec items were verified and found correct; recording so future rounds avoid re-investigation:

- All 8 lift-time canonicalisations match CLAUDE.md spec (`pcode-lift::value::{arithmetic, float}`).
- Sleigh nomenclature inversion (`Int2Comp` → `Neg`, `IntNeg` → `BitNot`) is correctly handled.
- Production panic discipline is fully enforced — 0 unjustified panics across ~106 production source files.
- `retain_reachable` 7-pass GC is sound across all four primary side-tables + wide_consts.
- All CC presets' return-value register lists match ABI specs.
- All CallOther arch-specific entries (CPUID, RDTSC, RDTSCP, RDMSR, WRMSR, RDFSBASE/GSBASE, WRFSBASE/GSBASE, SWAPGS, syscall, swi, SMCCC, RDPKRU, ARM DMB/ISB/DSB) cross-checked against Intel SDM / ARM ARM / SMCCC 1.2.
