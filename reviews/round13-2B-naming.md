# Round 13 — 2B: naming sweep

Branch: `review/ai7` · Scope: every `.rs`, `.py`, `.md` referenced from source.

## Summary

| Category | Count |
|---|---|
| Migration-narrative breadcrumbs in `.rs` source | 22 sites |
| Unclear / misleading identifiers | 2 items (LOW) |
| Short test-name labels | 2 items (LOW, doc-comment labels only) |
| Abbreviations needing expansion | 0 (all domain-standard) |
| Type names misrepresenting role | 0 |
| CLAUDE.md vs code discrepancies | 0 |

## Category 1 — Round-12-style breadcrumbs in source

Round 12 W2 stripped ~50 breadcrumbs.  The waves landed since then re-introduced these.  All are in comments / doc-comments; none in function or test names.  The fix in every case is to drop the ticket-ID prefix and keep the rationale prose.

| File:line | Breadcrumb | Suggested rewrite |
|---|---|---|
| `crates/entity-utils/src/worklist.rs:218` | `introduced in W9` | delete phrase; implementation shape is self-evident |
| `crates/cfg/src/cfg/mod.rs:68` | `hazard W14 fixed for the map` | "a prior bug where direct mutation desynchronised the map" |
| `crates/target/tests/cc_validation.rs:91` | `T-2: build routes through try_from_parts` | drop "T-2:" |
| `crates/pcode-lift/tests/value_lifter.rs:891` | `T-30 (IntLessEqual lowering shape)` | drop "T-30" |
| `crates/pcode-lift/src/vn_io.rs:247` | `round-12 EC-3` | drop tag; rationale already present |
| `crates/pcode-lift/src/vn_io.rs:325` | `in round-12 EC-3` | drop tag |
| `crates/target/src/call_other_abi.rs:495` | `round-12 CA-2` | "Regression test: `sysret` and `swapgs` are x86/x86_64-specific" |
| `crates/cfg/tests/options.rs:30` | `round-12 EC-1` | drop tag |
| `crates/cfg/src/cfg/options.rs:186-187` | `round-12 EC-1` | drop tag |
| `crates/cfg/src/cfg/options.rs:212` | `round-12 R12-T-G` | rationale prose suffices |
| `crates/cfg/src/cfg/options.rs:215-216` | `round-12 EC-1` | drop tag |
| `crates/cfg/src/cfg/mod.rs:142` | `round-12 S2.4` | drop tag |
| `crates/opt/src/indirect_branch_resolve/mod.rs:104` | `round-12 TY-2` | "Option is the idiomatic carrier" suffices |
| `crates/opt/src/pipeline.rs:36` | `round-12 TY-3` | drop tag |
| `crates/strider/src/orchestrator.rs:67-68` | `round-12 R12-T-H` | drop tag; "newtype prevents accidental swap" suffices |
| `crates/cfg/src/cfg/builder/region_builder.rs:359` | `round-12 R-9` | drop tag |
| `crates/pattern/src/rewrite.rs:155-156` | `round-12 R12-T-A` | drop tag; field-visibility rationale fully stated |
| `crates/pattern/src/matcher/bindings.rs:24` | `round-12 R12-T-N` | drop tag |
| `crates/pattern/tests/matching/ssa.rs:97` | `T-1 (M-1)` | drop `T-1 (M-1):`; the test function name is descriptive |
| `crates/pattern/tests/matching/control_flow.rs:199` | `T-20` | drop `T-20:` |
| `crates/opt/src/indirect_branch_resolve/inplace.rs:305` | `H0 —` | rename section header to "CC-threading regression tests for the in-place editors" |
| `crates/strider/tests/indirect_resolve_in_place_edits.rs:296` | `Pre-H0` | "Before the ABI-threading fix" |

**Notes:**
- `Phase 1` / `Phase 2` in `crates/opt/src/known_bits/mod.rs:465,470` are internal algorithm-phase labels (`propagate known bits` vs `replace fully-determined outputs`), NOT migration breadcrumbs.  No change.
- `r{DEPTH-1}` in `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:765` is a template-literal placeholder in a doc comment describing a parameterised graph shape.  Not a breadcrumb.

## Category 2 — Unclear / misleading identifiers

**`BFG` abbreviation in inline comments** — `crates/ir/src/function.rs:194`, `crates/opt/src/pipeline.rs:86,146`, `crates/strider/src/rewrite.rs:115` use `BFG` in explanatory prose.  The full type name `BuiltFunctionGraph` is used in public API everywhere.  Spell it out in these four comment sites for consistency.

**`LoopState` in `crates/strider/src/orchestrator.rs:247`** — Generic name for the indirect-branch fixed-point loop's state machine.  A more self-documenting name would be `FixpointLoopState`.  Low priority: struct is private to the module.

## Category 3 — Short test-name labels

`T-1 (M-1)` and `T-20` (in the breadcrumb table above) are doc-comment labels above test functions.  The function names themselves (`phi_input_addresses_predecessor_slot_not_phi_token`, `match_value_accessors_on_control_flow_capture_return_none`) are already descriptive — removing the ticket prefix from the doc-comment label is the entire fix.

## Category 4 — Abbreviations

All pervasive abbreviations (`vn`, `cc`, `sp`, `bfg`, `fg`, `OOB`) are domain-standard and used consistently.  No expansion warranted.

## Category 5 — Type names

`LoopState` is the only borderline case (see Category 2).  `SpecialTerm`, `CallOtherClass`, `CallOtherAbi`, `RegionIndex`, `VarPhi`, `MemPhi`, etc. all accurately capture role.

## CLAUDE.md / READMEs

All sampled terms verified against source: `Optimizer`/`OptimizerRaw` (correctly updated post-R12), `IndirectBranchResolve` (correctly described as free functions, no struct), `RewriteCtx`/`GraphRewriter`/`BuiltFunctionGraph`/`RegionIndex`/`LoopState`/`CallOtherClass`/`VarPhi`/`MemPhi`/`WideConstId`, `BuiltCallingConvention` + `Parts` + `build` — all present in source as documented.  Crate-README claims defer to 3A.
