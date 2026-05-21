# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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
- `strider-pattern-macros` — proc-macro that emits paired Rust + PyO3
  pattern builders from one annotated `*Def` struct.
- `strider-ir-test-utils` — `TestGraph` and asm-fingerprint-stamping
  helpers shared by every crate's tests.
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

  strider-pattern-macros (proc-macro; consumed by strider-analyze
    for Pat builders and by strider-py for Py*Pat mirrors)

  strider-ir-test-utils (dev-dep; depends on strider-ir)

  rsleigh (external path dep at ../rsleigh — Sleigh / GHIDRA p-code lifter)
```

All edges point upward; there are no back-edges.  `strider-lift` calls
back into `strider-analyze`'s indirect-branch resolver through the
`strider_lift::cfg::IndirectTargetResolver` trait object — the resolver
is constructed in `strider-analyze` and passed down at `Builder::build`
time, so the resolver-bearing dependency stays one-way.

### Key Crates

- **`strider-ir`** — the core sea-of-nodes IR graph and everything that
  doesn't depend on a target.  Public surface:

  - `Graph` — stores `NodeId`, `NodeOutputId`, `NodeInputId` via
    `cranelift-entity` PrimaryMaps.  Cacheable nodes are deduplicated
    by `(NodeKind, inputs, output_kinds)`.  Side-tables hold ancillary
    per-node data (`stack_phi_offsets`, `call_other_names`,
    `asm_fingerprints`, `call_clobbered_overrides`, `wide_consts`,
    `entry`, `cc_metadata`).
  - `FunctionBuilder` — builds the IR with SSA-like variable tracking.
    Variables map `rsleigh::Vn` → `VarId`.  Each region gets a
    `ControlState` node and per-variable `Phi(Some(vn))` nodes.  Carries
    `lift_addr: Option<u64>` for centralised lift-time fingerprint
    attribution.  Accepts the thin `FunctionBuilderCC` plain-data struct
    rather than the richer `strider_target::BuiltCallingConvention` so
    the IR crate doesn't pull a back-edge.
  - `FunctionBuilderCC` — plain-data DTO containing exactly the fields
    `FunctionBuilder` reads.  `strider-target` provides
    `impl From<&BuiltCallingConvention> for FunctionBuilderCC`.
  - `BuiltFunctionGraph` — produced by `FunctionBuilder::build`.  A thin
    wrapper around `Graph` that encodes `graph.entry.is_some()` and
    `graph.cc_metadata.is_some()` at the type level.  All CC metadata
    (variable map, call-clobbered list, ret-val regs, call-other clobber
    list, no-memory-clobber flag) lives on the wrapped `Graph`'s
    `cc_metadata` side-table.  `Deref<Target = Graph>` so accessors are
    one call away.  Implements `Clone`.
  - `ReadOnlyMemory` trait — defines `read_bytes(addr, buf) -> Result<()>`
    only; no binary-format knowledge.  Concrete impls live in
    `strider-reader`.  The optimizer's `LoadReadOnly` takes
    `&dyn ReadOnlyMemory` so it doesn't depend on the reader crate.
  - `NodeOutputKind` — `Control`, `Memory`, `PhiToken`, or
    `OutputType(NodeOutputType)`.
  - `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U80`
    (x87 80-bit extended), `U128`, `U256`, `U512`; floats `F32`, `F64`,
    `F80`.  Wide types (`U256` / `U512`) are stored via
    `IntConstWide(WideConstId)` interned in `Graph::wide_consts`;
    `IntConst(u128)` rejects them.
  - `walk::walk_graph(graph, entry)` — preorder traversal that follows
    both backward-data and forward-control edges.  Used by the validator
    and several passes.
  - `node_signature::{ExpectedOutputKind, expected_signature}` — single
    source of truth for expected input/output slot kinds per `NodeKind`.
  - `validate::validate(&graph, entry) -> Result<(), ValidationErrors>`
    — whole-graph validator split into three groups by file:
    - `validate/local_typing.rs` — per-node local typing against
      `expected_signature` (scoped to nodes reachable via `walk_graph`).
    - `validate/use_list_consistency.rs` — bidirectional consistency
      between inputs and outputs' use-lists.
    - `validate/graph_invariants.rs` — whole-graph rules:
      Entry/InitialMemory uniqueness, ControlState predecessor kinds,
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
    `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`,
    `MemPhi`, `Phi`, `StackStorePhi`) are exempt from the non-empty
    check.  Public API on `Graph`: `asm_fingerprint(id)`,
    `set_asm_fingerprint`, `extend_asm_fingerprint`,
    `extend_asm_fingerprint_from`.

- **`strider-target`** — pure target descriptions, no Sleigh or IR
  dependencies.

  - `SleighArch` — `.sla` + `.pspec` + `Endianness`.  Presets: `x86_64`,
    `x86`, `aarch64` / `aarch64be`, `arm` / `arm_be` / `arm_thumb`,
    `mipsbe32` / `mipsle32` / `mipsbe64` / `mipsle64`, `ppc32be` /
    `ppc32le` / `ppc64be` / `ppc64le`.
  - `ArchPreset` / `ArchContext` — closed enum + bundle threaded into
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
    `IndirectTargetResolver` trait — the cfg builder calls back through
    a trait object for indirect-branch resolution, so the
    cfg-to-analyze direction stays clean.  The only public construction
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
    - `RedundantPhis` — eliminates `Phi(Some(_))` / `Phi(None)` /
      `MemPhi` / `ControlState` with a single reachable predecessor.
    - `DeadBranchElimination` — removes `If(const)` branches and strips
      dead control edges.
    - `LoadReadOnly` — folds constant-address loads via
      `&dyn ReadOnlyMemory`.
    - `StackStoreDetect` — promotes SP-relative `Store` to
      `StackStore { offset }`.
    - `StackLoadForward` — forwards values from `StackStore` to
      subsequent same-offset `Load`.
    - `FunctionArgDetect` (post-pass) — canonicalises register / stack
      arg reads into `FunctionArg` nodes.
    - `CallStackArgCollect` (post-pass) — wires positional stack args
      into `Call` nodes.
    - Imperative peephole passes use `pattern::Matcher::find_all`
      rather than rolling their own matching.
  - `pattern` module — pattern DSL (`Pat` / `PatKind` / `Capture` /
    `Matcher` / `Match` and fluent builders).  Cross-pattern joins on
    shared captures via `Matcher::find_all_requirements`.
  - `strider` module — `Strider`, `AnalyzeOptions`, `AnalyzeOutcome`,
    `RegionLiftHandles`, and `PerRegionDriver` (the per-region driver
    that converts a `Cfg` into the IR graph region by region).
    `Strider::build_optimizer_pipeline`,
    `build_stable_optimizer_pipeline`,
    `build_destructive_optimizer_pipeline` produce the three pre-canned
    pipelines.
  - `orchestrator::run(config) -> Result<BuiltFunctionGraph>` — the
    canonical top-level entry, re-exported as
    `strider_analyze::run`.  Build the CFG, lift to IR, run the stable
    optimiser subset, drive the indirect-branch fixed-point loop
    (`Decision::FixedPoint` / `StableOnly` / `Rebuild`), then run the
    destructive subset once at the fixed-point exit.  `Config` carries
    the `Strider`, start address, `Sleigh`, optional ROM, function-size
    bound, per-target-address CC overrides, and a `compact` flag.
  - `indirect_resolve` module — `classify_anchor` /
    `apply_link_register` / `apply_tail_call` and the producer-shape
    classifier free functions (`classify_jump_table`,
    `classify_stack_array`, `ResolvedTargets { LinkRegister, Single,
    Multiple }`).  Called by the orchestrator's `LoopState`.
  - `indirect_resolver` — `MiniIrIndirectResolver`, the trait object
    that satisfies `strider_lift::cfg::IndirectTargetResolver`.
  - `GraphRewriter` — pattern-rewrite façade over
    `pattern::rewrite_rule`.
  - `errors` — typed error catalogue, including
    `UnresolvedIndirectBranch`.

- **`strider-pattern-macros`** — proc-macro crate (`proc-macro = true`).
  Emits a Rust pattern builder + `Pat` constructor + PyO3
  `#[gen_stub_pyclass] #[pyclass]` mirror + stub-gen methods from one
  annotated `*Def` struct.  10 of 14 pattern builders are macro-emitted;
  4 stay hand-written: `FunctionArgPat` (enum-dispatch source) plus the
  three binary-op builders (`IntBinaryOpPat`, `BoolBinaryOpPat`,
  `FloatBinaryOpPat`) whose required-construction shape doesn't fit the
  macro's field-based model.

