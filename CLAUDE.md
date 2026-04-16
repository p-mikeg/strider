# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Run Commands

```bash
# Build all crates
cargo build --workspace

# Run the main example (reads binary_tests/binary_test, outputs cfg.html, graph.html)
cargo run --example analyzer

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
reader → cfg → analyzer → ir ← ir-macros (proc-macro)
                           └→ opt
                           └→ pattern
dot (visualization, used by cfg and ir)
rsleigh (external, at ../rsleigh — Sleigh/GHIDRA p-code lifter)
```

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
    - **Layer C**: graph-level invariants (Entry/InitialMemory uniqueness, ControlState predecessor kinds, phi token ownership & per-predecessor arity for `ControlPhi`/`MemPhi` only — `StackStorePhi` has fixed arity 3, PostCall producer & uniqueness).
    - Aggregates all errors into a `ValidationErrors` bundle rather than failing fast.
- **`ir-macros`** — proc-macro crate exporting `match_value!`, a DSL for ergonomic IR pattern matching. Syntax: `match_value! { if let PATTERN = ctx, val { BODY } }`. `PATTERN` supports `node name` / `val name` bindings, `NodeKind::IntBinaryOp(op)` kind matches, and recursive input matching via `[input0, input1, ...]`. Expands to chained `ctx.get_node_from_output()` / `ctx.node_kind()` / `ctx.node_inputs_exact::<N>()` calls. Used heavily by `opt` passes.
- **`analyzer`** — Translates a `Cfg` to a `BuiltFunctionGraph`. `IrAnalyzer` handles register aliasing (sub-registers like `al`/`ah` in `rax` are accessed by shifting/masking the container register). `Analyzer::new(arch, sleigh_regs, cc)` takes an arch, sleigh register list, and a `CallingConvention`. `Analyzer::build_optimizer_pipeline()` returns the project's standard optimizer pipeline (default passes + `StackStoreDetect` + `CallStackArgCollect` post-pass) wired to the convention's stack-pointer varnode and stack-arg offsets. Supported calling conventions: `x86_cdecl`, `x86_64_systemv_abi`, `aarch64_aapcs64`, `arm_aapcs`.
- **`opt`** — IR optimization passes. Passes added via `OptimizerPipeline::add` run in a shared fixed-point loop; `add_post_pass` runs once after convergence. `OptimizerPipeline::run` calls `ir::validate::validate` at the very end so any malformed graph is reported as an `opt::Error::IrError(ValidationFailed(...))`. Passes:
  - `ConstantFold` — constant evaluation for all arithmetic, comparisons, booleans, truncation, extension; algebraic identities (`x+0→x`, `x^x→0`, nested AND-mask merging `(a&C1)&C2 → a&(C1&C2)`).
  - `KnownBits` — bit-level propagation of statically known zeros/ones to fold partially-known expressions.
  - `RedundantPhis` — eliminates `ControlPhi` and `MemPhi` nodes and `ControlState` nodes with a single reachable predecessor; detaches inputs of CFG-unreachable nodes (leaving them as zero-input zombies — the validator skips Layer A on these via reachability scoping).
  - `DeadBranchElimination` — removes `If` nodes whose condition is a `BoolConst`; strips the dead control edge from successor `ControlState` and `ControlPhi` nodes. Works together with `RedundantPhis`.
  - `LoadReadOnly` — resolves `Load` nodes whose address is a compile-time constant into constants by reading from a caller-supplied `ReadOnlyMemory` (e.g. `.rodata`/`.text` section).
  - `StackStoreDetect` — converts `Store` nodes whose address resolves to `InitialVar(stack_ptr) + K` into dedicated `NodeKind::StackStore { space, offset }` or `NodeKind::StackStorePhi { space }` (with per-predecessor offsets stored in `Graph::stack_phi_offsets`). Configured with the calling convention's stack-pointer varnode.
  - `CallStackArgCollect` (**post-pass**) — runs once after convergence; collects positional stack arguments at `Call` sites using the convention's stack-arg offsets.
