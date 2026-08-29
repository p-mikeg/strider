# CLAUDE.md

Guidance for Claude Code working in this repository. This file orients; the
per-crate detail lives in each crate's own module and item docs. When they
disagree with this file, trust the code. Longer-form docs live in `docs/`.

Strider lifts a native binary function to a sea-of-nodes IR and exposes it for
pattern queries from Rust or Python:

    binary -> CFG -> IR -> optimizations -> pattern queries (Rust / Python)

## Skill notes

This workspace is Rust-only, plus thin PyO3 bindings in `crates/strider-py/`.
Ignore JS/TS guidance from any plugin-provided skill; the conventions that apply
are clippy plus the workspace lints in `Cargo.toml`.

## Build & Run Commands

```bash
cargo build --workspace
cargo test --workspace
cargo test --package <crate-name> <test_name>   # single test
cargo clippy --workspace

# The rest of what CI gates on (.github/workflows/ci.yml):
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --release   # a debug_assert hides from the debug run
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links' cargo doc --workspace --no-deps
cargo +1.91.0 check --workspace --all-targets   # the declared MSRV

# Main demo: reads the committed fixtures/out/x86/arithmetic.elf::add and dumps
# cfg / graph / graph-opt as both .html and .dot at the workspace root.
# Source: crates/strider-orchestrator/examples/orchestrator_demo.rs
cargo run -p strider-orchestrator --example orchestrator_demo

# Per-arch IR cmp shapes (debug helper for the FlagCmpCanonicalize work).
cargo run -p strider-orchestrator --example dump_arch_cmps
```

Python (from the workspace root, never from `crates/strider-py`):

```bash
uv sync --group dev
uv run maturin develop
uv run pytest
uv run pyright     # type-checks strider/, its tests and examples; gate is 0 errors
```

## Crates

Sixteen crates: six generic utilities, nine strider-specific, plus the
`strider-ir-test-utils` dev-dependency. External dep `rsleigh` (GHIDRA's Sleigh
p-code lifter), vendored as a git submodule at `externals/rsleigh`, is used by
every crate that touches machine state, i.e. all but the five payload-agnostic
leaves (`dot`, `entity-utils`, `graph-algorithms`, `read-only-memory`,
`strider-graph`).

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
  `largest_container_in`, ...) over `rsleigh` and `rustc-hash`, with no
  workspace dependency, so ir / lift / opt / pattern share one "which tracked
  varnode contains this one".

Strider:

- `strider-ir` -- the sea-of-nodes IR.
- `strider-target` -- pure target descriptions (`SleighArch`,
  `CallingConvention` / `BuiltCallingConvention`, the `CallOther` ABI table).
- `strider-reader` -- ELF loader and `ReadOnlyMemory` backend.
- `strider-cfg` -- bytes -> `Cfg` of basic-block regions via Sleigh. IR-free.
- `strider-lift` -- CFG -> IR. The `Lifter<R>` engine owns the arch, `Sleigh<R>`
  and cached `SleighRegs`; the calling convention is a per-call argument.
- `strider-pattern` -- the graph-based pattern DSL (`Pattern` / `Capture` /
  `Matcher` / `Match` / builders) over `strider-graph` with the `NeverCacheable`
  policy. `Pat` is the Python class, not a Rust type.
- `strider-opt` -- optimization passes, the `OptimizerPipeline`, the
  `rewrite_rule` facade over `strider-pattern`, and the
  `indirect_branch_resolve` classifiers. Pure graph->graph; the classifier
  reports targets and the orchestrator rebuilds the CFG from them.
- `strider-orchestrator` -- `Strider::analyze` plus the re-exported lift engine
  (`Lifter` / `LiftOptions` / `LiftOutcome` at the crate root) and `strider-opt`
  re-exported as `opt`.
- `strider-ir-test-utils` -- `make_empty_fn` / `RegisterSet` builders,
  `MockRom`, asm-fingerprint stamping, and the `proptest-gen`-gated
  `proptest_gen` generator, shared by tests.
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
                      entity-utils, graph-algorithms, vn-container
strider-orchestrator -> strider-cfg, strider-ir, strider-lift, strider-opt,
                      strider-target
