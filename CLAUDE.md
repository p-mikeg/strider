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
# crates/strider-orchestrator/examples/orchestrator_demo.rs.
cargo run -p strider-orchestrator --example orchestrator_demo

# Dump per-arch IR cmp shapes (debug helper for the FlagCmpCanonicalize spec).
cargo run -p strider-orchestrator --example dump_arch_cmps

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

The workspace splits into **five generic utility crates** (not tied to
strider's domain) and **nine strider-specific crates** (plus the
`strider-ir-test-utils` dev-dependency):

Generic helpers:

- `dot` — Graphviz / dark-themed HTML renderer.
- `entity-utils` — `cranelift-entity` helpers (`DenseEntitySet`,
  `Worklist`, `EntityInterner`).  Use these instead of
  `std::collections::HashSet` / `HashMap` when keying by `NodeId` /
  `ValueId` and friends.
- `graphwalk` — generic preorder / postorder graph traversal.  Test code
  under `graphwalk/tests/common/` hosts the `graphmock` DSL for spinning
  up synthetic graphs in unit tests.
- `strider-graph` — generic despite the `strider-` name (named that
  deliberately): the payload-agnostic bipartite sea-of-nodes
  `Graph<N, V, C: NodeCacheable<N, V>>` that both `strider-ir` and
  `strider-pattern` build on.  Imposes no `Hash`/`Eq` bound on the
  payloads; dedup (if any) lives entirely in the `C` policy.
- `read-only-memory` — the `ReadOnlyMemory` trait (read access to a
  statically-known memory region) extracted into its own tiny crate so
  the optimizer / lifter / reader can each depend on it one-way without
  back-edging through the ELF-parsing reader crate.

Strider crates:

- `strider-ir` — the sea-of-nodes IR.
- `strider-target` — pure target descriptions (`SleighArch`,
  `CallingConvention`, `BuiltCallingConvention`, `CallOther` ABI table).
- `strider-reader` — ELF loader and `ReadOnlyMemory` backend.
- `strider-cfg` — CFG construction: bytes → `Cfg` of basic-block
  regions via Sleigh.  IR-free (no `strider-ir` dep); owns `Cfg` /
  `Builder` / `Region` / `RegionTerminator` / `Machine`+`PcodeInsnAddr`
  / `ResolvedTargets` / `is_addr_tail_call` and the public `CfgOptions`.
- `strider-lift` — CFG → IR.  The reusable `Lifter<R>` engine **owns**
  the arch + `Sleigh<R>` + cached `SleighRegs` (built once via
  `Lifter::new(arch, sleigh)`; the calling convention is a per-call arg).
  It builds + lifts a `strider_cfg::Cfg` into a `strider_ir::Function` via
  a per-CFG transient (`FunctionLifter`) that lifts both value-producing
  and control-flow opcodes as `&mut self` methods (no separate
  `ValueLifter`).  Owns `LiftOptions`, which embeds a
  `strider_cfg::CfgOptions` (`cfg`) alongside the IR-lift knob
  `per_address_ccs`.
- `strider-pattern` — the graph-based pattern DSL (`Pat` / `Capture` /
  `Matcher` / `Match` / fluent builders) plus its rewrite façade, built
  over `strider-graph` with the `NeverCacheable` policy.
- `strider-opt` — optimization passes, the `OptimizerPipeline`, and the
  `indirect_branch_resolve` classifiers / in-place editors (the
  indirect-branch *resolution logic*).  Pure graph→graph; no orchestrator
  back-edge.
- `strider-orchestrator` — the orchestrator (`Strider::analyze`) plus the
  re-exported lift engine (`strider_lift::lift::Lifter` /
  `LiftOptions` / `LiftOutcome`, surfaced at the crate root so downstream
  crates reach them without a direct `strider-lift` dep).  Depends on
  `strider-opt` and re-exports it as `opt` (so `strider_orchestrator::opt::…`
  reaches every pass).
- `strider-ir-test-utils` — `make_empty_fn` / `RegisterSet` builders
  and asm-fingerprint-stamping helpers shared by every crate's tests.
- `strider-py` — PyO3 bindings (`maturin develop` builds a wheel).

### Crate Dependency Flow

```
Dependency edges (X → Y means "X depends on Y"); every crate also
depends on the external `rsleigh`.

  read-only-memory → (leaf — only `anyhow`)
  strider-target   → (leaf — only `rsleigh`)
  strider-graph    → graphwalk (+ cranelift-entity, petgraph,
                     hashbrown, smallvec, rustc-hash)
  strider-ir       → strider-graph, read-only-memory, strider-target,
                     dot, entity-utils, graphwalk
  strider-reader   → strider-ir, strider-target, read-only-memory
  strider-cfg      → strider-target, dot, graphwalk, petgraph
                     (IR-free — NO strider-ir)
  strider-lift     → strider-cfg, strider-ir, strider-target
  strider-pattern      → strider-ir, strider-graph, strider-target,
                         entity-utils
  strider-opt          → strider-cfg, strider-ir, strider-pattern,
                         strider-target, entity-utils
                         (uses strider-cfg only for ResolvedTargets)
  strider-orchestrator → strider-opt, strider-ir, strider-cfg,
                         strider-lift, strider-pattern, strider-target,
                         dot
  strider-py       → strider-orchestrator, strider-opt, strider-cfg,
                     strider-reader, strider-ir, strider-target,
                     strider-pattern, dot

  strider-py generates its `Py*Pat` builders in-crate via a local
    `node_builder!` / `binary_op_builder!` `macro_rules!` in `pattern.rs`
    (no proc-macro crate).
  strider-ir-test-utils  — dev-dep; depends on strider-ir.
  rsleigh — external path dep at ../rsleigh (Sleigh / GHIDRA p-code lifter).
```

