# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Skill notes

This workspace is **Rust-only** (plus thin Python bindings via PyO3 in
`crates/strider-py/`).  When the `code-simplifier` skill (or any other
plugin-provided skill) emits JS/TS guidance ("use ES modules", "prefer
function over arrow functions", "follow React component patterns"),
ignore it — the project-relevant guidance is the Rust ecosystem
conventions established by clippy + the workspace lints in `Cargo.toml`.

For generating Python pattern code, prefer the project-local skill at
`.claude/skills/strider-py-pattern/SKILL.md` which has the full
strider-py builder cheat sheet + lift-time canonicalisations.

## Build & Run Commands

```bash
# Build all crates
cargo build --workspace

# Run the main example (reads fixtures/out/x86/arithmetic.elf::add — must be
# built first from fixtures/Makefile — and dumps cfg.html, graph.html, and
# graph-opt.html in the workspace root).  Lives at
# crates/strider-analyze/examples/orchestrator_demo.rs.
cargo run -p strider-analyze --example orchestrator_demo

# Dump per-arch IR cmp shapes (debug helper for the FlagCmpCanonicalize spec).
cargo run -p strider-analyze --example dump_arch_cmps

# Run all tests
cargo test --workspace

# Run a single test
cargo test --package <crate-name> <test_name>

# Lint
cargo clippy --workspace
```

## Architecture Overview

Strider is a Rust workspace binary-analysis tool that lifts a native binary
function to a sea-of-nodes IR and exposes it for arbitrary pattern queries
from Rust or Python.  The pipeline is:

**Binary → CFG → IR → Optimizations → Pattern Queries (Rust / Python)**

### Crate Inventory

The workspace splits into **three generic utility crates** (unprefixed —
not tied to strider's domain) and **seven strider-specific crates**:

Generic helpers:

- `dot` — Graphviz / dark-themed HTML renderer.
- `entity-utils` — `cranelift-entity` helpers (`DenseEntitySet`,
  `Worklist`).  Use these instead of `std::collections::HashSet` /
  `HashMap` when keying by `NodeId` / `NodeOutputId` and friends.
- `graphwalk` — generic preorder / postorder graph traversal.  Test code
  under `graphwalk/tests/common/` hosts the `graphmock` DSL for spinning
  up synthetic graphs in unit tests.

Strider crates:

- `strider-ir` — the sea-of-nodes IR.
- `strider-target` — pure target descriptions (`SleighArch`,
  `CallingConvention`, `BuiltCallingConvention`, `CallOther` ABI table).
- `strider-reader` — ELF loader and `ReadOnlyMemory` backend.
- `strider-lift` — value-producing pcode→IR lifter **and** CFG builder.
- `strider-analyze` — orchestrator, optimizer pipeline, pattern matcher,
  indirect-branch resolver, graph rewriter.
- `strider-pattern-macros` — proc-macro that emits PyO3 mirror
  builders from one annotated `*Def` struct (the Rust-side
  `pattern::*Pat` builders remain hand-written in `strider-analyze`).
- `strider-ir-test-utils` — `make_empty_fn` / `RegisterSet` builders
  and asm-fingerprint-stamping helpers shared by every crate's tests.
- `strider-py` — PyO3 bindings (`maturin develop` builds a wheel).

### Crate Dependency Flow

```
                strider-ir
                   ↑
        ┌──────────┼─────────────┐
        │          │             │
  strider-target  strider-reader │
        ↑          ↑             │
        └────┬─────┘             │
             │                   │
        strider-lift             │
             ↑                   │
      strider-analyze ───────────┘
             ↑
        strider-py

  strider-pattern-macros (proc-macro; consumed by strider-py to emit
    Py*Pat mirrors of the hand-written Rust Pat builders)

  strider-ir-test-utils (dev-dep; depends on strider-ir)

  rsleigh (external path dep at ../rsleigh — Sleigh / GHIDRA p-code lifter)
```

All edges point upward; there are no back-edges.  `strider-lift` calls
back into `strider-analyze`'s indirect-branch resolver through the
`strider_lift::cfg::IndirectResolverFn` callback type (an `Arc<dyn Fn>`
alias) — the resolver function lives in `strider-analyze` and is
installed on the cfg builder via `Builder::with_indirect_resolver`,
so the resolver-bearing dependency stays one-way.

### Key Crates

- **`strider-ir`** — the core sea-of-nodes IR graph and everything that
  doesn't depend on a target.  Public surface:

  - `Graph` — stores `NodeId`, `NodeOutputId`, `NodeInputId` via
    `cranelift-entity` PrimaryMaps.  Cacheable nodes are deduplicated
    by `(NodeKind, inputs, output_kinds)`.  Ancillary state lives in
    three forms on the `Graph` itself:
    - **Scalars on `Graph`:** `entry: Option<NodeId>` and
      `cc_metadata: Option<CcMetadata>` (the latter carries the
      variable map, call-clobbered list, ret-val regs, call-other
      clobbered list, and `no_memory_clobber` flag).  Both populated
      by `FunctionBuilder::build`.
    - **`PrimaryMap`:** `wide_consts` (`WideConstId → WideConstStorage`,
      consulted by `IntConstWide(WideConstId)` nodes for U256 / U512
      payloads that don't fit in the regular `IntConst(u128)`).
    - **Side-table registry on `Function` (`SecondaryMap<NodeId, _>`):**
      `stack_offsets` (SP-relative offset metadata for Store/Load
      populated by `StackOffsetDetect`), `call_other_names`,
      `asm_fingerprints`, `call_clobbered_overrides`, `phi_var_tag`
      (per-node `Option<Vn>` source-varnode tag for `Phi` nodes),
      `call_stack_arg_offsets_overrides`, and `arg_index_to_nodes`
      (populated by `FunctionArgDetect`).  `Graph` itself only
      holds structural state (nodes, edges, dedup cache,
      `wide_consts`); per-function overlay state lives on `Function`.
  - `FunctionBuilder` — builds the IR with SSA-like variable tracking.
    Variables map `rsleigh::Vn` → `VarId`.  Each region gets a
    `Region` node and per-variable `Phi` nodes whose source
    varnode tag is recorded in the `Graph::phi_var_tag` side-table.
    Carries `lift_addr: Option<u64>` for centralised lift-time
    fingerprint attribution.  `FunctionBuilder::new` accepts
    `&strider_target::BuiltCallingConvention` directly.
  - `FunctionBuilder::build` returns the populated `Function` directly —
    `entry` and `cc_metadata` are `Some(_)` after `build` succeeds.
  - `ReadOnlyMemory` trait — `read(&self, addr: u64, size: usize) ->
    Option<u64>`; returns up to 8 bytes as a target-endian-decoded
    `u64` (impl byte-swaps per arch endianness — e.g.
    `ElfFileMemReader` consults `is_little_endian`), or `None` for
    unmapped addresses / sizes > 8.  Blanket impls for `Arc<T>` and
    `Box<T>`.  Defined here (not in `strider-reader`) so optimiser
    passes can depend on the trait without back-edging through the
    reader crate.  Concrete impls live in `strider-reader`.  The
    optimizer's `LoadReadOnly` takes `&dyn ReadOnlyMemory` so it
    doesn't depend on the reader crate.
  - `NodeOutputKind` — `Control`, `Memory`, `PhiToken`, or
    `OutputType(NodeOutputType)`.
  - `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U80`
    (x87 80-bit extended), `U128`, `U256`, `U512`; floats `F32`, `F64`,
    `F80`.  Wide types (`U256` / `U512`) are stored via
    `IntConstWide(WideConstId)` interned in `Graph::wide_consts`;
    `IntConst(u128)` rejects them.
  - `walk::walk_graph(graph, entry)` (`pub(crate)`) — preorder
    traversal that follows both backward-data and forward-control
    edges.  Used by the validator and several internal passes; not
    exposed to downstream crates.
  - `node_signature::{ExpectedOutputKind, expected_signature}` — single
    source of truth for expected input/output slot kinds per `NodeKind`.
  - `validate::validate(function: &Function, entry: NodeId) -> Result<(), ValidationErrors>`
    — whole-graph validator split into three groups by file:
    - `validate/local_typing.rs` — per-node local typing against
      `expected_signature` (scoped to nodes reachable via `walk_graph`).
    - `validate/use_list_consistency.rs` — bidirectional consistency
      between inputs and outputs' use-lists.
    - `validate/graph_invariants.rs` — whole-graph rules:
      Entry/InitialMemory uniqueness, Region predecessor kinds,
      phi-token ownership and per-predecessor arity, FunctionArg
      uniqueness, wide-const consistency, and the always-on
      asm-fingerprint check (every reachable non-exempt node MUST carry
      ≥1 fingerprint).
    - Errors are aggregated into a `ValidationErrors` bundle rather than
      failing fast.
  - `graph_dot` module — IR-specific Graphviz / HTML rendering on top
    of the generic `dot` crate.
  - **Asm-fingerprint side-table** (`Graph::asm_fingerprints`) — every
    `NodeId` carries a sorted-deduplicated list of machine-instruction
    addresses identifying the asm insns whose lifting (or subsequent
    rewrite) contributed to the node's value.  Proof-of-correctness aid:
    when a pattern query captures a value node, its fingerprint names
    the asm instructions that explain the match.  The contract is
    **superset-only**: passes may *grow* fingerprints but must never
    shrink them or replace a node with one whose fingerprint omits an
    ancestor's addresses.  Two structurally identical (cacheable) nodes
    share one entry that is the **union** of every contributor's
    address.  Region / phi / initial-state kinds (`Entry`,
    `InitialMemory`, `InitialVar`, `Region`, `MemPhi`, `Phi`) are
    exempt from the non-empty check.  Public API on `Graph`: `asm_fingerprint(id)`,
    `set_asm_fingerprint`, `extend_asm_fingerprint`,
    `extend_asm_fingerprint_from`.

- **`strider-target`** — pure target descriptions, no Sleigh or IR
  dependencies.

  - `SleighArch` — `.sla` + `.pspec` + `Endianness`.  Presets: `x86_64`,
    `x86`, `aarch64` / `aarch64be`, `arm` / `arm_be` / `arm_thumb`,
    `mipsbe32` / `mipsle32` / `mipsbe64` / `mipsle64`, `ppc32be` /
    `ppc32le` / `ppc64be` / `ppc64le`.
  - `ArchPreset` — closed enum threaded into
    `strider_lift::cfg::Builder::for_arch` and `CallOther`
    classification.
  - `CallingConvention` / `BuiltCallingConvention` — names-of-registers
    DSL and its register-resolved counterpart.  Userland presets:
    `x86_cdecl`, `x86_64_systemv`, `x86_64_all_preserving`,
    `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`,
    `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`.  Linux
    kernel variants for the architectures that need them:
    `x86_linux_kernel`, `x86_64_linux_kernel`, `aarch64_linux_kernel`,
    `arm_linux_kernel`, `mips_linux_kernel_o32`,
    `mips_linux_kernel_n64`.  Linux syscall ABIs:
    `x86_linux_syscall`, `x86_64_linux_syscall`,
    `aarch64_linux_syscall`, `arm_linux_syscall`,
    `mips_linux_syscall_o32`, `mips_linux_syscall_n64`.  The
    link-register-as-callee-saved tradeoff (AArch64 `x30`, ARM `lr`,
    PowerPC `LR`) is preserved — the indirect-branch resolver's
    `LinkRegister` arm uses it.
  - `call_other_abi::classify(preset, name)` — CallOther classification
    (`NoOp` / `NoReturn` / `Call(CallOtherAbi)`) consumed by both
    `strider_lift::cfg::region_builder` (trap-region termination) and
    `strider_analyze::strider::PerRegionDriver::handle_call_other`.
    `ArchPreset` arrives via `cfg::Builder::for_arch(arch, …)`.
    `CallOtherAbi` carries `implicit_reads` / `implicit_writes` /
    `memory_edge` describing the ISA-fixed register-and-memory footprint
    beyond Sleigh's pcode-explicit args.
  - `PositionalArgLayout` — canonical positional-arg-layout DTO derived
    from a `BuiltCallingConvention` via
    `BuiltCallingConvention::positional_arg_layout()` (or
    `PositionalArgLayout::from_convention(&cc)`).  Single source of
    truth for positional argument slot order (register slots first,
    then stack slots at the convention's `stack_arg_offsets`); consumed
    by `FunctionArgDetect`, `CallStackArgCollect`, and `StackLoadForward`
    so each pass sees the same slot order.

- **`strider-reader`** — `ReadOnlyMemory` + `rsleigh::MemReader`
  backends.

  - `MemRegion` / `MemRegionsLookupTable` — generic region storage that
    any reader can compose.
  - `ElfFileMemReader` — loads an ELF object and serves both
    `rsleigh::MemReader` (for Sleigh instruction fetch) and
    `strider_ir::ReadOnlyMemory` from the same regions.
  - `apply_elf_relocations` / `apply_elf_relocations_autoload` — apply
    dynamic relocations; the `_autoload` variant lazily extends the
    region table with sections (e.g. `.got.plt`) that own relocation
    sites not yet covered.

- **`strider-lift`** — binary → IR.  Three layered modules:

  - `pcode_lift` — pure value-producing pcode→IR lifter
    (`ValueLifter::lift(insn) -> Result<bool>`).  Owns the
    register-aliasing logic (`vn_io.rs`).  See the "Register Aliasing"
    section below.
  - `cfg` — builds a Control Flow Graph (`Cfg<R>`) from a binary using
    `rsleigh`.  Uses `petgraph::StableDiGraph` internally.  Bounded-lift
    semantics (`function_max_size`), `is_addr_tail_call`, and
    `RegionBuilder::build` are the load-bearing primitives.  Exposes the
    `IndirectResolverFn<R>` callback type (an `Arc<dyn Fn>` alias) —
    the cfg builder calls back through the installed closure for
    indirect-branch resolution, so the cfg-to-analyze direction stays
    clean.  The only public construction
    path is `Builder::for_arch(arch, sleigh, addr, options)` so
    endianness and `ArchPreset` are derived from the arch atomically
    (the older `Builder::new` / `with_endianness` ctors silently
    defaulted `preset = X86_64` and have been removed).
  - `lifter::Lifter::region(addr)` — per-region facade wrapping the
    `Builder` + `DecodeCache`; preserves sequential-within-region
    decoding (Sleigh's `lift_one(&mut self)` carries context-register
    state, so out-of-order per-insn lifting across regions is not
    safe).
  - `region_driver::RegionDriver` — stateless `set_lift_addr` /
    `clear_lift_addr` funnel that wraps every per-instruction lift so
    asm-fingerprints stamp correctly.

- **`strider-analyze`** — optimization + pattern queries +
  orchestration.

  - `opt` module — optimization passes.  All passes implement
    `Optimizer`; the `OptimizerPipeline` runs a list of passes in a
    shared fixed-point loop and runs registered post-passes once after
    convergence.  `pipeline.run(graph, entry)` is the single entry
    point.  Built-in passes:
    - `ConstantFold` — constant evaluation, identities (`x+0→x`,
      `x^x→0`), AND-mask merging, etc.
    - `KnownBits` — bit-level zeros / ones lattice propagation.
    - `FlagCmpCanonicalize` — flag-tree → single `IntCmpOp` rewrite
      (AArch64 NZCV-style chains).
    - `IfCondInversion` — `If(BoolNeg(C)){A}{B}` → `If(C){B}{A}`.
    - `RedundantPhis` — eliminates `Phi` (tagged or anonymous) /
      `MemPhi` / `Region` with a single reachable predecessor.
    - `DeadBranchElimination` — removes `If(const)` branches and strips
      dead control edges.
    - `LoadReadOnly` — folds constant-address loads via
      `&dyn ReadOnlyMemory`.
    - `StackOffsetDetect` — annotates SP-relative `Store` / `Load`
      offsets in `Function::stack_offsets`; the unified memory chain
      is left intact.
    - `StackLoadForward` — forwards values from stack-tagged `Store`
      (via `Function::stack_offsets`) to subsequent same-offset `Load`.
    - `FunctionArgDetect` (post-pass) — canonicalises register / stack
      arg reads by populating the `Function::arg_index_to_nodes`
      side-table (carrier `NodeId` is `InitialVar` for register args,
      `Load` for stack args).  There is no `FunctionArg` `NodeKind`
      variant.
    - `CallStackArgCollect` (post-pass) — wires positional stack args
      into `Call` nodes.
    - Imperative peephole passes use `pattern::Matcher::find_all`
      rather than rolling their own matching.
  - `pattern` module — pattern DSL (`Pat` / `Capture` / `Matcher` /
    `Match` and fluent builders).  Cross-pattern joins on shared
    captures via `Matcher::find_all_requirements`.
  - `strider` module — `Strider`, `AnalyzeOptions`, `AnalyzeOutcome`,
    `RegionLiftHandles`, and `PerRegionDriver` (the per-region driver
    that converts a `Cfg` into the IR graph region by region).
    `Strider::build_optimizer_pipeline`,
    `build_stable_optimizer_pipeline`,
    `build_destructive_optimizer_pipeline` produce the three pre-canned
    pipelines.
  - `orchestrator::run(config) -> Result<Function>` — the
    canonical top-level entry, re-exported as
    `strider_analyze::run`.  Build the CFG, lift to IR, run the stable
    optimiser subset, drive the indirect-branch fixed-point loop
    (`Decision::FixedPoint` / `StableOnly` / `Rebuild`), then run the
    destructive subset once at the fixed-point exit.  `Config` carries
    the `Strider`, start address, `Sleigh`, optional ROM, function-size
    bound, per-target-address CC overrides, and a `compact` flag.
  - `opt::indirect_branch_resolve` module — free-function classifiers
    (`classify_anchor`, `classify_jump_table`, `classify_stack_array`)
    and in-place IR editors (`apply_link_register`, `apply_tail_call`),
    re-exported through `opt::mod.rs`.  `ResolvedTargets { LinkRegister,
    Single(u64), Multiple(Vec<u64>) }` lives in
    `strider_lift::cfg::builder::indirect_resolver` and is consumed by
    the orchestrator's fixed-point loop.  There is no `Optimizer`-
    implementing struct here — the orchestrator calls these directly,
    outside any pipeline.
  - `indirect_resolver` — `resolve_indirect_target` (free function),
    the cfg-time mini-IR resolver installed on the cfg builder via
    `strider_lift::cfg::Builder::with_indirect_resolver`.  Callers wrap
    it in an `IndirectResolverFn<R>` closure (the type alias
    `Arc<dyn Fn(...) -> Result<Option<ResolvedTargets>>>` exposed at
    `strider_lift::cfg`).
  - `GraphRewriter` — pattern-rewrite façade over
    `pattern::rewrite_rule`.
  - `dump_per_region` / `dump_neighborhood` — visualisation helpers
    re-exported at the crate root.  `dump_per_region` writes one
    `region_{idx}_{addr}.html` per region (index prevents collisions
    when two regions share a leading fingerprint address) (membership
    built via `strider_ir::walk::region_membership_from_exit`).
    `dump_neighborhood` writes a single depth-bounded HTML around a
    seed node for focused inspection.
  - `errors` — typed error catalogue, including
    `UnresolvedIndirectBranch`.

- **`strider-pattern-macros`** — proc-macro crate (`proc-macro = true`).
  Emits the PyO3 mirror only — `#[gen_stub_pyclass] #[pyclass]`
  wrapper + stub-gen methods — from one annotated `*Def` struct.  The
  Rust-side `Pat` builders themselves remain hand-written in
  `strider-analyze::pattern`; the macro's job is to spare you a
  byte-for-byte duplicate on the Python side.  10 of 14 pattern
  builders have macro-emitted mirrors; 4 PyO3 mirrors stay
  hand-written: `PyFunctionArgPat` (enum-dispatch source) plus the
  three binary-op mirrors (`PyIntBinaryPat`, `PyBoolBinaryPat`,
  `PyFloatBinaryPat`) whose required-construction shape doesn't fit
  the macro's field-based model.

- **`strider-ir-test-utils`** — `RegisterSet` (fluent builder over
  `FunctionBuilder::new_raw`), `make_empty_fn`, `make_fn_with_var`,
  `reg_vn`, and the `SENTINEL_LIFT_ADDR` constant
  (`0xDEAD_BEEF_0000_0001`).  Helpers auto-stamp the sentinel asm
  fingerprint on every node created through them so mock-graph tests
  satisfy the always-on asm-fingerprint check without manual stamping.

- **`strider-py`** — Python bindings (PyO3 + maturin + abi3-py39).
  High-level API: `strider.load(path).analyze(fn).find(pattern)`,
  auto-detects arch from ELF `e_machine`, with
  `Analysis.fingerprint(node) -> list[int]` as the proof-of-correctness
  helper.  Low-level API mirrors the Rust surface: `SleighArch`,
  `CallingConvention`, `MemoryMap`, `MemReader`, `ReadOnlyMemory`,
  `Sleigh`, `build_cfg`, `Strider`, `Graph`, `OptimizerPipeline`, plus
  `strider.run(arch, cc, mem, entry, ...)`.  `strider.opt` exposes
  per-pass classes; `strider.pattern` is a full mirror of the Rust
  pattern crate.  Cross-pattern joins on shared captures via
  `Graph.find_all_requirements([pat1, pat2, …])`.  Asm-fingerprint
  accessor: `match.asm_fingerprint(c) -> list[int]`.  Errors land as
  `strider.errors.{StriderError, LiftError, ReaderError, PatternError,
  RewriteError}` plus the typed `UnresolvedIndirectBranchError`.  Dev
  workflow uses uv: `uv sync --group dev` → `uv run maturin develop` →
  `uv run pytest`.

### IR Node Model

The IR is a sea-of-nodes graph where each `Node` has typed inputs
(`NodeOutputId` references) and outputs.  The `expected_signature` table
in `crates/strider-ir/src/node_signature.rs` is the single source of
truth for every node's input/output shape.  Node kinds, grouped:

- **Initial state:** `Entry`, `InitialMemory`, `InitialVar(Vn)`.
  arg tracking (introduced by `FunctionArgDetect`) is recorded in the
  `Function::arg_index_to_nodes` side-table mapping each CC argument
  index to its carrier `NodeId` (`InitialVar` for register args,
  `Load` for stack args) — there is no `FunctionArg` `NodeKind`
  variant.
- **Region / join:** `Region` (variadic Control inputs; outputs
  `Control` + `PhiToken`), `MemPhi` (φ for the memory token), `Phi`
  (unit-variant node kind covering both tagged and anonymous forms).
  The optional source-varnode tag lives in the
  `Graph::phi_var_tag: SecondaryMap<NodeId, Option<rsleigh::Vn>>` side-
  table: `Some(vn)` marks the lifter-emitted SSA φ for the register-
  aliased read of varnode `vn`; `None` (the default) marks an anonymous
  value phi synthesised by `StackLoadForward` when forwarding a
  `Load[sp+K]` across a `MemPhi`.
- **Conditional branch:** `If` (outputs true / false `Control` edges).
- **Indirect branch:** `IndirectBranch` (placeholder consumed by the
  orchestrator's indirect-resolution loop; rewritten in place by
  `apply_link_register` / `apply_tail_call` and replaced by the
  jump-table classifier on CFG rebuild).
- **Calls / returns:** `Call` (clobbers caller-saved registers and
  memory; variadic args), `CallOther { user_op_id }`, `Return`
  (variadic return values).
- **Memory:** `Load(VnSpace)`, `Store(VnSpace)`.
  Stack-relative offset metadata (populated by `StackOffsetDetect`)
  lives in `Function::stack_offsets` as a side-table keyed by
  `NodeId`; the underlying node kind stays `Store(VnSpace)` /
  `Load(VnSpace)`.
- **Integer:** `IntConst(u128)`, `IntConstWide(WideConstId)` (U256 /
  U512, interned in `Graph::wide_consts`), `IntUnaryOp` (`BitNot` for
  `~x`, `Neg` for `-x`), `IntBinaryOp` (no `Sub`; lifter lowers to
  `Add(_, Neg(_))`), `IntCmpOp` (`Equal`, `Less`, `Sless`, `Carry`,
  `Scarry`, `Sborrow`; no `LessEqual` / `SlessEqual` — both are
  lift-time-lowered shapes), `Truncate`, `Extend(ExtendOp)`,
  `Popcount`, `Lzcount`, `CastToInt`.
- **Boolean:** `BoolConst(bool)`, `BoolUnaryOp`, `BoolBinaryOp`,
  `CastToBool`.
- **Float:** `FloatConst(u64)` (bits), `FloatUnaryOp`, `FloatBinaryOp`
  (`Add` / `Mul` / `Div`; no `Sub`, lifter lowers to
  `Add(_, Neg(_))`), `FloatCmpOp` (`Equal`, `Less`; no `NotEqual` /
  `LessEqual` — both lifted to lowered shapes; `FLOAT_NAN(x)` is
  lowered to `BoolNeg(FloatEqual(x, x))`).
- **Float / int conversions:** `IntToFloat`, `FloatToInt`,
  `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`.
  The cast ops accept any value-typed input; `FloatToFloat` is
  float→float only.
- **Opaque / user-defined:** `SegmentOp { op_id }`, `CPoolRef`, `New`.

### Pattern DSL

`strider_analyze::pattern` exposes `Pat` / `Capture` / `Matcher` /
`Match` with fluent builders for every node kind.  Key points:

- The Rust-side builders are all hand-written in
  `strider-analyze::pattern`.  `strider-pattern-macros` emits the
  matching PyO3 mirror (`Py*Pat`) from the same `*Def` struct, so
  adding a field on the Python side updates the generated mirror
  automatically — the Rust builder must still be updated by hand.
- 10 of 14 builders have macro-emitted PyO3 mirrors; 4 mirrors stay
  hand-written: `PyFunctionArgPat` (enum-dispatch source) and the
  three binary-op mirrors (`PyIntBinaryPat`, `PyBoolBinaryPat`,
  `PyFloatBinaryPat`).
- **Lift-time canonicalisation** (the lifter applies these so patterns
  match the canonical shape):
  - `IntSub(a, b)` → `Add(a, Neg(b))`.
  - `IntLessEqual(a, b)` → `BoolNeg(IntLess(b, a))` (swap args).
  - `IntNotEqual(a, b)` → `BoolNeg(IntEqual(a, b))`.
  - `FloatSub(a, b)` → `FloatAdd(a, Neg(b))`.
  - `FloatNotEqual(a, b)` → `BoolNeg(FloatEqual(a, b))`.
  - `FloatLessEqual(a, b)` → `Or(FloatLess(a, b), FloatEqual(a, b))`
    (NaN-aware).
  - `FLOAT_NAN(x)` → `BoolNeg(FloatEqual(x, x))`.
  - `If(BoolNeg(C)){A}{B}` → `If(C){B}{A}` (via `opt::IfCondInversion`).
- **Commutative matching:** `add`, `mul`, `and`, `or`, `xor` (and bool
  equivalents), `int_cmp(Equal/Carry/Scarry)`, and `float_cmp(Equal)`
  automatically try both operand orderings.  Driven by
  `NodeKind::is_commutative()` — the single source of truth.

### Register Aliasing

Overlapping registers (x86 `rax`/`eax`/`ax`/`al`/`ah`, AArch64
`q0`/`d0`/`s0`, x87 `ST*`, etc.) are handled in
`strider_lift::pcode_lift::ValueLifter::{read_vn, write_vn}` (in
`crates/strider-lift/src/pcode_lift/vn_io.rs`).  All reads and writes
go through the largest containing register, with shift / mask operations
inserted for sub-register slices.  `find_largest_fitting_register` is
the entry point.  `vn_mask` enumerates supported widths: 1, 2, 4, 8,
10 (x87 80-bit extended), 16 (XMM / q-register), 32 (YMM), 64 (ZMM)
bytes.  Widths > 16 use a degraded `u128::MAX` mask; the wide-container
guard rejects sub-register aliasing within > 16-byte containers with a
clear error.

### External Dependency: rsleigh

`rsleigh` is a local path dependency at `../rsleigh` (not in this
workspace).  It wraps GHIDRA's Sleigh specification to lift machine
instructions to p-code (`rsleigh::Insn` with `Opcode`).  Key types used:
`Vn` (varnode — register / memory / const / unique), `VnSpace`,
`MemReader`, `Sleigh`, `SleighRegs`.

`Sleigh::lift_one(&mut self, addr)` is **not** stateless across calls —
the `&mut self` carries context-register state (ARM Thumb mode, x86
segment selectors, MIPS16 mode).  Decoded instructions can modify this
context (ARM `bx lr` switches Thumb / ARM mode).  Decode buffers reset
per call, but context-register state persists.  Practical consequence:
per-region sequential decoding must be preserved (`Lifter::region` and
`RegionBuilder::build` honour this).  Across regions, context state is
assumed fixed per function entry.
