# Round 11 — 2B: naming sweep

## Summary

Comprehensive naming-drift sweep across the 12-crate workspace.  Categories
reviewed: migration breadcrumbs in source comments / docstrings / module headers,
half-rename leftovers (`fg`/`function`/`OptimizerOnBuilt` after the
`BuiltFunctionGraph → RewriteCtx` migration), single-letter variables outside
tight loops, ambiguous abbreviations (`cc`, `bfg`, `b`, `Tb`), type names that
no longer reflect the type's role (`SpecialTerm` reference from `cfg`,
`from_packed`, `VarPat`), inconsistent sibling naming
(`output_kind` / `kind_of_output` / `node_kind` / `get_node_from_output`,
`build_int_const` vs `make_int_const`, `apply_elf_relocations[_with_extender|_autoload]`,
`classify_anchor[_with_rom][_with_rom_and_sp]`), and test names that describe setup
rather than the assertion.

| Category | Count |
|----------|-------|
| Migration breadcrumb references in source (Rust) | 40 |
| Migration breadcrumb references in source (Python) | 21 |
| Doc-line formatting bugs (orphaned `(R9-…)` glued onto previous prose) | 4 |
| Half-rename leftovers (`fg`, `function`, `OptimizerOnBuilt`, `bfg`, `body()` vs field `function`) | ~80 sites across opt + ir |
| Misleading constructor / type names | 8 |
| Inconsistent accessor naming (sibling drift) | 6 families |
| Test-name clarity issues | 7 |

The codebase has a substantial backlog of breadcrumbs from rounds 7 / 8 / 9 / 10 plus
"Slice N", "Audit IN", "Phase 1/2", "FB-3", "C2" tags whose context has long
expired.  Single biggest mass cleanup target: the `Slice N` / `audit XN` test
docstrings in `crates/opt/src/function_args/tests.rs`, plus the `fg:
RewriteCtxView` parameter naming convention across the entire `opt` crate
(literally an "fg" referring to a `RewriteCtxView` because it used to be a
`FunctionGraph`).

## Migration-breadcrumb hits