- **`strider-ir-test-utils`** — `make_empty_fn` / `TestGraph` and
  friends that auto-stamp a sentinel asm fingerprint
  (`SENTINEL_LIFT_ADDR = 0xDEAD_BEEF_0000_0001`) on every node created
  through the helper, so mock-graph tests satisfy the always-on
  asm-fingerprint check without manual stamping.

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

- **Initial state:** `Entry`, `InitialMemory`, `InitialVar(Vn)`,
  `FunctionArg { source, index }` (introduced by `FunctionArgDetect`).
- **Region / join:** `ControlState` (variadic Control inputs; outputs
  `Control` + `PhiToken`), `MemPhi` (φ for the memory token),
  `Phi(Option<Vn>)` — one node kind covers both forms.  `Phi(Some(vn))`
  is the lifter-emitted SSA φ for the register-aliased read of varnode
  `vn`; `Phi(None)` is a value phi not tied to any source varnode,
  synthesized by `StackLoadForward` when forwarding a `Load[sp+K]`
  across a `MemPhi`.
- **Conditional branch:** `If` (outputs true / false `Control` edges).
- **Indirect branch:** `IndirectBranch` (placeholder consumed by the
  orchestrator's indirect-resolution loop; rewritten in place by
  `apply_link_register` / `apply_tail_call` and replaced by the
  jump-table classifier on CFG rebuild).
- **Calls / returns:** `Call` (clobbers caller-saved registers and
  memory; variadic args), `CallOther { user_op_id }`, `Return`
  (variadic return values).
- **Memory:** `Load(VnSpace)`, `Store(VnSpace)`; after
  `StackStoreDetect`: `StackStore { space, offset }`,
  `StackStorePhi { space }` (per-predecessor offsets in
  `Graph::stack_phi_offsets`).
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

`strider_analyze::pattern` exposes `Pat` / `PatKind` / `Capture` /
`Matcher` / `Match` with fluent builders for every node kind.  Key
points:

- 10 of 14 builders are emitted by `strider-pattern-macros` from one
  annotated `*Def` struct; the macro emits the Rust builder + the PyO3
  mirror together, so adding a field updates both sides.
- 4 builders stay hand-written: `FunctionArgPat` (enum-dispatch source)
  and the three binary-op builders.
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
