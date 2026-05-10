# `cfg` — control-flow graph construction

Builds a function-scoped control-flow graph (CFG) from a binary by driving
GHIDRA's Sleigh p-code lifter ([`rsleigh`](../../../rsleigh)). Each basic block
(region) holds a sequence of `rsleigh::Insn` p-code instructions; edges encode
fall-through, unconditional branches, and the two arms of a conditional branch.
The CFG is the input to the [`strider`](../strider) crate's IR translator.

## Public surface

- `Cfg<R: rsleigh::MemReader>` — finished graph. Holds the `petgraph::StableDiGraph`
  of regions, the entry `RegionId`, and the `rsleigh::Sleigh<R>` lifter context
  (kept across analysis iterations so the SLA spec is loaded once).
- `Builder<R>` / `OptionsBuilder` — fluent constructors. Three constructors:
  `Builder::new(sleigh, start_addr, options)` — defaults to LE + x86_64; convenient
  but unsafe for non-x86_64 / big-endian binaries.
  `Builder::with_endianness(sleigh, start_addr, options, endianness)` — set
  endianness only.
  `Builder::for_arch(arch, sleigh, start_addr, options)` — **preferred**: derives
  both endianness and `ArchPreset` from a `target::SleighArch` atomically.
  `Builder::build()` produces a `Cfg`.
- `Region`, `RegionInstruction`, `RegionTerminator` — basic block, the lifted
  p-code instructions inside it, and the terminator kind.
- `RegionEdgeKind` — `Fallthrough` | `Branch` | `IfCaseTrue` | `IfCaseFalse`.
- `RegionId` — `petgraph::graph::NodeIndex` alias.
- `MachineInsnAddr`, `PcodeInsnAddr` — typed wrappers separating "byte address
  of a machine instruction" from "(machine-addr, pcode-sub-index)" tuples.
- `IfRegionState` — resolved/unresolved state of an `If` region.
- `DecodeCache` — per-address decode cache reused across rebuilds.
- `ResolvedTargets` — re-export from `opt`; the indirect-branch resolver hands
  these back to the builder when splicing in newly-discovered successor regions.
- `is_addr_tail_call(target, start, fn_max_size, allow_code_before_start_addr)`
  — predicate the builder consults when classifying out-of-range targets.

## Architecture

`src/cfg/builder/` houses the per-region decoder (`region_builder.rs`) and the
per-CFG builder (`mod.rs`) that splices regions together. The region builder
walks p-code emitted by `rsleigh::Sleigh::lift_one` for one machine instruction
at a time, classifying each opcode as fall-through, branch, indirect branch,
call, or return, then deciding whether the current region should terminate.

`src/cfg/builder/indirect_resolve.rs` is the bridge to the indirect-branch
resolver in `opt`: when the builder sees a `BranchIndirect`, it leaves a
placeholder anchor that the strider orchestrator's indirect-branch fixed-point loop
rewrites once the IR has been built and optimised. Resolved targets feed back
in via `ResolvedTargets`, splicing new regions into the CFG without rebuilding.

`src/cfg/types.rs` defines the data types; `query.rs` houses pure query helpers
(`is_addr_tail_call`, `IfRegionState`); `dot.rs` implements `GraphDotDumper`
from the [`dot`](../dot) crate so a `Cfg` can be rendered to `.dot` / `.html`.
The `petgraph::StableDiGraph` is exposed as `Cfg::graph` so callers can run
arbitrary graph algorithms directly.

## Key invariants

- Bounded-lift contract: when `Options::function_max_size = Some(n)`, every
  region terminates at or before `start + n`. Out-of-range successors become
  `RegionTerminator::TailCall { target }`. The `region_builder` bound-checks
  after every `next_pcode_addr` advance.
- A `CondBranch` whose successors split across the bound is rewritten: both OOB
  collapses to `TailCall`; exactly one OOB pops the trailing `CondBranch` insn
  and emits an unconditional `Branch` to the in-range successor.
- `start_addr_to_region_id` is kept in sync with the region graph; the indirect
  resolver maintains it when splicing new regions via `add_region`.
- Regions terminate on the first iteration when an opcode is classified as
  `NoReturn` by `target::call_other_abi::classify` (e.g. trap intrinsics).

## Tests

Integration tests live in `crates/cfg/tests/` (one file per concern:
`region_builder_*.rs`, `indirect_resolve.rs`, `bounded_lift_tail_call.rs`,
`build_end_to_end.rs`, etc.). There is no `src/<mod>/tests.rs` file.

```
cargo test --package cfg
cargo test --package cfg <test_name>
```

## Gotchas

- `Cfg::sleigh` is reused across the strider indirect-branch fixed-point loop. Reusing
  one `Sleigh` for many `lift_one` calls is sound because Sleigh has no per-CFG
  state (only per-call decode buffers). See `tests/sleigh_reuse.rs`.
- Indirect branches are NOT resolved by `cfg` itself. `Builder::build` returns a
  CFG with placeholder edges; the resolver lives in `opt::indirect_branch_resolve`
  and is driven by `strider::run`.
- `RegionEdgeKind::Fallthrough` and `Branch` are used interchangeably from the
  IR's perspective — the distinction matters only when reasoning about the
  source machine code.
- Depends on `rsleigh` (a local path crate at `../rsleigh`). The Sleigh SLA
  spec for the chosen architecture must be available at runtime via
  `target::SleighArch`.
- `Builder::new` silently defaults to LE + x86_64. A non-x86_64 caller that
  forgets to chain `with_endianness` / `with_preset` would silently misclassify
  CallOthers and decode bytes in the wrong byte order. `strider::run` uses
  `Builder::for_arch` to avoid this — outside-strider callers should follow
  suit.
