# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
# Build all crates
cargo build --workspace

# Run the main example (reads fixtures/out/x86/arithmetic.elf::add — must be
# built first from fixtures/Makefile — and dumps cfg.html, graph.html, and
# graph-opt.html in the workspace root). The example still lives in
# crates/strider/examples/strider.rs.  (The `strider` crate is now a thin
# re-export of `strider-analyze`; the example surface is unchanged.)
cargo run -p strider --example strider

# Run all tests
cargo test --workspace

# Run a single test
cargo test --package <crate-name> <test_name>

# Lint
cargo clippy --workspace
```

## V2 Rewrite Status

This repository is mid-way through the **strider v2 rewrite**
(`docs/superpowers/plans/2026-05-17-strider-v2-rewrite.md`).  The 12-crate v1
layout has been consolidated into **5 main crates + 1 proc-macro crate**, with
the old crate paths kept as thin re-export shims so downstream code
(strider-py, integration tests, examples) compiles unchanged.  The shim
crates are scheduled for deletion in Phase 6.2; the re-exports remain
functional today.

**V1 baseline snapshots (`crates/strider/tests/v1_baseline.rs`) PASS
throughout the rewrite** and are the ground-truth contract every phase
commit must preserve.

## Architecture Overview

This is a Rust workspace binary analysis tool that lifts native binaries to an
IR and exposes it for arbitrary pattern queries via Python.  The pipeline is:

**Binary → CFG → IR → Optimizations → Pattern Queries (Python)**

### Crate Dependency Flow (V2)

```
   strider-binary  →  strider-ir  →  strider-lift  →  strider-analyze  →  strider-py
   (renamed         (sea-of-nodes    (target + sleigh   (egraph opt +     (PyO3 / maturin;
    `reader`;        graph +          + cfg + lifter;    pattern DSL +     promoted to
    concrete         validator +      `From<BuiltCC>     indirect-branch    top-level
    ReadOnlyMemory   ReadOnlyMemory   for FunctionBuilderCC)  resolution +   `strider`
    impls: ELF/PE)   trait +          target re-exported  Salsa orchestrator) maturin pkg
                     FunctionBuilderCC for back-compat)                       in Phase 5)
                     plain-data +
                     egraph adapter +
                     graphwalk + dot
                     + entity-utils +
                     graphmock)

         ↑                                ↑
   strider-pattern-macros          (proc-macro crate;
   (proc-macro; emits Rust          consumed by strider-analyze
    + Python pattern types          for Pat builders and by
    from one annotated              strider-py for Py*Pat mirrors)
    `*Def` struct)

   rsleigh   (external path dep at ../rsleigh — Sleigh/GHIDRA p-code lifter;
              unchanged from v1)
```

**V6 verification confirmed no back-edges.**  The `cfg → opt` back-edge from
v1 was resolved by the `IndirectTargetResolver` callback trait (V6.A fix for
G9): `strider-lift::cfg::Builder` calls back into `strider-analyze` through
a trait object instead of statically depending on it.  The `ir → target`
back-edge was resolved by moving the thin `FunctionBuilderCC` plain-data
struct + `ReadOnlyMemory` trait into `strider-ir` (V6.A/B fixes).
`BuiltCallingConvention` keeps its rich API in `strider-lift::target` and
ships `impl From<BuiltCallingConvention> for FunctionBuilderCC`.

### Shim Crates (deletion-pending in Phase 6.2)

Each shim crate is a `pub use <new_crate>::*` re-export of its v1 surface.
All compile cleanly; no code paths break by importing from the old name.
Phase 6.2 deletes them along with cleaning up downstream imports.

| v1 crate | v2 home | Shim status |
|---|---|---|
| `reader` | `strider-binary` (rename pending) | Still present as `crates/reader/` |
| `ir` | `strider-ir` | Shim: `pub use strider_ir::*;` |
| `graphwalk`, `entity-utils`, `dot`, `graphmock` | `strider-ir` modules | Absorbed (Tasks 1.1–1.2) |
| `target` | `strider-lift::target` (re-exported standalone for cycle-break) | Still present as `crates/target/` |
| `pcode-lift` | `strider-lift::pcode_lift` | Shim |
| `cfg` | `strider-lift::cfg` | Shim |
| `opt` | `strider-analyze::opt` | Shim: `pub use strider_analyze::opt::*;` |
| `pattern` | `strider-analyze::pattern` | Shim |
| `strider` (orchestrator) | `strider-analyze::orchestrator` + re-exports | Shim: `pub use strider_analyze::{…};` |
| `strider-py` | top-level `strider` maturin crate (Phase 5 rename pending) | Still present as `crates/strider-py/` |

### Key Crates

- **`strider-binary`** (currently `reader`) — Concrete `ReadOnlyMemory`
  implementations and ELF loading.  Provides `ElfFileMemReader` (memory
  reader for rsleigh), `apply_elf_relocations(regions, &obj)`, and the
  autoload convenience wrapper `apply_elf_relocations_autoload(regions,
  &obj)` — the latter scans dynamic relocations, identifies any site
  addresses not yet covered by `regions`, and lazily extends with the
  section that owns each missing site (e.g. `.got.plt`) before applying.
  `MemoryMap.apply_elf_relocations` (Python) and `strider-py`'s reader
  default to the autoload variant.  Rename to `strider-binary` and
  extension to PE/Mach-O is mechanical (Phase 6, out-of-scope for this
  rewrite).

- **`strider-ir`** — The core sea-of-nodes IR graph and everything that
  doesn't depend on a target.  Absorbs v1's `ir`, `graphwalk`,
  `entity-utils`, `dot`, and `graphmock` crates as modules.  Surface:

  - **Modules:** `dot`, `entity_utils`, `graphmock`, `graphwalk`,
    `graph_dot` (IR-specific dot renderer — renamed from `ir::dot` to
    disambiguate from the generic `dot` module).
  - `Graph` — stores `NodeId`, `NodeOutputId`, `NodeInputId` via
    `cranelift-entity` PrimaryMaps.  Nodes are deduplicated/cached by
    (kind, inputs, output_kinds).  Per-node side-tables
    (`stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>`,
    `call_other_names: SecondaryMap<NodeId, Option<String>>`,
    `asm_fingerprints: SecondaryMap<NodeId, Vec<u64>>`) hold ancillary
    data.
  - **Asm-fingerprint side-table** (`asm_fingerprints`) — every `NodeId`
    carries a sorted-deduplicated list of machine-instruction addresses
    identifying the parent asm insns whose lifting (or subsequent
    rewrite) contributed to the node's value.  Proof-of-correctness aid:
    when a pattern query captures a value node, its fingerprint names
    the asm instructions that explain the match.  The contract is
    **superset-only**: passes may *grow* fingerprints but must never
    shrink them or replace a node with one whose fingerprint omits an
    ancestor's addresses.  Two structurally identical (cacheable) nodes
    share one entry that is the **union** of every contributor's
    address.  Region / phi / initial-state kinds (`Entry`,
    `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`,
    `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi`) are exempt from
    non-empty checks.  Public API on `Graph`: `asm_fingerprint(id)`,
    `set_asm_fingerprint`, `extend_asm_fingerprint`,
    `extend_asm_fingerprint_from`.  See
    `docs/superpowers/specs/2026-05-03-asm-fingerprints-design.md` for
    the full contract.
  - `FunctionBuilder` — builds the IR graph with SSA-like variable
    tracking.  Variables map `rsleigh::Vn` → `VarId`.  Each region gets
    a `ControlState` node + `VarPhi` nodes for variables.  Calls
    `validate::validate` at the end of `build()`.  Carries
    `lift_addr: Option<u64>` for centralised lift-time fingerprint
    attribution.  **`FunctionBuilder::new` accepts the thin
    `FunctionBuilderCC` plain-data struct, NOT `target::BuiltCallingConvention`**
    (V6.B fix).
  - `FunctionBuilderCC` — plain-data struct in `strider-ir` (NEW in v2).
    Contains only the fields `FunctionBuilder` actually consumes:
    `ret_stack_pop: i64`, `no_memory_clobber: bool`, callee-saved and
    clobber varnode lists.  Cuts the `strider-ir → target` back-edge.
    `strider-lift::target` provides
    `impl From<BuiltCallingConvention> for FunctionBuilderCC`.
  - `ReadOnlyMemory` trait — in `strider-ir` (NEW in v2, V6.A fix).
    Defines `read_bytes(addr, buf) -> Result<()>` only; no
    binary-format knowledge.  Concrete impls (`ElfFileMemReader`, etc.)
    live in `strider-binary`.  Cuts the `opt → reader` back-edge — the
    `LoadReadOnly` pass now takes any `&dyn ReadOnlyMemory`.
  - `NodeOutputKind` — `Control`, `Memory`, `PhiToken`, or
    `OutputType(NodeOutputType)`.
  - `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U80`
    (x87 80-bit extended), `U128`, `U256`, `U512`; floats `F32`, `F64`,
    `F80`.  Wide types (`U256` / `U512`) are stored via
    `IntConstWide(WideConstId)` interned in `Graph::wide_consts`;
    `IntConst(u128)` rejects them.
  - `walk::walk_graph(graph, entry)` — traversal that follows both
    backward-data and forward-control edges.  Used by the validator and
    several passes.
  - `node_signature::{ExpectedOutputKind, expected_signature}` — single
    source of truth for expected input/output slot kinds per `NodeKind`.
  - `validate::validate(&graph, entry) -> Result<(), ValidationErrors>` —
    whole-graph validator with three layers.  **The Layer-C
    asm-fingerprint check is always-on in v2 (G3)** — every reachable
    non-exempt node MUST carry ≥1 fingerprint.  `ValidateOptions {
    check_asm_fingerprints: true }` is retained as a no-op for
    backwards source compatibility but the flag has no effect.  Layers:
    - **Layer A:** per-node local typing against `expected_signature`
      (scoped to nodes reachable via `walk_graph`).
    - **Layer B:** bidirectional use-list consistency.
    - **Layer C:** graph-level invariants (Entry/InitialMemory
      uniqueness, ControlState predecessor kinds, phi token ownership
      & per-predecessor arity, **always-on asm-fingerprint check**).
    - Aggregates all errors into a `ValidationErrors` bundle rather
      than failing fast.
  - `test_helpers` module (gated by `test-utils` feature) — `TestGraph`
    helper that auto-stamps sentinel asm fingerprints
    (`0xDEAD_BEEF_0000_0000 | counter`) on every created node so
    mock-graph tests stay green under the always-on Layer-C check.
  - `egraph_adapter` module (NEW in v2, Phase 1 Task 1.5) —
    acyclic-value-slice egraph adapter.  Phis-as-opaque-leaves design
    per V1 verification.  Generic over `<A: egg::Analysis>`.  Exposes
    `EGraphAdapter::from_graph` and `extract_into_graph` for the
    value-only subgraph round-trip.  Hand-rolled `egg::Language` impl
    (`StriderLang`) — does NOT use `egg::define_language!` because
    payloads (`rsleigh::Vn`, `NodeOutputType`, op enums) don't derive
    `FromStr + Display`.  Memory chain, control edges, and `Store` /
    `StackStore` / `StackStorePhi` are structurally copied — never
    enter the egraph.  Multi-output nodes (`Call`, modeled `CallOther`)
    are per-value-output opaque leaves with `NodeOutputId.as_u32()`
    payloads.

- **`strider-lift`** — Binary → IR.  Absorbs v1's `target`,
  `pcode-lift`, and `cfg` crates plus the per-region driver
  (`process_insn` + `set_lift_addr` funnel from v1's
  `strider::insn::mod`).  Layered modules:

  - `target` — Re-exported from the standalone `crates/target/` (the
    cycle-break rationale is documented in
    `strider-lift::target`).  Owns:
    - `SleighArch` — `.sla` + `.pspec` + `Endianness`.  Presets:
      `x86_64`, `x86`, `aarch64` / `aarch64be`, `arm` / `arm_be` /
      `arm_thumb`, `mipsbe32` / `mipsle32` / `mipsbe64` / `mipsle64`,
      `ppc32be` / `ppc32le` / `ppc64be` / `ppc64le`.
    - `CallingConvention` / `BuiltCallingConvention` — same surface as
      v1.  Userland presets: `x86_cdecl`, `x86_64_systemv`,
      `x86_64_all_preserving`, `aarch64_aapcs64`, `arm_aapcs`,
      `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`,
      `powerpc64_elf_v2`.  Linux kernel: `*_linux_kernel`.  Linux
      syscall: `*_linux_syscall` (set `syscall_number_reg_name`).  The
      link-register-as-callee-saved tradeoff (AArch64 `x30`, ARM `lr`,
      PowerPC `LR`) is preserved — needed for the indirect-branch
      resolver's `LinkRegister` arm.
    - `target::call_other_abi::classify(preset, name)` — CallOther
      classification (`NoOp` / `NoReturn` / `Call(CallOtherAbi)`)
      consumed by both `cfg::region_builder` (trap-region termination)
      and `strider-lift::pcode_lift::IrStrider::handle_call_other`.
      `ArchPreset` arrives via `cfg::Builder::for_arch(arch, …)`.  See
      specs `docs/superpowers/specs/2026-05-05-callother-classification-design.md`
      (v1) + `2026-05-06-callother-precise-abi-design.md` (v2 — current).
    - `From<BuiltCallingConvention> for FunctionBuilderCC` (V6.B
      adapter from the thin strider-ir struct to the rich
      strider-lift type).
  - `pcode_lift` — Pure value-producing pcode → IR lifter
    (`ValueLifter::lift(insn) -> Result<bool>`).  Owns the
    register-aliasing logic (`vn_io.rs`).  See "Register Aliasing"
    section below.
  - `cfg` — Builds a Control Flow Graph (`Cfg<R>`) from a binary using
    rsleigh.  Uses `petgraph::StableDiGraph` internally.  Bounded-lift
    semantics (`function_max_size`), `is_addr_tail_call`, and
    `RegionBuilder::build` all unchanged from v1.  **NEW in v2:**
    `IndirectTargetResolver` callback trait (V6.A fix for G9) — the
    cfg `Builder` calls back into `strider-analyze` for indirect-branch
    resolution via a trait object instead of statically importing the
    opt pipeline.  Severs the cfg → opt back-edge.
  - `lifter` — `Lifter::region(addr)` per-region facade (Phase 2 Task
    2.4).  Wraps the v1 `Builder` + `DecodeCache`; preserves
    sequential-within-region decoding (per V3 — `rsleigh::Sleigh`
    carries context-register state through `&mut self`, so
    out-of-order per-insn lifting across regions is NOT safe).
  - `region_driver` — The `process_insn` + `set_lift_addr(Some(addr))
    … set_lift_addr(None)` funnel that was at v1
    `crates/strider/src/strider/insn/mod.rs`.  Centralises lift-time
    fingerprint attribution.

- **`strider-analyze`** — Optimization + pattern queries + orchestration.
  Absorbs v1's `opt`, `pattern`, the `strider` orchestrator, the
  indirect-resolver helpers, and `GraphRewriter`.  Surface:

  - `opt` module — Optimization passes.  **V2 has both v1 imperative
    passes AND new egg-based passes alongside each other** (see "V2
    Egraph Optimizer" section below).
  - `pattern` module — Pattern DSL.  Same surface as v1's `pattern`
    crate (Pat / PatKind / Capture / Matcher / Match / fluent
    builders).  10 of 14 builders are now emitted by the
    `strider-pattern-macros` proc-macro from one `*Def` struct.  4
    stay hand-written: `FunctionArgPat` (enum-dispatch source) plus
    the three binary-op builders (`IntBinaryOpPat`, `BoolBinaryOpPat`,
    `FloatBinaryOpPat`) whose required-construction shape doesn't fit
    the macro's field-based model.
  - `orchestrator` — `strider_analyze::run(config) -> Result<BuiltFunctionGraph>`.
    The v1 fixed-point loop with `Decision { FixedPoint, StableOnly,
    Rebuild }`, used by the back-compat `strider::run` re-export.
  - `orchestrator_salsa` (NEW in v2, Phase 3 Task 3.9) — Salsa-based
    orchestrator.  Pinned to `salsa = "0.26.2"` (matches rust-analyzer
    / Astral ruff pin per V2 verification).  See "V2 Salsa
    Orchestrator" section below.
  - `indirect_resolve` module — `classify_anchor` /
    `apply_link_register` / `apply_tail_call` and the producer-shape
    classifier free functions (`classify_jump_table`,
    `classify_stack_array`, `ResolvedTargets { LinkRegister, Single,
    Multiple }`).  Same surface as v1; called directly by the
    orchestrator's `LoopState`.
  - `indirect_resolver` — The trait object that satisfies
    `strider-lift::cfg`'s `IndirectTargetResolver` callback (V6.A
    plumbing for G9).
  - `Strider` / `AnalyzeOptions` / `AnalyzeOutcome` /
    `RegionLiftHandles` — per-iteration handle types (re-exported by
    the back-compat `strider::*` path).  `RegionLiftHandles` is being
    phased out by the Salsa orchestrator (G8) but still exists for
    the v1-compatible non-Salsa path.
  - `GraphRewriter` — pattern-rewrite façade over `pattern::rewrite_rule`.
  - `Strider::build_optimizer_pipeline()` — full v1-compatible
    pipeline: `opt::default_pipeline()` + `StackStoreDetect` +
    `StackLoadForward` (both fixed-point) + `CallStackArgCollect` and
    `FunctionArgDetect` as post-passes.
  - `Strider::build_stable_optimizer_pipeline()` — stable v1-compatible
    pipeline (`ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`,
    `IfCondInversion`, `StackStoreDetect`, `StackLoadForward`,
    `FunctionArgDetect` post-pass).
  - `Strider::build_destructive_optimizer_pipeline()` — destructive
    v1-compatible pipeline (`RedundantPhis`, `DeadBranchElimination`,
    `CallStackArgCollect` post-pass).
  - `opt::PipelineV2` (NEW in v2) — single interleaved
    destructive+nondestructive fixed-point loop per Section A of the
    plan.  Runs the egg saturate step + control-graph cleanup
    (`RedundantPhis` + `DeadBranchElimination`) in one outer loop
    instead of the v1 stable-then-destructive split.

- **`strider-pattern-macros`** (NEW in v2) — Proc-macro crate
  (`proc-macro = true`).  Emits a Rust pattern builder + Pat
  constructor + PyO3 `#[gen_stub_pyclass] #[pyclass]` mirror + stub-gen
  methods from one annotated `*Def` struct.  Phase 4 deliverable —
  cut the 16-type Rust×Python pattern duplication.  The hand-written
  reference shape it must mirror is documented in
  `crates/strider-pattern-macros/EMISSION_SPEC.md` and the working
  example is in `crates/strider-py/src/pattern_reference.rs`
  (the `PyStackStorePat` mirror produced before the macro existed).
  V4 verification constraint: `#[gen_stub_pyclass]` MUST precede
  `#[pyclass]`; `#[gen_stub_pymethods]` MUST precede `#[pymethods]`.
  10/14 builders migrated; 4 stay hand-written.

- **`strider-py`** — Python bindings (PyO3 + maturin + abi3-py39).
  Phase 5 promotes this to the top-level `strider` maturin crate; the
  rename is pending in Phase 5 Task 5.1.  Surface:

  - **High-level API** (Phase 5 Task 5.2 deliverable):
    `strider.load(path).analyze(fn).find(pattern)`.  Auto-detects arch
    from ELF `e_machine`.  `Analysis.fingerprint(node) -> list[int]`
    is the proof-of-correctness helper.
  - **Low-level API** (back-compat with v1 strider-py):
    `SleighArch`, `CallingConvention`, `MemoryMap`, `MemReader`,
    `ReadOnlyMemory`, `Sleigh`, `build_cfg`, `Strider`, `Graph`,
    `OptimizerPipeline`, plus `strider.run(arch, cc, mem, entry, ...)`.
  - `strider.opt` per-pass classes; `strider.pattern` full mirror of
    the Rust pattern crate.  Cross-pattern joins on shared captures:
    `Graph.find_all_requirements([pat1, pat2, …])`.  Stack-offset
    accessors on `Match`.  Asm-fingerprint accessor:
    `match.asm_fingerprint(c) -> list[int]`.
  - `MemoryMap.apply_elf_relocations(path)` applies dynamic relocations
    and autoloads any missing site sections (e.g. `.got.plt`).
  - `MemReader` and `ReadOnlyMemory` are subclassable Python ABCs.
  - Recommended dev workflow uses **uv** (PEP 735 dependency groups in
    `pyproject.toml`): `uv sync --group dev` → `uv run maturin develop`
    → `uv run pytest`.  `maturin build --release` produces a local
    abi3 wheel under `target/wheels/`.
  - Tests live under `crates/strider-py/tests/python/`.  Errors land
    as `strider.errors.{StriderError, LiftError, ReaderError,
    PatternError, RewriteError}` plus the typed
    `UnresolvedIndirectBranchError`.
  - See `docs/superpowers/specs/2026-05-01-strider-py-design.md` for
    the v1 design (still applies to v2 low-level surface); the
    high-level API is sketched in Phase 5 of the v2 rewrite plan.

### V2 Egraph Optimizer

V2 introduces **9 egg-based optimization passes** alongside v1's imperative
passes.  The egg passes operate on the **acyclic value-subgraph slice**
per V1 verification (`docs/superpowers/specs/2026-05-17-verification-results.md`):

- **Phi nodes are opaque leaves.**  Egg never sees a phi cycle; phi outputs
  enter the egraph as `Opaque(NodeOutputId)` leaves.
- **Control flow is pinned.**  `Control` ports on `If`, `ControlState`,
  `Return`, `Call`, `CallOther` never enter the egraph.
- **Memory chain is pinned.**  `Load` value outputs enter; the memory
  token threading through `Store` / `Load` / `Call` does not.
- **Bypass `egg::Runner`** — drive `EGraph::rebuild` + manual
  `Rewrite::search` / `apply`.  Runner's defaults (`iter_limit=30`,
  `node_limit=10_000`, `time_limit=5s`) are 100× oversized for ~100-node
  slices.

The 9 egg passes (in `crates/strider-analyze/src/opt/`):

| Pass | File | Notes |
|---|---|---|
| `ConstantFoldEgg` | `constant_fold_egg.rs` | All 5 of v1's rule groups: const-eval, identity, bool+float, reassoc+AND-mask, bitcast+extend. |
| `KnownBitsEgg` | `known_bits_egg.rs` | Uses `egg::Analysis::Data` for the bit lattice (zeros / ones / unknown). |
| `FlagCmpCanonicalizeEgg` | `flag_cmp_canonicalize_egg.rs` | Pure rewrite rules. |
| `IfCondInversionEgg` | `if_cond_inversion_egg.rs` | Faithful direct port; `If` nodes are pinned outside the egraph, the rewrite operates on the `cond` value-slice. |
| `StackStoreDetectEgg` | `stack_store_detect_egg.rs` | Uses `StackOffsetAnalysis::Data` per-eclass to track SP-relative offset. |
| `StackLoadForwardEgg` | `stack_load_forward_egg.rs` | Same analysis surface. |
| `LoadReadOnlyEgg` | `load_readonly_egg.rs` | Resolves `Load(IntConst)` via `&dyn ReadOnlyMemory`. |
| `CallStackArgCollectEgg` | `call_stack_arg_collect_egg.rs` | Post-pass (runs once after convergence). |
| `FunctionArgDetectEgg` | `function_arg_detect_egg.rs` | Post-pass. |

**`PipelineV2`** (in `crates/strider-analyze/src/opt/pipeline_v2.rs`)
runs an interleaved destructive+nondestructive fixed-point loop:

```
loop {
    saturate_egraph(value_slice)        // egg: ConstantFold+KnownBits+…
    extract_canonical(value_slice)
    cleanup = run_control_simplification()  // RedundantPhis + DeadBranchElim
    if !saturation_added_anything && !cleanup.changed { break }
}
```

This collapses v1's stable/destructive pipeline split (Finding G4): the
Salsa orchestrator + egraph saturation make the per-iteration
`RegionIndex` pinning obsolete, so destructive cleanup can interleave with
non-destructive saturation in one outer loop.

**V1 passes still exist in `crates/strider-analyze/src/opt/`** under their
original module names (`constant_fold/`, `known_bits/`,
`flag_cmp_canonicalize/`, `if_cond_inversion/`, `redundant_phis/`,
`dead_branch/`, `load_readonly/`, `stack_store/`, `stack_load_forward/`,
`function_args/`, `indirect_branch_resolve/`) and remain the v1-baseline
ground-truth path.  Phase 6 will deprecate them once V2 parity is
confirmed across all snapshot fixtures.

### V2 Salsa Orchestrator

`crates/strider-analyze/src/orchestrator_salsa.rs` (Phase 3 Task 3.9).
Pinned `salsa = "0.26.2"` (matches rust-analyzer / Astral ruff per V2
verification — note that `salsa-3.0` does NOT exist).

**Query graph:**

- `Binary` — `#[salsa::input]` with `Durability::HIGH`.  Set once per
  analysis.
- `IndirectTargets` — `#[salsa::input]` with `Durability::LOW`.  Grows
  monotonically as the driver classifies more anchors.
- `optimized_function` — `#[salsa::tracked]` query.  Runs the full lift
  + stable + destructive optimizer pipeline for the current
  `(Binary, IndirectTargets)` pair and returns an `Arc<BfgEntry>`.
  Marked `no_eq` because `BuiltFunctionGraph` has no equality
  (interned arenas, side-tables).

**External fixed-point loop** (per V2 verification — NOT salsa's
`cycle_fn`, whose 200-iter cap and monotone-domain requirement don't
fit our shape):

1. Query `optimized_function`.
2. Walk the returned BFG for unresolved `IndirectBranch` placeholders.
3. Classify each via the existing `indirect_resolve` helpers.
4. For any new resolutions, mutate the `IndirectTargets` input and loop.
5. Stop when classification produces no new targets.

**Phase 3 delivery scope:** wrapper-level memoization only — a repeat
call with the same `IndirectTargets` returns the cached BFG with zero
lift / opt work.  Per-region incrementality (split lift into per-region
tracked queries) is deferred to Phase 6.

The non-Salsa `strider_analyze::run` path (used by the back-compat
`strider::run` re-export and the v1 baseline snapshots) is unchanged.

### V2 Generalizations

Six wins from the read-only generalization audit
(`docs/superpowers/specs/2026-05-17-verification-results.md` and audit
findings G1–G15 in the plan) that landed in the rewrite:

- **G3 — Always-on Layer-C asm-fingerprint validation.**  `validate()`
  unconditionally flags reachable non-exempt nodes with empty
  fingerprints.  `TestGraph` (in `strider-ir::test_helpers`) auto-stamps
  sentinel addresses so existing mock-graph tests stay green.  Closes
  the v1 hazard where opt passes could silently produce
  fingerprint-empty output.
- **G6 — `NodeKind::is_commutative()` as single-source-of-truth.**
  Replaces v1's four separate `is_commutative_*` functions and
  per-builder `BinaryOpKind::is_commutative` methods.  Adding a
  commutative op is a single-site edit.
- **G8 — `RegionLiftHandles` / `RegionIndex` retired by Salsa.**  V1's
  8-field handle + per-iteration index rebuild is replaced by Salsa's
  `region_ir(addr)` query.  Eliminates the desync hazard if a pass
  rewrites a `ControlState` or `MemPhi` `NodeId` mid-iteration.  The
  non-Salsa path still uses `RegionLiftHandles` (re-exported from
  `strider-analyze`).
- **G9 — `IndirectTargetResolver` callback trait.**  Severs the v1
  `cfg → opt` back-edge.  `strider-lift::cfg::Builder` calls back into
  `strider-analyze`'s resolver through a trait object instead of
  importing the opt pipeline.  V6.A fix.
- **V6.A — `ReadOnlyMemory` trait extraction.**  Moved from `reader`
  into `strider-ir`.  Concrete impls (`ElfFileMemReader`, etc.) stay
  in `strider-binary`.  `opt::LoadReadOnly` no longer depends on the
  reader crate.
- **V6.B — `FunctionBuilderCC` plain-data struct.**  Thin DTO in
  `strider-ir` containing only the fields `FunctionBuilder` consumes.
  `strider-lift::target` provides
  `impl From<BuiltCallingConvention> for FunctionBuilderCC`.  Cuts the
  `ir → target` back-edge.

Findings G1, G2, G4, G5, G7, G10–G15 are partially landed or deferred —
see the plan's "Generalization Audit Findings" section for the full
catalogue.

### IR Node Model

The IR is a sea-of-nodes graph where each `Node` has typed inputs
(`NodeOutputId` references) and outputs.  The `expected_signature` table
in `crates/strider-ir/src/node_signature.rs` is the single source of
truth for every node's input/output shape.  Node kinds, grouped:

- **Initial state:** `Entry`, `InitialMemory`, `InitialVar(Vn)`
- **Region / join:** `ControlState` (variadic Control inputs; outputs
  `Control` + `PhiToken`), `VarPhi(Vn)` (SSA φ for varnode `Vn` at a
  join point), `MemPhi` (φ for the memory token)
- **Conditional branch:** `If` (outputs true/false `Control` edges)
- **Indirect branch:** `IndirectBranch` (placeholder consumed by the
  orchestrator's indirect-resolution loop; rewritten in place by
  `apply_link_register` / `apply_tail_call` and replaced by the
  jump-table classifier on CFG rebuild)
- **Calls / returns:** `Call` (clobbers caller-saved registers and
  memory; variadic args), `CallOther { user_op_id }`, `Return`
  (variadic return values)
- **Memory:** `Load(VnSpace)`, `Store(VnSpace)`; after
  `StackStoreDetect`: `StackStore { space, offset }`, `StackStorePhi
  { space }` (per-predecessor offsets in `Graph::stack_phi_offsets`)
- **Integer:** `IntConst(u128)`, `IntConstWide(WideConstId)` (U256/U512
  — interned in `Graph::wide_consts`), `IntUnaryOp` (`BitNot` for
  `~x`, `Neg` for `-x`), `IntBinaryOp` (no `Sub`; lifter lowers to
  `Add(_, Neg(_))`), `IntCmpOp` (`Equal`, `Less`, `Sless`, `Carry`,
  `Scarry`, `Sborrow`; no `LessEqual` / `SlessEqual` / `Borrow`),
  `Truncate`, `Extend(ExtendOp)`, `Popcount`, `Lzcount`, `CastToInt`
- **Boolean:** `BoolConst(bool)`, `BoolUnaryOp`, `BoolBinaryOp`,
  `CastToBool`
- **Float:** `FloatConst(u64)` (bits), `FloatUnaryOp`, `FloatBinaryOp`
  (`Add` / `Mul` / `Div`; no `Sub`, lifter lowers to `Add(_, Neg(_))`),
  `FloatCmpOp` (`Equal`, `Less`; no `NotEqual` / `LessEqual` — both
  are lift-time-lowered shapes; `FLOAT_NAN(x)` is also lowered to
  `BoolNeg(FloatEqual(x, x))`)
- **Float / int conversions:** `IntToFloat`, `FloatToInt`,
  `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`.
  The cast ops accept any value-typed input; `FloatToFloat` is
  float→float only.
- **Opaque / user-defined:** `SegmentOp { op_id }`, `CPoolRef`, `New`

### Pattern DSL

The `strider-analyze::pattern` module exposes the same surface as v1's
`pattern` crate.  Pat / PatKind / Capture / Matcher / Match — see the
v1 documentation history for full builder details.  Key v2 changes:

- 10 of 14 builders are emitted by `strider-pattern-macros` from a
  single `*Def` struct.  Adding a field to one of these patterns is a
  single-source edit; the Rust builder + the PyO3 mirror update
  together.
- 4 builders stay hand-written: `FunctionArgPat` (enum-dispatch source)
  and the three binary-op builders (`IntBinaryOpPat`,
  `BoolBinaryOpPat`, `FloatBinaryOpPat`) whose required-construction
  shape doesn't fit the macro's field-based model.
- **Lift-time canonicalisation** is unchanged from v1: `IntSub` →
  `Add(_, Neg(_))`; `IntLessEqual` → `BoolNeg(IntLess(_, _))` (swap
  args); `IntNotEqual` → `BoolNeg(IntEqual(_, _))`; `FloatSub` →
  `FloatAdd(_, Neg(_))`; `FloatNotEqual` → `BoolNeg(FloatEqual(_, _))`;
  `FloatLessEqual` → `Or(FloatLess(_, _), FloatEqual(_, _))` (NaN-
  aware); `If(BoolNeg(C)){A}{B}` → `If(C){B}{A}` (via
  `opt::IfCondInversion`).
- **Commutative matching:** `add`, `mul`, `and`, `or`, `xor` (and bool
  equivalents), `int_cmp(Equal/Carry/Scarry)`, and `float_cmp(Equal)`
  automatically try both operand orderings.

### Register Aliasing

Overlapping registers (x86 `rax`/`eax`/`ax`/`al`/`ah`, AArch64 `q0`/`d0`/`s0`,
x87 `ST*`, etc.) are handled by `strider_lift::pcode_lift::ValueLifter::{read_vn,
write_vn}` (in `crates/strider-lift/src/pcode_lift/vn_io.rs`).  All reads
and writes go through the largest containing register, with shift/mask
operations inserted for sub-register slices.  `find_largest_fitting_register`
is the entry point.  `vn_mask` enumerates supported widths: 1, 2, 4, 8,
10 (x87 80-bit extended), 16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes.
Widths > 16 use a degraded `u128::MAX` mask; the wide-container guard
rejects sub-register aliasing within > 16-byte containers with a clear
error.

This logic is structurally unchanged from v1 — the move from
`pcode-lift` to `strider-lift::pcode_lift` is purely path renaming.

### External Dependency: rsleigh

`rsleigh` is a local path dependency at `../rsleigh` (not in this
workspace).  It wraps GHIDRA's Sleigh specification to lift machine
instructions to p-code (`rsleigh::Insn` with `Opcode`).  Key types used:
`Vn` (varnode — register/memory/const/unique), `VnSpace`, `MemReader`.

**V3 verification correction:** `Sleigh::lift_one(&mut self, addr)` is
**NOT** stateless across calls — the `&mut self` carries
context-register state (ARM Thumb mode, x86 segment selectors, MIPS16
mode).  Decoded instructions can modify this context (ARM `bx lr`
switches Thumb/ARM mode).  Decode buffers reset per call, but
context-register state persists.  Practical consequence: per-region
sequential decoding (v1's `RegionBuilder::build` invariant) MUST be
preserved.  v2's `Lifter::region(addr)` honours this.  Across regions,
assume context state is fixed per function entry (the v1 implicit
invariant).
