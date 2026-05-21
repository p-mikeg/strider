# strider v4 simplification plan

**Goal:** Strip another ~2,500–3,000 LOC of dead code, duplicate types, and
hand-rolled boilerplate from the v3 codebase; replace remaining hand-rolled
preset tables with data-driven slices; collapse wrapper layers whose
cycle-break justifications no longer exist.

**Inputs:** Four parallel read-only audits over the entire workspace.  Each
finding cited below carries a file path + line range; the audits in
`/tmp/claude-1000/.../tasks/a*.output` are the ground truth.

**Scope:** structural cuts + generalizations + per-file code simplification.
No new features.  All work pinned to the existing `v3_baseline.rs` snapshot
contract; the snapshots must not move except where regeneration is itself
the documented unit of work for a theme (only Themes B2 and C2 plausibly
touch IR shape; both gated on regeneration review).

**Skills required for execution:** superpowers + subagent-driven-development
+ test-driven + debugging + code-review + writing.  Push after every commit.
Use entity-utils structures over std collections for entity-keyed
bookkeeping.  Keep every code path O(n) or O(n·log n).  No
plan-identifier strings in code, doc comments, or commit messages.

---

## Reachable cuts at a glance

| Phase | Theme count | Est. Rust LOC removed | Risk |
|---|---:|---:|---|
| 1: Pure dead-code deletion | 13 | ~1,200 | Low |
| 2: Wrapper / DTO collapse | 5 | ~450 | Medium |
| 3: API consolidation | 8 | ~350 | Medium |
| 4: Generalizations (data tables, traits) | 5 | ~700 | Medium-high (some snapshot work) |
| 5: Per-file simplification sweep | 1 | ~150 | Low |
| 6: Docs + validation + tag | 3 | n/a | n/a |
| **Total** | **35 themes** | **~2,850 LOC** | |

---

# Phase 1 — Pure dead-code deletion

Each theme here removes a public symbol with **zero callers across the
workspace**.  All are independent and can be dispatched in parallel.
Verify zero callers by running the `grep` commands in the theme before
deleting.

## Theme A1: Delete `BfgContentCache`

**Files:**
- Delete: `crates/strider-analyze/src/orchestrator/cache.rs` (58 LOC)
- Modify: `crates/strider-analyze/src/orchestrator/mod.rs:42` (remove `pub mod cache`)
- Delete: `crates/strider-analyze/tests/orchestrator_cache.rs` (~30 LOC)

**Verify:** `grep -rn "BfgContentCache" --include="*.rs" crates/` returns
only the definition and its test file.

**Steps:**
1. Confirm grep shows only the two self-referential sites.
2. Delete the three files / lines.
3. `cargo test -p strider-analyze --test v3_baseline` — should still pass.
4. Commit.

**LOC:** ~90.

## Theme A2: Delete the `Lifter` facade

**Files:**
- Delete: `crates/strider-lift/src/lifter.rs` (253 LOC)
- Delete: `crates/strider-lift/tests/lifter_facade.rs` (~30 LOC)
- Modify: `crates/strider-lift/src/lib.rs` (remove `pub mod lifter`)
- Modify: `crates/strider-lift/src/cfg/decode_cache.rs:42-97` — drop
  `decode_stats()`, `unique_addresses()`, `total_lift_calls()`, the
  `AtomicUsize` counter, and `DecodeStats` struct.

**Verify:** `grep -rn "use strider_lift::lifter\|Lifter::\|DecodeStats\b" --include="*.rs" crates/`
returns only the deleted file's own tests.

**Steps:**
1. Confirm grep shows zero external consumers.
2. Delete files + accessors.
3. Build the workspace; `cargo test -p strider-lift`.
4. Commit.

**LOC:** ~290.

## Theme A3: Retire `cfg_test_api` + every `pub mod test_api` re-export pyramid

**Files:**
- Delete: `crates/strider-lift/src/cfg_test_api.rs` (19 LOC)
- Modify: `crates/strider-lift/src/lib.rs` — remove `pub use cfg_test_api::*`
- Modify: `crates/strider-lift/src/cfg/builder/mod.rs:318-399` — delete inner `pub mod test_api`
- Modify: `crates/strider-lift/src/cfg/builder/region_builder.rs:683-803` — delete `pub mod test_api` + `TestRegionBuilder`
- Modify: `crates/strider-lift/src/cfg/dot.rs:19-35` — delete `pub mod test_api`
- Modify: `crates/strider-lift/src/cfg/mod.rs` — delete `Cfg::from_parts_for_tests`
  (lines 124-138) and the matching `pub use ... test_api::*` lines (31-38).

