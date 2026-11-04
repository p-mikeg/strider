# strider-cfg

Decodes one function's bytes into a control-flow graph of straight-line p-code
regions through GHIDRA's Sleigh (`rsleigh`). IR-free: `strider-lift` builds the
IR from a `Cfg`.

## API

- `DecodeEngine::new(arch, reader)?.build_cfg(entry, &opts)` -> `Cfg`. Every
  build decodes as on a fresh engine. On an arch with an ISA mode the entry's
  low bit selects it (Thumb, MIPS16e).
- `Region`: maximal straight-line p-code entered only at `start_addr`, ending
  at its `RegionTerminator`. It can end mid-instruction; `contains_addr` owns
  bytes down to the p-code index.
- `CfgOptions<'a>`: the function `bound` plus `fragment_ranges` (cold
  partitions also inside the function), borrowed tables (`known_targets`,
  `derived_targets` which win at their exact key, `no_return_targets`,
  `call_other_overrides`, `local_entries`) and `data_ranges`, never decoded.
- Queries on `Cfg` (`regions`, `region_containing`, `region_predecessors`,
  `region_if`, `switch_arm_regions`, ...), Graphviz output (`dot_dumper`,
  `neighborhood_dot`), and `insn_text` / `insn_plain_text` for one p-code op.

## Region ownership

A target inside an existing region splits it at the target's instruction or
p-code boundary instead of decoding those bytes again. Incoming edges move to
the first half, which keeps the start; the second keeps the region id and its
outgoing edges. The first decode fixes the ISA mode of those bytes; a later
edge in another mode is reported, not decoded twice.

Overlapping code is the exception: sequential decoding can step over a region
start inside an instruction, leaving those bytes two owners. The stepped-over
start is reported as `InteriorTarget`.

A region decodes sequentially: `Sleigh::lift_one` carries context (ARM/Thumb
and IT state, MIPS16, ppc `linkreg`) an instruction can commit at a later
address. As in GHIDRA's `ContextDatabase`, a committed value holds until the
next change point. Each queued edge captures the context reaching its target
and the builder restores it there, so a seated indirect target or a backward
edge decodes in the mode that reaches it.

## Terminators

`Unconditional` (a branch, a fall-through into an existing region, a
zero-p-code-op instruction, the first half of a split, or a fall-through onto
a `TailCall` stub), `CondBranch`, `Return`, `NoReturn` (a no-return trap or
callee, or a call whose fall-through holds no code or lies past the bound),
`TailCall`, `Switch` and `UnresolvedIndirectBranch`. Only `Unconditional`,
`CondBranch` and `Switch` have outgoing edges.

A `BranchIndirect` is seated from `derived_targets` / `known_targets` or
deferred as `UnresolvedIndirectBranch` for a resolver above this crate.
`LinkRegister` seats a `Return`, an out-of-range `Single` a `TailCall`, and
anything else a `Switch`, so a later round can widen the site.

A target inside a region but off every p-code boundary cannot be split out
(overlapping code, or a table entry read past its end). A direct branch to one
edges to the owning region. A table with such an arm, or an arm out of the
bound, defers whole. An arm found off-boundary after the seal, or decoded in
the other ISA mode by a direct edge, is dropped alone; a `Switch` left with no
arm defers. Direct edges are explored before any arm, so a direct decode wins
an ISA-mode clash with an arm.

## Incompleteness

`Cfg::incomplete()` is a sorted set of `(PcodeInsnAddr, Reason)`. A build
resolves nothing, so every deferred site is `UnresolvedBranch` and every seat
`UnverifiedSeed`. The target reasons are `UnmappedTarget`,
`UndecodableTarget`, `PastFunctionEnd`, `IsaModeConflict` and
`InteriorTarget`; each one's contract is on `Reason`. A direct branch to
unmapped or undecodable bytes leaves through a `TailCall` stub, a call
falling into them ends `NoReturn`, and a seeded arm reaching them is dropped.
An unmapped entry, or a fall-through into bytes Sleigh rejects that no call
explains, fails the build.

Beside it: `undecodable_seeded_targets` (each arm dropped because it, or code
it leads to before the next dispatch, would not decode, with its site) and
`link_register_seated` / `tail_call_seated` (live sites a seat consumed).
