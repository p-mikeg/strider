# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
# Build all crates
cargo build --workspace

# Run the main example (reads fixtures/out/x86/test.elf — must be built first
# from fixtures/Makefile — and dumps cfg.html, graph.html, and graph-opt.html
# in the workspace root). The example lives in crates/strider/examples/strider.rs.
cargo run -p strider --example strider

# Run all tests
cargo test --workspace

# Run a single test
cargo test --package <crate-name> <test_name>

# Lint
cargo clippy --workspace
```

## Architecture Overview

This is a Rust workspace binary analysis tool that lifts native binaries to an IR and exposes it for arbitrary pattern queries via Python. The pipeline is:

**Binary → CFG → IR → Optimizations → Pattern Queries (Python)**

### Crate Dependency Flow

```
   target  ←  ir  ←  pcode-lift  ←  cfg  ←  strider
     ↑       ↑↑          ↑                    ↓
     └───── opt ←────── pattern  ←────────────┘
            ↑
   reader (ELF + ReadOnlyMemory, used by opt::LoadReadOnly and the example)

   dot      (visualization helper, used by cfg, ir, and the example)
   rsleigh  (external, at ../rsleigh — Sleigh/GHIDRA p-code lifter)
```

Layering in words: `target` is pure data (architectures + calling conventions);
`ir` is the sea-of-nodes graph; `pcode-lift` is the value-producing pcode→IR
lifter (factored out of `strider`); `cfg` builds basic blocks; `strider` ties
the CFG to the IR and runs the indirect-branch fixed-point loop; `opt` rewrites
the IR; `pattern` queries it.  Errors propagate via `anyhow::Result` workspace-wide.

### Key Crates

- **`reader`** — Loads ELF binaries and provides a memory reader (`ElfFileMemReader`) for rsleigh.
- **`cfg`** — Builds a Control Flow Graph (`Cfg<R>`) from a binary using rsleigh. Uses `petgraph::StableDiGraph` internally. Regions (basic blocks) contain p-code instructions (`rsleigh::Insn`). Edge kinds: `Fallthrough`, `Branch`, `IfCaseTrue`, `IfCaseFalse`.
- **`ir`** — Sea-of-nodes style IR graph. Core types:
  - `Graph` — stores `NodeId`, `NodeOutputId`, `NodeInputId` via `cranelift-entity` PrimaryMaps. Nodes are deduplicated/cached by (kind, inputs, output_kinds). Per-node side-tables (e.g. `stack_phi_offsets: SecondaryMap<NodeId, Vec<i64>>`) hold ancillary data.
  - `FunctionBuilder` — builds the IR graph with SSA-like variable tracking. Variables map `rsleigh::Vn` (varnode) → `VarId`. Each region gets a `ControlState` node + `VarPhi` nodes for variables. Calls `validate::validate` at the end of `build()`.
  - `NodeOutputKind` — `Control`, `Memory`, `PhiToken`, or `OutputType(NodeOutputType)`.
  - `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U128`, `U256`; floats `F32`, `F64`.
  - `walk::walk_graph(graph, entry)` — traversal that follows both backward-data and forward-control edges. Used by the validator and several passes.
  - `node_signature::{ExpectedOutputKind, expected_signature}` — single source of truth for expected input/output slot kinds per `NodeKind`. `ExpectedOutputKind` coarsens int/float widths via `AnyInt` / `AnyFloat` / `AnyValue`.
  - `validate::validate(&graph, entry) -> Result<(), ValidationErrors>` — whole-graph validator with three layers:
    - **Layer A**: per-node local typing against `expected_signature` (scoped to nodes reachable via `walk_graph`, since optimization passes leave detached zombie nodes in the arena).
    - **Layer B**: bidirectional use-list consistency.
    - **Layer C**: graph-level invariants (Entry/InitialMemory uniqueness, ControlState predecessor kinds, phi token ownership & per-predecessor arity for `VarPhi`/`MemPhi` only — `StackStorePhi` has fixed arity 3).
    - Aggregates all errors into a `ValidationErrors` bundle rather than failing fast.
- **`target`** — Pure target-description data (no IR, no rsleigh state machine). Owns:
  - `SleighArch` — pairs a `.sla` spec + `.pspec` + `Endianness`. Presets: `x86_64`, `x86`, `aarch64` / `aarch64be`, `arm` / `arm_be` / `arm_thumb`, `mipsbe32` / `mipsle32` / `mipsbe64` / `mipsle64`.
  - `CallingConvention` (static-string register names) and `BuiltCallingConvention` (resolved `rsleigh::Vn` varnodes; produced by `CallingConvention::build`). Carries the stack-pointer varnode, integer + float return-value regs, callee-saved regs, positional `stack_arg_offsets`, the `ret_stack_pop` delta (`8` on x86_64, `0` on AAPCS), and the optional link-register varnode (`lr` on AArch64/ARM, `ra` on MIPS, `None` on x86/x86_64). CC presets: `x86_cdecl`, `x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`.
- **`pcode-lift`** — Pure value-producing pcode → IR lifter, factored out of `strider`. `ValueLifter::lift(insn) -> Result<bool>` returns `Ok(true)` for value-producing opcodes (arithmetic, casts, `Load`, etc.) and `Ok(false)` for control-flow / `Store` / call opcodes that the caller must route. **Owns the register-aliasing logic** (`vn_io.rs`): all reads/writes go through the largest containing register, with shift/mask ops for sub-registers (`al`/`ah`/`ax`/`eax`/`rax`, x87 `ST*` 80-bit slices, 16-byte `XMM`/`q*` containers). Both `strider` (per-region IR translation) and `cfg` (single-block mini-IR for indirect-branch resolution) reuse it.
- **`strider`** — Translates a `Cfg` to a `BuiltFunctionGraph` and drives the indirect-branch fixed-point. The canonical entry is `strider::run(config) -> Result<BuiltFunctionGraph>`; `Strider::new(arch, sleigh_regs, cc)` is the per-iteration handle used inside `run`. CC + arch types are re-exported from `strider` for back-compat. The actual register-aliasing logic lives in `pcode_lift::ValueLifter` (see `pcode-lift` above) — `IrStrider` is the per-function context that drives it region-by-region. Key surfaces:
  - `strider::run(config) -> Result<BuiltFunctionGraph>` (`crates/strider/src/orchestrator.rs`) — top-level entry point.  Builds the CFG, lifts to IR, runs the optimizer pipelines (stable then destructive), and drives the indirect-branch fixed-point.  `RunConfig` owns the `Sleigh` and threads it through every iteration.  The fixed-point loop is implemented as a small `LoopState` returning a `Decision { FixedPoint, StableOnly, Rebuild }` per step.
  - `Strider::analyze_cfg(&cfg) -> Result<AnalyzeOutcome>` — per-iteration lift driver used inside `run`.  `AnalyzeOutcome` bundles `graph: BuiltFunctionGraph`, `unresolved_branches: Vec<(PcodeInsnAddr, ir::Value)>` (placeholder anchors for indirect-branch resolution), and `region_handles: Vec<RegionLiftHandles>` (per-iteration index snapshot consumed by the orchestrator's `RegionIndex`).  Callers that only want the graph write `analyze_cfg(&cfg)?.graph`.
  - `Strider::build_optimizer_pipeline()` — full pipeline: `opt::default_pipeline()` + `StackStoreDetect` + `StackLoadForward` (both fixed-point), + `CallStackArgCollect` and `FunctionArgDetect` as post-passes.
  - `Strider::build_stable_optimizer_pipeline()` — passes whose rewrites survive a later iteration adding new phi inputs (`ConstantFold`, `KnownBits`, `StackStoreDetect`, `StackLoadForward`, + `FunctionArgDetect` post-pass). Used while the IR `Graph` is still growing under the indirect-branch resolver.
  - `Strider::build_destructive_optimizer_pipeline()` — node-removal passes the orchestrator runs **once** at the fixed-point exit (`RedundantPhis`, `DeadBranchElimination`, `CallOtherElide`, + `CallStackArgCollect` post-pass).  Running these mid-iteration would invalidate phi `NodeId`s the orchestrator's per-iteration `RegionIndex` pins.
  - `indirect_resolve` (`crates/strider/src/indirect_resolve/`) — indirect-branch resolver helpers: `classify_anchor` inspects a placeholder's anchored producer after the stable pipeline has run; `inplace::{apply_link_register, apply_tail_call}` rewrite the IR in place.  Both are called by `orchestrator.rs`'s `LoopState`.
  - `GraphRewriter` (`crates/strider/src/rewrite.rs`) — thin façade over `pattern::rewrite_rule` that walks the reachable graph, applies a substitution rule at every candidate root, and exposes a `re_optimize` shortcut.  Use case: collapse a resolved jump table after the orchestrator returns.
- **`opt`** — IR optimization passes. Passes added via `OptimizerPipeline::add` run in a shared fixed-point loop; `add_post_pass` runs once after convergence. `OptimizerPipeline::run` calls `ir::validate::validate` at the very end so any malformed graph is reported as an `opt::Error::IrError(ValidationFailed(...))`. Three pre-built top-level pipelines: `default_pipeline()` (all passes), `stable_default_pipeline()` (rewrites that survive phi-input growth — `ConstantFold` + `KnownBits`), `destructive_default_pipeline()` (node-removal passes safe only at fixed point — `RedundantPhis` + `DeadBranchElimination` + `CallOtherElide`). See `crates/strider/src/strider/pipeline.rs` for how `Strider` layers convention-aware passes on top. Passes:
  - `ConstantFold` — constant evaluation for all arithmetic, comparisons, booleans, truncation, extension; algebraic identities (`x+0→x`, `x^x→0`, nested AND-mask merging `(a&C1)&C2 → a&(C1&C2)`).
  - `KnownBits` — bit-level propagation of statically known zeros/ones to fold partially-known expressions.
  - `RedundantPhis` — eliminates `VarPhi` and `MemPhi` nodes and `ControlState` nodes with a single reachable predecessor; detaches inputs of CFG-unreachable nodes (leaving them as zero-input zombies — the validator skips Layer A on these via reachability scoping).
  - `DeadBranchElimination` — removes `If` nodes whose condition is a `BoolConst`; strips the dead control edge from successor `ControlState` and `VarPhi` nodes. Works together with `RedundantPhis`.
  - `CallOtherElide` — drops opaque `CallOther` nodes whose user-op is a known IR-level no-op (e.g. ARM `setISAMode`); the names live in `opt::NO_OP_USER_OPS`.
  - `LoadReadOnly` — resolves `Load` nodes whose address is a compile-time constant into constants by reading from a caller-supplied `ReadOnlyMemory` (e.g. `.rodata`/`.text` section). Configured with a ROM image, so `default_pipeline()` doesn't include it; the example and `Strider::build_optimizer_pipeline` callers layer it on.
  - `StackStoreDetect` — converts `Store` nodes whose address resolves to `InitialVar(stack_ptr) + K` into dedicated `NodeKind::StackStore { space, offset }` or `NodeKind::StackStorePhi { space }` (with per-predecessor offsets stored in `Graph::stack_phi_offsets`). Configured with the calling convention's stack-pointer varnode.
  - `StackLoadForward` — forwards values from `StackStore`-classified stores to subsequent `Load`s at the same stack offset, eliminating the round-trip through memory. Convention- and arch-aware (needs endianness for partial-overlap reads).
  - `IndirectBranchResolve` (`opt::indirect_branch_resolve`) — producer-shape classifier for `BranchIndirect` placeholders. Recognises link-register returns, tail calls, jump-tables, stack-array dispatch. Drives the resolver in `strider::indirect_resolve`.
  - `CallStackArgCollect` (**post-pass**) — runs once after convergence; collects positional stack arguments at `Call` sites using the convention's stack-arg offsets.
  - `FunctionArgDetect` (**post-pass**) — canonicalises register- and stack-passed argument reads at the function boundary into `FunctionArg` nodes, so patterns can match on argument-position rather than raw `InitialVar` reads.
- **`pattern`** — IR graph pattern matching over a `BuiltFunctionGraph`.  Supports arbitrary queries: memory accesses, call arguments, return values, branch conditions, data-flow chains, etc.  Core types:
  - `Pat` / `PatKind` — Arc-wrapped pattern value.  Covers every `NodeKind` the IR can produce: `Any`, `IntConst` / `AnyIntConst`, `BoolConst`, `FloatConst` / `AnyFloatConst`, all integer binary/unary/cmp ops (`Add`, `Sub`, `Mul`, `Shl`, `IntEq`, `IntLt`, `IntSlt`, …), boolean ops, all float ops, cast ops (`CastToBool`, `CastToInt`, `CastToFloat`, `Truncate`, `Extend`, `Popcount`, `Lzcount`, `Piece`, `Extract`, `Insert`), float conversions, memory ops (`Load(space)`, `Store(space)`, `StackStore`, `StackStorePhi`), `Phi(Vn)`, `InitialVar(Vn)`, `FunctionArg`, `Call`, `CallOther`, `Return`, `If`, plus `WithCapture` / `WithPredicate` post-match wrappers.
  - `Capture` — single capture variable type (replaces the older `Var`/`NodeVar` split); globally unique via atomic counter.  A binding stores both the matched `NodeId` and (for value-producing patterns) the matched `NodeOutputId`.  Multiple occurrences in one `Pat` must agree.
  - `Matcher<'g>` — wraps `&BuiltFunctionGraph`.  `find_all(&pat) -> Vec<Match>` walks all candidate roots; `match_at(node, &pat) -> Option<Match>` checks a single node.
  - `Match` — result of a successful match.  `root() -> NodeId`, `node(c) -> Option<NodeId>` (always `Some(_)` for a successful capture), `output(c) -> Option<NodeOutputId>` (`Some` for value-producing patterns; `None` for control-flow), and typed extractors `get_int(c) -> Option<i128>` (signed, sign-extended), `get_uint(c) -> Option<u128>` (unsigned, masked), `get_bool(c) -> Option<bool>`, `get_float_bits(c) -> Option<u64>`, `get_vn(c) -> Option<rsleigh::Vn>`.
  - Builder types (fluent): `IntBinaryOpPat` / `BoolBinaryOpPat` / `FloatBinaryOpPat` (`.ordered()`, `.capture(c)`, `.when(f)`), `LoadPat` (`.space`, `.addr`, `.capture(c)`, `.when(f)`), `StorePat`, `StackStorePat`, `StackStorePhiPat`, `PhiPat`, `FunctionArgPat`, `CallPat` / `CallOtherPat` (`.at(addr)`, `.arg(idx, p)`, `.capture(c)`, `.when(f)`), `RetPat` (`.preceded_by(call_pat)`, `.ret_val(idx, p)`, `.capture(c)`, `.when(f)`), `IfPat` (`.cond(p)`, `.true_branch`, `.false_branch`, `.capture(c)`, `.when(f)`).  Every builder exposes the same `.capture(c: Capture)` method; control-flow builders bind only `node(c)`, value-producing builders bind both `node(c)` and `output(c)`.  Multi-output nodes (`Load = [Memory, Value]`) bind the value slot.
  - `Pat` itself has `.capture(c: Capture) -> Pat` and `.when<F>(f) -> Pat` to wrap any existing pattern with a post-match guard.
  - Free constructors: `add(l,r)`, `sub`, `mul`, `load()`, `store()`, `stack_store()`, `stack_store_phi()`, `phi()`, `phi_for(vn)`, `call()`, `call_other()`, `ret()`, `if_node()`, `initial_var()`, `var(c)`, `any()`, `int_const(n)`, `bool_const(b)`, `predicate(f)` (= `any().when(f)`).
  - **Commutative matching**: `add`, `mul`, `and`, `or`, `xor` (and `BoolBinaryOp` equivalents), `int_cmp(Equal/Carry/Scarry)`, and `float_cmp(Equal/NotEqual)` automatically try both operand orderings.  Non-commutative ops (`sub`, `div`, `shl`, …) keep stated order.  `.ordered()` forces left-to-right on the typed builders.
  - All field methods (`.addr()`, `.arg()`, `.cond()`, `.ret_val()`, etc.) accept `impl Into<Pat>`, so builder types compose without explicit `.into()`.  `ret().preceded_by(p)` matches the Return's direct ctrl predecessor (typically a `ControlState`); `if_node().true_branch(p)` / `.false_branch(p)` walk the single consumer of the If's true/false output, honouring the `ignore_control_states` flag for transparent ControlState passthrough.  Depends on: `ir`, `rsleigh`.
