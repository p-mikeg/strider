# Round 12 — 2B: naming sweep

## Summary

Workspace-wide sweep on branch `review/ai6` (forked from `008f530`).
Compared to round 11, breadcrumbs have been **substantially reduced**
(the largest mass-cleanup target — the `fg: RewriteCtxView` parameter
convention in the `opt` crate — is now done; `OptimizerOnBuilt` is
collapsed; `MachineInsnAddr::as_u64` and `PcodeInsnAddr::machine_addr*`
no longer claim to be on a "migration path"; the `R9-*` doc-line
formatting bugs are fixed; the `simple_graph()` / `diamond()` /
`loop_graph()` tests were renamed; `Builder::new` deprecation no
longer requires `#![allow(deprecated)]` everywhere).

What's left is a smaller, more focused residue: a handful of round-8/9
regression-test docstrings still cite review-document paths or `H-N` /
`D-N` audit IDs whose context has long expired; the `_v1` / `_v2`
ELF-ABI names are intentional domain names, not breadcrumbs; round 11
introduced a fresh wave of `T-NN (round 11):` comment prefixes (six
sites) which are themselves the next breadcrumb generation; `bfg`,
`Tb`, `VarPat`, three `classify_anchor*` variants, three
`apply_elf_relocations*` variants, the `get_*` and `_if_needed`
accessor families, the `make_int_const` vs `build_int_const` doublet,
the `wrap_built` vs `for_built` verb-mismatch, and the
`SpecialTerm`/`IrStrider` naming all survive unchanged from round 11.

| Category | Count |
|----------|-------|
| Migration-breadcrumb hits in Rust source (incl. `T-NN (round 11)`) | 21 |
| Migration-breadcrumb hits in Python tests | 13 |
| Mid-round audit-ID labels (H-N / D-N / I-N / G-N / Slice N / Task N / Phase N / Audit IN / Pre-H0 / round*-tag) | ~25 |
| `tier-2` mentions in cfg + opt README | 3 |
| Half-rename leftovers (parameter-name `function` vs `ctx`; `wrap_built` vs `for_built`; `body()` vs field `function`; `bfg`) | ~50 sites total |
| Misleading constructor / type / accessor names | 9 |
| Inconsistent sibling-accessor families | 5 |
| Test-name clarity issues | 4 |

## Migration-breadcrumb hits (Rust source)

The first six are **new in round 11**; the rest are stale residue from rounds 7-10.