**Verify:** `grep -rn "cfg_test_api\|cfg::test_api\|TestRegionBuilder\|from_parts_for_tests" --include="*.rs" crates/`
returns only the definition sites.

**LOC:** ~280.

## Theme A4: Delete `RegionDriver`

**Files:**
- Delete: `crates/strider-lift/src/region_driver.rs` (121 LOC)
- Modify: `crates/strider-lift/src/lib.rs` — remove `pub mod region_driver` + re-export.
- Modify: `crates/strider-analyze/src/strider/pipeline.rs:391, 413` and
  `crates/strider-analyze/src/strider/insn/mod.rs:39-41` — replace
  `RegionDriver::set_lift_addr(&mut self.builder, term_addr); … ;
  RegionDriver::clear_lift_addr(&mut self.builder);` with direct
  `self.builder.set_lift_addr(Some(term_addr)); … ;
  self.builder.set_lift_addr(None);`.

**Verify:** `grep -rn "RegionDriver\b" --include="*.rs" crates/` is empty
after the change.

**LOC:** ~125.

## Theme A5: Delete `FunctionBoundary` enum + accessor + setter

**Files:**
- Modify: `crates/strider-lift/src/cfg/options.rs:27-49, 99-117, 199-231`
  — delete `FunctionBoundary` enum, `Options::function_boundary()`,
  `OptionsBuilder::set_function_boundary()`.  Keep
  `set_function_max_size` + `allow_code_before_start_addr` (the real
  scalars callers use).

**Verify:** `grep -rn "FunctionBoundary\|function_boundary\|set_function_boundary" --include="*.rs" crates/`
returns only `arch.rs`.

**LOC:** ~85.

## Theme A6: Delete `entity_utils::Worklist`

**Files:**
- Delete: `crates/entity-utils/src/worklist.rs` (239 LOC + tests)
- Modify: `crates/entity-utils/src/lib.rs` — remove `pub mod worklist`
  and the `pub use worklist::Worklist` re-export.

**Verify:** `grep -rn "Worklist" --include="*.rs" crates/` returns only
`entity-utils/src/`.

**LOC:** ~240.

**Note:** This crate has a user-memory directive
(`project_prefer_entity_utils`) recommending entity-utils for
NodeId/etc bookkeeping.  `Worklist` was the unbuilt half of that
recommendation; if a future pass needs one, restoration is a 30-line
rewrite.  Document the deletion intentionally.

## Theme A7: Delete dead `elf_*` helpers + downgrade two relocation entry points

**Files:**
- Modify: `crates/strider-reader/src/elf.rs` — delete the four
  single-item / filter-taking converters
  (`elf_segment_to_mem_region`, `elf_section_to_mem_region`,
  `elf_segments_to_mem_regions`, `elf_sections_to_mem_regions`) plus
  the two exec-filter presets (`elf_get_executable_segments_as_mem_regions`,
  `elf_get_executable_sections_as_mem_regions`).
- Change visibility on `apply_elf_relocations` and
  `apply_elf_relocations_with_extender` from `pub` to `pub(crate)`
  (only the `_autoload` form is consumed by strider-py).

**Verify:** `grep -rn "strider_reader::elf::\(elf_segment_\|elf_section_\|elf_segments_\|elf_sections_\|elf_get_executable_\)" --include="*.rs" crates/`
returns nothing.

**LOC:** ~150.

## Theme A8: Delete `set_call_other_clobbered_for_test` (zero callers)

**Files:**
- Modify: `crates/strider-ir/src/function.rs:155-169` — delete the method.

**Verify:** `grep -rn "set_call_other_clobbered_for_test" --include="*.rs" crates/`
returns only the definition.

**LOC:** ~15.

## Theme A9: Delete `OptimizerRaw` trait (no direct implementors)

**Files:**
- Modify: `crates/strider-analyze/src/opt/pipeline.rs:103-170` — delete
  `OptimizerRaw` trait + its blanket impl.  Make `OptimizerPipeline`
  store `Box<dyn Optimizer>` directly; inline `with_rewrite_ctx` into
  the pipeline's run loop.

