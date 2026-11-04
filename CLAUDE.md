# CLAUDE.md

Orientation for Claude Code in this repository. Detail lives in each crate's
module and item docs; when they disagree with this file, trust the code.
Longer-form docs live in `docs/`.

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
cargo test --workspace --release   # release keeps overflow-checks on and
                                   # debug-assertions off (root Cargo.toml)
RUSTDOCFLAGS='-D rustdoc::broken_intra_doc_links' cargo doc --workspace --no-deps
cargo +1.91.0 check --workspace --all-targets   # the declared MSRV
cargo +1.91.0 check -p <crate> --all-targets    # every package on its own
cargo run --release -p strider-orchestrator --example dump_arch_cmps
                                   # and orchestrator_demo (plain and -- memory);
                                   # the tree must stay clean afterwards

# Re-bless fixtures/golden, then review git diff:
UPDATE_EXPECT=1 cargo test -p strider-orchestrator --test golden

# Main demo: dumps cfg / graph / graph-opt of fixtures/out/x86/arithmetic.elf::add
# as .html and .dot at the workspace root; `-- memory` does x64/memory.elf::main.
cargo run -p strider-orchestrator --example orchestrator_demo
cargo run -p strider-orchestrator --example orchestrator_demo -- memory

# The p-code of cmp-then-branch sequences per flag model
# (reference for FlagCmpCanonicalize's rules).
cargo run -p strider-orchestrator --example dump_arch_cmps
```

Python (from the workspace root, never from `crates/strider-py`):

```bash
uv sync --group dev
uv run maturin develop --features test-hooks   # without it, 2 tests skip
uv run pytest
uv run pyright     # strider/, its tests and examples; gate is 0 errors
```

`test-hooks` is a default-off `strider-py` feature carrying `_rebuild` (installs
a reader after the handle exists, to build a collectable cycle in tests) and
`CfgOptions._call_other_override_builds` (a resolution counter). CI enables it
for `maturin develop` and never for `maturin build`, so the wheel lacks it.

CI also runs every `crates/strider-py/examples/python/*.py` on Python 3.9 and
3.13, `pytest` against `maturin develop --release`, and checks that a
`maturin build --release` wheel carries every `.pyi` stub, `py.typed` and
`explore.html`.

## Crates

Sixteen crates: six generic utilities, nine strider-specific, plus the
`strider-ir-test-utils` dev-dependency. `rsleigh` (GHIDRA's Sleigh p-code
lifter, a git submodule at `externals/rsleigh`) is used by every crate except
`dot`, `entity-utils`, `graph-algorithms`, `read-only-memory` and
`strider-graph`.

Generic:

- `dot`: Graphviz / dark-themed HTML renderer.
- `entity-utils`: `cranelift-entity` helpers (`DenseEntitySet`, `Worklist`,
  `EntityInterner`, `UnionDag`). Prefer these over `std` sets and maps keyed
  by `NodeId` / `ValueId`.
- `graph-algorithms`: traversal (`walk`) and dominance (Lengauer-Tarjan,
  frontiers, dominator-tree preorder, iterated-DF phi placement) over opaque
  node ids. The brute-force `oracle` is behind `test-util`.
- `strider-graph`: the payload-agnostic bipartite sea-of-nodes
  `Graph<N, V, C: NodeCacheable<N, V>>`; dedup lives in the `C` policy.
- `read-only-memory`: the `ReadOnlyMemory` trait.
- `vn-container`: varnode container geometry (`vn_contains`,
  `largest_container_in`, ...), shared by target / ir / lift / opt / pattern.

Strider:

- `strider-ir`: the sea-of-nodes IR. `strider_ir::eval` is the integer
  evaluator, `ExpectedValueKind::admits` the slot-kind check,
  `NodeKind::operand_width` the operand-width rule. `AssumptionOptions` lives
  here (strider-opt re-exports it) because a `Function` holds its claims.
- `strider-target`: `SleighArch`, `CallingConvention` /
  `BuiltCallingConvention`, the `CallOther` ABI table, `ArchNames`
  (`SleighArch::names`) and `DecodeTables` (`SleighArch::decode_tables`). One
  `family/<name>.rs` per architecture family; the crate README says how to add
  a preset, row or family.
- `strider-reader`: ELF loader and `ReadOnlyMemory` backend.
- `strider-cfg`: bytes -> `Cfg` of regions via Sleigh. `DecodeEngine<R>` owns
  the `Sleigh<R>` and its decode context and builds every CFG. IR-free.
- `strider-lift`: CFG -> IR. `Lifter<R>` wraps a `DecodeEngine<R>`; the
  calling convention is a per-call argument.
- `strider-pattern`: the pattern DSL (`Pattern` / `Capture` / `Matcher` /
  `Match` / builders) over `strider-graph`. `Pat` is the Python class, not a
  Rust type. A pattern over `MAX_PATTERN_NODES` (256) or past the 1 MiB
  lowering stack budget is refused through `Pattern::root()`, as is a cyclic
  wiring. `TemplatePat` is sealed and implemented for `Captured<P>` only at
  `P = Var`.
- `strider-opt`: optimization passes, `OptimizerPipeline`, the `rewrite_rule`
  facade, and the `indirect_branch_resolve` classifiers (`classify_sites`).
  Pure graph -> graph. `classify_target`, `run_one`, `run_one_unvalidated`
  and `run_post` are public only under `test-util`.
- `strider-orchestrator`: `Strider::analyze` and `AnalyzeOptions`, re-exporting
  `Lifter` / `LiftOutcome` and `strider-opt` as `opt`.
- `strider-ir-test-utils`: shared test builders (`make_empty_fn`,
  `RegisterSet`, `MockRom`), the `interp` reference interpreter, a seeded
  `Rng`, `graph_oracle`, and the `proptest-gen`-gated generator.
- `strider-py`: PyO3 bindings.

## Dependency graph (X -> Y = X depends on Y)

```
graph-algorithms   -> entity-utils
strider-target     -> vn-container
strider-cfg        -> strider-target, dot
strider-ir         -> strider-graph, strider-target, read-only-memory, dot,
                      entity-utils, graph-algorithms, vn-container
                      (+ external dashu-int)
strider-reader     -> read-only-memory
strider-lift       -> strider-cfg, strider-ir, strider-target,
                      graph-algorithms, vn-container
strider-pattern    -> strider-ir, strider-graph, vn-container
strider-opt        -> strider-cfg, strider-ir, strider-pattern, strider-target,
                      entity-utils, graph-algorithms, vn-container
strider-orchestrator -> strider-cfg, strider-ir, strider-lift, strider-opt,
                      strider-target
strider-py         -> orchestrator, opt, cfg, reader, ir, target, pattern, dot
strider-ir-test-utils -> strider-ir, strider-target, graph-algorithms
```

Production dependencies only. `strider-ir-test-utils` depends normally on
`strider-ir` and `graph-algorithms` with `test-util`, so a workspace-wide build
unifies those features on; a workspace-wide test build also turns on
strider-graph's `edge-read-count`, so run a bench per package (`-p`). External
`petgraph` is a production dependency of `strider-cfg` and `strider-pattern`
only.

Dev-dependencies are not a DAG: `strider-ir` dev-depends on
`strider-ir-test-utils`, and several crates dev-depend on themselves to give
their `tests/` a feature (`test-util`, `test-injectors`).

## IR node model

`NodeKind` (`strider-ir/src/node/kind.rs`), grouped:

- Initial: `Entry`, `InitialMemory`, `InitialVar(InitialVnId)`.
- Region / phi: `Region`, `MemPhi`, `Phi`.
- Control: `If`, `Switch(SwitchTableId)`, `IndirectBranch` (unresolved
  placeholder), `Unreachable` (no-return sink), `Return`.
- Calls: `Call { cc: Option<CcId> }` (`None` is `default_cc`),
  `CallOther { user_op_id }`.
- Memory: `Load(VnSpace)`, `Store(VnSpace)`.
- Integer (incl. booleans): `IntConst(IntConstId)`, `IntUnaryOp`, `IntBinaryOp`,
  `IntCmpOp`, `Truncate`, `Extend(ExtendOp)`, `Popcount`, `Lzcount`.
- Float: `FloatConst(u64)`, `FloatUnaryOp`, `FloatBinaryOp`, `FloatCmpOp`.
- Conversions: `IntToFloat`, `FloatToInt`, `FloatToFloat`, `IntBitsToFloat`,
  `FloatBitsToInt`.
- Opaque: `CPoolRef`, `New`.

strider-ir warns on `_` arms over enums (`clippy::wildcard_enum_match_arm`, an
error under CI's `-D warnings`), so a new `NodeKind` variant forces a decision
at every classifying `match`. The lint does not see tuple scrutinees, guarded
`_ if` arms or `matches!`.

Op sub-enums (`node/ops.rs`): `IntUnaryOp{Neg}`;
`IntBinaryOp{Add,And,Or,Xor,Div,Sdiv,Rem,Srem,ShiftRight,
SShiftRight,ShiftLeft,Mul}`; `IntCmpOp{Equal,Sless,Less,Carry,Scarry,Sborrow}`
(output `I1`); `FloatBinaryOp{Add,Mul,Div}`;
`FloatUnaryOp{Neg,Abs,Sqrt,Ceil,Floor,Round}`; `FloatCmpOp{Equal,Less}`;
`ExtendOp{ZeroExtend,SignExtend}`. Subtraction and the `<=` / `!=` comparisons
arrive already lowered.

`ValueType` (`node/value_type.rs`): `enum { Int(u16), Float(u16) }`, the width
in bits, limited to the `SUPPORTED` list: integers 1, 8, 16, 24, 32, 40, 48,
56, 64, 72, 80, 96, 112, 128, 256, 512 and floats 16, 32, 64, 80, 128. Every
width a Sleigh varnode or temporary carries needs an entry, or the function's
lift fails. `ValueType::I1` is the only named type; write the rest
`ValueType::int(32)` / `ValueType::float(64)` (panic on an unlisted width).
`bit_width(I1) == 1`; `int_from_bytes(1)` is `I8`. `Debug` spells `I32`,
`Display` `i32`.

`expected_signature` (`node/signature.rs`) defines each kind's slot kinds.
`Graph` holds only structure; per-function data (`default_cc`, `byte_order`,
tracked varnodes, interners, side tables) lives on `Function`. The entry is
node 0: `Function::new` mints it first and `compact` keeps it first.

Integer constants intern through the function side tables' `const_interner`
into a `dashu_int::UBig`, masked to the declared width (`eval::mask`) before
dedup, so equal constants collapse. Read them via `IRViewer::int_const_u128` /
`int_const_i128` / `int_const_ibig`, never off the `UBig`. DOT labels are
compared across builds, so they spell the value (decimal raw, `const
{v:#x}:<ty>` pretty), never a `Debug` form.

IR traits: `IRViewer` (point reads), `IRWalker: IRViewer` (control-aware
walks), `IRBuilder: IRViewer` (`create_node` / `create_node_attributed`),
`IRBuilderExt: IRBuilder` (the blanket `build_*` vocabulary). `Function`,
`FunctionBuilder` and `EditFunction` implement them.

## Lift-time canonicalisations

The lifter emits these shapes so patterns match the canonical form:

```
IntSub(a,b)         -> Add(a, Neg(b))
IntLessEqual(a,b)   -> Xor(Less(b,a), IntConst(1)):I1
IntSlessEqual(a,b)  -> Xor(Sless(b,a), IntConst(1)):I1
IntNotEqual(a,b)    -> Xor(Equal(a,b), IntConst(1)):I1
FloatSub(a,b)       -> FloatAdd(a, Neg(b))   (a NaN b's sign flips)
FloatNotEqual(a,b)  -> Xor(FloatEqual(a,b), IntConst(1)):I1
FloatLessEqual(a,b) -> Or(FloatLess(a,b), FloatEqual(a,b)):I1   (NaN-aware)
FLOAT_NAN(x)        -> Xor(FloatEqual(x,x), IntConst(1)):I1
BoolNeg(x)          -> Xor(x, IntConst(1)):I1
IntNeg(x)           -> Xor(x, all_ones)   (Sleigh IntNeg is complement ~x)
If(Xor(C,IntConst(1)):I1){A}{B} -> If(C){B}{A}   (opt::IfCondInversion)
```

Commutative matching tries both operand orders for the kinds
`NodeKind::is_commutative` names: int `Add/Mul/And/Or/Xor`, float `Add/Mul`,
`IntCmpOp::{Equal,Carry,Scarry}`, `FloatCmpOp::Equal`. A pattern with one
pinned operand commutes too. `NodeInputs::interchangeable` skips the swap when
both pattern operands accept the same pairs with the same bindings.

## Cross-cutting invariants

- The workspace is single-threaded: prefer `Box` / moves / `&`-borrows, `Rc`
  only where needed. No Rust crate spawns a thread; a `Send` bound buys moving
  a value between threads, which lets `strider-py` drop the GIL around
  `analyze` so separate handles run in parallel.
  - `ReadOnlyMemory: Send`, not `Sync` (an impl may hold a `RefCell` memo).
  - Every closure a pattern lowers to is `Send`, so a `.when()` predicate
    captures an `Arc<AtomicUsize>`, not an `Rc<Cell<_>>`.
  - `strider-py` has no `unsendable` pyclass. `analyze`, `build_cfg` and
    `add_elf` borrow the handle's `PyCell` exclusively, so a concurrent call
    on it raises `StriderError`; read-only calls may overlap. Only `analyze`
    releases the GIL, and not over a Python reader. A daemon thread analysing through a Python reader at interpreter
    shutdown still aborts, which no Rust check can intercept.
- `Sleigh::lift_one(&mut self)` carries context-register state an instruction
  can commit at a later address (ARM/Thumb mode, IT blocks), so a region
  decodes sequentially (`RegionBuilder::build`). The state survives between
  `build_cfg` calls on one engine, so `RegionBuilder::rederive_flow` erases each
  var in `SleighArch::flow_commit_vars` and re-lifts its instruction; how it
  erases each var follows its `FlowCommit` class (the sla's `noflow`
  declaration). Only ARM needs it. Two edges reaching one address with
  different IT state keep the first decode.
- Register aliasing (x86 rax/eax/ax, AArch64 q/d/s, x87 ST*, ...) is handled in
  the lifter's `read_vn` / `write_vn` through the largest containing register.
  `Function::new` dedups tracked varnodes into their largest containers and
  sorts them, which fixes `InitialVnId` numbering.
- Indirect-branch resolution is a re-lift fixed-point loop in
  `Strider::analyze`. Unresolvable sites, sites whose answer narrows twice,
  sites still growing at `MAX_RESOLUTION_ITERATIONS`, and returns whose target
  is not provably the entry return address are reported, never an `Err`; a
  return the caller seeded as `LinkRegister` is `UnverifiedSeed` instead.
- Analysis never reads a callee's code: its stack pop and clobbers come from
  its calling convention, so a callee-cleanup `ret $imm16` or an i386 PC thunk
  needs a `per_address_ccs` entry.
- A converged CFG is never silently incomplete: `Cfg::incomplete()` is a set
  of `(PcodeInsnAddr, Reason)` over seven reasons, empty meaning complete.
  Target reasons accumulate over rounds; site reasons are the final round's.
  Each reason's contract is on `strider_cfg::Reason`.
- Memory precision rests on `Function::assumptions`, nine claims the IR cannot
  prove, so a wrong one miscompiles. `AnalyzeOptions.assumptions` seeds each
  lift and `Function::assume` adds claims, withdrawing none. Five default on;
  `msr_register_keeps_mode` and `fp_rounding_mode_is_nearest` are read by the
  lift. `AssumptionOptions::none()` is the sound floor for RAM-like memory
  and IEEE-default float controls. `LoadForward` and `FunctionArgDetect`
  narrow under `Policy::strict` (the claims less the call relaxations),
  because later walks honour no call relaxation. Per-field detail is in
  `crates/strider-ir/src/function/assumptions.rs`.

## strider-py

Submodules `strider.ir`, `.lift`, `.cfg`, `.sleigh`, `.reader`, `.opt`,
`.pattern` (with `.pattern.constraints`), `.template`, and `.explore` (the
`visualize` explorer, outside `__all__`), plus the top-level
`strider.StriderError`. `strider.lift.lifter(arch, mem, rom=None)` builds a
handle; `analyze(entry, cc, opts)` returns an `AnalyzeResult`
(`.cfg` / `.function` / `.unresolved`, unpackable as a 3-tuple) and never
raises over incompleteness. The `.pyi` stubs are the typed surface.

`CfgOptions.known_targets` / `call_other_abis` and
`LifterOptions.per_address_ccs` read back as memoized `MappingProxyType`s and
`no_return_targets` as a memoized `frozenset`, so a large seeded table costs
nothing per attribute read. A `CfgOptions` converts its tables once and every
`analyze` borrows them; `per_address_ccs` is resolved per arch on first use.
`ElfLifter.analyze` / `build_cfg` add a symbol's size bound, the ELF's `$d`
data ranges and its ppc64 local entries, which every per-address table is
looked up through (`strider_cfg::CfgOptions.local_entries`).