| File:line | Breadcrumb | Suggested rewrite |
|-----------|------------|-------------------|
| `crates/cfg/src/cfg/options.rs:9` | `(R9-2D M3)` glued onto same line as docstring | drop the round tag entirely; describe rule in one sentence |
| `crates/cfg/src/cfg/options.rs:99` | `(R9-2D M3): canonical accessor that resolves the documented` | drop the round tag |
| `crates/cfg/src/cfg/types.rs:79` | `(R9-2D H2): canonical accessor for the migration path that` glued to prose | drop the round tag |
| `crates/cfg/src/cfg/builder/indirect_resolve.rs:191` | `// (Task 15).` (no other context) | delete or expand to what Task 15 was |
| `crates/ir/src/builder/mod.rs:410` | `(R9-1A I3) closed the prior leak path` glued to prose | drop the round tag |
| `crates/ir/src/builder/tests.rs:1612` | `Regression for round8-1A HIGH:` | rephrase as "Regression: …" |
| `crates/ir/src/builder/tests.rs:1529` | `// FB-3 type-design: pin lift_at's restore-on-exit contract.` | drop the FB-3 prefix |
| `crates/ir/src/builder/tests.rs:546` | `// C2 (strider): pin Graph::memory_output_of` | drop C2 marker |
| `crates/ir/src/validate/tests.rs:308` | `round 9's reachability gate (Ask-8 R2 F2 fix in …)` | "the reachability gate added in `check_layer_c_control_state`" |
| `crates/ir/tests/builder_extended_use.rs:37,43,45,51` | `Round 1` / `round 1` / `Round 2` / `round 2` (referring to a multi-step assertion within the same test, NOT a workspace audit round) | rename to "Step 1" / "Step 2" — the word "Round" overloads with the audit terminology |
| `crates/opt/src/test_support.rs:10` | `reviews/round8-repetition-sweep.md (#1)` | drop reviews path or move to commit message |
| `crates/opt/src/sp_expr.rs:865,896` | `Regression for round8-correctness-edge-cases H1`, `H2` | rephrase |
| `crates/opt/src/constant_fold/tests.rs:1444` | `── Comprehensive tests added in Task 2.E ──` | drop Task 2.E |
| `crates/opt/src/function_args/mod.rs:529` | `Round 10 H10-S6 (R10-2C):` | drop |
| `crates/opt/src/function_args/mod.rs:220` | `Slice 4 extends this with memory-shadow disqualification; slice 5 extends` | rename to feature names |
| `crates/opt/src/function_args/tests.rs:19,76,141,207,241,321,664` | `Slice 1` … `Slice 5 (audit I2)` … `Slice 4 (audit B2 blocker)` | replace with the actual test concern |
| `crates/opt/src/function_args/tests.rs:394` | `Audit I4:` | drop |
| `crates/opt/src/if_cond_inversion/tests.rs:177` | `Regression for round8-correctness-invariants H-2:` | drop |
| `crates/opt/src/indirect_branch_resolve/mod.rs:102-103` | `(round 9 P5 / R9-2D M6)` | drop both |
| `crates/opt/src/indirect_branch_resolve/mod.rs:631,680` | `Regression for round8-2C H5:` and `arms were deleted in round 9 wave 30 (D3+D4)` | drop |
| `crates/opt/src/indirect_branch_resolve/inplace.rs:305,414` | `H0 — calling-convention threading tests`, `Regression for round8-2C H4:` | drop |
| `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:935` | `CORRECTNESS — H2:` | drop H2 prefix |
| `crates/opt/src/stack_load_forward/mod.rs:264` | `10 R10-1C I-6 fix.` | drop |
| `crates/opt/src/stack_load_forward/tests.rs:27` | `see reviews/round8-repetition-sweep.md (#1).` | drop |
| `crates/opt/src/stack_store/tests.rs:587` | `── Comprehensive tests added in Task 5 ──` | drop Task 5 |
| `crates/opt/tests/asm_fingerprint_propagation.rs:237` | `the round-9 H-2 contract` | "the asm-fingerprint contract" |
| `crates/opt/tests/wide_const_passthrough.rs:1,3` | `//! Wide-const Phase-1 passthrough …` and `Wide arithmetic folding is deferred to Phase 2` | drop Phase-1 / Phase 2 (no Phase 2 work tracked here) |
| `crates/pattern/tests/matching/matcher_api.rs:239` | `Phases 2/3/4 implement the actual walk-through; Phase 1 only adds the API surface` | drop Phase 1/2/3/4 — feature has shipped |
| `crates/reader/src/elf.rs:705` | `Round 10 R10-1E I-2.` | drop |
| `crates/reader/tests/elf_relocations.rs:93` | `The wave-1 M3 fix` | drop wave-1 reference |
| `crates/reader/tests/elf_converters.rs:277` | `── Task 1: a FILTER-REJECTED malformed section is silent, not an error ──` | drop "Task 1" prefix |
| `crates/reader/tests/mem_region.rs:283` | `it's only meaningful once Task 1 privatizes them.` | drop |
| `crates/strider/benches/scaling.rs:92` | `round8-correctness-cross-arch §1.` | drop |
| `crates/strider/src/orchestrator.rs:215` | `Pre-fix (round 9 Ask-8 R2 F7) the comparison was >=` | rephrase |
| `crates/strider/src/orchestrator.rs:805` | `(same reasoning as H-4 above)` | the "H-4" anchor doesn't exist anywhere local |
| `crates/strider/src/orchestrator.rs:1057` | `These tests pin the wave-2 fix (>= → >)` | drop wave-2 |
| `crates/strider/src/test_utils.rs:13` | `reviews/round8-repetition-sweep.md (#5) — 18 sites duplicated …` | drop |
| `crates/strider/tests/common_smoke.rs:18` | `convention (Task 16) and the BE shift formula fix (Task 4).` | drop both |
| `crates/strider/tests/indirect_resolve_classify.rs:283` | `Round 10 T-3 regression:` | drop "Round 10 T-3" |
| `crates/strider/tests/per_address_cc.rs:62` | `Pre-Task-9 (no per-address CC)` | "before per-address CC was added" |
| `crates/strider/tests/common/mod.rs:219` | `See round8-correctness-cross-arch §1.` | drop |
| `crates/strider-py/src/reader.rs:577-578` | `Mirrors the wrap_when fix from round 9 wave 31 (H-8).` | drop |
| `crates/strider-py/tests/python/test_optimizer_pipeline.py:13` | `"""G7 (Round-7 follow-up): pin Python's manually-listed default` | drop G7 / Round-7 |
| `crates/strider-py/tests/python/test_optimizer_pipeline.py:197` | `"""Round 9 H-IMP I-5 (R9-1F-03) regression:` | drop |
| `crates/strider-py/tests/python/test_pattern_full_coverage.py:259,273` | `G9 (Round-7 follow-up):` and `intentional G9 test exception` | drop G9 |
| `crates/strider-py/tests/python/test_sleigh.py:45,49,80` | `(round8-1F HIGH / MED)`, `Regression for round8-1F HIGH:`, `Regression for round8-1F MED:` | drop round8 prefixes |
| `crates/strider-py/tests/python/test_pattern_match.py:56,60,86,118,122,148,167` | `── Round 8 regression tests ──`, `round8-1F MED`, `round8-correctness-borrowing HIGH`, `── Round 9 H-8 regression: KeyboardInterrupt …`, `Round 9 H-8: …`, `Round 9 H-8 companion:` | drop all round/H tags; describe the regression in one short sentence each |
| `crates/strider-py/tests/python/test_pattern_full_builders.py:351` | `Round 9 D19 (R9-1F-01): the previous allowed set …` | drop |
| `crates/strider-py/tests/python/test_typed_errors_e2e.py:152,167,199,234` | `round 10 R10-1F F-05`, `Round 9 wave 25 (I-10): …` (×3) | drop |

