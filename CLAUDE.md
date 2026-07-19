# CLAUDE.md

Guidance for Claude Code working in this repository. This file orients; the
per-crate detail lives in each crate's own module and item docs, which are the
source of truth. When they disagree with this file, trust the code.

Strider lifts a native binary function to a sea-of-nodes IR and exposes it for
pattern queries from Rust or Python:

    binary -> CFG -> IR -> optimizations -> pattern queries (Rust / Python)

## Skill notes

This workspace is Rust-only (plus thin Python bindings via PyO3 in
`crates/strider-py/`). When the `code-simplifier` skill (or any other
plugin-provided skill) emits JS/TS guidance ("use ES modules", "prefer function
over arrow functions", "follow React component patterns"), ignore it: the
project-relevant guidance is the Rust conventions established by clippy plus the
workspace lints in `Cargo.toml`.

## Build & Run Commands

```bash
cargo build --workspace
cargo test --workspace
cargo test --package <crate-name> <test_name>   # single test
cargo clippy --workspace

# Main demo: reads fixtures/out/x86/arithmetic.elf::add (build it first from
# fixtures/Makefile) and dumps cfg.html / graph.html / graph-opt.html at the
# workspace root. Source: crates/strider-orchestrator/examples/orchestrator_demo.rs
cargo run -p strider-orchestrator --example orchestrator_demo

# Per-arch IR cmp shapes (debug helper for the FlagCmpCanonicalize work).
cargo run -p strider-orchestrator --example dump_arch_cmps
```

Python (from the workspace root, never from `crates/strider-py`):

```bash
uv sync --group dev
uv run maturin develop
uv run pytest
```

## Crates

Sixteen crates: six generic utilities, nine strider-specific, plus the
`strider-ir-test-utils` dev-dependency. External path dep `rsleigh` at
`../rsleigh` (GHIDRA's Sleigh p-code lifter) is used by every crate.

Generic:

- `dot` -- Graphviz / dark-themed HTML renderer.
- `entity-utils` -- `cranelift-entity` helpers (`DenseEntitySet`, `Worklist`,
  `EntityInterner`). Use these over `std` `HashSet`/`HashMap` when keying by
  `NodeId` / `ValueId`.
- `graph-algorithms` -- generic traversal (`walk`) and dominance-based SSA
  support (dominance frontiers, dominator-tree preorder, iterated-DF phi
  placement) over opaque node ids. Test-only `graphmock` DSL under `tests/`.
- `strider-graph` -- generic despite the name: the payload-agnostic bipartite
  sea-of-nodes `Graph<N, V, C: NodeCacheable<N, V>>` that `strider-ir` and
  `strider-pattern` build on. No `Hash`/`Eq` bound on payloads; dedup lives in
  the `C` policy, backed by the hash-on-demand `NodeCache`.
- `read-only-memory` -- the `ReadOnlyMemory` trait, in its own crate so the
  optimizer / lifter / reader depend on it one-way without back-edging through
  the ELF reader.
- `vn-container` -- varnode container geometry (`vn_contains`,
  `largest_container_in`, ...), depending only on `rsleigh`, so ir / lift /
  pattern share one "which tracked varnode contains this one".

Strider:

- `strider-ir` -- the sea-of-nodes IR.
- `strider-target` -- pure target descriptions (`SleighArch`,
  `CallingConvention` / `BuiltCallingConvention`, the `CallOther` ABI table).
- `strider-reader` -- ELF loader and `ReadOnlyMemory` backend.
- `strider-cfg` -- bytes -> `Cfg` of basic-block regions via Sleigh. IR-free.
- `strider-lift` -- CFG -> IR. The `Lifter<R>` engine owns the arch, `Sleigh<R>`
  and cached `SleighRegs`; the calling convention is a per-call argument.
- `strider-pattern` -- the graph-based pattern DSL (`Pat` / `Capture` /
  `Matcher` / `Match` / builders) plus its rewrite facade, over `strider-graph`
  with the `NeverCacheable` policy.
- `strider-opt` -- optimization passes, the `OptimizerPipeline`, and the
  `indirect_branch_resolve` classifiers. Pure graph->graph; resolution is
  rebuild-driven (no in-place IR editor, no orchestrator back-edge).
- `strider-orchestrator` -- `Strider::analyze` plus the re-exported lift engine
  (`Lifter` / `LiftOptions` / `LiftOutcome` at the crate root) and `strider-opt`
  re-exported as `opt`.
- `strider-ir-test-utils` -- `make_empty_fn` / `RegisterSet` builders and
  asm-fingerprint stamping shared by tests.
- `strider-py` -- PyO3 bindings (`maturin develop` builds a wheel).

## Dependency graph (X -> Y = X depends on Y)

```
graph-algorithms   -> entity-utils
strider-cfg        -> strider-target, dot
strider-ir         -> strider-graph, strider-target, read-only-memory, dot,
                      entity-utils, graph-algorithms, vn-container
strider-reader     -> read-only-memory
strider-lift       -> strider-cfg, strider-ir, strider-target,
                      graph-algorithms, vn-container
strider-pattern    -> strider-ir, strider-graph, vn-container
strider-opt        -> strider-cfg, strider-ir, strider-pattern, strider-target,
                      entity-utils, graph-algorithms
strider-orchestrator -> strider-cfg, strider-ir, strider-lift, strider-opt,
                      strider-target
strider-py         -> orchestrator, opt, cfg, reader, ir, target, pattern, dot
strider-ir-test-utils (dev) -> strider-ir, strider-target
```

Leaves (no workspace deps): `dot`, `entity-utils`, `read-only-memory`,
`strider-graph`, `strider-target`, `vn-container`. It is a DAG, no back-edges.
Notably `strider-cfg` is IR-free, `strider-reader` depends only on
`read-only-memory`, and the orchestrator has no direct `strider-pattern` or
`dot` dependency.

## IR node model

`NodeKind` (`strider-ir/src/node/kind.rs`), grouped:

- Initial: `Entry`, `InitialMemory`, `InitialVar(InitialVnId)`.
- Region / phi: `Region`, `MemPhi`, `Phi`.
- Control: `If`, `Switch`, `IndirectBranch` (unresolved placeholder),
  `Unreachable` (no-return sink), `Return`.
- Calls: `Call`, `CallOther { user_op_id }`.
- Memory: `Load(VnSpace)`, `Store(VnSpace)`.
- Integer (incl. booleans): `IntConst(ConstId)`, `IntUnaryOp`, `IntBinaryOp`,
  `IntCmpOp`, `Truncate`, `Extend(ExtendOp)`, `Popcount`, `Lzcount`.
- Float: `FloatConst(u64)`, `FloatUnaryOp`, `FloatBinaryOp`, `FloatCmpOp`.
- Conversions: `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`,
  `FloatBitsToInt`.
- Opaque: `SegmentOp { op_id }`, `CPoolRef`, `New`.

Op sub-enums (`node/ops.rs`): `IntUnaryOp{Neg}` (complement `~x` is
`Xor(x, all_ones)`, no `BitNot`); `IntBinaryOp{Add,And,Or,Xor,Div,Sdiv,Rem,Srem,
ShiftRight,SShiftRight,ShiftLeft,Mul}` (no `Sub`, lowered to `Add(_, Neg(_))`);
`IntCmpOp{Equal,Sless,Less,Carry,Scarry,Sborrow}` (output `I1`; no
`LessEqual`/`SlessEqual`); `FloatBinaryOp{Add,Mul,Div}` (no `Sub`);
`FloatUnaryOp{Neg,Abs,Sqrt,Ceil,Floor,Round}`; `FloatCmpOp{Equal,Less}`;
`ExtendOp{ZeroExtend,SignExtend}`.

`ValueType` (`node/value_type.rs`): `I1, I8, I16, I32, I48, I64, I80, I128,
I256, I512, F32, F64, F80`. Booleans are the 1-bit integer `I1`, so there is no
`Bool` type or `BoolConst`/`BoolBinaryOp`/`CastToBool`/`CastToInt`/`CastToFloat`.
`bit_width(I1) == 1` (the one case where width != byte_size*8);
`int_for_byte_size(1) -> I8`, never `I1`.

`expected_signature` (`node_signature.rs`) is the SSoT for each kind's slot
kinds. `Graph` holds only structure; per-function overlay (entry, `default_cc`,
tracked varnodes, interners, side-tables) lives on `Function`, which wraps a
`Graph`. `entry` is a non-optional `NodeId` (a `Function` always has one), so
`EditFunction::new(&mut Function)` is infallible.

Integer constants: `IntConst(ConstId)` interns through
`Function::const_interner` into `ConstValue { Bits(u128), Wide(Box<[u64]>) }`
(all int constants, not only wide ones); read via `IRViewer::int_const_u128` /
`int_const_i128`, never by matching the payload. Interning masks to the declared
width before dedup, so equal constants from different paths collapse.

IR trait layering: `IRViewer` (point reads), `IRWalker: IRViewer` (control-aware
walks), `IRBuilder: IRViewer` (`create_node` / `create_node_attributed`),
`IRBuilderExt: IRBuilder` (the blanket `build_*` vocabulary). `Function`,
`FunctionBuilder`, and `EditFunction` implement them.

## Lift-time canonicalisations

The lifter emits these shapes so patterns match the canonical form:

```
IntSub(a,b)         -> Add(a, Neg(b))
IntLessEqual(a,b)   -> Xor(Less(b,a), IntConst(1)):I1
IntSlessEqual(a,b)  -> Xor(Sless(b,a), IntConst(1)):I1
IntNotEqual(a,b)    -> Xor(Equal(a,b), IntConst(1)):I1
FloatSub(a,b)       -> FloatAdd(a, Neg(b))
FloatNotEqual(a,b)  -> Xor(FloatEqual(a,b), IntConst(1)):I1
FloatLessEqual(a,b) -> Or(FloatLess(a,b), FloatEqual(a,b)):I1   (NaN-aware)
FLOAT_NAN(x)        -> Xor(FloatEqual(x,x), IntConst(1)):I1
If(Xor(C,IntConst(1)):I1){A}{B} -> If(C){B}{A}   (opt::IfCondInversion)
```

Commutative matching tries both operand orders, driven by the single source of
truth `NodeKind::is_commutative`: int `Add/Mul/And/Or/Xor`, float `Add/Mul`,
`IntCmpOp::{Equal,Carry,Scarry}`, `FloatCmpOp::Equal`.

## Cross-cutting invariants

- No `Arc` / `Send` / `Sync` in core types: the workspace is single-threaded.
  Use `Box` / moves / `&`-borrows; opt into `Rc` at a call site only if needed.
  (`read-only-memory` is the one deliberate exception.)
- `Sleigh::lift_one(&mut self)` is NOT stateless: it carries context-register
  state (ARM/Thumb, x86 segment, MIPS16), so per-region decoding must stay
  sequential (`RegionBuilder::build`).
- Register aliasing (x86 rax/eax/ax, AArch64 q/d/s, x87 ST*, ...) is handled in
  the lifter's `read_vn` / `write_vn`: reads and writes go through the largest
  containing register with shift/mask for sub-register slices. Varnode
  canonicalisation (dedup into largest containers, deterministic sort) is owned
  by `FunctionBuilder::new`.
- Indirect-branch resolution is a monotone re-lift fixed-point loop in
  `Strider::analyze`; unresolvable branches are a result
  (`unresolved_indirect_branches`), not an error.
- SP-alias precision is tuned by `OptOptions` (`alias_mode`, `calls_clobber`,
  `assume_distinct_sp_bases_disjoint`), threaded through `OptCtx` into every
  SP-aware pass.

## strider-py

Domain-namespaced submodules (`strider.ir`, `.lift`, `.cfg`, `.sleigh`,
`.reader`, `.opt`, `.pattern`, `.template`) plus the single top-level
`strider.StriderError`. `strider.lift.lifter(arch, mem, rom=None)` builds the
lift handle; `analyze(entry, cc, opts)` returns an `AnalyzeResult` with
`.cfg` / `.function` / `.unresolved` (also unpackable as a 3-tuple). Pattern
queries (`find_all` / `find_unique`) and rendering (`Function.to_dot(pretty=)`)
live on the returned objects. See the `.pyi` stubs for the typed surface.