| File:line | Breadcrumb | Suggested rewrite |
|-----------|------------|-------------------|
| `crates/entity-utils/src/worklist.rs:217` | `/// T-23 (round 11): enqueue deduplicates …` | drop "T-23 (round 11):" — describe assertion only |
| `crates/strider/tests/optimizer_pipeline_subsets.rs:142` | `// ── T-17 (round 11): default pipeline composition ──` | drop "T-17 (round 11):" — keep the section header |
| `crates/ir/src/node/output_type.rs:443` | `/// T-14 (round 11): bit_mask_u128 for U256/U512 …` | drop tag |
| `crates/ir/src/node/output_type.rs:452` | `/// T-14 (round 11): get_unsigned_int for U256/U512 …` | drop tag |
| `crates/opt/src/known_bits/tests.rs:469` | `// ── T-25 (round 11): Kb constructor invariant ──` | drop tag |
| `crates/target/tests/cc_validation.rs:4` | `//! T-16 from round10-test-plan.md: pin that listing the SP varnode …` | drop both `T-16` and the cross-doc citation |
| `crates/target/src/call_other_abi.rs:822` | `/// Regression for round8-17 D-1: x86/x86_64 memory fences …` | "Regression: x86/x86_64 memory fences …" |
| `crates/ir/tests/builder_extended_use.rs:43,45,51` | `entry() must be stable after round 1`, `Round 2: another mutation - round 1's node`, `entry() must be stable after round 2` | rename to `Step 1` / `Step 2` — "Round" overloads with audit-round terminology |
| `crates/strider-py/tests/python/test_sleigh.py:45,49,80` | `Hash/eq contract regression tests (round8-1F HIGH / MED)`, `Regression for round8-1F HIGH:`, `Regression for round8-1F MED:` | drop round8 prefixes |
| `crates/strider/tests/common/mod.rs:219` | `// See round8-correctness-cross-arch §1.` | drop the doc-path citation |
| `crates/strider-py/tests/python/test_pattern_match.py:56,60,86,118,122,148,167` | `── Round 8 regression tests ──`, `round8-1F MED`, `round8-correctness-borrowing HIGH`, `── Round 9 H-8 regression …`, `Round 9 H-8: …`, `Round 9 H-8 companion:` | drop all round/H tags |
| `crates/strider-py/tests/python/test_typed_errors_e2e.py:164,196,231` | `Round 9 wave 25 (I-10): …` (×3) | drop |
| `crates/strider-py/tests/python/test_optimizer_pipeline.py:13` | `G7 (Round-7 follow-up): pin Python's manually-listed default` | drop "G7 (Round-7 follow-up):" |
| `crates/strider-py/tests/python/test_pattern_full_coverage.py:259,273` | `G9 (Round-7 follow-up):` and `intentional G9 test exception` | drop "G9 …" |
| `crates/ir/src/builder/tests.rs:1529` | `// FB-3 type-design: pin lift_at's restore-on-exit contract.` | drop "FB-3 type-design:" |
| `crates/ir/src/builder/tests.rs:1612` | `/// Regression for round8-1A HIGH: build_int_const and make_int_const …` | "Regression: build_int_const and make_int_const …" |
| `crates/opt/src/indirect_branch_resolve/inplace.rs:414` | `/// Regression for round8-2C H4: apply_tail_call must propagate …` | drop |
| `crates/opt/src/sp_expr.rs:865,896` | `Regression for round8-correctness-edge-cases H1`, `H2` | drop |
| `crates/opt/src/if_cond_inversion/tests.rs:167` | `Regression for round8-correctness-invariants H-2:` | drop |
| `crates/strider/benches/scaling.rs:92` | `// round8-correctness-cross-arch §1.` | drop |
| `crates/cfg/src/cfg/builder/indirect_resolve.rs:191` | `// (Task 15).` (no other context) | delete or expand to what Task 15 was |
| `crates/strider/tests/common_smoke.rs:18` | `convention (Task 16) and the BE shift formula fix (Task 4).` | drop both |
| `crates/strider/tests/per_address_cc.rs:62` | `Pre-Task-9 (no per-address CC)` | "before per-address CC was added" |
| `crates/strider/tests/indirect_resolve_classify.rs:283` | `/// Round 10 T-3 regression: calling classify_anchor twice …` | drop "Round 10 T-3" |
| `crates/strider/src/test_utils.rs:13` | `//! reviews/round8-repetition-sweep.md (#5) — 18 sites duplicated …` | drop the reviews-path citation |
| `crates/strider/src/orchestrator.rs:814` | `// (same reasoning as H-4 above).` | **dangling reference** — there is no H-4 anchor anywhere in this file |
| `crates/strider/src/orchestrator.rs:1068` | `// These tests pin the wave-2 fix (>= → >) …` | drop "wave-2" |
| `crates/opt/src/test_support.rs:10` | `//! reviews/round8-repetition-sweep.md (#1) — …` | drop |
| `crates/opt/src/stack_load_forward/tests.rs:28` | `// see reviews/round8-repetition-sweep.md (#1).` | drop |
| `crates/opt/src/stack_store/tests.rs:586` | `// ── Comprehensive tests added in Task 5 ──` | "── Comprehensive tests ──" |
| `crates/opt/src/constant_fold/tests.rs:1428` | `// ── Comprehensive tests added in Task 2.E ──` | drop "Task 2.E" |
| `crates/opt/src/function_args/mod.rs:220` | `Slice 4 extends this with memory-shadow disqualification; slice 5 extends …` | rename to feature names |
| `crates/opt/src/function_args/tests.rs:19,76,141,207,241,321,664` | `Slice 1` … `Slice 5 (audit I2)` … `Slice 4 (audit B2 blocker)` | replace with actual test concern |
| `crates/opt/src/function_args/tests.rs:394` | `Audit I4:` | drop |
| `crates/opt/src/indirect_branch_resolve/jump_table_tests.rs:935` | `// CORRECTNESS — H2: idx == N taken-true …` | drop "H2:" prefix |
| `crates/opt/src/known_bits/tests.rs:416` | `// KnownBits Phase 2 first folds the Or to IntConst; …` | implementation references "Phase 2" — see KnownBits internal-phase note below |
| `crates/opt/src/known_bits/mod.rs:468,473` | `// Phase 1 — propagate known bits to fixed point.`, `// Phase 2 — replace fully-determined outputs …` | this is **valid internal-phase naming** (algorithm has two phases) — not a breadcrumb; flagged only because the test comment at line 416 cross-references "Phase 2" without a forward link |
| `crates/pcode-lift/src/vn_io.rs:217,307` | `// Phase 1: sub-register reads within a wide (>16-byte) container …`, `// Phase 1 wide-container guard …` | drop "Phase 1" — there is no "Phase 2" anywhere |
| `crates/opt/tests/wide_const_passthrough.rs:1,3` | `//! Wide-const Phase-1 passthrough …`, `Wide arithmetic folding is deferred to Phase 2 …` | drop Phase-1 / Phase 2 (no Phase 2 work tracked here) |
| `crates/pattern/tests/matching/matcher_api.rs:239` | `Phases 2/3/4 implement the actual walk-through; Phase 1 only adds the API surface` | drop Phase 1/2/3/4 — feature has shipped |
| `crates/pattern/tests/matching/if_pat_symmetric.rs:4,72` | `Before Phase 6, IfPat itself tried two layouts (direct + inverted).`, `semantics here are unchanged by Phase 6.` | drop "Phase 6" — the migration is done |
| `crates/strider/tests/common/mod.rs:198` | `// Phase 5: thread the calling-convention link-register varnode …` | drop "Phase 5" |
| `crates/strider/benches/scaling.rs:162,174` | `// Phase 1: N stores at distinct SP offsets.`, `// Phase 2: N loads at the same offsets.` | valid internal-step naming inside a bench fixture — borderline, but recommend renaming to "Step 1" / "Step 2" to avoid the same "Phase" overload |
| `crates/reader/tests/elf_relocations.rs:93` | `// + dynsym + dynstr + rela.dyn + program headers.  The wave-1 M3 fix …` | drop "wave-1 M3" |
| `crates/reader/tests/elf_converters.rs:277` | `// ── Task 1: a FILTER-REJECTED malformed section is silent, not an error ──` | drop "Task 1:" |
| `crates/reader/tests/mem_region.rs:283` | `it's only meaningful once Task 1 privatizes them.` | drop |
| `crates/strider/tests/indirect_resolve_in_place_edits.rs:227,236,241,243` | `// ── H0: ABI threading for in-place tail-call resolution ──`, `**Pre-H0:**`, `**Post-H0:**`, `H0: pattern queries …` | drop "H0" — describe what the test pins instead |
| `crates/strider-py/tests/python/test_mem_reader_kbd_interrupt.py:1` | `"""Regression for H-2: a Python MemReader.read that raises …` | drop "H-2:" |