**Verify:** `grep -rn "impl OptimizerRaw for\|Box<dyn OptimizerRaw>" --include="*.rs" crates/`
returns only the blanket impl itself.

**LOC:** ~30.

**Note:** Verify strider-py's `PyOptimizerPipeline` doesn't reach for
`OptimizerRaw` (it shouldn't, per audit).  If it does, update at the
same time.

## Theme A10: Delete `Builder::with_preset` + dead `options_*` test-api accessors

**Files:**
- Modify: `crates/strider-lift/src/cfg/builder/mod.rs:144-163` — delete
  `Builder::with_preset` and the two `options_link_register_vn` /
  `options_read_only_memory` test-api accessors.

**Verify:** `grep -rn "Builder::with_preset\|options_link_register_vn\|options_read_only_memory" --include="*.rs" crates/`
returns only the definition sites.

**LOC:** ~25.

## Theme A11: Delete `Endianness::read_u16` / `read_u32`

**Files:**
- Modify: `crates/strider-target/src/arch.rs:28-44` — drop the two
  methods and their inline tests (only `read_u64` is consumed).

**Verify:** `grep -rn "endianness\.read_u16\|endianness\.read_u32\|Endianness::read_u16\|Endianness::read_u32" --include="*.rs" crates/`
returns nothing.

**LOC:** ~20.

## Theme A12: Delete `ArchContext`

**Files:**
- Modify: `crates/strider-target/src/arch.rs:114-118, 166-172` — delete
  the `ArchContext` struct + `SleighArch::context()` accessor.

**Verify:** `grep -rn "ArchContext\b" --include="*.rs" crates/` returns
only the definition.

**LOC:** ~20.

## Theme A13: Delete `ResolvedTargets::multiple` validating ctor

**Files:**
- Modify: `crates/strider-lift/src/cfg/builder/indirect_resolver.rs:69-80`
  — drop the unused validating ctor.

**Verify:** `grep -rn "ResolvedTargets::multiple" --include="*.rs" crates/`
returns nothing.

**LOC:** ~12.

**Phase 1 total:** ~1,400 LOC across 13 independent themes.

---

# Phase 2 — Wrapper / DTO collapse

These themes require coordinated edits across crate boundaries.  Run them
sequentially; each must be a green build before the next starts.  The
order below honours the dependency graph (later themes need the earlier
ones to land first).

## Theme B1: Move `BuiltCallingConvention` into `strider-ir`; delete `FunctionBuilderCC`

**Files:**
- Move: `crates/strider-target/src/calling_convention/mod.rs` →
  the `BuiltCallingConvention` struct + `try_new` + all accessors → into
  `crates/strider-ir/src/calling_convention.rs` (new file).  Keep
  `CallingConvention` (the static-string preset enum) + `build()` in
  strider-target, returning the now-strider-ir type.
- Delete: `crates/strider-ir/src/function_builder_cc.rs` (63 LOC).
- Delete: `crates/strider-target/src/calling_convention/mod.rs` lines
  318-360 (both `From<BuiltCallingConvention>` impls).
- Modify: `crates/strider-ir/src/builder/mod.rs:91` —
  `FunctionBuilder::new` takes `&BuiltCallingConvention` (strider-ir
  type), not `&FunctionBuilderCC`.
- Modify: the 3 call sites in `crates/strider-analyze/src/strider/`
  to drop the `FunctionBuilderCC::from(&cc)` shim.

**Steps:**
1. Subagent dispatch: write tests for `BuiltCallingConvention::try_new`
   in its new home (move + adapt the inline tests).
2. Move the type.  Add the strider-target re-export
   `pub use strider_ir::calling_convention::BuiltCallingConvention;`
   for back-compat OR change `strider-target::CallingConvention::build()`
   to return the strider-ir type directly (preferred — fewer indirections).
3. Update call sites.  Build the workspace.
4. Delete `FunctionBuilderCC` and the two From impls.
5. Run `cargo test --workspace --no-fail-fast`.  Snapshot baseline
   should not move.
6. Commit.

**Verify dep direction:** `cargo metadata` after the move should show
`strider-target → strider-ir` (no cycle).  This was already true; the
DTO existed for an older cycle that's gone.

**LOC:** ~170 (62 DTO + 45 From impls + 60 LOC of `.into()`/clone
boilerplate at call sites).

## Theme B2: Delete `FunctionGraph` + collapse `body()`/`graph()`

**Files:**
- Modify: `crates/strider-ir/src/function.rs:14-38` — delete
  `FunctionGraph` struct.
- Modify: `crates/strider-ir/src/builder/mod.rs:101-178` — remove
  `body()`/`body_mut()`; keep only `graph()`/`graph_mut()`.  Move
  `entry: NodeId` directly onto `FunctionBuilder`.  Recompute
  `entry_control` / `entry_memory` in `set_entry_region`
  (`builder/vars.rs:72-75`) via `Graph::node_outputs`.
- Modify: `crates/strider-ir/src/builder/mod.rs:101-160` and all
  ~8 callers of `body()` → `graph()`.

**LOC:** ~80.

## Theme B3: Trim `RegionLiftHandles` to its two used fields

**Files:**
- Modify: `crates/strider-analyze/src/strider/pipeline.rs:22-48` — reduce
  `RegionLiftHandles` to `{ exit_control: NodeOutputId, exit_vn_to_value: Arc<HashMap<Vn, NodeOutputId>> }`
  (or replace with a tuple/2-field struct entirely).  Drop population
  of the seven unread fields (`pipeline.rs:469-508`).

**Verify:** the orchestrator's `RegionIndex::from_handles` reads only
the two retained fields.  Confirm via:
```bash
grep -rn "\.start_addr\|\.entry_control_state\|\.entry_mem_phi\|\.entry_control\b\|\.entry_memory\b\|\.exit_memory\b\|\.entry_var_phis" --include="*.rs" crates/strider-analyze/
```

**LOC:** ~35.

## Theme B4: Delete the `indirect_resolve/` shim layer

**Files:**
- Delete: `crates/strider-analyze/src/indirect_resolve/mod.rs` (21 LOC)
- Delete: `crates/strider-analyze/src/indirect_resolve/classify.rs` (72 LOC of shim + ~350 LOC of inline tests)
- Delete: `crates/strider-analyze/src/indirect_resolve/inplace.rs` (8 LOC)
- Modify: `crates/strider-analyze/src/lib.rs:20` — drop `pub mod indirect_resolve`.
- Modify: `crates/strider-analyze/src/orchestrator/mod.rs:53-55` — import
  from `crate::opt::indirect_branch_resolve` directly.
- **Test migration:** move the ~350 LOC of `classify.rs` inline tests
  into `crates/strider-analyze/src/opt/indirect_branch_resolve/classify.rs`
  (where the implementation lives).

**LOC:** ~100 (the test bulk moves rather than deleting).

## Theme B5: Collapse `IndirectTargetResolver` trait → `Fn` callback

**Files:**
- Modify: `crates/strider-lift/src/cfg/builder/indirect_resolver.rs` —
  replace `pub trait IndirectTargetResolver` with a type alias
  `pub type IndirectResolverFn<R> = Arc<dyn Fn(...) -> Result<Option<ResolvedTargets>> + Send + Sync>;`
  or accept a non-`Arc`'d boxed closure.
- Modify: `crates/strider-analyze/src/indirect_resolver.rs` — delete
  `MiniIrIndirectResolver` (unit struct).  Replace with a free
  function exported by name; the orchestrator passes
  `Arc::new(resolve_indirect_target)`.
- Modify: `crates/strider-analyze/src/orchestrator/mod.rs:968-970` —
  pass `Arc::new(resolve_indirect_target)` instead of
  `Arc::new(MiniIrIndirectResolver)`.

**LOC:** ~50.

**Phase 2 total:** ~435 LOC.

---

# Phase 3 — API consolidation

These themes change shape but not semantics.  Test sweep after each: run
`v3_baseline`, `cross_arch_shape`, and per-crate test suites.

## Theme C1: Pipeline builder consolidation (6 → 1)

**Files:**
- Modify: `crates/strider-analyze/src/opt/mod.rs:113-191` — keep only
  `default_pipeline()` returning the full sequence.  Delete
  `stable_default_pipeline()` + `destructive_default_pipeline()`.
- Modify: `crates/strider-analyze/src/strider/pipeline.rs:186-247` —
  replace the three `Strider::build_*_optimizer_pipeline()` methods
  with one `Strider::build_pipeline(modes: PipelineModes)` taking a
  bitflag-driven mode, OR (preferred — fewer types) one
  `build_pipeline()` and let the orchestrator filter by pass name when
  it needs stable-only vs destructive-only mid-iteration.
- Modify: `crates/strider-analyze/src/opt/pipeline.rs` — drop
  `optimizer_names` + `optimizer_count` introspection fields.
- Delete: the `tests/optimizer_pipeline_subsets.rs` test that pinned
  the partition.

**Risk:** the orchestrator currently calls stable-pass-set + destructive-pass-set
separately to decide convergence.  Audit `orchestrator/mod.rs:447,
460, 468-474` before locking in the merge.  If the orchestrator really
needs the partition at runtime, keep two builders but make them share
a single pass list (single source of truth).

**LOC:** ~145 + ~50 LOC of deleted test.

## Theme C2: Reduce `Decision` enum (StableOnly fold-in)

**Files:**
- Modify: `crates/strider-analyze/src/orchestrator/mod.rs:184, 191-202,
  447, 460, 468-474` — collapse to `Decision { Continue, Rebuild,
  FixedPoint }`.  After `apply_in_place_edits` returns non-empty
  edits, always re-run the stable pipeline; the variant disappears.

**LOC:** ~30.

**Risk:** lower-confidence per audit.  Run with v3_baseline + edge
cases to verify the convergence semantics actually match.

## Theme C3: Collapse strider-py `RomInput` / `ReaderInput` / `ReaderInputClone` (3 → 1)

**Files:**
- Modify: `crates/strider-py/src/reader.rs:721-818` — one
  `MemInput { Map(PyMemoryMap), Cb(Py<PyAny>) }` enum with three
  methods (`into_arc`, `into_any`, `clone_one`).  Update
  `run.rs`'s `mem.into_clone()?.materialise()?.materialise()?` chain
  to `mem.clone_one()?` / `mem.into_any()?`.

**LOC:** ~60.

## Theme C4: Collapse strider-py `MemoryMap` lock dance (4 RwLocks → 1)

**Files:**
- Modify: `crates/strider-py/src/reader.rs` — replace the four
  `Arc<RwLock<...>>` fields (`regions`, `table`, `elfs`, `endianness`)
  with one `Arc<RwLock<MemoryMapInner>>`.  Add a helper
  `fn write_inner(&self, ctx: &'static str) -> PyResult<RwLockWriteGuard<...>>`
  to centralise poison-error mapping.

**LOC:** ~50.

## Theme C5: `forall_preset!` macro for `arch.rs` + `cc.rs` classmethods

**Files:**
- New: `crates/strider-py/src/preset_macros.rs` — `forall_preset!` macro
  that takes a list of preset names + emits one
  `#[classmethod] fn NAME(_cls: ...) -> Self { ... }` per name.
- Modify: `crates/strider-py/src/cc.rs:14-191` — replace the 24
  hand-stamped classmethods with one macro invocation.
- Modify: `crates/strider-py/src/arch.rs:14-84` — same for the 15 arch classmethods.

**Verify:** the macro-expanded surface matches the current Python API
byte-for-byte.  Run pytest after the change to confirm no Python user
sees a different name.

**LOC:** ~100 net (replace ~150 LOC of stamp with ~50 LOC of macro
declaration).

## Theme C6: Strider-py pipeline-builder passthrough collapse

**Files:**
- Modify: `crates/strider-py/src/strider_cls.rs:85-102` and
  `crates/strider-py/src/opt.rs` — collapse the three pure-passthrough
  pipeline-builder methods (`build_optimizer_pipeline`,
  `_stable_`, `_destructive_`) into one
  `build_pipeline(mode: Optional[str] = None)`.  Tied to Theme C1.

**LOC:** ~50.

## Theme C7: Move `from_graph_and_entry_for_rewrite` to `strider-ir-test-utils`

**Files:**
- Modify: `crates/strider-ir/src/function.rs:171-203` — delete the
  `#[doc(hidden)]` method.  Replace with a minimal
  `BuiltFunctionGraph::from_graph_with_empty_cc(graph, entry)`
  constructor.
- Modify: `crates/strider-ir-test-utils/src/lib.rs` — add
  `pub fn rewrite_only_bfg(graph, entry) -> BuiltFunctionGraph`
  wrapping the new ctor.
- Modify: `crates/strider-analyze/tests/orchestrator_cache.rs:16` —
  call the test-utils form (but note this test goes away in A1 too).

**LOC:** ~25.

## Theme C8: `BuiltCallingConvention` `pub` fields + delete 9 accessors

**Files:**
- Modify: `crates/strider-ir/src/calling_convention.rs` (post-B1 home)
  — make the 10 `pub(crate)` fields `pub` (the type is immutable
  post-`try_new`).  Delete the 9 1-line accessor methods.
- Update callers across the workspace.

**Risk:** breaks every reader of `cc.foo()` → `cc.foo`.  Mechanical
sweep; subagent can do it.

**LOC:** ~50.

**Phase 3 total:** ~360 LOC.

---

# Phase 4 — Generalizations (data tables + traits)

These are the highest-LOC wins but require the most care.  Each one is
its own sub-project; do not combine.

## Theme D1: Calling-convention presets → `&[CcPresetRow]` data table

**Files:**
- Modify: `crates/strider-target/src/calling_convention/mod.rs:362-962`
  — replace the 14 hand-rolled zero-arg fn-builders
  (`x86_64_systemv`, `x86_cdecl`, `aarch64_aapcs64`, `arm_aapcs`,
  `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`,
  `powerpc64_elf_v2`, `x86_64_all_preserving`, the four
  `*_linux_kernel` variants, the six `*_linux_syscall` derived
  variants) with one
  ```rust
  pub static CC_PRESETS: &[CcPresetRow] = &[
      CcPresetRow { name: "x86_64_systemv", arg_regs: &["RDI","RSI",...], ... },
      ...
  ];
  ```
  plus a `lookup(name: &str) -> Option<&'static CcPresetRow>` helper
  and (for compat) keep the zero-arg constructor names but make them
  one-line wrappers `pub fn x86_64_systemv() -> CallingConvention { lookup("x86_64_systemv").unwrap().into() }`.
- The `*_linux_syscall` derived variants store an `Option<&'static CcPresetRow>`
  base-row reference + per-row override fields (or get duplicated outright
  if that's simpler — measure).

**Steps:**
1. Spike: convert ONE preset (`x86_64_systemv`) to the row shape.
   Verify the rest of the code can call it.  Commit.
2. Convert the rest in batches of 3-4 per commit (rollback-friendly).
3. Confirm v3_baseline + cross_arch_shape stay green.
4. Add a doctest showing the table format.

**Trade-off:** loses some compile-time type-checking of preset names
(the lookup is at runtime via string).  Mitigation: add an
`#[deprecated]` shim per preset name that calls `lookup(...)` so
existing callers (and Python `arch.rs` classmethods) keep working.
Or skip the deprecation and keep the named functions as thin wrappers.

**LOC:** ~400 net.

## Theme D2: `call_other_abi::classify_arch_specific` → data table

**Files:**
- Modify: `crates/strider-target/src/call_other_abi.rs:76-263` — replace
  the 16-arm `match (preset, name)` with
  ```rust
  static ARCH_SPECIFIC: &[(ArchPreset, &str, CallOtherClass)] = &[ ... ];
  ```
  plus a single-pass `iter().find(...)` lookup, OR a `phf` map keyed
  on `(ArchPreset, &str)`.

**Risk:** none — the existing match arms are already shaped like
data with per-line comments.  Mechanical conversion.

**LOC:** ~190.

## Theme D3: Side-table coalesce — 4 SecondaryMaps → 1 `NodeSideData`

**Files:**
- Modify: `crates/strider-ir/src/graph/mod.rs:94-153` — replace the
  four parallel `SecondaryMap<NodeId, V>` fields
  (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`,
  `call_clobbered_overrides`) with one
  ```rust
  side_data: SecondaryMap<NodeId, NodeSideData>,
  // NodeSideData { stack_phi_offsets: Vec<i64>, call_other_name: Option<String>,
  //                asm_fingerprints: Vec<u64>, call_clobbered_override: Option<Vec<Vn>> }
  ```
- Modify: getters/setters in `graph/store.rs` (collapse 8 accessors
  to a single `side_data(id)`/`side_data_mut(id)` pair).
- Modify: `graph/compact.rs:237-241` — single-line remap.

**Risk:** medium.  Cache locality should improve (one allocation per
node touching ancillary data instead of four), but most nodes don't
touch all four — so the size penalty matters.  **Measure first** with
`cargo bench` (the existing `scaling` bench in
`crates/strider-analyze/benches/scaling.rs`).  If the working-set
size grows more than the locality wins, skip this theme.

**LOC:** ~80 + cleaner compaction logic.

## Theme D4: PeepholePass trait + generic driver

**Files:**
- New: `crates/strider-analyze/src/opt/peephole.rs` —
  ```rust
  pub trait PeepholePass {
      fn name(&self) -> &'static str;
      fn root_kinds(&self) -> &[NodeKind];
      fn try_rewrite(&self, ctx: &mut RewriteCtx, node: NodeId) -> Result<bool>;
  }
  pub fn run_peephole<P: PeepholePass>(pass: &P, ctx: &mut RewriteCtx) -> Result<OptimizationResult> { ... }
  ```
- Modify: the 11 `impl Optimizer for X` blocks (`constant_fold/`,
  `known_bits/`, `flag_cmp_canonicalize/`, `if_cond_inversion/`,
  `redundant_phis/`, `dead_branch/`, `load_readonly/`, `stack_store/`,
  `stack_load_forward/`, `function_args/`, `indirect_branch_resolve/`)
  — many will collapse to `impl PeepholePass for X` shaving 5-15 LOC
  of boilerplate each.  The generic driver does the worklist + fixed-
  point loop.

**Risk:** medium-high.  Some passes don't fit the simple "match root
kind → rewrite" shape (stack-store detect needs pre-pass analysis;
load_readonly needs the `&dyn ReadOnlyMemory`).  Per-pass signatures
might need a `type Context` GAT.  Spike with `ConstantFold` first.

**LOC:** ~150 saved across 11 passes.

## Theme D5: Flatten `ops/` into `graph/`

**Files:**
- Move: `crates/strider-ir/src/ops/op_kinds.rs` → `crates/strider-ir/src/ops.rs`
  (or fold into `node/`).
- Move: methods from `ops/builder.rs` (3 helpers) + `ops/consts.rs`
  (6 helpers) into `crates/strider-ir/src/graph/store.rs`.
- Move: `ops/rewrite.rs::replace_all_uses` → `crates/strider-ir/src/graph/uses.rs`.
- Delete: `crates/strider-ir/src/ops/mod.rs`.

**LOC:** no net change but flattens 5 files to 1.

**Phase 4 total:** ~700 LOC saved structurally + cleaner layout.

---

# Phase 5 — Per-file simplification sweep

A focused cleanup pass on the highest-density files using the
`code-simplifier` subagent.  Target patterns:

- `match` blocks with single-arm `_ => ...` that should be `if let`
- Long `fn` (>200 lines) that should split
- Hand-rolled `impl Default` that could be derived
- `Option<Vec<T>>` where `Vec<T>` would express the same constraint (empty == None)
- Deep nesting → early returns
- Manual `Iterator` re-implementations
- Repeated `let x = self.field.read().unwrap(); use x.foo` lock dance

## Theme E1: Top-file simplification sweep

**Targets (largest non-test files):**
1. `crates/strider-analyze/src/orchestrator/mod.rs` (1,050 LOC non-test)
2. `crates/strider-analyze/src/opt/sp_expr.rs` (978 LOC)
3. `crates/strider-analyze/src/opt/indirect_branch_resolve/stack_array.rs` (877 LOC)
4. `crates/strider-analyze/src/opt/indirect_branch_resolve/jump_table.rs` (771 LOC)
5. `crates/strider-analyze/src/opt/constant_fold/rules.rs` (740 LOC)
6. `crates/strider-analyze/src/pattern/matcher/mod.rs` (704 LOC)
7. `crates/strider-py/src/pattern.rs` (1,984 LOC — but mostly macro invocations; lower priority)
8. `crates/strider-py/src/reader.rs` (826 LOC)
9. `crates/strider-target/src/calling_convention/mod.rs` (post-D1)
10. `crates/strider-lift/src/cfg/builder/mod.rs`

**Steps:** dispatch a `code-simplifier` subagent per file with the
target-pattern checklist above.  Each subagent commits its own work.
Cap at ~30 LOC reduction per file; if a target is unfruitful (no
real simplifications), skip.  Aim for ~150 LOC total across 10 files.

**LOC:** ~150.

---

# Phase 6 — Docs + validation + tag

## Theme F1: CLAUDE.md refresh

**Files:**
- Modify: `CLAUDE.md` — update the pattern-macros emission count
  from "10/14 builders" to "13/14 builders" + one hand-written.
  Remove the reference to a deleted `pattern_reference.rs`.  Remove
  the references to `test_helpers` + `graphmock` in
  `crates/strider-ir/` (they're now in `strider-ir-test-utils` and
  `graphwalk/tests/common/`).  Update any other stale paths
  introduced by Phases 1-5.

## Theme F2: EMISSION_SPEC.md refresh

**Files:**
- Modify: `crates/strider-pattern-macros/EMISSION_SPEC.md` — drop
  the Reference-type rows; update to match current macro behaviour
  (the `constructor_args` extension that absorbs the three binary-op
  builders).

## Theme F3: Final review + `v4-final` tag

**Steps:**
1. Run all three: v3_baseline, cross_arch_shape, full per-crate suites.
2. Dispatch `pr-review-toolkit:code-reviewer` over `v3-final..HEAD`.
3. Address findings.
4. Tag `v4-final` and push.

---

# Execution order

**Parallelizable batch (Phase 1, all 13 themes):** dispatch all together
as one batch of subagents.  Each is read-and-confirm-then-delete; no
cross-theme dependencies.  Expected wall-clock: ~1 hour with 4-way
parallelism.

**Serial chain (Phase 2):** B1 → B2 → B3 → B4 → B5.  Each must build
clean before the next.

**Phase 3:** C1 + C2 share dependencies; the rest (C3-C8) are
independent and can run in parallel.

**Phase 4:** each theme is its own sub-project; never combine.  D1 is
the highest-LOC and should land first; D2-D5 in any order.

**Phase 5:** parallel one-subagent-per-file.

**Phase 6:** sequential at the end.

**Estimated total:** 4-7 days at the v3 cadence.

---

# Risk callouts

1. **Theme B1 (BuiltCallingConvention move)** — touches the
   strider-ir / strider-target boundary.  Verify the dep graph stays
   acyclic via `cargo metadata | jq` or `cargo tree -i strider-ir`
   before merging.

2. **Theme C2 (Decision enum reduction)** — `StableOnly` may carry
   subtle convergence semantics the audit missed.  Run extended
   regression: v3_baseline + cross_arch_shape + `tests/proptest_optimizer.rs`
   for 1000 iterations.

3. **Theme D1 (CC preset data table)** — the `*_linux_kernel` and
   `*_linux_syscall` derived variants currently use "let mut cc =
   userland(); cc.field = newval;" mutation.  The row table must
   represent the override delta correctly.  Spike one row + verify
   before mass conversion.

4. **Theme D3 (side-table coalesce)** — cache-locality vs working-set
   tradeoff.  **Bench-gate this theme.** If the bench shows no win,
   skip it.

5. **Theme D4 (PeepholePass trait)** — some passes don't fit the
   simple "match root kind → rewrite" shape.  Spike one pass first;
   if it needs a `Context` GAT to absorb stack-store / load_readonly,
   the boilerplate-savings calculus may not work.

---

# What this plan deliberately does NOT do

- **Move `egraph` back in.**  Removed in v3; out of scope.
- **Reintroduce salsa.**  Removed in v3; out of scope.
- **Split `strider-analyze`.**  Per the v3 simplification, the consolidation
  was deliberate.  No re-fracturing.
- **Rename anything cross-cutting.**  No `strider-*` prefix changes; v3
  settled the prefix rule.
- **Change snapshot baselines.**  D2 + D3 are the only themes that
  plausibly touch IR shape, and both are gated on snapshot review.

---

# Notes on memory directives

Per saved user feedback:
- All bookkeeping for `NodeId`/`NodeOutputId`/`NodeInputId` must use
  entity-utils structures.  Theme G2 of the audit (HashMap → DenseEntitySet
  in validator) is the most concrete instance; the simplification
  sweep should also catch any new HashMap-keyed-by-NodeId.
- O(n) / O(n·log n) only.  The PeepholePass driver (D4) must do one
  worklist walk per pass, not a per-pass full graph scan.
- No plan identifiers (Phase X / Theme Y / Bug-N) in code, commit
  messages, or doc comments.
- Push after every commit.
- Preserve all v1 test functionality — covered by v3_baseline +
  cross_arch_shape + per-crate suites.