### Doc-line formatting bugs

Four sites where a `/// (R9-…):` continuation got concatenated onto the
previous prose-line because of a missing newline.  The result is that the
short summary visible in `cargo doc` no longer ends after the first sentence —
the audit-tag continuation runs into the abstract.  Fix is mechanical:
break the line.

- `crates/cfg/src/cfg/options.rs:9` — `Function-extent boundary for tail-call classification.  /// (R9-2D M3): the previous` (the `(R9-2D M3)` should drop entirely; secondarily, the line break is missing)
- `crates/cfg/src/cfg/options.rs:99` — `(fn_max_size, allow_code_before_start_addr).      /// (R9-2D M3): canonical accessor`
- `crates/cfg/src/cfg/types.rs:79` — `Read the parent machine instruction's address.      /// (R9-2D H2): canonical accessor`
- `crates/ir/src/builder/mod.rs:410` — `restore via the inner guard's Drop impl.      /// (R9-1A I3) closed`

## Half-rename leftovers

| Old name | New name | Site | Line |
|----------|----------|------|------|
| `fg: RewriteCtxView<'_>` (parameter conventions inherited from when this was `FunctionGraph`) | `ctx` or `fn_view` | `crates/opt/src/test_support.rs` | 24, 33, 39, 48 |
| `fg: RewriteCtxView<'_>` (32 sites) | `ctx` | `crates/opt/src/dead_branch/mod.rs`, `flag_cmp_canonicalize/tests.rs`, `function_args/mod.rs`, `function_args/tests.rs`, `indirect_branch_resolve/{jump_table,classify,stack_array}.rs`, `known_bits/mod.rs`, `stack_load_forward/tests.rs` | passim |
| `fg: &mut pattern::RewriteCtx<'_>` (10 sites) | `ctx: &mut pattern::RewriteCtx<'_>` | `crates/opt/src/constant_fold/{mod,rules}.rs`, `dead_branch/mod.rs`, `function_args/mod.rs`, `indirect_branch_resolve/inplace.rs` | passim |
| `function: &mut pattern::RewriteCtx<'_>` (parameter on `OptimizerOnBuilt::optimize_built` impls) — the `RewriteCtx` is **not** a function, it's a graph + entry pair | `ctx: &mut RewriteCtx<'_>` or `g: &mut RewriteCtx<'_>` | `crates/opt/src/{constant_fold,dead_branch,flag_cmp_canonicalize,function_args,if_cond_inversion,known_bits,load_readonly}/mod.rs` and others | passim |
| `OptimizerOnBuilt` trait — name implies "operates on `BuiltFunctionGraph`", but the trait now takes `&mut RewriteCtx<'_>`.  The "OnBuilt" suffix is a leftover from before the migration. | `OptimizerOnRewriteCtx` or just collapse into `Optimizer` (the blanket impl is now the only reason for the split) | `crates/opt/src/pipeline.rs:141` (definition) + ~17 impls | 141, 154, 289, 329 |
| `optimize_built` trait method on `OptimizerOnBuilt` — same reasoning | `optimize` (after collapsing the trait) or `optimize_with_ctx` | `crates/opt/src/pipeline.rs:148` + ~17 impls | 148 |
| `pipeline::run_on_built` | `run_on_built` is now the only entry point — the `_on_built` suffix has lost meaning | `crates/opt/src/pipeline.rs:297` | 297 |
| `bfg` local variable (still used in tests) — once the type is `BuiltFunctionGraph`, the abbreviation is opaque to a new reader | `built` or `function_graph` | `crates/ir/src/function.rs:301-311`, `builder/tests.rs:1731-1734`, `crates/opt/tests/wide_const_passthrough.rs:27-62` | passim |
| `body()` / `body_mut()` accessor returning `FunctionGraph` while the field is named `function: FunctionGraph` | rename accessor to `function()` / `function_mut()` (or rename field to `body`) | `crates/ir/src/builder/mod.rs:152, 157` | 152, 157 |
| `from_graph_and_entry_for_rewrite` — already `#[deprecated]` and `#[doc(hidden)]`; the verbosity is a half-rename trail | document its replacement (`pattern::RewriteCtx::new`) and remove if not actually needed by the small set of pattern test scaffolds; the name itself is fine but the four `#[allow(deprecated)]` sites in `crates/pattern/tests/` are the real rename leftover | `crates/ir/src/function.rs:204` | 204 |
| `Builder::new` (`cfg::Builder::new`) — already `#[deprecated]` but 9 test files still need `#![allow(deprecated)]` to use the older default | replace test-side calls with `Builder::for_arch(…)` and remove the workspace-wide `#![allow(deprecated)]` headers | 9 test files (`crates/cfg/tests/*.rs`, `crates/strider/{examples,tests}/*.rs`, `crates/strider-py/src/cfg.rs`) | passim |