## `tier-2` references in README files

| File:line | Current | Suggested rewrite |
|-----------|---------|-------------------|
| `crates/cfg/README.md:45` | "the strider orchestrator's **tier-2** fixed-point loop rewrites …" | "the strider orchestrator's **indirect-branch** fixed-point loop rewrites …" |
| `crates/cfg/README.md:82` | "`Cfg::sleigh` is reused across the strider **tier-2** fixed-point loop." | "across the strider **indirect-branch** fixed-point loop" |
| `crates/opt/README.md:66` | "The strider **tier-2** fixed-point splits passes into stable vs destructive." | "The strider **indirect-branch** fixed-point splits passes …" |

(CLAUDE.md itself describes this loop as "indirect-branch fixed-point" everywhere; the READMEs are the only sites using the older "tier-2" framing.)

## `target/README.md` round-7/8/9 history note

`crates/target/README.md:119-120`:
```
- `x86_64_systemv` was renamed from `x86_64_systemv_abi` in round 8;
  the deprecated alias was deleted in round 9 phase C. Use
```
The "round 8" / "round 9 phase C" callouts are now historical noise — the rename happened so long ago that any reader who needs to know the old name will be reading commit history, not the README. Suggest: "`x86_64_systemv` was previously named `x86_64_systemv_abi` (alias since removed). Use …".

