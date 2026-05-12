# `ir` — sea-of-nodes intermediate representation

Strider's IR. Each lifted function is a directed graph of typed nodes with
deduplication-on-construction, SSA-like variable tracking with automatic
phi-insertion at join points, and a three-layer validator. The `ir` crate
defines the data model only — lifting is in [`pcode-lift`](../pcode-lift) and
[`strider`](../strider); rewrites are in [`opt`](../opt); queries are in
[`pattern`](../pattern).

## Public surface

- `Graph` (`graph::Graph`) — node/output/input arena. Internally three
  `cranelift_entity::PrimaryMap`s keyed by `NodeId` / `NodeOutputId` /
  `NodeInputId`, plus per-node side-tables (`stack_phi_offsets`,
  `call_other_names`, `asm_fingerprints`).
- `FunctionBuilder` (`builder::FunctionBuilder`) — builds the graph with SSA
  variable tracking. `set_region`, `set_lift_addr`, `build_*` constructors,
  `build()` to consume into a `BuiltFunctionGraph`. Carries an optional
  `lift_addr: Option<u64>` that, when set, unions the address into every newly
  created node's asm-fingerprint.
- `BuiltFunctionGraph` — finished graph plus `entry: NodeId`, `variables:
  PrimaryMap<VarId, rsleigh::Vn>`, `call_clobbered`, `ret_val_regs`, and
  `call_other_clobbered` (the per-Call/CallOther output-slot maps).
- `FunctionGraph` — under-construction view exposed by the builder for
  `dot::GraphDotDumper`.
- `node::{NodeId, NodeInputId, NodeOutputId, NodeKind, NodeOutputKind,
  NodeOutputType, FunctionArgSource}` — graph entity types.
- `RegionId`, `VarId` — basic-block identifier and per-region SSA variable id.
- `Value` (alias for `NodeOutputId`), `ValueType` (alias for `NodeOutputType`).
- Op enums: `IntBinaryOp`, `IntUnaryOp`, `IntCmpOp`, `BoolBinaryOp`,
  `BoolUnaryOp`, `FloatBinaryOp`, `FloatUnaryOp`, `FloatCmpOp`, `ExtendOp`.
- `validate::{validate, validate_with_options, ValidateOptions, ValidationError,
  ValidationErrors}` — three-layer graph validator.
- `walk::{walk_graph, cfg_reachable, GraphWalk, NodeIdSet}` — traversal helpers.
- `error::{Result, UnknownCallOtherError}` — typed errors.
- `dot::{GraphDotDumper, FunctionDotDumper}` — render to DOT/HTML via the
  [`dot`](../dot) crate.
- `test_utils` (cfg = `feature = "test-utils"`) — mock-IR helpers
  (`make_empty_fn`, `reg_vn`, `sp_vn_x86`, `sp_vn_x86_64`, ...).

## Architecture

The IR follows a **sea-of-nodes** style: there are no basic blocks at the node
level — control flow is encoded by `ControlState` / `If` / `IfCase` nodes whose
`Control` outputs feed downstream nodes' control inputs, while `Memory` and
value tokens flow through their own dedicated edges.

`src/node/` defines the core data: `NodeKind` (the variant enum covering every
op the IR supports), `NodeOutputKind` (`Control`, `Memory`, `PhiToken`, or
`OutputType(_)`), `NodeOutputType` (`Bool` / `U8` / `U16` / `U32` / `U64` /
`U80` (x87 80-bit) / `U128` / `U256` / `U512` (AVX-512 zmm) / `F32` / `F64` /
`F80` (x87 extended-precision)).
`src/node_signature.rs` is the **single source of truth** for each NodeKind's
expected input and output slot kinds — both the validator and the builder
consult it.

`src/builder/` orchestrates SSA construction: `vars.rs` tracks per-region
`VarId → Value` maps, `nodes.rs` exposes typed `build_*` constructors,
`call.rs` handles the call/return signature wiring, and `coerce.rs` inserts
casts when an operation expects a different output type than the producer
supplies. The builder calls `validate::validate` at the end of `build()` so
malformed graphs surface immediately.