## Unclear names

| Site | Current | Proposed | Why |
|------|---------|----------|-----|
| `crates/ir/src/graph/access.rs:20` | `output_kind(NodeOutputId)` | (keep) | baseline for the family — the family below is inconsistent with this one |
| `crates/ir/src/graph/access.rs:158` | `kind_of_output(NodeOutputId) -> &NodeKind` | `node_kind_of_output` (or just inline at call sites) | sibling of `output_kind` (returns `NodeOutputKind`) and `node_kind(NodeId)` (returns `&NodeKind`) — three names, three different shapes |
| `crates/ir/src/graph/access.rs:100` | `get_node_from_output(NodeOutputId) -> NodeId` | `node_of_output` or `defining_node` | discouraged `get_` prefix and verbose; the comment even calls the typical use a "shorthand for `node_kind(get_node_from_output(_))`" |
| `crates/ir/src/graph/access.rs:136` | `memory_output_of(NodeId) -> NodeOutputId` | (keep) | this one matches the rest of the family well |
| `crates/ir/src/graph/access.rs:183` | `input_output_id(NodeInputId) -> NodeOutputId` | `referenced_output(input)` or `output_of_input(input)` | reads as "input → output" but really means "the output that the input slot references"; ambiguous |
| `crates/ir/src/graph/access.rs:168` | `node_input_id_at(NodeId, idx)` | `input_id(node, idx)` or `input_at(node, idx)` | "node_input_id_at" packs three nouns + a positional adverb; reads awkwardly |
| `crates/ir/src/builder/coerce.rs:18,33,77,101,119,135` | `get_output_type`, `get_as_bool`, `get_as_unsigned_int`, `get_as_signed_int`, `get_as_int`, `get_as_float_bits` | drop `get_` prefix per Rust API conventions | `get_` is discouraged for plain accessors; cluster of 6 in one file |
| `crates/ir/src/ops/consts.rs:82` | `make_int_const(val, ty)` | `build_int_const_immediate` or unify with `build_int_const` | duplicate of `crates/ir/src/builder/nodes.rs:86 build_int_const`; doc on `make_int_const` even references `build_int_const` for masking parity |
| `crates/opt/src/indirect_branch_resolve/mod.rs:232` | `AnchorAddr::from_packed(machine_addr, insn_index)` | `AnchorAddr::new(machine_addr, insn_index)` | the doc at line 220 says "we use a packed pair rather than `Box<dyn Any>`" but no actual packing happens — `from_packed` mis-suggests a u128 or bit-packed-u64 input |
| `crates/strider/src/strider/mod.rs:14` + `crates/strider/src/strider/pipeline.rs:131` | `IrStrider` (per-function context) and `Strider` (per-arch handle) co-existing | rename `IrStrider` → `FunctionLifter` or `StriderLifter` | confusing naming: `IrStrider` is the per-function lifter; `Strider` is the cross-function arch handle — calling the inner type "Ir-prefixed" while the outer is the bare name reads backwards |
| `crates/strider/src/rewrite.rs:74` | `GraphRewriter::wrap_built(&mut BuiltFunctionGraph)` | `GraphRewriter::for_built` (matches the existing `RewriteCtx::for_built` constructor in the same dependency chain) | inconsistent with `pattern::RewriteCtx::for_built` — sibling concept, different verb |
| `crates/strider/src/strider/pipeline.rs:499-575` | `enum SpecialTerm { … }` and `SpecialTerm::skips_opcode` | rename to `LiftSpecialTerm` or move into `crates/strider/src/strider/insn/` and rename to `RegionTerminationKind` (ratio of "what's lifted vs CFG terminator") | comment in `crates/cfg/src/cfg/builder/region_builder.rs:341` references `SpecialTerm::TailCall::skips_opcode` cross-crate, but `SpecialTerm` lives in strider not cfg — cfg can't see strider; the cross-crate citation is misleading |
| `crates/pattern/src/pat/any.rs:24` | `pub struct VarPat` (and free ctor `var(c: Capture)` at `crates/pattern/src/pat/ctor/wildcards.rs:25`) | rename to `CaptureAnyPat` and `capture_any` | the only capture type in the pattern crate is `Capture`; `Var` is leftover from the older Var/NodeVar split — coexists confusingly with the existing `CapturePat` (which wraps an inner pattern) |
| `crates/pattern/tests/matching/support/graph.rs:24` | `pub struct Tb { fb: FunctionBuilder }` | `pub struct GraphBuilder` or `TestBuilder` | single-letter type name — even the doc comment expands to "Test graph builder"; the tests would read better with `GraphBuilder::empty()` than `Tb::empty()` |
| `crates/pattern/tests/matching/support/graph.rs:306-310` | `Tb::as_bool(v)`, `Tb::as_int(v, ty)` | `convert_to_bool(v)` / `convert_to_int(v, ty)` | Rust convention is that `as_X` is a cheap reference-typed cast; these methods *build IR nodes* (delegate to `convert_to_bool_if_needed`) — different cost model |
| `crates/opt/src/stack_load_forward/tests.rs:523` | `let cc = b.build_int_const(0xCCu64, NodeOutputType::U32)?;` | `let sentinel = b.build_int_const(0xCCu64, …)?;` or `let cc_byte` | this file uses `cc` for "calling convention" elsewhere (and across the workspace `cc` ⇒ CC); reusing `cc` for the literal value `0xCC` collides |
| `crates/cfg/src/cfg/builder/mod.rs:122` (`with_endianness`) — already `#[deprecated]` but the deprecation note doesn't make the issue obvious | rename in deprecation note to `Builder::new` (which is what's actually deprecated; `with_endianness` itself just wraps `new`'s legacy default) | the deprecation chain has two surfaces (`new` + `with_endianness`) — the cross-link in CLAUDE.md says "the orchestrator delivers the preset via `cfg::Builder::for_arch`", but `with_endianness` shows up in lots of test files and is also deprecated | `crates/cfg/src/cfg/builder/mod.rs` | 98, 117 |
| `crates/strider/src/indirect_resolve/classify.rs:20,36,55` | `classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp` | collapse to a single `classify_anchor(opts: ClassifyAnchorOptions)` or rename to `classify_anchor_basic` / `classify_anchor_with_rom` / `classify_anchor_full` | the three variants form an O(N²) name explosion as parameters get added; an options struct (or builder) would scale better |
| `crates/reader/src/elf.rs:476,670,751` | `apply_elf_relocations`, `apply_elf_relocations_with_extender`, `apply_elf_relocations_autoload` | `apply_elf_relocations`, `apply_elf_relocations_with(extender)`, `apply_elf_relocations_autoloading` | one suffix names the *parameter* (`_with_extender`), another names the *behavior* (`_autoload` — really a noun used as adjective).  Mixed noun-adjective convention |
| `crates/ir/src/wide_const.rs:72,86` | `from_le_bytes_u256`, `from_le_bytes_u512` | `from_le_bytes_256` and `from_le_bytes_512`, or move bit-width into a generic `<const N: usize>` parameter | `_u256` / `_u512` is fine but inconsistent with std's `u32::from_le_bytes` (which doesn't repeat the type in the method name) |
| `crates/cfg/src/cfg/types.rs:46` | `MachineInsnAddr::as_u64()` (with doc "canonical accessor for the migration path that will eventually tighten the `addr` field to `pub(crate)`") | accept the migration-pending status: rename `addr` field to `_addr` or just commit to the migration | the doc-line "canonical accessor" reads as half-finished work |
| `crates/cfg/src/cfg/types.rs:97` | `PcodeInsnAddr::machine_addr_u64()` (next to `machine_addr()` and `insn_index()`) | `parent_addr_u64` or unify with `.machine_addr().as_u64()` at call sites | three accessor variants on a 2-field struct |
| `crates/ir/src/builder/coerce.rs:152, 177, 49, 226` | `truncate_if_needed`, `extend_if_needed`, `convert_to_bool_if_needed`, `convert_to_int_if_needed` | drop the `_if_needed` suffix or make the no-op behavior explicit (`truncate_or_pass`, `narrow_to`) | `_if_needed` reads like English filler; the actual semantic difference vs the unconditional builders deserves a clearer name |
| `crates/strider/benches/scaling.rs:204,267` | `pub fn run_diamond_cfg(n: usize)`, `pub fn run_jump_table_scenario(n: usize)` | `build_diamond_cfg`, `build_jump_table_scenario` | these don't run anything — they construct test fixtures.  Compare with the same file's `build_pushes(n)` etc. which are correctly named |
| `crates/strider/src/rewrite_tests.rs:43,68` | `add_x_plus_zero`, `sub_of_two_add_zeros` | `function_with_add_x_plus_zero`, `function_with_two_add_zero_subtraction` | these are test-fixture function-graph constructors but read like operations |