## Half-rename leftovers

### Mixed `ctx` / `function` parameter naming on `Optimizer::optimize`

`Optimizer::optimize(&self, _: &mut pattern::RewriteCtx<'_>)` impls
are split between two conventions for the parameter name. The trait
definition at `crates/opt/src/pipeline.rs:148` uses `ctx`:

```rust
fn optimize(
    &self,
    ctx: &mut pattern::RewriteCtx<'_>,
) -> crate::Result<OptimizationResult>;
```

But four of the ten passes still use `function:` (a stale name from
when the parameter was a `FunctionGraph`):

| File:line | Parameter |
|-----------|-----------|
| `crates/opt/src/constant_fold/mod.rs:62` | `function: &mut pattern::RewriteCtx<'_>` |
| `crates/opt/src/dead_branch/mod.rs:256` | `ctx: …` ✓ |
| `crates/opt/src/flag_cmp_canonicalize/mod.rs:69` | `function: …` |
| `crates/opt/src/function_args/mod.rs:92` | `ctx: …` ✓ |
| `crates/opt/src/if_cond_inversion/mod.rs:58` | `function: …` |
| `crates/opt/src/known_bits/mod.rs:467` | `function: …` |
| `crates/opt/src/load_readonly/mod.rs:50` | `function: …` |
| `crates/opt/src/redundant_phis/mod.rs:172` | `function: …` |
| `crates/opt/src/stack_load_forward/mod.rs:61` | `ctx: …` ✓ |
| `crates/opt/src/stack_store/call_args.rs:301` | `ctx: …` ✓ |
| `crates/opt/src/stack_store/detect.rs:110` | `function: …` |
| `crates/opt/src/flag_cmp_canonicalize/mod.rs:115` | `fn try_apply_rule(function: &mut pattern::RewriteCtx<'_>, …)` |
| `crates/opt/src/known_bits/mod.rs:427` | `pub fn analyze(function: pattern::RewriteCtxView<'_>) …` |
| `crates/opt/src/worklist.rs:49` | `pub(crate) fn seeded_kind<P>(function: &pattern::RewriteCtx<'_>, …)` |

Recommendation: rename all `function: &mut pattern::RewriteCtx<'_>`
parameters to `ctx` to match the trait signature and the cleaned-up
passes.

### `wrap_built` vs `for_built` verb mismatch

| Site | Verb | Type |
|------|------|------|
| `crates/strider/src/rewrite.rs:74` | `GraphRewriter::wrap_built` | constructor accepting `&mut BuiltFunctionGraph` |
| `crates/pattern/src/rewrite.rs:178` | `RewriteCtx::for_built` | constructor accepting `&mut BuiltFunctionGraph` |

Sibling concepts, different verbs.  Pick one (`for_built` matches the
`for_arch` convention in `cfg::Builder::for_arch`) and rename the
other.  Used at ~17 call sites across strider + strider-py.

### `body()` / `body_mut()` accessors vs field name `function`

`crates/ir/src/builder/mod.rs:102` declares:
```rust
pub struct FunctionBuilder {
    pub(crate) function: FunctionGraph,
    …
}
```

But the accessors at `:152, :157` return `&FunctionGraph` from a
field named `function` via methods named `body()` / `body_mut()`:

```rust
pub fn body(&self) -> &FunctionGraph { &self.function }
pub fn body_mut(&mut self) -> &mut FunctionGraph { &mut self.function }
```

A new reader reading `b.body_mut()` cannot infer the type from the
name.  Rename either the accessor (to `function()` / `function_mut()`)
or the field (to `body`).  Recommend renaming the accessor.

### `bfg` local variable (test sites)