strider-py         -> orchestrator, opt, cfg, reader, ir, target, pattern, dot
strider-ir-test-utils (dev) -> strider-ir, strider-target
```

Workspace production dependencies only. Leaves (no workspace deps): `dot`,
`entity-utils`, `read-only-memory`, `strider-graph`, `strider-target`,
`vn-container`. Notably `strider-cfg` is IR-free, `strider-reader` depends only
on `read-only-memory`, and the orchestrator reaches `strider-pattern` and `dot`
only from its tests and examples. External `petgraph` is a production
dependency of `strider-cfg`, `strider-graph`, `strider-ir`, `strider-lift`,
`strider-opt` and `strider-pattern`.

Dev-dependencies are not in that graph, and are not a DAG:
`strider-ir-test-utils` depends on `strider-ir`, which dev-depends back on it.

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
`Xor(x, all_ones)`); `IntBinaryOp{Add,And,Or,Xor,Div,Sdiv,Rem,Srem,ShiftRight,
SShiftRight,ShiftLeft,Mul}`; `IntCmpOp{Equal,Sless,Less,Carry,Scarry,Sborrow}`
(output `I1`); `FloatBinaryOp{Add,Mul,Div}`;
`FloatUnaryOp{Neg,Abs,Sqrt,Ceil,Floor,Round}`; `FloatCmpOp{Equal,Less}`;
`ExtendOp{ZeroExtend,SignExtend}`. Subtraction and the `<=` / `!=` comparisons
arrive already lowered; see Lift-time canonicalisations.

`ValueType` (`node/value_type.rs`): `I1, I8, I16, I24, I32, I40, I48, I56, I64,
I72, I80, I96, I112, I128, I256, I512, F16, F32, F64, F80, F128`. Booleans are
the 1-bit integer `I1`. The width set is measured against the sla specs: an
unmapped width fails the whole function's lift, so every width a Sleigh varnode
or temporary carries needs a variant. `bit_width(I1) == 1` (the one case where
width != byte_size*8); `int_for_byte_size(1) -> I8`, never `I1`.

`expected_signature` (`node_signature.rs`) is the SSoT for each kind's slot
kinds. `Graph` holds only structure; per-function overlay (entry, `default_cc`,
`endianness`, tracked varnodes, interners, side-tables) lives on `Function`,
which wraps a `Graph`. `entry` is a non-optional `NodeId` (a `Function` always
has one), so `EditFunction::new(&mut Function)` is infallible.

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
BoolNeg(x)          -> Xor(x, IntConst(1)):I1
IntNeg(x)           -> Xor(x, all_ones)   (Sleigh IntNeg is complement ~x)
If(Xor(C,IntConst(1)):I1){A}{B} -> If(C){B}{A}   (opt::IfCondInversion)
```

Commutative matching tries both operand orders, driven by the single source of
truth `NodeKind::is_commutative`: int `Add/Mul/And/Or/Xor`, float `Add/Mul`,
`IntCmpOp::{Equal,Carry,Scarry}`, `FloatCmpOp::Equal`.

## Cross-cutting invariants

- The workspace is single-threaded: prefer `Box` / moves / `&`-borrows, and
  opt into `Rc` at a call site only if needed. Nothing runs in parallel, so a
  `Send` bound never buys concurrency here; it buys the right to MOVE a value
  between threads, which is what a Python caller needs.
  - `strider-reader`: a mapped image is shared by every region cut from it.
  - `read-only-memory`: `ReadOnlyMemory: Send` so `strider-py` can drop the GIL
    around `analyze`. The `Sync` half it also carries is unused.
  - `strider-pattern`: every boxed closure a pattern lowers to is `+ Send`, and
    `MatchPat` requires it, so a compiled `Pattern` moves with the `Pat` that
    owns it. A `.when()` predicate must therefore be `Send`: capture an
    `Arc<AtomicUsize>`, not an `Rc<Cell<_>>`. `JoinPredicateFn` is
    `Arc<... + Send + Sync>`: `JoinConstraint` is `Clone`, so the predicate is
    shared rather than duplicated.
  - `strider-py`: `OptimizerPipeline` is the only `#[pyclass(unsendable)]`.
    `Lifter` moves and drops anywhere but decodes only on its creating thread,
    which `ThreadPinned` enforces with a catchable `StriderError`, because
    `Sleigh` is thread-affine (see below). `BufferReader` is an
    `Arc<Mutex<_>>`, so a clone shares the region memo and the wrapper is
    still `Send`; `_LoadedElf` holds two of those and a `Mutex` symbol table.
    Everything else moves, so a REPL or notebook that introspects off-thread
    does not abort the interpreter.
