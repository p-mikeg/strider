# strider-lift

Turns one function's `strider_cfg::Cfg` into a `strider_ir::Function`: a
semi-pruned SSA sea-of-nodes graph with one IR `Region` per CFG region and every
machine register modelled as an SSA variable.

## API

- `Lifter<R>`: wraps a `strider_cfg::DecodeEngine<R>`, reused across functions
  and rebuild rounds. `build_cfg(entry, cfg_opts)` decodes;
  `build_ir(cfg, cc)` / `build_ir_with(cfg, cc, cfg_opts, per_address_ccs,
  claims, entry_values)` lift; `entry_values` binds registers to known
  entry values (ppc64 ELFv1 `r2`).
- `LiftOutcome`: the `Function` and its `LiftSites` (`unresolved_branches`,
  `switch_anchors`, `return_sites`).

## The tracked varnode set

Only REGISTER and UNIQUE varnodes are SSA variables; CONST becomes a literal and
RAM a `Load` / `Store`. The set is frozen before the lift, and a write to a
register outside it fails the function. It holds:

- every REGISTER / UNIQUE varnode the pcode names,
- the convention's stack pointer, argument and return registers, plus the
  argument registers of the `per_address_ccs` overrides this CFG's direct
  calls select,
- each `CallOther`'s ABI footprint (x86-64 `syscall` reads `R10`, writes `R11`),
- the declared register enclosing each resolved register-space LOAD / STORE
  address,
- each input-less rounding-mode op's register, unless
  `fp_rounding_mode_is_nearest`.

`Function::new` folds the set into largest containers and sorts it.

## Register aliasing

Reads and writes go through the largest tracked container, so `eax` and `rax`
are one variable: a read shifts and truncates, a write clears the slice in the
container and ORs the value in. Bit offsets follow `register_endianness`.

A register holds an integer at its own width: an `I1` is zero-extended on write
and floats are bitcast first, so a register `Phi` is type-homogeneous. A
conditional branch tests a flag `!= 0` at its slot width.

## Register-space LOAD / STORE

A sla addresses the REGISTER space when an instruction field picks the register
(ARM `vld1.N {dX[i]}`). An address that resolves to a declared register is an
ordinary register read or write. Otherwise every tracked register is first
stored into its slot; an unresolved STORE is then followed by re-reading every
tracked register from the space, and an unresolved LOAD reads the space. The
space uses `register_endianness` (`Function::byte_order`).

## SSA construction

Semi-pruned: phis at the iterated dominance frontier of each variable's def
sites, less UNIQUE temporaries no region reads before writing. The def sites
come from `record_insn_effects`, a hand-written mirror of the lift's write
paths: a spurious def costs a dead phi, a missed one miscompiles.

Regions are renamed in dominator-tree pre-order, each starting from its
immediate dominator's variable map. Reading a variable no definition reaches
is an error.

## Lift-time canonicalisations

    IntSub(a, b)         -> Add(a, Neg(b))
    IntNotEqual(a, b)    -> Xor(Equal(a, b), IntConst(1)):I1
    IntLessEqual(a, b)   -> Xor(Less(b, a), IntConst(1)):I1
    IntSlessEqual(a, b)  -> Xor(Sless(b, a), IntConst(1)):I1
    IntNeg(x)            -> Xor(x, all_ones)        (Sleigh IntNeg is ~x)
    BoolNeg(x)           -> Xor(x, IntConst(1)):I1
    FloatSub(a, b)       -> FloatAdd(a, Neg(b))     (a NaN b's sign flips)
    FloatNotEqual(a, b)  -> Xor(FloatEqual(a, b), IntConst(1)):I1
    FloatNan(x)          -> Xor(FloatEqual(x, x), IntConst(1)):I1
    FloatLessEqual(a, b) -> Or(FloatLess(a, b), FloatEqual(a, b)):I1

All are exact but `FloatSub` on a NaN subtrahend, whose sign IEEE 754 leaves
unspecified. A shift count wider than the output saturates rather than
truncating (p-code compares the full count). BOOL ops read their input's low
bit (p-code: the whole byte); every vendored spec feeds them 0/1.

## Terminators

`Return` and a link-register `BranchIndirect` (ARM `pop {pc}`) emit a convention
`Return`. A resolved `Switch` gets one control output per arm and keeps its
selector live; a `TailCall` becomes `Call` + `Return`; an unresolved indirect
branch becomes an `IndirectBranch` anchoring the dispatch varnode and, on ARM
and MIPS, the ISA-mode bit, so a resolver decodes each target in the right mode.

A control cycle with no exit reaches no terminator, so an
`If(true) { back edge } { Unreachable }` is seated on each to keep it valid.

## Fail-closed

Decompiler-internal opcodes (`Insert`, `Extract`, `PtrAdd`, `PtrSub`,
`MultiEqual`, `SegmentOp`, `Indirect`) are named errors; the opcode match is
exhaustive. An unmapped width, a `Subpiece` offset at or past its input's
width, mismatched signed-shift widths or an unknown ABI register name fails the
function's lift.
