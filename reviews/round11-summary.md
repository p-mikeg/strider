# Round 11 — R7 Final Consolidation Summary

Audit branch: `review/ai5` (forked from `review/ai4` HEAD `2cea799`).
Reports consumed: round11-coverage-manifest, 1A–1F, 2A–2D, 3A–3B, skill-audit, test-plan, simplifications (16 files total).
No round-7/8/9/10 reports were read; all carried-forward items are sourced from round-11 report findings only.

## 1. Verification

| Check | Result |
|-------|--------|
| `cargo build --workspace --all-targets` | PASS |
| `cargo clippy --workspace -- -D warnings` | PASS |
| `cargo clippy --workspace --all-targets -- -D warnings` | **FAIL — 89 errors** (opt lib-test target; H-1) |
| `cargo test --workspace` | PASS (0 failures) |
| `cd crates/strider-py && uv run pytest` | PASS (774 passed, 11 skipped) |

## 2. HIGH Findings (18)

| ID | Source | File:Line | Summary | EA | Test |
|----|--------|-----------|---------|-----|------|
| H-1 | 1C F-1 | `opt/src/{test_support.rs:18, stack_store/tests.rs:6, constant_fold/tests.rs:15-27, known_bits/tests.rs:8-15, load_readonly/tests.rs:24-31, if_cond_inversion/tests.rs:43-59}` | 89 clippy `--all-targets` errors: stale `BuiltFunctionGraph` imports + 87 `(&fg).into()` useless conversions | EA1 | T-10 |
| H-2 | 1F F-1 | `strider-py/src/reader.rs:492-514` | `PyMemReaderAdapter::read` swallows `KeyboardInterrupt`/`SystemExit` | EA1 | T-5 |
| H-3 | 1F F-2 | `strider-py/src/cfg.rs:52-55` | `build_cfg` calls deprecated `Builder::new` (X86_64 default); snapshot CFG preset disagrees with orchestrator on non-x86 | EA1 | T-6 |
| H-4 | 2D F-3 | `pattern/src/rewrite.rs:215-229` | `RewriteCtxView` `pub graph`/`pub entry`; external rebind hazard | EA1 | T-25 |
| H-5 | 2D F-4 | `pattern/src/rewrite.rs:154-157` | `RewriteCtx` `pub graph`/`pub entry`; mutable rebind hazard | EA1 | — |
| H-6 | 2D F-5 | `ir/src/function.rs:45-109` | `BuiltFunctionGraph` 5 CC-bearing fields `pub`; `Match::get_vn` slot-positional miscount on mutation | EA1 | — |
| H-7 | 2D F-12 | `opt/src/known_bits/mod.rs:36-50` | `Kb { pub ones, pub zeros }`; struct-literal bypasses `try_new` invariant | EA1 | T-25 |
| H-8 | 3B F-1 | `cfg/src/cfg/options.rs:9` | Malformed merged doc-comment (literal `///` mid-line) | — | T-15 |
| H-9 | 3B F-2 | `cfg/src/cfg/options.rs:99` | Same merge bug on `Options::function_boundary` | — | T-15 |
| H-10 | 3B F-3 | `cfg/src/cfg/types.rs:79` | Same merge bug on `PcodeInsnAddr::machine_addr` | — | T-15 |
| H-11 | 3B F-4 | `ir/src/builder/mod.rs:410` | Same merge bug on `lift_at` | — | T-15 |
| H-12 | 3B F-5 | `target/src/arch.rs:150` | Same merge bug on `SleighArch::preset` | — | T-15 |
| H-13 | 3B F-6 | `target/src/calling_convention/tests.rs:682` | Same merge bug on test doc | — | T-15 |
| H-14 | 3B F-14 | `target/src/calling_convention/mod.rs:99` | Broken intra-doc link `[Self::from_parts]` (method doesn't exist) | — | — |
| H-15 | 3B F-23 | `pattern/src/pat/builders/branch.rs:40`, `ctor/control.rs:142` | Cross-crate `[opt::IfCondInversion]` link from pattern | — | — |
| H-16 | 3B F-27 | `opt/src/pipeline.rs:118, 251` | `[crate::Error]` link — opt has no Error type | — | — |
| H-17 | 3B F-31 | `opt/src/indirect_branch_resolve/mod.rs:66` | `[cfg::Builder::with_known_targets]` cross-crate link | — | — |
| H-18 | 3B F-35 | `strider/src/strider/pipeline.rs:182-183` | `build_optimizer_pipeline` doc lists 4 passes; actual is 6 | — | T-17 |

## 3. MED Findings

### 3A — Code Correctness (13)

| ID | Source | File:Line | Summary | EA | Test |
|----|--------|-----------|---------|-----|------|
| M-1 | 1D F-1 | `pattern/src/pat/builders/phi.rs:38-43, 73-77, 102-107` | `PhiPat::input(idx)` addresses raw input slot, not predecessor slot | EA2 | T-1 |
| M-2 | 1E F-1 | `target/src/calling_convention/mod.rs:755-788` | `CallingConvention::build` bypasses `try_from_parts` validator | EA1 | T-2 |
| M-3 | 1E F-2 | `reader/src/elf.rs:907-916` | MIPS64 `R_MIPS_REL32` writes 8 bytes (should be 4) | EA3 | T-3 |
| M-4 | 1E F-3 | `reader/src/elf.rs:670-713` | `apply_elf_relocations` rollback claim is partial | — | — |
| M-5 | 1E F-4 | `strider/src/strider/insn/mod.rs:218-224` | `handle_call_other` clobber-write loop overwrites pcode-explicit value output | EA2 | T-4 |
| M-6 | 1F F-3 | `strider-py/examples/python/07_callback_rom.py:43` | Example `read` 3-arg signature; adapter calls 2-arg | — | T-12 |
| M-7 | 1F F-4 | `strider-py/README.md:280-285` | README documents wrong 3-arg `read` signature | — | T-12 |
| M-8 | 1F F-5 | `strider-py/src/errors.rs:31-39` | Orchestrator path raises generic `StriderError` instead of `LiftError` | — | T-7 |
| M-9 | 1A F-1 | `ir/src/node/output_type.rs:177-189` | `bit_mask_u128` doc/code contradiction on U256/U512 | — | T-14 |
| M-10 | 1A F-2 | `ir/src/graph/store.rs:50-75` | `Graph::set_node_kind` doesn't enforce signature compatibility | — | T-13 |
| M-11 | 1B F-1 | `cfg/src/cfg/builder/region_builder.rs:348-383` + `strider/src/strider/pipeline.rs:349-413` | Empty-insns Branch region's IR control edge not wired | EA2 | T-11 |
| M-12 | 2C F-1 | `opt/src/function_args/mod.rs:491-518` | `mem_chain_is_dirty` returns false (clean/unsafe) for malformed Call/MemPhi | — | T-8 |
| M-13 | 2C F-2 | `strider/src/orchestrator.rs:582-588` | `apply_in_place_edits` silently skips `InitialVar` with non-1 outputs | — | T-9 |

### 3B — Type Design (11)

| ID | Source | File:Line | Summary |
|----|--------|-----------|---------|
| M-14 | 2D F-1 | `cfg/src/cfg/types.rs:60-100` | `PcodeInsnAddr` pub fields (~14 reads + 7 struct-literals to migrate) |
| M-15 | 2D F-6 | `ir/src/function.rs:13-23` | `FunctionGraph` four pub fields with `new_invalid` sentinel |
| M-16 | 2D F-7 | `cfg/src/cfg/mod.rs:62-73` | `Cfg::start_addr_to_region_id` pub map with desync caveat |
| M-17 | 2D F-8 | `cfg/src/cfg/types.rs:216-227` | `Region` pub fields + cross-field invariant |
| M-18 | 2D F-11 | `target/src/calling_convention/mod.rs:127-148` | `BuiltCallingConventionParts` all-pub fields + unguarded `from_parts_unchecked` |
| M-19 | 2D F-13 | `opt/src/indirect_branch_resolve/mod.rs:144-191` | `IndirectBranchResolve.unresolved_anchors` precondition unchecked |
| M-20 | 2D F-15 | `opt/src/indirect_branch_resolve/mod.rs:81-99` | `ResolvedTargets::Multiple(Vec<u64>)` non-empty bypassed |
| M-21 | 2D F-17 | `strider/src/orchestrator.rs:60-106` | `RunConfig` partial-state pair + primitive-obsession `start_addr` |
| M-22 | 2D F-19 | `strider/src/strider/pipeline.rs:21-47` | `RegionLiftHandles` 9 pub fields + cross-field invariants |
| M-23 | 2D F-23 | `pattern/src/matcher/bindings.rs:81-89` | `bind_capture` is pub; doc claims read-only |
| M-24 | 2D F-25 | `cfg/src/cfg/builder/mod.rs:98-108` | `Builder::new`/`with_endianness` deprecated but used in 9 test files |

### 3C — Naming & Comments (9)

| ID | Source | Summary |
|----|--------|---------|
| M-25 | 2B | ~80 `fg: RewriteCtxView<'_>` half-rename sites in opt |
| M-26 | 2B | `OptimizerOnBuilt` trait name stale post-S1.1 |
| M-27 | 2B | `Regression for round8-correctness-edge-cases` breadcrumbs |
| M-28 | 2B | `Slice 1`…`Slice 5 (audit B2 blocker)` test-doc prefixes |
| M-29 | 3B F-7 | `(round 9 P5 / R9-2D M6)` breadcrumb |
| M-30 | 3B F-8 | `(round 9 V4 / R9-2D H3)` breadcrumb |
| M-31 | 3B F-10 | `Pre-fix (round 9 Ask-8 R2 F7)` breadcrumb |
| M-32 | 3B F-12 | `round 9 wave 31 (H-8)` historical reference |
| M-33 | 3B F-36 | `LoopState::sleigh` doc names `Builder::with_endianness` (deprecated) |

### 3D — Additional broken intra-doc links (~25)

`crates/ir/src/lib.rs:18,31` (`[node::Graph]` → `[Graph]`); `crates/ir/src/builder/mod.rs:552` (`[BuiltFunctionGraph]`); `crates/pattern/src/matcher/mod.rs:365` (`[Capture]`); `crates/opt/src/indirect_branch_resolve/inplace.rs:41-42` (`[Graph::add_node_input]`); `crates/cfg/src/cfg/types.rs:5,115,213` (`[RegionGraph]` private alias); `crates/strider/src/rewrite.rs:89` (`[mem::take]`); 19 more in 3B findings 15–40.

## 4. Per-axis Emphasis A Breakdown

| Axis | Count | Key findings |
|------|-------|-------------|
| EA1 (code-vs-code) | 12 | H-1, H-2, H-3, H-4, H-5, H-6, H-7, M-2, M-14–M-24 |
| EA2 (IR-vs-pcode) | 3 | M-1, M-5, M-11 |
| EA3 (IR-vs-assembly) | 3 | M-3, M-4, H-3 |

## 5. Per-category Emphasis B Breakdown (Simplifications)

| Category | Entries | Net LOC | Highest-value |
|----------|---------|---------|---------------|
| 1. Delete | 14 | -1100 | S1.1: `IndirectBranchResolve` struct + integration test (~-700) |
| 2. Merge | 11 | -180 | S2.5: `default_pipeline = stable + destructive`; S2.4: trait collapse |
| 3. Inline | 9 | -55 | S3.2: `Strider::arch()`/`calling_convention()` |
| 4. Stdlib idioms | 7 | -25 | S4.1: `Worklist::enqueue` two-pass collapse |
| 5. Visibility | 8 | 0 | S5.4: RewriteCtx; S5.7: BuiltFunctionGraph CC fields |
| 6. Wrappers | 4 | -50 | S6.4: `AnchorAddr` (cascades from S1.2) |
| 7. Partial-state | 8 | -100 | S7.1/S7.2: `cfg::Options`/`RunConfig` `FunctionBoundary` enum |
| **Total** | **61** | **-1510** | |

Baseline: ~55,134 LOC. Projected: ~53,600 LOC. Net reduction: ~2.7%.

## 6. Round-10 Deferral Resolution Table

| Round-10 item | Status | Round-11 finding | Test |
|---------------|--------|-------------------|------|
| T-3: MIPS reloc width | Open | M-3 | T-3 |
| T-9: `apply_in_place_edits` skip | Open | M-13 | T-9 |
| T-10: clippy `--all-targets` gate | Open | H-1 | T-10 |
| T-14: `bit_mask_u128` doc | Open | M-9 | T-14 |
| T-22: `arm_be` smoke | Open | 1E N-4 | T-19 |
| 1B I-2 (`as u8` truncation) | Open | 1B LOW | S4.7 |
| 1D I-1 (`PhiPat::input`) | Open | M-1 | T-1 |
| 1F F-08 (KBI/SysExit) | Open | H-2 | T-5 |
| Perf: KnownBits stale map | Open | 1C F-14 | T-24 |
| Perf: `Worklist::enqueue` | Open | 1F F-6 / S4.1 | T-23 |
| Round-10 `#[deprecated]` cleanup | Open | S1.4 | T-10 / S1.4 |

All 11 deferred items remain open. No fixes were applied this round (audit-only).

## 7. Coverage Table

| Wave | Crates | Files fully read | Notes |
|------|--------|-----------------|-------|
| 1A | `ir` | 47/53 | benches/examples skipped |
| 1B | `pcode-lift` + `cfg` | 47/47 | full coverage |
| 1C | `opt` | 49/53 | benches skipped |
| 1D | `pattern` | full src + selected tests | — |
| 1E | `strider` + `target` + `reader` | ~35 full + 37 partial | examples skipped |
| 1F | `strider-py` + `dot` + `graphwalk` + `entity-utils` | 37 .rs + ~70 .py | sampled |

Total `.rs`: 322 source/test. Total `.py`: 50 test. READMEs: 13. SKILLs: 19.

## 8. Multi-round Signal

1. **H-1 is the highest-priority fix** — blocks `--all-targets` clippy CI gate. Mechanical: delete 5 shadow helpers + drop 2 imports.
2. **H-2 / H-3 silently affect non-x86 users** — `MemReader` non-interruptible; `build_cfg` snapshot disagrees with orchestrator preset.
3. **H-8 through H-13 are one bug applied 6 times** — automated edit merged `///` with `(R9-...)` breadcrumbs.
4. **H-4/H-5/H-6/H-7 are the same encapsulation pattern** — types with non-trivial invariants exposing mutable fields.
5. **EA2 findings (M-1, M-5, M-11) are highest-value untested correctness items** — IR diverges from pcode semantics.
6. **Simplification S1.1 alone is -700 LOC** — deleting `IndirectBranchResolve` enables S2.4 and S6.4 to cascade.

## 9. Bottom Line

| Severity | Count |
|----------|-------|
| HIGH | 18 |
| MED | ~44 |
| LOW | ~210 |

Recommended order:
1. H-1 (clippy gate, T-10) — unblocks CI in ~1 hour.
2. H-8 through H-13 (malformed doc-comments, T-15) — mechanical text fixes.
3. H-2 + H-3 — user-visible non-x86 regressions.
4. M-1 + M-5 + M-11 — EA2 correctness.
5. H-7 + H-4/H-5/H-6 — encapsulation batch.
6. S1.1 — largest single LOC win.