- `Sleigh::lift_one(&mut self)` is NOT stateless: it carries context-register
  state (ARM/Thumb, x86 segment, MIPS16), so per-region decoding must stay
  sequential (`RegionBuilder::build`).
- Register aliasing (x86 rax/eax/ax, AArch64 q/d/s, x87 ST*, ...) is handled in
  the lifter's `read_vn` / `write_vn`: reads and writes go through the largest
  containing register with shift/mask for sub-register slices.
  `FunctionBuilder::new` dedups tracked varnodes into their largest containers;
  `Function::new` sorts them, which is what fixes `InitialVnId` numbering.
- Indirect-branch resolution is a re-lift fixed-point loop in `Strider::analyze`
  that converges on the induced edge set; unresolvable branches are a result
  (`unresolved_indirect_branches`), not an error. A site that NARROWS twice has
  an unstable answer: it is abandoned and reported, never an `Err`. Exhausting
  `MAX_RESOLUTION_ITERATIONS` while every site still grows is the discovery
  depth limit, and those sites come back as unresolved too.
- A converged CFG is never silently incomplete, but it reports through FOUR
  channels on `AnalyzeResult`, and a consumer asking "may this be incomplete?"
  reads all four. `unresolved_indirect_branches` holds a site that lost a
  successor or whose re-derived widening could not be seated (an interworking
  `Switch` carries no ISA-mode input, so a re-derived arm has no mode to decode
  in); empty means fully resolved. `unverified_seeded_sites` holds a dispatch
  the CFG consumed as `Return` / `TailCall` -- a complete answer that cannot be
  verified, not a loss, which is why an ARM `pop {pc}` epilogue lands here and
  not in the first channel. `isa_mode_conflicts` and `interior_branch_targets`
  carry the other two. The first, third and fourth accumulate across rounds, so
  a later round cannot launder an earlier loss; `unverified_seeded_sites` is
  derived once from the final CFG. `isa_mode_conflicts` is structurally always
  empty off ARM and MIPS: both producers gate on `SleighArch::isa_mode_var()`,
  `Some` only for the four ARM and four MIPS presets.
- SP-alias precision is tuned by `OptOptions` (`resolve_indirect_branches`,
  `assumptions`), threaded through `OptCtx` into every SP-aware pass.
  `assumptions` is an `AssumptionOptions` holding `stack_global_disjoint`,
  `assume_incoming_args_survive_calls`, `distinct_sp_bases_disjoint`,
  `callee_preserves_stack_args`, `noalias_allocators` and `escape_analysis`:
  each is a claim about the code being analysed that the IR cannot prove, so a
  wrong one miscompiles. Every field's risky value is the positive one; the
  first two default ON (the pipeline is unusably imprecise without them) and
  the rest off, so `AssumptionOptions::none()`, not `::default()`, is the
  configuration sound under any input. `callee_preserves_stack_args` is inert
  alone: its only reader, `in_outgoing_arg_area` in `mem_analysis`, is reached
  only under `escape_analysis` or a non-empty `noalias_allocators`.
  `noalias_allocators` (pure `malloc`-like callee addresses) is published onto
  the `Function` so `decompose` classifies a `Call` return as a heap base;
  distinct heap objects are disjoint and a load steps through such a call.

## strider-py

Domain-namespaced submodules (`strider.ir`, `.lift`, `.cfg`, `.sleigh`,
`.reader`, `.opt`, `.pattern`, `.template`, plus `.explore`, which backs
`visualize` and is bound but outside `__all__`) plus the single top-level
`strider.StriderError`. `strider.lift.lifter(arch, mem, rom=None)` builds the
lift handle; `analyze(entry, cc, opts)` returns an `AnalyzeResult` with
`.cfg` / `.function` / `.unresolved` (also unpackable as a 3-tuple). Pattern
queries (`find_all` / `find_unique`) and rendering (`Function.to_dot(pretty=)`)
live on the returned objects. See the `.pyi` stubs for the typed surface.
