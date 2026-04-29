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
                         strider-error  (shared error-wrapper machinery)
                              ↑   (every crate below depends on it)

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
the IR; `pattern` queries it. `strider-error` provides the wrapper-error /
location-chain machinery every crate's error type uses.

### Key Crates

- **`reader`** — Loads ELF binaries and provides a memory reader (`ElfFileMemReader`) for rsleigh.
- **`cfg`** — Builds a Control Flow Graph (`Cfg<R>`) from a binary using rsleigh. Uses `petgraph::StableDiGraph` internally. Regions (basic blocks) contain p-code instructions (`rsleigh::Insn`). Edge kinds: `Fallthrough`, `Branch`, `IfCaseTrue`, `IfCaseFalse`.
- **`ir`** — Sea-of-nodes style IR graph. Core types:
  - `Graph` — stores `NodeId`, `NodeOutputId`, `NodeInputId` via `cranelift-entity` PrimaryMaps. Nodes are deduplicated/cached by (kind, inputs, output_kinds). Per-node side-tables (e.g. `stack_phi_offsets: HashMap<NodeId, Vec<i64>>`) hold ancillary data.
  - `FunctionBuilder` — builds the IR graph with SSA-like variable tracking. Variables map `rsleigh::Vn` (varnode) → `VarId`. Each region gets a `ControlState` node + `ControlPhi` nodes for variables. Calls `validate::validate` at the end of `build()`.
  - `NodeOutputKind` — `Control`, `Memory`, `ControlPhi`, or `OutputType(NodeOutputType)`.
  - `NodeOutputType` — integers `Bool`, `U8`, `U16`, `U32`, `U64`, `U128`, `U256`; floats `F32`, `F64`.
  - `walk::walk_graph(graph, entry)` — traversal that follows both backward-data and forward-control edges. Used by the validator and several passes.
  - `node_signature::{ExpectedOutputKind, expected_signature}` — single source of truth for expected input/output slot kinds per `NodeKind`. `ExpectedOutputKind` coarsens int/float widths via `AnyInt` / `AnyFloat` / `AnyValue`.
  - `validate::validate(&graph, entry) -> Result<(), ValidationErrors>` — whole-graph validator with three layers:
    - **Layer A**: per-node local typing against `expected_signature` (scoped to nodes reachable via `walk_graph`, since optimization passes leave detached zombie nodes in the arena).
    - **Layer B**: bidirectional use-list consistency.
    - **Layer C**: graph-level invariants (Entry/InitialMemory uniqueness, ControlState predecessor kinds, phi token ownership & per-predecessor arity for `ControlPhi`/`MemPhi` only — `StackStorePhi` has fixed arity 3).
    - Aggregates all errors into a `ValidationErrors` bundle rather than failing fast.
- **`target`** — Pure target-description data (no IR, no rsleigh state machine). Owns:
  - `SleighArch` — pairs a `.sla` spec + `.pspec` + `Endianness`. Presets: `x86_64`, `x86`, `aarch64` / `aarch64be`, `arm` / `arm_be` / `arm_thumb`, `mipsbe32` / `mipsle32` / `mipsbe64` / `mipsle64`.
  - `CallingConvention` (static-string register names) and `BuiltCallingConvention` (resolved `rsleigh::Vn` varnodes; produced by `CallingConvention::build`). Carries the stack-pointer varnode, integer + float return-value regs, callee-saved regs, positional `stack_arg_offsets`, the `ret_stack_pop` delta (`8` on x86_64, `0` on AAPCS), and the optional link-register varnode (`lr` on AArch64/ARM, `ra` on MIPS, `None` on x86/x86_64). CC presets: `x86_cdecl`, `x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`, `mips_o32`, `mips_n64`.