## Test-name clarity

| Site | Current name | Suggested name | Why |
|------|--------------|----------------|-----|
| `crates/graphwalk/tests/graphmock_dsl.rs:13` | `simple_graph()` | `parses_linear_chain` | the test parses the DSL and checks no panic; "simple_graph" describes the *fixture*, not the assertion |
| `crates/graphwalk/tests/graphmock_dsl.rs:24` | `diamond()` | `parses_diamond_topology` | same — describes fixture not assertion |
| `crates/graphwalk/tests/graphmock_dsl.rs:34` | `loop_graph()` | `parses_self_referential_cycle` | same |
| `crates/entity-utils/src/worklist.rs:210` | `debug_format_smoke()` | `debug_format_includes_remaining_count` (or whatever invariant the test pins) | "smoke" tells the reader nothing |
| `crates/ir/src/node_signature.rs:662` | `smoke_vn()` (a helper) + nearby tests | rename helper to `dummy_vn` / `placeholder_vn`, or just `vn_zero` since it constructs a zero-offset register vn | "smoke" suggests a smoke test, but the function is a helper |
| `crates/opt/src/sp_expr.rs:460` | `ranges_disjoint_basic()` | `ranges_disjoint_returns_true_for_non_overlapping` | "_basic" is just filler; the assertion is well-defined |
| `crates/opt/src/stack_load_forward/tests.rs:98` | `forward_basic()` | `forward_load_after_matching_store_returns_stored_value` | "_basic" doesn't describe what's being checked |
| `crates/pattern/tests/matching/matcher_api.rs:13` | `new_on_empty_graph_does_not_panic()` | (keep — clear) | listed as a positive example for contrast |
| `crates/strider/tests/graph_rewriter.rs:230` | `replace_input_then_reoptimize_then_replace_again_works()` | `replace_input_reoptimize_replace_again_preserves_validity` | "_works" is filler; the actual invariant is that re-running keeps the graph valid |
| `crates/reader/tests/elf_reader.rs:28` | `simple_text_elf_fixture_round_trips_through_elf_reader()` | `text_elf_round_trips_through_reader` | drop "fixture"; "simple" is filler.  The "Round" in "round_trips" is fine — it's a verb (round-trip), unambiguous |