- **`dot`** — Renders `GraphDotDumper` implementors to `.dot` and `.html` files with dark/light style.
- **`graphwalk`** — Generic graph traversal utilities.
- **`entity-utils`** — Entity set and worklist data structures.
- **`graphmock`** — Mock graph for tests.
- **`strider-py`** *(planned)* — Python bindings (PyO3) that are the primary user-facing interface. Users write IR patterns with named captures in Python and get back matched values. The Rust `pattern` crate is the engine; this crate is the API.

### IR Node Model

The IR is a sea-of-nodes graph where each `Node` has typed inputs (`NodeOutputId` references) and outputs. The `expected_signature` table in `crates/ir/src/node_signature.rs` is the single source of truth for every node's input/output shape. Node kinds, grouped:

- **Initial state:** `Entry`, `InitialMemory`, `InitialVar(Vn)`
- **Region / join:** `ControlState` (variadic Control inputs; outputs `Control` + `PhiToken`), `VarPhi(Vn)` (SSA φ for varnode `Vn` at a join point), `MemPhi` (φ for the memory token)
- **Conditional branch:** `If` (outputs true/false `Control` edges), `IfCase(bool)`
- **Calls / returns:** `Call` (clobbers caller-saved registers and memory; variadic args), `CallOther { user_op_id }`, `Return` (variadic return values)
- **Memory:** `Load(VnSpace)`, `Store(VnSpace)`; after `StackStoreDetect`: `StackStore { space, offset }`, `StackStorePhi { space }` (per-predecessor offsets in `Graph::stack_phi_offsets`)
- **Integer:** `IntConst(u64)`, `IntUnaryOp`, `IntBinaryOp`, `IntCmpOp`, `Truncate`, `Extend(ExtendOp)`, `Popcount`, `Lzcount`, `Piece`, `Extract { lsb, len }`, `Insert { lsb, len }`, `CastToInt`
- **Boolean:** `BoolConst(bool)`, `BoolUnaryOp`, `BoolBinaryOp`, `CastToBool`
- **Float:** `FloatConst(u64)` (bits), `FloatUnaryOp`, `FloatBinaryOp`, `FloatCmpOp`, `FloatIsNan`
- **Float / int conversions:** `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`. The cast ops (`CastToInt`, `CastToBool`, `CastToFloat`) accept any value-typed input; `FloatToFloat` is float→float only.
- **Opaque / user-defined:** `SegmentOp { op_id }`, `CPoolRef`, `New`

### Register Aliasing

Overlapping registers (x86 `rax`/`eax`/`ax`/`al`/`ah`, AArch64 `q0`/`d0`/`s0`, x87 `ST*`, etc.) are handled by `pcode_lift::ValueLifter::{read_vn, write_vn}` (in `crates/pcode-lift/src/vn_io.rs`). All reads and writes go through the largest containing register, with shift/mask operations inserted for sub-register slices. `find_largest_fitting_register` is the entry point. `vn_mask` enumerates supported widths: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register) bytes.

### External Dependency: rsleigh

`rsleigh` is a local path dependency at `../rsleigh` (not in this workspace). It wraps GHIDRA's Sleigh specification to lift machine instructions to p-code (`rsleigh::Insn` with `Opcode`). Key types used: `Vn` (varnode — register/memory/const/unique), `VnSpace`, `MemReader`.
