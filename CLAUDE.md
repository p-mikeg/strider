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
Dependency edges (X → Y means "X depends on Y"); every crate also
depends on the external `rsleigh`.

  strider-target   → (leaf — only `rsleigh`)
  strider-ir       → strider-target, dot, entity-utils, graphwalk
  strider-reader   → strider-ir, strider-target
  strider-lift     → strider-ir, strider-target, dot, graphwalk
  strider-analyze  → strider-ir, strider-lift, strider-target, dot, entity-utils
  strider-py       → strider-analyze, strider-lift, strider-reader, strider-ir,
                     strider-target, strider-pattern-macros, dot

  strider-pattern-macros — proc-macro (no strider deps); consumed by strider-py
    to emit Py*Pat mirrors of the hand-written Rust Pat builders.
  strider-ir-test-utils  — dev-dep; depends on strider-ir.
  rsleigh — external path dep at ../rsleigh (Sleigh / GHIDRA p-code lifter).
```

`strider-target` is the foundational leaf (pure descriptions, no IR /
Sleigh deps); `strider-reader` depends on it for the `Endianness` enum
consumed by `ReadOnlyMemory::read`.  The graph is a DAG rooted at
`strider-py` with `strider-target` at the bottom — there are no
back-edges.  `strider-lift` calls
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
    - **`EntityInterner`:** `wide_const_interner`
      (`WideConstId → WideConstStorage`, value-deduped; consulted by
      `IntConstWide(WideConstId)` nodes for I256 / I512 payloads that
      don't fit in the regular `IntConst(u128)`).  Accessors:
      `wide_const(id)` / `wide_const_opt(id)` / `intern_wide_const(value)`.
    - **Side-table registry on `Function`:** the `NodeId`-keyed
      `SecondaryMap` side-tables `stack_offsets` (SP-relative offset
      metadata for Store/Load populated by `StackOffsetDetect`),
      `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`,
      `phi_var_tag` (per-node `Option<Vn>` source-varnode tag for `Phi`
      nodes), and `call_stack_arg_offsets_overrides`; plus the
      index-keyed `arg_index_to_nodes` (`FxHashMap<u32, Vec<NodeId>>`,
      populated by `FunctionArgDetect`).  All are remapped by
      `Function::compact`.  `Graph` itself only holds structural state
      (nodes, edges, dedup cache, `wide_const_interner`); per-function overlay
      state lives on `Function`.
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
    `ElfFileMemReader` decodes via the `strider_target::Endianness`
    SSoT, `Endianness::read_u64`), or `None` for
    unmapped addresses / sizes > 8.  Blanket impls for `Arc<T>` and
    `Box<T>`.  Defined here (not in `strider-reader`) so optimiser
    passes can depend on the trait without back-edging through the
    reader crate.  Concrete impls live in `strider-reader`.  The
    optimizer's `LoadReadOnly` takes `&dyn ReadOnlyMemory` so it
    doesn't depend on the reader crate.
  - `NodeOutputKind` — `Control`, `Memory`, `PhiToken`, or
    `OutputType(NodeOutputType)`.
  - `NodeOutputType` — integers `I1` (the 1-bit boolean), `I8`, `I16`,
    `I32`, `I64`, `I80` (x87 80-bit extended), `I128`, `I256`, `I512`;
    floats `F32`, `F64`, `F80`.  There is no separate `Bool` type or
    category: a boolean is the 1-bit integer `I1`, so `is_integer()` is
    true for it and `bit_width(I1) == 1` (the lone case where bit width
    isn't `byte_size * 8`).  `NodeOutputType::int_for_byte_size(n)` /
    `float_for_byte_size(n)` map a varnode byte size to a type (byte size
    1 → `I8`, never `I1`); there is no `TryFrom<u32>`.  Wide types
    (`I256` / `I512`) are stored via `IntConstWide(WideConstId)` interned
    in `Graph::wide_const_interner`; `IntConst(u128)` rejects them.
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
      phi-token ownership and per-predecessor arity, value-`Phi`
      input-type consistency (every value input must carry the phi's
      own output type), Call/Return CC-arity (output / input slot counts
      vs the calling convention, honouring per-`Call` clobber overrides),
      wide-const consistency (including a dedicated check that
      `IntConstWide` declares a I256/I512 output type), and the always-on
      asm-fingerprint check (every reachable non-exempt node MUST carry
      ≥1 fingerprint).
    - Errors are aggregated into a `ValidationErrors` bundle rather than
      failing fast.
  - `function_dot` module — IR-specific Graphviz / HTML rendering on top
    of the generic `dot` crate.  The pretty `FunctionDotDumper`
    (`Function::dot_dumper`) inlines constants, adds virtual nodes, and
    needs a `Sleigh` for register names; `Function::raw_dot` /
    `raw_html` (the `function_dot::raw` submodule) render the graph
    **exactly as stored** instead — one node per reachable `NodeId`, one
    edge per input edge, side-tables shown inline, no Sleigh — for
    debugging the real graph shape.
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
    `clobbers_memory` (a `bool`) describing the ISA-fixed
    register-and-memory footprint beyond Sleigh's pcode-explicit args.
  - `PositionalArgLayout` — canonical positional-arg-layout DTO derived
    from a `BuiltCallingConvention` via
    `PositionalArgLayout::from_convention(&cc)`.  Single source of
    truth for positional argument slot order (register slots first,
    then stack slots at the convention's `stack_arg_offsets`); consumed
    by `FunctionArgDetect`, `CallStackArgCollect`, and `LoadForward`
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

- **`strider-lift`** — binary → IR.  Two modules (`pcode_lift`, `cfg`):

  - `pcode_lift` — pure value-producing pcode→IR lifter
    (`ValueLifter::lift(insn) -> Result<bool>`).  Owns the
    register-aliasing logic (`vn_io.rs`) and the checked input
    accessors `first_input_or_err` / `nth_input_or_err` (every
    production-code varnode access returns a typed error instead of
    panicking on an out-of-bounds index).  See the "Register Aliasing"
    section below.
  - `cfg` — builds a Control Flow Graph (`Cfg<R>`) from a binary using
    `rsleigh`.  Uses `petgraph::StableDiGraph` internally.  The
    load-bearing per-region machinery lives in
    `cfg/builder/region_builder.rs`: `RegionBuilder::build` decodes one
    machine instruction at a time, preserving sequential-within-region
    decoding (Sleigh's `lift_one(&mut self)` carries context-register
    state, so out-of-order per-insn lifting across regions is not safe),
    funnels each lift through the `set_lift_addr` fingerprint attribution
    point, and consults the `DecodeCache`.  Bounded-lift semantics
    (`function_max_size`) and `is_addr_tail_call` live alongside it.
    Exposes the `IndirectResolverFn<R>` callback type (an `Arc<dyn Fn>`
    alias) — the cfg builder calls back through the installed closure for
    indirect-branch resolution, so the cfg-to-analyze direction stays
    clean.  The only public construction path is
    `Builder::for_arch(arch, sleigh, addr, options)` so endianness and
    `ArchPreset` are derived from the arch atomically (the older
    `Builder::new` / `with_endianness` ctors silently defaulted
    `preset = X86_64` and have been removed).

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
    - `FlagCmpCanonicalize` — flag-tree → single `IntCmpOp` rewrite.
      Covers both the raw AArch64 NZCV-style chains and the
      decomposed `(a≠b)∧¬(a<b)` / `(a=b)∨(a<b)` shapes that ARM/Thumb
      (and post-`ConstantFold` trees) leave once the branch's inverted
      sense is stripped, so every flag arch folds to a direct comparison.
    - `IfCondInversion` — `If(BitNot(C)){A}{B}` → `If(C){B}{A}` (the
      1-bit `BitNot` is logical NOT).
    - `RedundantPhis` — eliminates `Phi` (tagged or anonymous) /
      `MemPhi` / `Region` with a single reachable predecessor.
    - `DeadBranchElimination` — removes `If(const)` branches and strips
      dead control edges.
    - `LoadReadOnly` — folds constant-address loads via
      `&dyn ReadOnlyMemory`.
    - `StackOffsetDetect` — annotates SP-relative `Store` / `Load`
      offsets in `Function::stack_offsets`; the unified memory chain
      is left intact.
    - `LoadForward` — forwards values from stack-tagged `Store`
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
  - Error handling — fallible operations return `anyhow::Result`
    (`opt::Result` aliases it; `pattern::error` adds only the internal
    `RewriteSkip` / `PatternBuildError` sentinels).  There is no bespoke
    error catalogue in this crate; an indirect branch that can't be
    resolved at the fixed-point exit surfaces as
    `strider_lift::cfg::RegionTerminator::UnresolvedIndirectBranch` plus
    an `anyhow` error from the orchestrator.  (The typed Python-facing
    exception hierarchy lives in `strider-py`.)

- **`strider-pattern-macros`** — proc-macro crate (`proc-macro = true`).
  Emits the PyO3 mirror only — `#[gen_stub_pyclass] #[pyclass]`
  wrapper + stub-gen methods — from one annotated `*Def` struct.  The
  Rust-side `Pat` builders themselves remain hand-written in
  `strider-analyze::pattern`; the macro's job is to spare you a
  byte-for-byte duplicate on the Python side.  Most pattern builders have
  macro-emitted mirrors (including the binary-op mirrors `PyIntBinaryPat`
  / `PyFloatBinaryPat`, driven via the macro's `constructor_args`).
  `PyFunctionArgPat` (an enum-dispatch source whose shape doesn't fit the
  field-based model) stays a hand-written `#[pyclass]`, and `bool_binary`
  is a plain function (a boolean AND/OR/XOR is `IntBinaryOp` at `I1`, so
  it needs no dedicated builder).

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
  accessor: `match.asm_fingerprint(c) -> list[int]`.  Every Rust error
  (including an unresolved indirect branch) lands in Python as a single
  `strider.errors.StriderError` exception carrying an informative
  message; the hierarchy is intentionally flat (no typed subclasses).
  Dev workflow uses uv: `uv sync --group dev` → `uv run maturin develop` →
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
  value phi synthesised by `LoadForward` when forwarding a
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
- **Integer (incl. booleans):** `IntConst(u128)`, `IntConstWide(WideConstId)`
  (I256 / I512, interned in `Graph::wide_const_interner`), `IntUnaryOp` (`BitNot`
  for `~x`, `Neg` for `-x`), `IntBinaryOp` (`And` / `Or` / `Xor` /
  `Add` / `Mul` / shifts / …; no `Sub`; lifter lowers to
  `Add(_, Neg(_))`), `IntCmpOp` (`Equal`, `Less`, `Sless`, `Carry`,
  `Scarry`, `Sborrow`; no `LessEqual` / `SlessEqual` — both are
  lift-time-lowered shapes; output is `I1`), `Truncate`,
  `Extend(ExtendOp)`, `Popcount`, `Lzcount`.  **Booleans are the 1-bit
  integer `I1`** — there is no `BoolConst` / `BoolBinaryOp` /
  `BoolUnaryOp` / `CastToBool` / `CastToInt`: a bool constant is
  `IntConst(0|1):I1`, logical and/or/xor are `IntBinaryOp::{And,Or,Xor}`
  at `I1`, logical not is `IntUnaryOp::BitNot` at `I1`, bool→int widening
  is `Extend(ZeroExtend)`, and int→bool conversion is never needed (Sleigh
  always feeds an already-`I1` condition).
- **Float:** `FloatConst(u64)` (bits), `FloatUnaryOp`, `FloatBinaryOp`
  (`Add` / `Mul` / `Div`; no `Sub`, lifter lowers to
  `Add(_, Neg(_))`), `FloatCmpOp` (`Equal`, `Less`; output `I1`; no
  `NotEqual` / `LessEqual` — both lifted to lowered shapes; `FLOAT_NAN(x)`
  is lowered to `BitNot(FloatEqual(x, x))` at `I1`).
- **Float / int conversions:** `IntToFloat`, `FloatToInt`,
  `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`.  There is no
  `CastToFloat`: an int→float cast is a same-width `IntBitsToFloat`, and a
  float→float reprecision is `FloatToFloat`.  `FloatToFloat` is
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
- Most builders have macro-emitted PyO3 mirrors (the binary-op mirrors
  `PyIntBinaryPat` / `PyFloatBinaryPat` are emitted via the macro's
  `constructor_args`); `PyFunctionArgPat` (enum-dispatch source) stays
  hand-written, and `bool_binary` is now a plain function returning a
  `Pat` (a boolean AND/OR/XOR is just `IntBinaryOp` at `I1`).
- **Querying booleans by width** (booleans are `I1`, not a distinct type):
  `value_of_width(n)` / `bool_value()` filter by *output* width (width 1 =
  "produces a bool", including comparisons); `inputs_of_width(n, inner)` /
  `bool_inputs(inner)` filter by *input* width (width 1 = "operates on
  booleans", excluding comparisons whose operands are wider).
- **Lift-time canonicalisation** (the lifter applies these so patterns
  match the canonical shape):
  - `IntSub(a, b)` → `Add(a, Neg(b))`.
  - `IntLessEqual(a, b)` → `BitNot(IntLess(b, a))` at `I1` (swap args;
    logical-not of a 1-bit value is `IntUnaryOp::BitNot`).
  - `IntNotEqual(a, b)` → `BitNot(IntEqual(a, b))` at `I1`.
  - `FloatSub(a, b)` → `FloatAdd(a, Neg(b))`.
  - `FloatNotEqual(a, b)` → `BitNot(FloatEqual(a, b))` at `I1`.
  - `FloatLessEqual(a, b)` → `Or(FloatLess(a, b), FloatEqual(a, b))` at
    `I1` (NaN-aware; `Or` is `IntBinaryOp::Or`).
  - `FLOAT_NAN(x)` → `BitNot(FloatEqual(x, x))` at `I1`.
  - `If(BitNot(C)){A}{B}` → `If(C){B}{A}` (via `opt::IfCondInversion`,
    matching a 1-bit `BitNot`).
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
per-region sequential decoding must be preserved
(`RegionBuilder::build` honours this).  Across regions, context state is
assumed fixed per function entry.
