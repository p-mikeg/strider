# strider-lift

Turns one function's `strider_cfg::Cfg` into a `strider_ir::Function`: a
sea-of-nodes graph in pruned SSA, with one IR `Region` per CFG region, the
control and memory edges wired along the CFG's edges, and every machine
register modelled as an SSA variable.

## What's here

- `Lifter<R>`, the engine: built once per architecture over an
  `rsleigh::Sleigh<R>` and reused across functions and rebuild rounds, since
  `Sleigh::regs()` is expensive and the register table never changes.
  `build_cfg(entry, cfg_opts, per_address_ccs)` decodes; `build_ir(cfg, cc)` /
  `build_ir_with(cfg, cc, opts)` lift. The calling convention is per function,
  hence an argument rather than engine state.
- `LiftOutcome`: the `Function`, plus `unresolved_branches` (each unresolved
  `BranchIndirect`'s pcode address and its `IndirectBranch` placeholder) and
  `switch_anchors` (each seated `Switch`, so a resolver can re-derive and WIDEN
  a table that resolved before the CFG finished growing).
- `LiftOptions`: the `CfgOptions` to decode with, `per_address_ccs` for direct
  call targets, and `compact`, which this crate only carries for the
  orchestrator to read after the pipeline.
- `FunctionLifter`, internal: the per-function context holding the
  `FunctionBuilder`, the varnode-to-container map, and the per-instruction
  constant tracker.

## The tracked varnode set

A varnode is an SSA variable only if it is REGISTER or UNIQUE space; CONST
becomes a literal and RAM a `Load` / `Store`. The set is frozen before the lift
starts and comes from four places, because a register missing from it has no
`InitialVar` and a write to it fails the whole function:

- every REGISTER / UNIQUE varnode the decoded pcode names,
- the convention, above all the stack pointer, which a function may never name
  yet the stack analysis still needs, plus each `per_address_ccs` override's
  argument registers,
- each `CallOther`'s ABI footprint, whose implicit reads and writes appear in no
  pcode operand (x86-64 `syscall` reads `R10` and writes `R11`),
- every register a LOAD or STORE reaches through the REGISTER space by a
  computed address, which is likewise in no operand.

`FunctionBuilder::new` then folds the set into largest containers.

## Register aliasing

Reads and writes go through the largest *tracked* varnode containing the one
addressed, so `eax` and `rax` are one variable rather than two. A read shifts
its slice out of the container and truncates; a write reads the container,
positions the value at the slice's bit offset, clears that slot and ORs it back.
Masks are built in container coordinates, so an upper-half write (AArch64
`FCVT D0,S0`) clears the right half. Bit offsets follow the arch's
`register_endianness`.

A register slot holds an integer at the register's natural width: an `I1` flag
is zero-extended on the way in and a float is rejected, every float producer
having bitcast first. That is what makes a cross-region `Phi` over a register
type-homogeneous, and it is why the conditional-branch lifter narrows a flag
back to `I1` on the way out.

## A register-space LOAD or STORE is a register access

The REGISTER space is addressable, and a sla uses it when an instruction field
picks the register (ARM `vld1.N {dX[i]}`). When the address resolves against the
declared register file the access becomes an ordinary read or write of that
register, not a memory operation. When it does not resolve, the STORE lands in
the REGISTER space AND every tracked register is re-read out of it, so no
register keeps a value the write may have destroyed and the optimizer cannot
forward one across it. An unresolved LOAD reads the space, so two accesses at
the same address forward to each other instead of each yielding a fresh unknown.

## SSA construction

Cytron pruned SSA. `collect_def_sites` walks every region's pcode and records
what the lift will write; `iterated_frontier` turns that into the set of
variables needing a `Phi` at each region. `record_insn_defs` is a HAND-WRITTEN
mirror of the lift's write paths, not a shared code path: a def it records that
the lift never writes costs a dead phi, one the lift writes and it misses loses
the phi and miscompiles.

Renaming walks the dominator tree in pre-order: each region takes its immediate
dominator's finished variable map and overrides the variables carrying a phi of
their own, so a region is renamed only after the dominator whose values reach
it. The terminator handlers wire their own successors as they lift; the
fallthrough edges are wired afterwards, from each source region's final map.

Reading a variable no definition reaches is an error, not a default value. The
walk covers only the regions reachable from the entry, so an unreachable region
branching into one that carries phis fails the lift rather than seating
`ValueId(0)`, the `Entry` node's control output, as a phi operand.

## Lift-time canonicalisations

Each shape has exactly one form in the IR, so a pattern matches one thing:

    IntSub(a, b)         -> Add(a, Neg(b))
    IntNotEqual(a, b)    -> Xor(Equal(a, b), IntConst(1)):I1
    IntLessEqual(a, b)   -> Xor(Less(b, a), IntConst(1)):I1
    IntSlessEqual(a, b)  -> Xor(Sless(b, a), IntConst(1)):I1
    IntNeg(x)            -> Xor(x, all_ones)        (Sleigh IntNeg is ~x)
    BoolNeg(x)           -> Xor(x, IntConst(1)):I1
    FloatSub(a, b)       -> FloatAdd(a, Neg(b))
    FloatNotEqual(a, b)  -> Xor(FloatEqual(a, b), IntConst(1)):I1
    FloatNan(x)          -> Xor(FloatEqual(x, x), IntConst(1)):I1
    FloatLessEqual(a, b) -> Or(FloatLess(a, b), FloatEqual(a, b)):I1

Every one is exact. Subtraction wraps identically to adding the negation; the
integer negations are `Xor` with the all-ones constant of their width, which at
`I1` is 1; `x != x` is IEEE 754's definition of NaN. The float `<=` is the one
that does NOT follow the integer pattern: `Not(Less(b, a))` answers TRUE on a
NaN operand, where IEEE 754 requires false, and both `Less` and `Equal` answer
false on NaN, so their `Or` is right.

Two further rewrites are not canonicalisations but semantics. A shift count
wider than the output is SATURATED rather than truncated, since p-code tests the
full count against the output width and truncating `0x1_0000_0000` to `I32`
would turn an x86 SIMD shift-by-register into a no-op. A float result written to
a register is bitcast to an integer, registers holding no float type.

## Terminators

`Return` and a link-register `BranchIndirect` (ARM `bx lr`) both emit a
convention `Return` reading the CC's return registers through the aliasing path.
The CFG's dedicated terminators are lifted after a region's instruction loop,
from the region's last machine address: a resolved `Switch` with one control
output per arm and its selector kept live, a `TailCall` as `Call` + `Return`, and
an unresolved indirect branch as an `IndirectBranch` placeholder anchoring the
dispatch varnode plus, on ARM and MIPS, the ISA-mode bit the branch's
instruction committed, so a resolver can decode each target in the right mode.

A control cycle that never exits (`while (1)`, a spin loop, x86's `hlt`) roots
no terminator, so `retain_reachable` would drop its whole body and `validate`
rejects the shape. One `If(true) { back edge } { Unreachable }` is seated on each
such cycle to give it a root.

## Fail-closed

`Insert` / `Extract`, `PtrAdd` / `PtrSub`, `MultiEqual`, `SegmentOp` and
`Indirect` are all decompiler-internal: no SLEIGH production emits them, so each
is a named error rather than a guessed lowering. The opcode match is exhaustive
over `rsleigh::Opcode`, so a new opcode is a compile error here.

A width no `ValueType` carries, a `Subpiece` offset at or past its input's
width, a signed shift whose operand and output widths disagree, an ABI register
name outside the arch's table: each fails the whole function's lift rather
than producing an approximation.

Depends on `strider-cfg`, `strider-ir`, `strider-target`, `graph-algorithms`,
`vn-container`, `rsleigh`, `anyhow`, `petgraph` and `rustc-hash`.
