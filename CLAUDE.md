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

This is a Rust workspace binary analysis tool that lifts native binaries to an IR suitable for analysis/optimization. The pipeline is:

**Binary → CFG → IR → (Optimizations)**

### Crate Dependency Flow

```
reader → cfg → analyzer → ir
                           └→ opt
dot (visualization, used by cfg and ir)
rsleigh (external, at ../rsleigh — Sleigh/GHIDRA p-code lifter)
```

### Key Crates

- **`reader`** — Loads ELF binaries and provides a memory reader (`ElfFileMemReader`) for rsleigh.
- **`cfg`** — Builds a Control Flow Graph (`Cfg<R>`) from a binary using rsleigh. Uses `petgraph::StableDiGraph` internally. Regions (basic blocks) contain p-code instructions (`rsleigh::Insn`). Edge kinds: `Fallthrough`, `Branch`, `IfCaseTrue`, `IfCaseFalse`.
- **`ir`** — Sea-of-nodes style IR graph. Core types:
  - `Graph` — stores `NodeId`, `NodeOutputId`, `NodeInputId` via `cranelift-entity` PrimaryMaps. Nodes are deduplicated/cached by (kind, inputs, output_kinds).
  - `FunctionBuilder` — builds the IR graph with SSA-like variable tracking. Variables map `rsleigh::Vn` (varnode) → `VarId`. Each region gets a `ControlState` node + `ControlSelector` phi-like nodes for variables.
  - `NodeOutputKind` — `Control`, `Memory`, `ControlSelector`, or `OutputType(NodeOutputType)`.
  - `NodeOutputType` — `Bool`, `U8`, `U16`, `U32`, `U64`.
- **`analyzer`** — Translates a `Cfg` to a `BuiltFunctionGraph`. `IrAnalyzer` handles register aliasing (sub-registers like `al`/`ah` in `rax` are accessed by shifting/masking the container register). `Analyzer::new` takes an arch, sleigh register list, and calling convention.
- **`opt`** — IR optimization passes (WIP). `RedundantSelectors` pass exists but optimizer is currently commented out in the example.
- **`pattern`** — IR graph pattern matching over a `BuiltFunctionGraph`. Core types:
  - `Pat` / `PatKind` — Arc-wrapped pattern value. Variants: `Any`, `Capture(Var)`, `IntConst`, `BoolConst`, all integer binary ops (`Add`, `Sub`, `Mul`, `Shl`, …), comparison ops (`IntEq`, `IntLt`, `IntSlt`, …), boolean ops, cast ops (`CastToBool`, `Truncate`, `Extend`, `Popcount`), `Load(space)`, `Store(space)`, `Selector(Vn)`, `InitialVar(Vn)`, `Call`, `Return`, `If`, and `Contains(p)` (forward ctrl-chain search).
  - `Var` / `NodeVar` — capture variables for data outputs (`NodeOutputId`) and control nodes (`NodeId`); globally unique via atomic counter. Multiple occurrences in a pattern must bind to the same value.
  - `Matcher<'g>` — wraps `&BuiltFunctionGraph`, pre-indexes `Call`/`Return`/`If` nodes. `find_all(&pat) -> Vec<Match>` searches all candidate root nodes.
  - `Match` — result of a successful match: `root: NodeId`, `get(Var) -> Option<NodeOutputId>`, `get_node(NodeVar)`, `get_int_const`, `get_bool_const`.
  - Builder types (fluent): `LoadPat`, `StorePat`, `SelectorPat`, `CallPat` (`.at(addr)`, `.arg(idx, p)`, `.capture(nv)`), `RetPat` (`.preceded_by(call_pat)`, `.ret_val(idx, p)`), `IfPat` (`.cond(p)`, `.true_branch`, `.false_branch`).
  - Free constructors: `add(l,r)`, `sub`, `mul`, `load()`, `store()`, `selector()`, `call()`, `ret()`, `if_node()`, `contains(p)`, `initial_var()`, `var(v)`, `any()`, `int_const(n)`, `bool_const(b)`.
  - Matching is **not commutative**. `contains(p)` walks forward through the ctrl chain (transparent to `ControlState`, `IfCase`, `Call` nodes). `ret().preceded_by(call_pat)` walks backward through ctrl. Depends on: `ir`, `rsleigh`.
- **`dot`** — Renders `GraphDotDumper` implementors to `.dot` and `.html` files with dark/light style.
- **`graphwalk`** — Generic graph traversal utilities.
- **`entity-utils`** — Entity set and worklist data structures.
- **`graphmock`** — Mock graph for tests.

### IR Node Model

The IR is a sea-of-nodes graph where each `Node` has typed inputs (`NodeOutputId` references) and outputs. Important node kinds:
- `Entry`, `InitialMemory` — function entry points
- `ControlState` — represents a basic block's entry, outputs `Control` + `ControlSelector`
- `ControlSelector(Vn)` — phi-like node that selects a variable value based on control flow
- `MemSelector` — phi-like for memory
- `If` — conditional branch, outputs two `Control` edges (true/false)
- `Call` — clobbers caller-saved registers and memory
- `Load(space)` / `Store(space)` — memory operations tagged with address space
- `Return`

### Register Aliasing

The analyzer handles overlapping registers (e.g., x86 `rax`/`eax`/`ax`/`al`) by always reading/writing the largest containing register and inserting shift/mask operations for sub-registers. The `find_largest_fitting_register` method drives this.

### External Dependency: rsleigh

`rsleigh` is a local path dependency at `../rsleigh` (not in this workspace). It wraps GHIDRA's Sleigh specification to lift machine instructions to p-code (`rsleigh::Insn` with `Opcode`). Key types used: `Vn` (varnode — register/memory/const/unique), `VnSpace`, `MemReader`.