- **`pcode-lift`** — Pure value-producing pcode → IR lifter, factored out of `strider`. `ValueLifter::lift(insn) -> Result<bool>` returns `Ok(true)` for value-producing opcodes (arithmetic, casts, `Load`, etc.) and `Ok(false)` for control-flow / `Store` / call opcodes that the caller must route. **Owns the register-aliasing logic** (`vn_io.rs`): all reads/writes go through the largest containing register, with shift/mask ops for sub-registers (`al`/`ah`/`ax`/`eax`/`rax`, x87 `ST*` 80-bit slices, 16-byte `XMM`/`q*` containers). Both `strider` (per-region IR translation) and `cfg` (single-block mini-IR for indirect-branch resolution) reuse it.
- **`strider-error`** — Shared error-wrapper machinery every crate's error type uses. The `define_error!` macro wraps a `thiserror` enum in a struct carrying a `Box<ErrorKind>`, an origin `Backtrace`, and a `LocationChain` of `&'static Location` values pushed at every `?` / `From::from` boundary. `bridge_error!` connects one crate's wrapper to another's, extending the chain across the dependency boundary. `format_traceback` renders the result Python-style for the planned PyO3 surface (`strider-py`).
- **`strider`** — Translates a `Cfg` to a `BuiltFunctionGraph` and drives the indirect-branch fixed-point. `Strider::new(arch, sleigh_regs, cc)` takes a `target::SleighArch`, a Sleigh register list, and a `target::CallingConvention`; CC + arch types are re-exported from `strider` for back-compat. The actual register-aliasing logic lives in `pcode_lift::ValueLifter` (see `pcode-lift` above) — `IrStrider` is the per-function context that drives it region-by-region. Key surfaces:
  - `Strider::analyze_cfg(&cfg) -> Result<AnalyzeOutcome>` — single canonical entry point. `AnalyzeOutcome` bundles `graph: BuiltFunctionGraph`, `unresolved_branches: Vec<(PcodeInsnAddr, ir::Value)>` (placeholder anchors for tier-2 indirect-branch resolution), and `region_handles: Vec<RegionLiftHandles>` (per-region IR-handle snapshots used by the cache). Callers that only want the graph write `analyze_cfg(&cfg)?.graph`.
  - `Strider::build_optimizer_pipeline()` — full pipeline: `opt::default_pipeline()` + `StackStoreDetect` + `StackLoadForward` (both fixed-point), + `CallStackArgCollect` and `FunctionArgDetect` as post-passes.
  - `Strider::build_stable_optimizer_pipeline()` — passes whose rewrites survive a later iteration adding new phi inputs (`ConstantFold`, `KnownBits`, `StackStoreDetect`, `StackLoadForward`, + `FunctionArgDetect` post-pass). Used while the IR `Graph` is still growing under the indirect-branch resolver.
  - `Strider::build_destructive_optimizer_pipeline()` — node-removal passes the orchestrator runs **once** at the fixed-point exit (`RedundantPhis`, `DeadBranchElimination`, `CallOtherElide`, + `CallStackArgCollect` post-pass). Running these mid-iteration would invalidate `RegionIrCache` pinned `NodeId`s.
  - `RegionIrCache` (`crates/strider/src/cache/`) — persistent `MachineInsnAddr → RegionIrEntry` map. Entries pin entry/exit control + memory `NodeOutputId`s, per-var entry `ControlPhi` `NodeId`s, the `MemPhi`/`ControlState` ids, and the exit `vn_to_value` map so a future iteration can append a new predecessor's value to existing phis without invalidating body refs.
  - `indirect_resolve_tier2` (`crates/strider/src/indirect_resolve_tier2/`) — post-IR resolver for `BranchIndirect` placeholders the cfg-time tier 1 couldn't classify. `classify_anchor` inspects the placeholder's anchored producer after the stable pipeline has run; `inplace::{apply_link_register, apply_tail_call}` rewrite the IR in place; `orchestrator::run` is the outer fixed-point.
  - `GraphRewriter` (`crates/strider/src/rewrite.rs`) — thin façade over `pattern::rewrite_rule` that walks the reachable graph, applies a substitution rule at every candidate root, and exposes a `re_optimize` shortcut. Use case: collapse a tier-2-resolved jump table after the orchestrator returns.
- **`opt`** — IR optimization passes. Passes added via `OptimizerPipeline::add` run in a shared fixed-point loop; `add_post_pass` runs once after convergence. `OptimizerPipeline::run` calls `ir::validate::validate` at the very end so any malformed graph is reported as an `opt::Error::IrError(ValidationFailed(...))`. Three pre-built top-level pipelines: `default_pipeline()` (all passes), `stable_default_pipeline()` (rewrites that survive phi-input growth — `ConstantFold` + `KnownBits`), `destructive_default_pipeline()` (node-removal passes safe only at fixed point — `RedundantPhis` + `DeadBranchElimination` + `CallOtherElide`). See `crates/strider/src/strider/pipeline.rs` for how `Strider` layers convention-aware passes on top. Passes:
  - `ConstantFold` — constant evaluation for all arithmetic, comparisons, booleans, truncation, extension; algebraic identities (`x+0→x`, `x^x→0`, nested AND-mask merging `(a&C1)&C2 → a&(C1&C2)`).
  - `KnownBits` — bit-level propagation of statically known zeros/ones to fold partially-known expressions.
  - `RedundantPhis` — eliminates `ControlPhi` and `MemPhi` nodes and `ControlState` nodes with a single reachable predecessor; detaches inputs of CFG-unreachable nodes (leaving them as zero-input zombies — the validator skips Layer A on these via reachability scoping).
  - `DeadBranchElimination` — removes `If` nodes whose condition is a `BoolConst`; strips the dead control edge from successor `ControlState` and `ControlPhi` nodes. Works together with `RedundantPhis`.
  - `CallOtherElide` — drops opaque `CallOther` nodes whose user-op is a known IR-level no-op (e.g. ARM `setISAMode`); the names live in `opt::NO_OP_USER_OPS`.
  - `LoadReadOnly` — resolves `Load` nodes whose address is a compile-time constant into constants by reading from a caller-supplied `ReadOnlyMemory` (e.g. `.rodata`/`.text` section). Configured with a ROM image, so `default_pipeline()` doesn't include it; the example and `Strider::build_optimizer_pipeline` callers layer it on.
  - `StackStoreDetect` — converts `Store` nodes whose address resolves to `InitialVar(stack_ptr) + K` into dedicated `NodeKind::StackStore { space, offset }` or `NodeKind::StackStorePhi { space }` (with per-predecessor offsets stored in `Graph::stack_phi_offsets`). Configured with the calling convention's stack-pointer varnode.
  - `StackLoadForward` — forwards values from `StackStore`-classified stores to subsequent `Load`s at the same stack offset, eliminating the round-trip through memory. Convention- and arch-aware (needs endianness for partial-overlap reads).
  - `IndirectBranchResolve` (`opt::indirect_branch_resolve`) — producer-shape classifier for `BranchIndirect` placeholders. Recognises link-register returns, tail calls, jump-tables, stack-array dispatch. Drives the tier-2 resolver in `strider::indirect_resolve_tier2`.
  - `CallStackArgCollect` (**post-pass**) — runs once after convergence; collects positional stack arguments at `Call` sites using the convention's stack-arg offsets.
  - `FunctionArgDetect` (**post-pass**) — canonicalises register- and stack-passed argument reads at the function boundary into `FunctionArg` nodes, so patterns can match on argument-position rather than raw `InitialVar` reads.
- **`pattern`** — IR graph pattern matching over a `BuiltFunctionGraph`. Supports arbitrary queries: memory accesses, call arguments, return values, branch conditions, data-flow chains, etc. Core types:
  - `Pat` / `PatKind` — Arc-wrapped pattern value. Covers every `NodeKind` the IR can produce: `Any`, `Capture(Var)`, `IntConst` / `AnyIntConst`, `BoolConst`, `FloatConst` / `AnyFloatConst`, all integer binary/unary/cmp ops (`Add`, `Sub`, `Mul`, `Shl`, `IntEq`, `IntLt`, `IntSlt`, …), boolean ops, all float ops (`FloatBinaryOp`, `FloatUnaryOp`, `FloatCmpOp`, `FloatIsNan`), cast ops (`CastToBool`, `CastToInt`, `CastToFloat`, `Truncate`, `Extend`, `Popcount`, `Lzcount`, `Piece`, `Extract`, `Insert`), float conversions (`IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`), memory ops (`Load(space)`, `Store(space)`, `StackStore`, `StackStorePhi`), `Phi(Vn)`, `InitialVar(Vn)`, `Call`, `CallOther`, `Return`, `If`, `Contains(p)` (forward ctrl-chain search), `WithCapture { inner, var }` (post-match output binding), `WithPredicate { inner, func }` (post-match predicate guard).
  - `Var` / `NodeVar` — capture variables for data outputs (`NodeOutputId`) and control nodes (`NodeId`); globally unique via atomic counter. Multiple occurrences in a pattern must bind to the same value.
  - `Matcher<'g>` — wraps `&BuiltFunctionGraph`, pre-indexes `Call`/`Return`/`If` nodes. `find_all(&pat) -> Vec<Match>` searches all candidate root nodes.
  - `Match` — result of a successful match: `root: NodeId`, `get(Var) -> Option<NodeOutputId>`, `get_node(NodeVar)`, `get_int_const`, `get_bool_const`.
  - Builder types (fluent): `IntBinaryOpPat` / `BoolBinaryOpPat` / `FloatBinaryOpPat` (`.ordered()`, `.capture(v)`, `.when(f)`), `LoadPat` (`.space`, `.addr`, `.capture(v)`, `.when(f)`), `StorePat`, `StackStorePat`, `StackStorePhiPat`, `PhiPat`, `FunctionArgPat` (same capture API), `CallPat` / `CallOtherPat` (`.at(addr)`, `.arg(idx, p)`, `.capture_node(nv)`, `.when(f)`), `RetPat` (`.preceded_by(call_pat)`, `.ret_val(idx, p)`, `.capture_node(nv)`, `.when(f)`), `IfPat` (`.cond(p)`, `.true_branch`, `.false_branch`, `.capture_node(nv)`, `.when(f)`).
  - **Capture rule:** value-producing builders expose `.capture(v: Var)` (binds `NodeOutputId` via `IntoPat`, value-kind filtered so multi-output nodes like `Load = [Memory, Value]` always capture the value slot). Control-flow builders (`CallPat`, `IfPat`, `RetPat`, `CallOtherPat`) expose `.capture_node(nv: NodeVar)` (binds `NodeId`) since these sites have no single "the value" output. No builder exposes both.
  - `Pat` itself has `.capture(v: Var) -> Pat` and `.when<F>(f) -> Pat` to wrap any existing pattern with a post-match guard.
  - Free constructors: `add(l,r)`, `sub`, `mul`, `load()`, `store()`, `stack_store()`, `stack_store_phi()`, `phi()`, `phi_for(vn)`, `call()`, `call_other()`, `ret()`, `if_node()`, `initial_var()`, `var(v)`, `any()`, `int_const(n)`, `bool_const(b)`, `predicate(f)` (= `any().when(f)`).
  - **Commutative matching**: `add`, `mul`, `and`, `or`, `xor` (and `BoolBinaryOp` equivalents) automatically try both operand orderings. Non-commutative ops (`sub`, `div`, `shl`, …) keep stated order. `.ordered()` forces left-to-right.
  - All field methods (`.addr()`, `.arg()`, `.cond()`, `.ret_val()`, etc.) accept `impl Into<Pat>`, so builder types compose without explicit `.into()`. `ret().preceded_by(p)` matches the Return's direct ctrl predecessor (typically a `ControlState`); `if_node().true_branch(p)` / `.false_branch(p)` match the single consumer of the If's true/false output via `ConsumersSpec::Indexed` — no multi-step walk. Depends on: `ir`, `rsleigh`.
- **`dot`** — Renders `GraphDotDumper` implementors to `.dot` and `.html` files with dark/light style.
- **`graphwalk`** — Generic graph traversal utilities.
- **`entity-utils`** — Entity set and worklist data structures.
- **`graphmock`** — Mock graph for tests.
- **`strider-py`** *(planned)* — Python bindings (PyO3) that are the primary user-facing interface. Users write IR patterns with named captures in Python and get back matched values. The Rust `pattern` crate is the engine; this crate is the API.

### IR Node Model

The IR is a sea-of-nodes graph where each `Node` has typed inputs (`NodeOutputId` references) and outputs. The `expected_signature` table in `crates/ir/src/node_signature.rs` is the single source of truth for every node's input/output shape. Node kinds, grouped:

- **Initial state:** `Entry`, `InitialMemory`, `InitialVar(Vn)`
- **Region / join:** `ControlState` (variadic Control inputs; outputs `Control` + `ControlPhi` dispatch token), `ControlPhi(Vn)` (SSA φ for varnode `Vn` at a join point), `MemPhi` (φ for the memory token)
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