| File:line range | Count | Suggested |
|------------------|-------|-----------|
| `crates/opt/tests/wide_const_passthrough.rs:27, 29, 32, 37, 40, 43, 51, 54, 56, 59, 62, 83, 87, 89` | 14 sites | `let mut built` or `let mut function_graph` |
| `crates/strider/tests/per_address_cc.rs` + `per_address_cc_indirect.rs` (~16 sites total) | 16+ | same |
| `crates/strider/tests/optimizer_pipeline_subsets.rs` (several) | several | same |

`bfg` ("built function graph") survives entirely in test files now;
no production-side `bfg` use remains. Borderline, but inconsistent
with the `built` / `built_fn` style used in `crates/strider/src/rewrite_tests.rs`.

### `_v1` / `_v2` are intentional ABI names

The 12 `powerpc64_elf_v1` / `powerpc64_elf_v2` occurrences in
`crates/target/src/calling_convention/{mod,tests}.rs` and the Python
binding mirror are **real domain names** (the PowerPC ELFv1 / ELFv2
ABIs) — not migration breadcrumbs.  Listed here only so a future
sweep doesn't mistakenly try to "consolidate" them.

Similarly, `cfg_v1` / `cfg_v2` / `sp_v1` / `sp_v2` are SSA-renaming
naming in test bodies (e.g. `crates/cfg/tests/known_targets.rs`,
`crates/opt/tests/pipeline_with_stack.rs`) — they denote successive
SSA versions of the same logical entity, which is standard SSA
practice.  Not breadcrumbs.

### `Optimizer` vs `OptimizerRaw` trait split (post-W15 status)

The W15 trait collapse intended by round-11 W15 actually produced two
traits (`Optimizer` + `OptimizerRaw`) in `crates/opt/src/pipeline.rs`,
not one.  The doc-comment chain at lines 88-141 explains the
rationale clearly: `OptimizerRaw` is the dispatch trait that holds
`Box<dyn …>`, and `Optimizer` is the ergonomic trait every pass
should implement.  This is reasonable as-is — the naming is no longer
a breadcrumb but a deliberate split.  ✓ clean.

`OptimizerOnBuilt` itself is fully gone; no traces in source.

## Unclear / misleading names