`src/graph/` is the storage layer: `store.rs` (raw arena), `access.rs` (typed
getters), `uses.rs` (use-list maintenance), `compact.rs` (post-rewrite
arena compaction). Nodes are deduplicated by `(kind, inputs, output_kinds)` so
two structurally equal nodes share a single `NodeId`. The `asm_fingerprints`
side-table is the union of every contributor's machine address — when two
identical nodes merge, the merged fingerprint is the **union** of their
ancestors' addresses.

`src/validate/` runs three independent layers and aggregates errors into a
`ValidationErrors` bundle: **Layer A** (`layer_a.rs`) checks per-node typing
against `expected_signature` for nodes reachable via `walk_graph`; **Layer B**
(`layer_b.rs`) verifies bidirectional use-list consistency; **Layer C**
(`layer_c.rs`) enforces graph-level invariants (Entry/InitialMemory uniqueness,
ControlState predecessor kinds, phi token ownership and per-predecessor arity).
An opt-in **Layer C** check via `ValidateOptions { check_asm_fingerprints: true }`
flags every reachable non-exempt node with an empty fingerprint.

## Key invariants

- **Asm-fingerprint contract**: every reachable non-exempt `NodeId` carries a
  sorted-deduped list of machine-instruction addresses. Passes may *grow*
  fingerprints but never shrink them. Two cacheable identical nodes share one
  entry that is the **union** of all contributors. Region/phi/initial-state
  kinds (`Entry`, `InitialMemory`, `InitialVar`, `FunctionArg`, `ControlState`,
  `MemPhi`, `VarPhi`, `ValuePhi`, `StackStorePhi`) are exempt.
- **Dedup-on-construction**: `Graph::create_node` returns an existing `NodeId`
  if one already exists with the same `(kind, inputs, output_kinds)`.
- **SSA invariant**: every value-typed `NodeOutputId` has exactly one producer.
  Use-edges go input→output; the use-list (`Graph::uses`) is the inverse.
- **Validator scoping**: Layer A checks only nodes reachable via `walk_graph`
  from `entry`. Optimisation passes leave detached zombie nodes in the arena;
  these are intentionally skipped.
- **Type coercion is explicit**: the builder inserts `CastToInt` / `Truncate` /
  `Extend` / `CastToBool` / `CastToFloat` nodes when input types don't match;
  no implicit width/sign coercion happens at use-sites.

## Tests

Inline unit tests in `src/<mod>/tests.rs` (one per submodule: `builder/tests.rs`,
`graph/tests.rs`, `node/tests.rs`, `validate/tests.rs`, `dot/tests.rs`).
Integration tests in `crates/ir/tests/` cover end-to-end build-and-validate
flows, dedup-cache behaviour, walk reachability, proptest-driven graph
invariants, and the `feature = "test-utils"` mock helpers.

```
cargo test --package ir
cargo test --package ir <test_name>
cargo bench --package ir --bench validate
```

## Gotchas

- **Lift-time canonicalisation**: several pcode opcodes are lowered at lift
  time — `IntSub(a,b)` → `Add(a, Neg(b))`, `IntLessEqual(a,b)` →
  `BoolNeg(IntLess(b,a))`, `FloatSub` → `FloatAdd(_, Neg(_))`, etc. The IR
  contains no `Sub`, `LessEqual`, `SlessEqual`, `NotEqual`, `FloatSub`,
  `FloatNotEqual`, or `FloatLessEqual` variants. See the lib.rs doc and the
  CLAUDE.md "lift-time canonicalisation" subsection for the full list.
- **`IntUnaryOp::BitNot` vs `Neg`**: `BitNot` is bitwise complement (`~x`);
  `Neg` is two's-complement negation (`-x`). rsleigh's `IntNeg` opcode lifts
  to `BitNot` (Sleigh nomenclature predates the rename).
- The `test-utils` feature is required by some downstream test crates (see
  e.g. `pattern`, `opt`, `strider` dev-deps). Don't enable it in production
  builds.
- `BuiltFunctionGraph::call_other_clobbered` is the function-default clobber
  list (everything except SP). Per-CallOther overrides live on
  `Graph::call_clobbered_overrides` and shadow the default at any given site.