`strider-target` and `read-only-memory` are the foundational leaves
(pure descriptions / a single trait, no IR / Sleigh deps);
`strider-reader` depends on `strider-target` for the `Endianness` enum
it uses to decode integers from the raw bytes a `ReadOnlyMemory::read`
fill returns.  The graph is a DAG rooted at `strider-py` — there are no
back-edges.  Indirect-branch resolution needs no cfg-time callback: the
orchestrator classifies each unresolved branch against the optimised IR
(the `strider-opt` `IndirectBranchClassify` post-pass) and feeds the
results back through `CfgOptions::known_targets` into the next CFG
rebuild, so `strider-cfg` stays a pure leaf with no analysis dependency.

### Key Crates

- **`strider-ir`** — the core sea-of-nodes IR graph and everything that
  doesn't depend on a target.  Public surface:

  - `Graph` — a **type alias**,
    `strider_graph::Graph<NodeKind, ValueKind, IrCacheable>`
    (`crates/strider-ir/src/graph/mod.rs`).  The generic
    `strider_graph::Graph` stores `NodeId`, `ValueId`, `UseId` via
    `cranelift-entity` PrimaryMaps and owns the structural verbs
    (`create_node`, `add_node_input`, `update_input`,
    `replace_all_uses`, the read accessors, and the typed / fallible
    structural exact accessors `node_inputs_exact` / `node_outputs_exact`
    / `node_input_id_at` / `kind_of_value`, all inherent on the generic
    `Graph`).  Cacheable node kinds (`NodeKind::is_cacheable`) are
    deduplicated by their `(NodeKind, inputs, output_kinds)` structure;
    non-cacheable kinds (`Region`, `Phi`, `MemPhi`, `Call`, …) always
    allocate fresh.
    - **The dedup cache is hash-on-demand**, owned generically by
      `strider_graph::NodeCache` — a `hashbrown::HashTable<NodeId>`
      paired with a `SecondaryMap<NodeId, u64>` of per-node structural
      hashes.  It stores **no** owned key payloads: equality is resolved
      by re-reading a candidate's `(kind, inputs, output-kinds)` back out
      of the `RawStore`, so collisions coexist and eviction is O(1) (the
      cached hash locates a node's bucket without re-hashing).  The
      strider-specific *policy* is `IrCacheable`
      (`crates/strider-ir/src/graph/cache.rs`), a ZST implementing the
      three stateless `strider_graph::NodeCacheable` hooks: `should_cache`
      (gates on `NodeKind::is_cacheable`), `hash` (a raw `FxHasher` over
      the structural key), and `eq` (the re-read structural compare).  The
      cache is purely mechanical — it embeds no domain-specific
      normalisation.  Integer-constant canonicalisation (masking an
      `IntConst` payload to its declared width, plus the small→wide
      promotion) happens at construction in
      `Function::create_node_attributed`, before a node ever reaches the
      cache, so equal constants minted by different paths dedup.
    - **`Graph` holds only structural state** (nodes, edges, the dedup
      cache).  Per-function overlay state — `entry: Option<NodeId>`, the
      resolved calling convention `default_cc:
      strider_target::BuiltCallingConvention`, the ordered tracked-varnode
      list `all_vns`, the `vn_to_container` map, the wide-const interner,
      and all the side-tables — lives on `Function`, which wraps a `Graph`.
      (There is no `cc_metadata` / `CcMetadata`: the convention SSoT is
      `default_cc` + `all_vns`, and clobber / ret-val / `preserves_memory`
      reads go through `default_cc`.)  Clobber / ret-val derivations
      (`call_clobbered_for`, `call_ret_vals_for`) resolve every CC register
      onto its tracked container via `Function::container_of` before
      membership/exclusion, so a narrower ABI register (`eax`) matches the
      wider tracked container (`rax`).
    - **`vn_to_container` (on `Function`):** an
      `FxHashMap<rsleigh::Vn, rsleigh::Vn>` mapping every REGISTER/UNIQUE
      varnode in the original (pre-dedup) tracked set plus every
      CC-referenced register to its largest tracked container (or itself).
      Built once in `FunctionBuilder::new`; read via
      `Function::container_of` (map hit → on-the-fly `all_vns` containment
      scan → self).  CONST is left to the graph's structural dedup cache and
      RAM (load/store) is deliberately not canonicalized, so neither is in
      the map.  Plain `rsleigh::Vn` keys/values, so `compact` leaves it
      untouched.
    - **`wide_const_interner` (on `Function`):** an
      `entity_utils::EntityInterner` (`WideConstId → WideConstStorage`,
      value-deduped; referenced by `IntConst(IntPayload::Wide(WideConstId))`
      nodes for the I80 / I128 / I256 / I512 payloads that don't fit inline
      in `IntPayload::Small(u64)`).  Accessors: `wide_const(id)` /
      `wide_const_opt(id)` / `intern_wide_const(value)`.
    - **Side-table registry on `Function`:** the `NodeId`-keyed
      `SecondaryMap` side-tables `stack_offsets` (SP-relative offset
      metadata for Store/Load populated by `StackOffsetDetect`),
      `call_other_names`, `asm_fingerprints`, and `call_descriptor`; the
      `ValueId`-keyed `value_vn` (source-varnode tags for `Phi` outputs and
      `Call`/`CallOther` ret-val / clobber outputs, read / written via
      `get_vn_for_value` / `set_vn_for_value`); plus the index-keyed
      `arg_index_to_values` (`FxHashMap<u32, Vec<ValueId>>`; register args
      recorded at builder entry, stack args by `FunctionArgDetect`).  All are remapped by
      `Function::compact`; `Function::retain_reachable` drops the entries
      for culled nodes.
  - `FunctionBuilder` — builds the IR with SSA-like variable tracking.
    Variables map `rsleigh::Vn` → `VarId`.  Each region gets a
    `Region` node and per-variable `Phi` nodes whose source
    varnode tag is recorded in `Function::value_vn` (via `set_vn_for_value`).
    Carries `lift_addr: Option<u64>` for centralised lift-time
    fingerprint attribution.  `FunctionBuilder::new(all_used_variables,
    cc, endianness)` takes the tracked-varnode list, an owned
    `strider_target::BuiltCallingConvention` (moved directly into
    `Function::default_cc` — no clone), and the target endianness.
  - `FunctionBuilder::build` returns the populated `Function` directly —
    `entry()` is `Some(_)` after `build` succeeds.
  - `ReadOnlyMemory` trait — lives in the standalone `read-only-memory`
    crate (re-exported by `strider-reader` as `crate::ReadOnlyMemory`),
    NOT in `strider-ir`.  One method,
    `read(&self, addr: u64, buf: &mut [u8]) -> anyhow::Result<()>`:
    fill-all-or-error (no partial fill, no truncation), copying the bytes
    **raw** — there is NO endianness swap.  Callers that need an integer
    decode the raw bytes per the target's endianness (the optimizer does
    this via `strider_target::Endianness`).  Blanket impls for `Arc<T>`
    and `Box<T>`.  Concrete impls live in `strider-reader`.  The
    optimizer's `LoadReadOnly` takes `&dyn ReadOnlyMemory` so it doesn't
    depend on the reader crate.
  - `ValueKind` — `Control`, `Memory`, `PhiToken`, or
    `Typed(ValueType)`.
  - `ValueType` — integers `I1` (the 1-bit boolean), `I8`, `I16`,
    `I32`, `I64`, `I80` (x87 80-bit extended), `I128`, `I256`, `I512`;
    floats `F32`, `F64`, `F80`.  There is no separate `Bool` type or
    category: a boolean is the 1-bit integer `I1`, so `is_integer()` is
    true for it and `bit_width(I1) == 1` (the lone case where bit width
    isn't `byte_size * 8`).  `ValueType::int_for_byte_size(n)` /
    `float_for_byte_size(n)` map a varnode byte size to a type (byte size
    1 → `I8`, never `I1`); there is no `TryFrom<u32>`.  Constants wider
    than 64 bits (`I80` / `I128` / `I256` / `I512`) don't fit
    `IntPayload::Small(u64)`, so they're interned in
    `Function::wide_const_interner` and referenced via
    `IntConst(IntPayload::Wide(WideConstId))`.
  - **IR trait layering** — point reads, control-aware walks, and node
    creation are split across four traits (no `IrGraphExt`, no `Builder`
    trait — both dissolved):
    - `IRViewer` (`crates/strider-ir/src/viewer.rs`) — the read trait
      with one required method `fn function(&self) -> &Function`; every
      point read is a default method over `self.function()`:
      `node_kind` / `node_inputs(_exact)` / `node_outputs(_exact)` /
      `value_kind` / `producer` / `kind_of_value` / `value_type` /
      `require_*` / `validate_value_inputs` / `const_value` / `get_as_*` /
      `int_const_val` / `bool_const_val` / `memory_output_of` /
      `reachable_kind_iter` / `infer_float_type`.  `Function` implements
      it directly; `FunctionBuilder` and `EditFunction` get it too.
    - `IRWalker: IRViewer` — the control-aware walks (`walk` / `walk_from`
      / `walk_info` / `postorder` / `reverse_postorder` /
      `reverse_postorder_filter` / `postorder_filter` / `walk_kind` /
      `count_kind` / `has_kind`), blanket-impl'd for every `IRViewer`.
      `EditFunction` shadows the order-producing ones with versions that
      reuse its cached live/roots bookkeeping instead of re-walking from
      entry.
    - `IRBuilder: IRViewer` (`builder/build_trait.rs`) — the creation
      seam (`create_node_attributed` + `create_node`), implemented by
      `FunctionBuilder` and `EditFunction`.
    - `IRBuilderExt: IRBuilder` (`builder/builder_ext.rs`) — the blanket
      `build_*` construction vocabulary (`build_int_const`,
      `build_int_binary_operation`, `build_float_*`, …) plus coercion
      helpers.
  - `EditFunction` (`crates/strider-ir/src/function/edit.rs`) — the
    destructive-edit context used by the optimizer's rewrite rules: a
    borrowed `&mut Function` plus an **owned** `FunctionState` (live-set /
    roots bookkeeping it self-maintains).  Single constructor
    `EditFunction::new(&mut Function) -> Result<Self>` (errors if the
    function has no entry).  It does NOT cull pre-existing dead nodes at
    construction — call `cull_dead(&mut self)` explicitly for that.
    Implements `IRViewer` / `IRBuilder` and shadows the order-producing
    `IRWalker` methods with cached-state versions.
  - `walk::walk_graph(graph, entry)` (`pub(crate)`) — preorder
    traversal that follows both backward-data and forward-control
    edges.  Used by the validator and several internal passes; not
    exposed to downstream crates.  The control-aware walk family above
    layers on top of it via `IRWalker`.
  - `node_signature::{ExpectedValueKind, expected_signature}` — single
    source of truth for expected input/output slot kinds per `NodeKind`.
  - `validate::validate(function: &Function) -> Result<(), ValidationErrors>`
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
      wide-const consistency (including a dedicated check that an
      `IntConst(IntPayload::Wide(..))` node declares an I80/I128/I256/I512
      output type matching its interned byte size), and the always-on
      asm-fingerprint check (every reachable non-exempt node MUST carry
      ≥1 fingerprint).
    - Errors are aggregated into a `ValidationErrors` bundle rather than
      failing fast.
  - `function::dot` module — IR-specific Graphviz / HTML rendering on top
    of the generic `dot` crate.  The pretty `FunctionDotDumper`
    (`Function::dot_dumper`) inlines constants, adds virtual nodes, and
    needs a `Sleigh` for register names; `Function::raw_dot` /
    `raw_html` (the `function::dot::raw` submodule) render the graph
    **exactly as stored** instead — one node per reachable `NodeId`, one
    edge per input edge, side-tables shown inline, no Sleigh — for
    debugging the real graph shape.
  - **Asm-fingerprint side-table** (`Function::asm_fingerprints`) — every
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
    exempt from the non-empty check.  Public API on `Function`:
    `asm_fingerprint(id)`, `extend_asm_fingerprint`,
    `extend_asm_fingerprint_from`.

- **`strider-target`** — pure target descriptions, no Sleigh or IR
  dependencies.

  - `SleighArch` — `.sla` + `.pspec` + `Endianness`.  Presets: `x86_64`,
    `x86`, `aarch64` / `aarch64be`, `arm` / `arm_be` / `arm_thumb`,
    `mipsbe32` / `mipsle32` / `mipsbe64` / `mipsle64`, `ppc32be` /
    `ppc32le` / `ppc64be` / `ppc64le`.
  - `ArchPreset` — closed enum threaded into
    `strider_cfg::Builder::for_arch` and `CallOther`
    classification.
  - `CallingConvention` / `BuiltCallingConvention` — names-of-registers
    DSL and its register-resolved counterpart.  Userland presets:
    `x86_cdecl`, `x86_64_systemv`, `x86_64_all_preserving`,
    `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`,
    `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`.  The one
    Linux kernel-internal preset is `x86_linux_kernel` (x86 32-bit
    `-mregparm=3`) — the only arch whose kernel CC diverges from its
    userland ABI; every other arch's kernel CC equals the userland
    preset, so callers use that directly.  Syscalls are **not** calling
    conventions: the `syscall` / `int 0x80` / `svc` traps lift to
    `CallOther` (classified via `call_other_abi`), so there are no
    syscall CC presets.  The link-register-as-callee-saved tradeoff
    (AArch64 `x30`, ARM `lr`, PowerPC `LR`) is preserved — the
    indirect-branch resolver's `LinkRegister` arm uses it.
  - `call_other_abi::classify(preset, name)` — CallOther classification
    (`NoOp` / `NoReturn` / `Call(CallOtherAbi)`) consumed by both
    `strider_cfg::region_builder` (trap-region termination) and
    `strider_lift`'s `FunctionLifter::handle_call_other` (`lift/call.rs`).
    `ArchPreset` arrives via `strider_cfg::Builder::for_arch(arch, …)`.
    `CallOtherAbi` carries `implicit_reads` / `implicit_writes` /
    `clobbers_memory` (a `bool`) describing the ISA-fixed
    register-and-memory footprint beyond Sleigh's pcode-explicit args.
  - `StackArgs { base_offset, increment }` — the unbounded stack-arg
    layout: the N-th stack argument sits at `base_offset + N*increment`
    bytes from the call-time SP (every supported ABI's stack-arg series is
    a uniform stride = its word size, so this is exact and has no upper
    bound).  `offset_of(n)` gives a slot's byte offset; `index_of(off,
    size)` is the strict within-one-slot index; `slot_of(off)` floors a
    byte offset onto its containing slot (no size bound — a wider-than-slot
    argument anchors at the slot its first byte lands in).
  - `PositionalArgLayout { registers: Vec<Vn>, stack: Option<StackArgs> }`
    — positional-arg layout derived via `cc.positional_arg_layout()`
    (register slots `0..registers.len()`, then unbounded stack slots);
    `first_stack_index()` / `stack_offset_of(index)`.  `None` stack =
    no stack args.
  - **Stack-arg passes (incoming + outgoing) classify any number of
    slots:** `FunctionArgDetect` floors each entry-SP load onto its slot
    (`slot_of`) and runs a width-aware cursor that maps each anchored
    argument to one *positional ordinal* (a wider-than-slot argument
    consumes the slots it spans but advances the ordinal by one).
    `CallStackArgCollect` mirrors it for a `Call`: a slot cursor anchored
    at the call-time SP probes each slot via the shared
    `crate::sp_expr::reaching_sp_store` (the `MemPhi`-sound memory-SSA
    walker `find_nearest_clobber` + `SpAliasOracle`), appending one Call
    input per anchored store and advancing past its slot span; collection
    is intentionally **over-inclusive** (incidental in-window stack writes
    are indistinguishable from arg pushes once lowered, so every plausible
    reaching store is collected).  `reaching_sp_store` is the one SP-store
    lookup shared with the indirect-branch stack-array classifier
    (`indirect_branch_resolve::table`), which probes typed table entries.
    `store_alias_verdict` consults `Function::stack_offsets` (the SSoT for
    post-optimization SP offsets) before `decompose_sp`.

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

- **`strider-cfg`** — bytes → CFG.  Builds a Control Flow Graph
  (`Cfg`) from a binary using `rsleigh`.  IR-free (no `strider-ir` dep).
  Uses `petgraph::StableDiGraph` internally.  The load-bearing per-region
  machinery lives in `builder/region_builder.rs`: `RegionBuilder::build`
  decodes one machine instruction at a time, preserving
  sequential-within-region decoding (Sleigh's `lift_one(&mut self)`
  carries context-register state, so out-of-order per-insn lifting across
  regions is not safe), and delegates to GHIDRA's internal
  `DisassemblyCache` (rsleigh's `Sleigh` owns it) for per-address
  memoisation.  (IR fingerprint attribution via `set_lift_addr` is not
  done here — it happens later in `strider-lift`'s CFG→IR region driver.)
  Bounded-lift semantics (`fn_max_size`) and
  `is_addr_tail_call` live alongside it.  The only public construction
  path is `Builder::for_arch(arch, sleigh, addr, &CfgOptions)` so
  endianness and `ArchPreset` are derived from the arch atomically.
  `CfgOptions` is the public SSoT for CFG-shaping knobs (`fn_max_size`,
  `allow_code_before_start_addr`, `known_targets`); the orchestrator
  seeds `known_targets` to thread IR-resolved indirect branches back into
  a CFG rebuild.  `ResolvedTargets` (the resolved-branch enum) lives in
  `builder/indirect_resolver.rs`.

- **`strider-lift`** — CFG → IR.  Lifts a `strider_cfg::Cfg` into a
  `strider_ir::Function`.  One module (`lift`):

  - `Lifter<R>` — the reusable lift engine: it **owns** the target arch,
    the `rsleigh::Sleigh<R>`, and a cached `SleighRegs`.  Built once via
    `Lifter::new(arch, sleigh)`; the calling convention is a **per-call**
    argument (not stored).  Entry points: `build_cfg(&mut self, entry,
    &CfgOptions)`, `build_ir(&self, &Cfg, cc)` /
    `build_ir_with(&self, &Cfg, cc, &LiftOptions)`, and the build+lift
    convenience `lift(&mut self, entry, cc, &LiftOptions)`.  Not `Clone`
    (the owned `Sleigh` isn't cheaply cloneable).
  - `FunctionLifter` — the per-CFG transient (`Lifter::build_ir`
    builds one, borrowing the `Lifter` for arch/Sleigh/regs + the per-call
    cc).  It owns the IR `FunctionBuilder` and lifts **every** pcode
    opcode — value-producing *and* control-flow — as `&mut self` methods,
    routed through a single `process_insn` match (`lift/dispatch.rs`)
    over a flat by-family handler layout: the value families
    (`arithmetic` / `boolean` / `cast` / `float` / `integer` / `memory` /
    `misc`), plus the control handlers `handle_branch` / `handle_return` /
    `handle_call` / `handle_switch` / `handle_tail_call` /
    `handle_unresolved_indirect_branch` (`lift/control.rs`), `handle_store`
    (`lift/memory.rs`), and `handle_call_other` (`lift/call.rs`).
    `read_vn` / `write_vn` (`lift/vn_io.rs`) own the register-aliasing
    dispatch.  There is no separate `ValueLifter` struct — value lifting
    was unified onto the per-CFG driver.
  - `lift/pcode_util.rs` — the free decode helpers: `vn_sort_key`
    (re-exported at `strider_lift::lift::vn_sort_key` for the
    orchestrator's cached vn table), the checked input accessor
    `nth_input_or_err` (every production-code varnode access returns a
    typed error instead of panicking on an out-of-bounds index), and
    `decode_space_id`.  See the "Register
    Aliasing" section below.
  - `LiftOptions` (crate root) is the whole-lift options type: it embeds
    a `strider_cfg::CfgOptions` (`cfg`, handed to
    `strider_cfg::Builder::for_arch` as `&lift_opts.cfg`) plus the IR-lift
    knob `per_address_ccs` and the post-pipeline `compact` knob.  The
    tracked varnode set is scanned fresh from the CFG at lift time
    (`Lifter::find_all_unique_vns`), so it is NOT a `LiftOptions` field.

- **`strider-opt`** (optimization passes) **+ `strider-orchestrator`**
  (orchestration).  `strider-opt` is the crate root for the former `opt`
  module; `strider-orchestrator` re-exports it as `opt` (`pub use
  strider_opt as opt;`) and adds the `strider` lift driver and the
  `orchestrator`.  Paths below
  written `opt::X` resolve as `strider_opt::X` (and equivalently
  `strider_orchestrator::opt::X`).

  - `opt` module (crate `strider-opt`) — optimization passes.  All passes implement
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
    - `IfCondInversion` — matches an `If` whose cond is `Xor(C, IntConst(1)):I1`
      (the canonical shape of logical NOT after the BitNot→Xor lift) and
      rewrites it to `If(C){B}{A}` (branches swapped).
    - `RedundantPhis` — eliminates `Phi` (tagged or anonymous) /
      `MemPhi` / `Region` with a single reachable predecessor.
    - `DeadBranchElimination` — removes `If(const)` branches and strips
      dead control edges.
    - `LoadReadOnly` — folds constant-address loads via
      `&dyn ReadOnlyMemory`.
    - `StackOffsetDetect` — annotates SP-relative `Store` / `Load`
      offsets in `Function::stack_offsets`; the unified memory chain
      is left intact.
    - `LoadForward` — forwards a `Store`'s value to a subsequent `Load`
      when the load's nearest may-clobbering memory def (found via the
      `memory_ssa::walk_memory_ssa` walker) is an exact-match store to the
      same location.  A control-merge `MemPhi`, a `Call`, or a
      non-exact-overlapping store blocks the forward; it NEVER synthesises
      a value-`Phi`.
    - `FunctionArgDetect` (post-pass) — detects stack-passed arg reads
      (`Load[sp + K]`) and records their carrier values in the
      `Function::arg_index_to_values` side-table.  Register-passed args are
      recorded at builder entry (`FunctionBuilder::set_entry_region`), not
      by this pass; carrier `NodeId` is `InitialVar` for register args,
      `Load` for stack args.  There is no `FunctionArg` `NodeKind` variant.
    - `CallStackArgCollect` (post-pass) — wires positional stack args
      into `Call` nodes.
    - Imperative peephole passes use `pattern::Matcher::find_all`
      rather than rolling their own matching.
  - the `strider-pattern` crate (a separate dependency, not a module of
    `strider-opt`) — pattern DSL (`Pat` / `Capture` / `Matcher` /
    `Match` and fluent builders).  Cross-pattern joins on shared
    captures via `Matcher::find_joined`.
  - The reusable lift engine `Lifter` (re-exported at the crate root from
    `strider-lift`, alongside `LiftOptions` / `LiftOutcome`) builds + lifts
    a CFG; `strider_opt::default_pipeline()` is the single canned pipeline.
    (There is no `LiftDriver` wrapper — it was removed; the Python `Lifter`
    binding and the test helpers hold a `Lifter` directly.)
  - `Strider::analyze(entry, cc, &LiftOptions, &OptOptions, Option<OptimizerPipeline>) -> Result<AnalyzeResult>`
    (the `Strider` handle is re-exported at the crate root) — the canonical
    top-level entry.  Build the CFG, lift to IR, run the optimiser pipeline
    (the caller's via `Some(p)`, else `default_pipeline()`; the
    `IndirectBranchClassify` post-pass is always appended), then loop: each
    iteration folds new classifications into `CfgOptions::known_targets` and
    re-lifts; it stops once no indirect branch is deferred (fully resolved)
    or nothing new resolves (the rest are unresolvable).  A single pipeline
    runs every iteration (node-removing passes included — no
    stable/destructive phase split), built once and reused.  Unresolvable
    branches are **not** an error — they're returned in
    `AnalyzeResult::unresolved_indirect_branches` (their placeholders remain
    in `function`); `compact` (a `LiftOptions` knob) is applied at finalize.
    `Strider::new(arch, sleigh, rom)` builds the handle from a target arch +
    owned `Sleigh` + optional ROM.
  - `opt::indirect_branch_resolve` module — the live-IR indirect-branch
    classifiers: `classify_anchor` (producer-shape classifier) and
    `classify_table_dispatch` (the unified rodata-jump-table /
    stack-array recognizer), driven by the `IndirectBranchClassify`
    optimizer post-pass that runs once on the converged graph.
    `ResolvedTargets { LinkRegister, Single(u64), Multiple(Vec<u64>) }`
    lives in `strider-cfg` (re-exported as `strider_cfg::ResolvedTargets`);
    the orchestrator records each classification into
    `CfgOptions::known_targets` and rebuilds the CFG.  Resolution is
    rebuild-driven — there is no cfg-time resolver callback and no
    in-place IR editor.
  - `apply_rules_count(ctx: &mut EditFunction, rules: &[R]) -> Result<usize>`
    — the whole-graph rewrite driver: walks every reachable node and tries
    each rule (round-robin) at it, returning the total per-`(node, rule)`
    fire count.  The caller owns ctx construction (`EditFunction::new`); a
    single rule is `std::slice::from_ref(&rule)` and a boolean is
    `count > 0`.  (Replaces the former `GraphRewriter` façade and the
    `GraphEditFunctionExt::with_rewrite_ctx` extension trait, both removed.)
  - Error handling — fallible operations return `anyhow::Result`
    (`opt::Result` aliases it; `pattern::error` adds only the internal
    `RewriteSkip` / `PatternBuildError` sentinels).  There is no bespoke
    error catalogue in this crate.  An indirect branch that can't be
    resolved is **not** an error: `Strider::analyze` returns an
    `AnalyzeResult { function, unresolved_indirect_branches }` whose
    `unresolved_indirect_branches` lists the pcode address of each branch
    whose `IndirectBranch` placeholder is still in the function (empty when
    every branch resolved).  A caller wanting full resolution asserts that
    list is empty.  (The typed Python-facing exception hierarchy lives in
    `strider-py`.)

  The repetitive `Py*Pat` builders are generated in-crate by local
  `macro_rules!` in `pattern.rs` (there is no separate proc-macro crate):
  `node_builder!` emits the node-rooted builders (`Load` / `Store` /
  `Ret` / `Phi` / `MemPhi` / `ValuePhi` / `CallOther`) from a compact
  field-set spec (operand kind, slot, root flavor), and
  `binary_op_builder!` emits the binary-op builders (`PyIntBinaryPat` /
  `PyFloatBinaryPat` / `PyBoolBinaryPat`).  The `.when()` wiring and
  capture handling live in one place inside `node_builder!`.  `Call`
  (the `at_target` literal-vs-Pat precedence over a separate field), `If`
  (branches take a finished `Pattern`, not an operand slot), and
  `PyFunctionArgPat` (enum-dispatch `source`, not a uniform slot) keep
  their quirks and stay hand-written.  `bool_binary` returns a chainable
  `BoolBinaryPat` symmetric with `int_binary` / `float_binary` — a
  boolean op is still an `IntBinaryOp` at `I1`, and the bool builder
  keeps an `I1`-output guard so it never matches a same-shaped wide
  integer op.

- **`strider-ir-test-utils`** — `RegisterSet` (fluent builder over
  the single `FunctionBuilder::new` constructor), `make_empty_fn`,
  `make_fn_with_var`,
  `reg_vn`, and the `SENTINEL_LIFT_ADDR` constant
  (`0xDEAD_BEEF_0000_0001`).  Helpers auto-stamp the sentinel asm
  fingerprint on every node created through them so mock-graph tests
  satisfy the always-on asm-fingerprint check without manual stamping.

- **`strider-py`** — Python bindings (PyO3 + maturin + abi3-py39).
  The single lift+optimise+resolve handle is `strider.Lifter` (build one
  via `strider.lifter(arch, mem, rom=None)` or `strider.Lifter(arch, mem,
  rom=None)`); `cc` is NOT fixed at construction — it's a required
  argument of every `analyze` call, so one handle can analyse functions
  under different calling conventions.  `lifter.build_cfg(entry, ...)` is
  structural-only (no lift/optimise/indirect-branch resolution);
  `lifter.analyze(entry, cc, **opts) -> (Function, unresolved_addrs)`
  drives the full fixed-point loop (per-call opts `function_max_size` /
  `allow_code_before_start_addr` / `compact` / `per_address_ccs` /
  `calls_clobber` / `assume_distinct_sp_bases_disjoint` / `alias_mode`).
  `strider.load_elf(path) -> ElfLifter` auto-detects arch/CC from the ELF
  `e_machine` (override via `arch=`/`cc=`/`apply_relocations=`) and
  delegates to `load_elf_from_segments(path, ...)` (regions collected by
  walking `PT_LOAD` segments, falling back to sections for `ET_REL`);
  `load_elf_from_sections(path, ...)` forces the section-header-walk
  strategy even for a linked binary that does carry `PT_LOAD` segments.
  All three return an `ElfLifter`.  `ElfLifter` **is** a `Lifter` (`isinstance(x, strider.Lifter)` is
  true) that additionally wires the ELF's sections as both the code
  reader and the `LoadReadOnly` rom, and adds `symbol` / `symbol_size` /
  `symbols` / `entry_point` / `read` / `reader` plus a name-aware
  `analyze(target, ...)` that accepts a `str` symbol name or an address.
  `lifter.optimize(function, pipeline=None)` runs an `OptimizerPipeline`
  over `function`'s IR in place (mutating it, same as `analyze`'s
  internal run); `pipeline=None` builds and runs the canonical default
  pipeline instead.  This is the sole way to re-run/apply optimization
  passes on an already-lifted `Function` — `Function.optimize` and
  `Function.reoptimize` were removed in favor of this single
  `Lifter`-owned entry point (a bare `Function` carries no pipeline
  state of its own).
  Pattern queries (`find_all` / `find_one` / `find_joined`) and the
  addr-only `fingerprint`/`asm_fingerprint` live directly on the returned
  `Function`/`Node` — there is no separate `Analysis` wrapper class.  The
  Sleigh-needing pretty renders (`dump_html` / `dump_dot` / `html_str`)
  and the p-code audit-trail helper
  (`fingerprint_pcode(node, function=None) -> list[(addr, text)]`, which
  accepts a `Node`, a `Match`, or a raw `int` node id) live on `Lifter`
  instead, since only it owns the Sleigh.  Low-level API mirrors the
  Rust surface: `SleighArch`, `CallingConvention`, `BufferReader` (a
  RAW-region reader for non-ELF / custom sources — ELF parse + symbols
  live on the internal `_LoadedElf` that `ElfLifter` wraps, built by
  `strider.load_elf(path)`), `MemReader`, `ReadOnlyMemory`, `Sleigh`,
  `Function`, `Cfg`, `OptimizerPipeline`.  `strider.opt` exposes
  per-pass classes (every built-in pass is now zero-argument — the
  calling convention is read from the function under analysis at run
  time, not bound into the pass at construction).  `strider.pattern` is
  a full mirror of the Rust pattern crate, with descriptive constructor
  names: `int_and` / `int_or` / `int_xor` / `int_not` (bitwise ops —
  `and_`/`or_`/`xor`/`bit_not`/`not_` are gone), `anything()` (the
  wildcard, formerly `any_()`), `if_else` (the `If` pattern builder,
  formerly `if_()`).  `strider.template` is the build-side (`replace`)
  mirror of `strider.pattern` — free functions (`var(c)`, `int_const`,
  `add`, `int_and`, …) construct a `Template` from only the build-valid
  subset (no `.when()`, no commutativity, no wildcards, since those are
  match-only concepts); `Function.rewrite`/`rewrite_all` type `replace`
  as a `Template`, though a bare `strider.pattern.Pat` is still accepted
  for back-compat.  Cross-pattern joins on shared captures via
  `Function.find_joined([pat1, pat2, …])`.  Asm-fingerprint accessor:
  `match.asm_fingerprint(c) -> list[int]`.  Every Rust error lands in
  Python as a single `strider.errors.StriderError` exception carrying an
  informative message; the hierarchy is intentionally flat (no typed
  subclasses).  An unresolved indirect branch is **not** an error: it is
  reported via `analyze`'s second return value, `unresolved_addrs` (a
  `list[int]` of machine addresses, empty when fully resolved).
  Dev workflow uses uv: `uv sync --group dev` → `uv run maturin develop` →
  `uv run pytest`.

### IR Node Model

The IR is a sea-of-nodes graph where each `Node` has typed inputs
(`ValueId` references) and outputs.  The `expected_signature` table
in `crates/strider-ir/src/node_signature.rs` is the single source of
truth for every node's input/output shape.  Node kinds, grouped:

- **Initial state:** `Entry`, `InitialMemory`, `InitialVar(Vn)`.
  arg tracking is recorded in the `Function::arg_index_to_values`
  side-table mapping each CC argument index to its carrier value
  (`InitialVar` output for register args, recorded at builder entry;
  `Load` output for stack args, recorded by `FunctionArgDetect`) — there
  is no `FunctionArg` `NodeKind` variant.
- **Region / join:** `Region` (variadic Control inputs; outputs
  `Control` + `PhiToken`), `MemPhi` (φ for the memory token), `Phi`
  (unit-variant node kind covering both tagged and anonymous forms).
  The optional source-varnode tag lives in the `Function::value_vn` map
  (keyed by the Phi's output `ValueId`), read / written via
  `get_vn_for_value` / `set_vn_for_value`: `Some(vn)` marks the
  lifter-emitted SSA φ for the register-aliased read of varnode `vn`; no
  entry marks an anonymous value phi (consumed by the indirect-branch
  jump-table classifier's `Phi`-of-`IntConst` arm).  No optimizer pass synthesises anonymous
  value phis — `LoadForward` forwards only an exact-match dominating
  store and treats a control-merge `MemPhi` as an opaque boundary.
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
- **Integer (incl. booleans):** `IntConst(IntPayload)` — one node kind for
  every integer constant, where `IntPayload` is `Small(u64)` (inline, I1…I64)
  or `Wide(WideConstId)` (I80/I128/I256/I512, interned in
  `Function::wide_const_interner`); read the value via
  `IRViewer::int_const_u128` / `int_const_val`, never by matching the payload.
  `IntUnaryOp`
  (`Neg` for `-x`; bitwise complement `~x` is `Xor(x, all_ones)` — no
  dedicated `BitNot` variant), `IntBinaryOp` (`And` / `Or` / `Xor` /
  `Add` / `Mul` / shifts / …; no `Sub`; lifter lowers to
  `Add(_, Neg(_))`), `IntCmpOp` (`Equal`, `Less`, `Sless`, `Carry`,
  `Scarry`, `Sborrow`; no `LessEqual` / `SlessEqual` — both are
  lift-time-lowered shapes; output is `I1`), `Truncate`,
  `Extend(ExtendOp)`, `Popcount`, `Lzcount`.  **Booleans are the 1-bit
  integer `I1`** — there is no `BoolConst` / `BoolBinaryOp` /
  `BoolUnaryOp` / `CastToBool` / `CastToInt`: a bool constant is
  `IntConst(0|1):I1`, logical and/or/xor are `IntBinaryOp::{And,Or,Xor}`
  at `I1`, logical not is `Xor(x, IntConst(1)):I1`, bool→int widening
  is `Extend(ZeroExtend)`, and int→bool conversion is never needed (Sleigh
  always feeds an already-`I1` condition).
- **Float:** `FloatConst(u64)` (bits), `FloatUnaryOp`, `FloatBinaryOp`
  (`Add` / `Mul` / `Div`; no `Sub`, lifter lowers to
  `Add(_, Neg(_))`), `FloatCmpOp` (`Equal`, `Less`; output `I1`; no
  `NotEqual` / `LessEqual` — both lifted to lowered shapes; `FLOAT_NAN(x)`
  is lowered to `Xor(FloatEqual(x, x), IntConst(1)):I1`).
- **Float / int conversions:** `IntToFloat`, `FloatToInt`,
  `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`.  There is no
  `CastToFloat`: an int→float cast is a same-width `IntBitsToFloat`, and a
  float→float reprecision is `FloatToFloat`.  `FloatToFloat` is
  float→float only.
- **Opaque / user-defined:** `SegmentOp { op_id }`, `CPoolRef`, `New`.

### Pattern DSL

`strider_pattern` exposes `Pat` / `Capture` / `Matcher` /
`Match` with fluent builders for every node kind.  Key points:

- The Python `Py*Pat` builders are generated in-crate by local
  `macro_rules!` in `strider-py`'s `pattern.rs` (`node_builder!` for the
  node-rooted builders, `binary_op_builder!` for the binary-op builders)
  — there is no separate proc-macro crate.  The Rust-side `strider-pattern`
  builders are hand-written; the Python mirror is a thin runtime-recursion
  bridge onto them.
- The binary-op builders `PyIntBinaryPat` / `PyFloatBinaryPat` /
  `PyBoolBinaryPat` come from `binary_op_builder!`; `Call`, `If`, and
  `PyFunctionArgPat` (enum-dispatch source) stay hand-written.
  `bool_binary` returns a chainable `BoolBinaryPat`, symmetric with
  `int_binary` / `float_binary` — a boolean AND/OR/XOR is still
  `IntBinaryOp` at `I1`, and the bool builder pins the output to `I1`
  (with an `I1`-output post-match guard) so it never matches a
  same-shaped wide integer op.
- **Querying booleans by width** (booleans are `I1`, not a distinct type):
  `value_of_width(n)` / `bool_value()` filter by *output* width (width 1 =
  "produces a bool", including comparisons); `inputs_of_width(n, inner)` /
  `bool_inputs(inner)` filter by *input* width (width 1 = "operates on
  booleans", excluding comparisons whose operands are wider).
- **Lift-time canonicalisation** (the lifter applies these so patterns
  match the canonical shape):
  - `IntSub(a, b)` → `Add(a, Neg(b))`.
  - `IntLessEqual(a, b)` → `Xor(IntLess(b, a), IntConst(1)):I1` (swap
    args; logical-not of a 1-bit value is `Xor(_, IntConst(1)):I1` since
    bitwise complement `~x` is `Xor(x, all_ones)`).
  - `IntNotEqual(a, b)` → `Xor(IntEqual(a, b), IntConst(1)):I1`.
  - `FloatSub(a, b)` → `FloatAdd(a, Neg(b))`.
  - `FloatNotEqual(a, b)` → `Xor(FloatEqual(a, b), IntConst(1)):I1`.
  - `FloatLessEqual(a, b)` → `Or(FloatLess(a, b), FloatEqual(a, b))` at
    `I1` (NaN-aware; `Or` is `IntBinaryOp::Or`).
  - `FLOAT_NAN(x)` → `Xor(FloatEqual(x, x), IntConst(1)):I1`.
  - `If(Xor(C, IntConst(1)):I1){A}{B}` → `If(C){B}{A}` (via
    `opt::IfCondInversion`).
- **Commutative matching:** `add`, `mul`, `and`, `or`, `xor` (and bool
  equivalents), `int_cmp(Equal/Carry/Scarry)`, and `float_cmp(Equal)`
  automatically try both operand orderings.  Driven by
  `NodeKind::is_commutative()` — the single source of truth.

### Register Aliasing

Overlapping registers (x86 `rax`/`eax`/`ax`/`al`/`ah`, AArch64
`q0`/`d0`/`s0`, x87 `ST*`, etc.) are dispatched by the lifter's
`read_vn` / `write_vn` (`FunctionLifter` methods in
`crates/strider-lift/src/lift/vn_io.rs`), which route REGISTER / UNIQUE
varnodes to the IR builder's `read_reg_vn` / `write_reg_vn`.  The
aliasing logic itself lives on the `strider_ir::FunctionBuilder`
(`crates/strider-ir/src/builder/vn_io.rs`): all reads and writes go
through the largest containing register, with shift / mask operations
inserted for sub-register slices.  `find_largest_fitting_register` is
the entry point — it delegates to the persisted `Function::container_of`
(there is no builder-lifetime container cache).  `vn_mask` enumerates
supported widths: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM /
q-register), 32 (YMM), 64 (ZMM) bytes.  Widths > 16 use a degraded
`u128::MAX` mask; the wide-container guard rejects sub-register aliasing
within > 16-byte containers with a clear error.

Varnode canonicalization is owned entirely by `FunctionBuilder::new`
(the lifter does NOT sort / dedup / map): it seeds the CC registers,
drops varnodes contained in a larger tracked varnode
(`dedup_overlapping_largest`), sorts the result deterministically by
`(space, offset, size)` so `VarId` assignment is stable, and builds the
`vn_to_container` map.  Canonicalization applies only to REGISTER /
UNIQUE space — CONST relies on the graph's structural dedup cache and RAM
(load/store) is intentionally left alone.

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