| Site | Current | Proposed | Why |
|------|---------|----------|-----|
| `crates/ir/src/graph/access.rs:20,100,136,158,168,183` | family `output_kind` / `kind_of_output` / `node_kind` / `get_node_from_output` / `memory_output_of` / `input_output_id` / `node_input_id_at` | unify on one of the following grammars: `kind_of_output` + `kind_of_node` + `node_of_output` + `output_of_input`, OR `output_kind` + `node_kind` + `defining_node` + `referenced_output` | seven accessors, three different grammars. `get_` is discouraged in Rust API guidelines |
| `crates/ir/src/builder/coerce.rs:18,33,77,101,119,135` | `get_output_type` / `get_as_bool` / `get_as_unsigned_int` / `get_as_signed_int` / `get_as_int` / `get_as_float_bits` | drop `get_` prefix | discouraged by Rust API guidelines; cluster of six in one file |
| `crates/ir/src/builder/coerce.rs:49,152,177,226` | `convert_to_bool_if_needed` / `truncate_if_needed` / `extend_if_needed` / `convert_to_int_if_needed` | drop `_if_needed` suffix; mention idempotency in the doc-comment | `_if_needed` is English filler |
| `crates/ir/src/ops/consts.rs:82` | `make_int_const(val, ty)` | `build_int_const_immediate` or unify with `build_int_const` | duplicate of `crates/ir/src/builder/nodes.rs:86 build_int_const`; the doc on `make_int_const` even references `build_int_const` for masking parity |
| `crates/strider/src/strider/pipeline.rs:523,547,557,565` | `enum SpecialTerm { … }` and `SpecialTerm::skips_opcode` | rename to `RegionTerminationKind` or `LiftSpecialTerm` and move into `crates/strider/src/strider/insn/` | comment in `crates/cfg/src/cfg/builder/region_builder.rs:341` references `SpecialTerm::TailCall::skips_opcode` cross-crate, but `SpecialTerm` lives in strider, not cfg — cfg can't see strider; the cross-crate citation is misleading |
| `crates/strider/src/strider/mod.rs:14` + `crates/strider/src/strider/pipeline.rs:131` | `IrStrider` (per-function context) and `Strider` (per-arch handle) co-existing | rename `IrStrider` → `FunctionLifter` or `StriderRegionDriver` | confusing naming: the **inner** per-function type carries the "Ir-" prefix while the **outer** per-arch type is the bare name — backwards from the usual convention |
| `crates/pattern/src/pat/any.rs:24` + `crates/pattern/src/pat/ctor/wildcards.rs:25` | `pub struct VarPat` and ctor `var(c: Capture)` | rename to `CaptureAnyPat` / `capture_any` | the only capture type in the pattern crate is `Capture`; `Var` is leftover from the older Var/NodeVar split |
| `crates/pattern/tests/matching/support/graph.rs:24` | `pub struct Tb { fb: FunctionBuilder }` (used at 40+ call sites as `Tb::empty()`) | rename to `GraphBuilder` / `TestBuilder` | single-letter type name; the doc-comment even expands to "test graph builder" |
| `crates/pattern/tests/matching/support/graph.rs:306-310` | `Tb::as_bool(v)`, `Tb::as_int(v, ty)` | `Tb::convert_to_bool(v)` / `Tb::convert_to_int(v, ty)` | Rust convention is that `as_X` is a cheap reference-typed cast; these methods build IR nodes |
| `crates/strider/src/indirect_resolve/classify.rs:20,36,55` | `classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp` | introduce a `ClassifyAnchorOptions { link_register, rom, stack_ptr }` struct and collapse to one `classify_anchor(graph, anchor, opts)` | the three-variant API has O(N²) growth as parameters get added |
| `crates/reader/src/elf.rs:476,682,768` | `apply_elf_relocations`, `apply_elf_relocations_with_extender`, `apply_elf_relocations_autoload` | suffix `with_extender` names a *parameter*; `autoload` names a *behavior*. Pick one convention. Suggest: `apply_elf_relocations(regions, &obj)`, `apply_elf_relocations_with(regions, &obj, extender)`, `apply_elf_relocations_autoloading(regions, &obj)`. | mixed noun-as-suffix vs verb-as-suffix |
| `crates/strider/benches/scaling.rs:204,267` | `pub fn run_diamond_cfg(n)`, `pub fn run_jump_table_scenario(n)` | `build_diamond_cfg`, `build_jump_table_scenario` | these don't run anything — they construct test fixtures. Same file's `build_pushes(n)` etc. is named correctly |
| `crates/strider/src/rewrite_tests.rs:43,68` | `add_x_plus_zero(x: u64) -> BuiltFunctionGraph`, `sub_of_two_add_zeros(a, b)` | `function_returning_x_plus_zero`, `function_with_two_zero_addends_subtracted` | these are test-fixture constructors returning a function graph; the names read like operations |
| `crates/ir/src/node_signature.rs:699` | helper `fn smoke_vn() -> rsleigh::Vn` | `dummy_vn` or `placeholder_vn` or `zero_offset_vn` | "smoke" suggests a smoke test, but the function is a helper |
| `crates/cfg/src/cfg/types.rs:46,123` | doc-comments on `MachineInsnAddr::as_u64` and `PcodeInsnAddr::machine_addr_u64` still say "canonical accessor for the migration path that will eventually tighten …" | drop the "migration path" qualifier — the fields are now `pub(crate)` (see line 78), so the migration is done | half-finished doc trail |

## Inconsistent sibling accessors