- **`pattern`** — IR graph pattern matching over a `BuiltFunctionGraph`. Supports arbitrary queries: memory accesses, call arguments, return values, branch conditions, data-flow chains, etc. Core types:
  - `Pat` / `PatKind` — Arc-wrapped pattern value. Covers every `NodeKind` the IR can produce: `Any`, `Capture(Var)`, `IntConst` / `AnyIntConst`, `BoolConst`, `FloatConst` / `AnyFloatConst`, all integer binary/unary/cmp ops (`Add`, `Sub`, `Mul`, `Shl`, `IntEq`, `IntLt`, `IntSlt`, …), boolean ops, all float ops (`FloatBinaryOp`, `FloatUnaryOp`, `FloatCmpOp`, `FloatIsNan`), cast ops (`CastToBool`, `CastToInt`, `CastToFloat`, `Truncate`, `Extend`, `Popcount`, `Lzcount`, `Piece`, `Extract`, `Insert`), float conversions (`IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`), memory ops (`Load(space)`, `Store(space)`, `StackStore`, `StackStorePhi`), `Phi(Vn)`, `InitialVar(Vn)`, `Call`, `CallOther`, `Return`, `If`, `Contains(p)` (forward ctrl-chain search), `WithCapture { inner, var }` (post-match output binding), `WithPredicate { inner, func }` (post-match predicate guard).
  - `Var` / `NodeVar` — capture variables for data outputs (`NodeOutputId`) and control nodes (`NodeId`); globally unique via atomic counter. Multiple occurrences in a pattern must bind to the same value.
  - `Matcher<'g>` — wraps `&BuiltFunctionGraph`, pre-indexes `Call`/`Return`/`If` nodes. `find_all(&pat) -> Vec<Match>` searches all candidate root nodes.
  - `Match` — result of a successful match: `root: NodeId`, `get(Var) -> Option<NodeOutputId>`, `get_node(NodeVar)`, `get_int_const`, `get_bool_const`.
  - Builder types (fluent): `IntBinaryOpPat` / `BoolBinaryOpPat` / `FloatBinaryOpPat` (`.ordered()`, `.capture(v)`, `.when(f)`), `LoadPat` (`.space`, `.addr`, `.capture_output(v)`, `.capture_node(nv)`, `.capture(v)`, `.when(f)`), `StorePat`, `StackStorePat`, `StackStorePhiPat`, `PhiPat` (same capture methods), `CallPat` / `CallOtherPat` (`.at(addr)`, `.arg(idx, p)`, `.capture(nv)`, `.when(f)`), `RetPat` (`.preceded_by(call_pat)`, `.ret_val(idx, p)`, `.capture(nv)`, `.when(f)`), `IfPat` (`.cond(p)`, `.true_branch`, `.false_branch`, `.capture(nv)`, `.when(f)`).
  - `Pat` itself has `.capture(v: Var) -> Pat` and `.when<F>(f) -> Pat` to wrap any existing pattern with a post-match guard.
  - Free constructors: `add(l,r)`, `sub`, `mul`, `load()`, `store()`, `stack_store()`, `stack_store_phi()`, `phi()`, `phi_for(vn)`, `call()`, `call_other()`, `ret()`, `if_node()`, `contains(p)`, `initial_var()`, `var(v)`, `any()`, `int_const(n)`, `bool_const(b)`, `predicate(f)` (= `any().when(f)`).
  - **Commutative matching**: `add`, `mul`, `and`, `or`, `xor` (and `BoolBinaryOp` equivalents) automatically try both operand orderings. Non-commutative ops (`sub`, `div`, `shl`, …) keep stated order. `.ordered()` forces left-to-right.
  - All field methods (`.addr()`, `.arg()`, `.cond()`, `.ret_val()`, etc.) accept `impl Into<Pat>`, so builder types compose without explicit `.into()`. `contains(p)` walks forward through ctrl chain. `ret().preceded_by(call_pat)` walks backward. Depends on: `ir`, `rsleigh`.
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
- **Calls / returns:** `Call` (clobbers caller-saved registers and memory; variadic args), `PostCallMemState` and `PostCallVarState(Vn)` (consume the Call's Control output and re-establish memory / specific varnode liveness across the call), `CallOther { user_op_id }`, `Return` (variadic return values)
- **Memory:** `Load(VnSpace)`, `Store(VnSpace)`; after `StackStoreDetect`: `StackStore { space, offset }`, `StackStorePhi { space }` (per-predecessor offsets in `Graph::stack_phi_offsets`)
- **Integer:** `IntConst(u64)`, `IntUnaryOp`, `IntBinaryOp`, `IntCmpOp`, `Truncate`, `Extend(ExtendOp)`, `Popcount`, `Lzcount`, `Piece`, `Extract { lsb, len }`, `Insert { lsb, len }`, `CastToInt`
- **Boolean:** `BoolConst(bool)`, `BoolUnaryOp`, `BoolBinaryOp`, `CastToBool`
- **Float:** `FloatConst(u64)` (bits), `FloatUnaryOp`, `FloatBinaryOp`, `FloatCmpOp`, `FloatIsNan`
- **Float / int conversions:** `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`, `FloatBitsToInt`, `CastToFloat`. The cast ops (`CastToInt`, `CastToBool`, `CastToFloat`) accept any value-typed input; `FloatToFloat` is float→float only.
- **Opaque / user-defined:** `SegmentOp { op_id }`, `CPoolRef`, `New`

### Register Aliasing

The analyzer handles overlapping registers (e.g., x86 `rax`/`eax`/`ax`/`al`) by always reading/writing the largest containing register and inserting shift/mask operations for sub-registers. The `find_largest_fitting_register` method drives this.

### External Dependency: rsleigh

`rsleigh` is a local path dependency at `../rsleigh` (not in this workspace). It wraps GHIDRA's Sleigh specification to lift machine instructions to p-code (`rsleigh::Insn` with `Opcode`). Key types used: `Vn` (varnode — register/memory/const/unique), `VnSpace`, `MemReader`.