| Family | Sites | Convention drift |
|--------|-------|------------------|
| Output / node lookup family | `Graph::output_kind`, `Graph::kind_of_output`, `Graph::node_kind`, `Graph::get_node_from_output`, `Graph::memory_output_of`, `Graph::input_output_id`, `Graph::node_input_id_at` | 7 names, 3 grammars |
| Coerce-builder `get_*` accessors | `FunctionBuilder::get_output_type`, `get_as_bool`, `get_as_unsigned_int`, `get_as_signed_int`, `get_as_int`, `get_as_float_bits` | 6 names, all prefixed `get_` against Rust API conventions |
| Coerce-builder `_if_needed` mutators | `convert_to_bool_if_needed`, `truncate_if_needed`, `extend_if_needed`, `convert_to_int_if_needed` | 4 names, all suffixed `_if_needed` |
| `classify_anchor*` family | `classify_anchor`, `classify_anchor_with_rom`, `classify_anchor_with_rom_and_sp` | 3 variants, name encodes parameter list |
| `apply_elf_relocations*` family | `apply_elf_relocations`, `apply_elf_relocations_with_extender`, `apply_elf_relocations_autoload` | 3 variants, mixed parameter-as-suffix and behavior-as-suffix |

## Test-name clarity issues

| Site | Current | Suggested |
|------|---------|-----------|
| `crates/ir/src/node_signature.rs:699` | helper `smoke_vn()` (used by tests) | `dummy_vn()` or `placeholder_vn()` |
| `crates/entity-utils/src/worklist.rs:213` | `fn debug_format_smoke()` | `fn debug_format_includes_remaining_count` (or whatever the assertion actually pins) |
| `crates/opt/src/stack_store/tests.rs:23` | `fn simple_sp_minus_4_becomes_stack_store()` | drop "simple_" prefix — `sp_minus_4_becomes_stack_store` is already clear |
| `crates/strider/tests/graph_rewriter.rs:225` | `fn replace_input_then_reoptimize_then_replace_again_works()` | `replace_input_reoptimize_replace_again_preserves_validity` — `_works` is filler |

## Things that look like breadcrumbs but actually aren't

To save the next sweep some time:

- `_v1` / `_v2` in `powerpc64_elf_v{1,2}` are real ABI version names (PowerPC ELFv1 / ELFv2).
- `cfg_v1` / `cfg_v2` / `sp_v1` / `sp_v2` in test bodies are SSA-style successive-state names, not migration tags.
- `wave-1` / `wave-2` are still in two places (`reader/tests/elf_relocations.rs:93`, `strider/src/orchestrator.rs:1068`) but should drop the wave-N prefix since the "fix" they cite is the only behaviour anyone reads today.
- `Phase 1` / `Phase 2` in `crates/opt/src/known_bits/mod.rs:468,473` describe an **algorithmic** two-phase pass (propagate to fixed point, then replace), not a migration phase — keep as-is, but consider adding a short header so test-side cross-references like `crates/opt/src/known_bits/tests.rs:416` aren't ambiguous.
- `Step 1` / `Step 2` etc. are fine when they refer to a multi-step test assertion (e.g. `tests/builder_extended_use.rs` currently uses `Round 1` / `Round 2`; just rename to `Step` to remove the audit-round overload).

## What's clean since round 11

- `OptimizerOnBuilt` trait is gone — replaced by the `Optimizer` + `OptimizerRaw` split with a blanket impl. The new naming is intentional, not breadcrumb-trail.
- `fg: RewriteCtxView<'_>` parameter naming was the round-11 biggest mass cleanup target — that is now done in production code (only `crates/opt/src/sp_expr.rs` test bodies still use `let fg = b.build()?;` for local variables, which is acceptable since `fg` is locally scoped).
- The `cfg::Builder::new` deprecation chain no longer leaks `#![allow(deprecated)]` to nine test files (only `crates/cfg/src/cfg/builder/mod.rs:104` carries one, inside the deprecated wrapper itself).
- `R9-2D M3` / `R9-2D H2` / `R9-1A I3` doc-line formatting bugs in `cfg/src/cfg/options.rs`, `types.rs`, and `ir/src/builder/mod.rs` are fixed.
- `graphwalk/tests/graphmock_dsl.rs` tests `simple_graph()` / `diamond()` / `loop_graph()` were renamed to `parses_linear_chain` / `parses_diamond_topology` / `parses_self_referential_cycle`.
- `crates/ir/src/wide_const.rs::from_le_bytes_u256` / `_u512` are gone.
- `AnchorAddr::from_packed` is gone.
- `ranges_disjoint_basic`, `forward_basic` test-name nitpicks from round 11 are gone.
