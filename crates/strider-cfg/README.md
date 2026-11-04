# strider-cfg

Decodes one function's bytes into a control-flow graph of straight-line p-code
regions through GHIDRA's Sleigh (`rsleigh`). IR-free: `strider-lift` builds the
IR from a `Cfg`.

## What's here

- `DecodeEngine::new(arch, reader)?.build_cfg(entry, &opts)` -> `Cfg`. The
  engine owns the arch's Sleigh and its decode context, so every build decodes
  as on a fresh engine. On an arch with an ISA mode the entry's low bit selects
  it (Thumb, MIPS16e).
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

A target an existing region owns splits it at the target's instruction or
p-code boundary, rather than decoding those bytes again. Incoming edges move to
the first half, which keeps the start; the second keeps the region id and its
outgoing edges. So the first decode fixes the ISA mode of those bytes, and a
later edge in another mode is reported, not decoded twice.

The one exception is overlapping code: sequential decoding can step over a
region start inside an instruction it decodes, leaving those bytes with two
owners. The stepped-over start is reported as `InteriorTarget`.

Decoding within a region is sequential: `Sleigh::lift_one` carries context
(ARM/Thumb and IT state, MIPS16, ppc `linkreg`) that an instruction can commit
at a later address. That context flows as in GHIDRA's `ContextDatabase`: a
committed value holds until the next change point. Each queued edge captures
the context reaching its target and the builder restores it before decoding
there, so a seated indirect target or a backward edge decodes in the mode that
reaches it.

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
`UnverifiedSeed`. Target reasons:

- `UnmappedTarget`: a direct branch or fall-through to bytes the reader lacks.
  It leaves through a `TailCall` stub. An unmapped seeded arm is dropped; an
  unmapped entry fails the build.
- `UndecodableTarget`: a direct branch to bytes Sleigh rejects or
  `data_ranges` marks, or a fall-through into them past a call, directly or
  through a run of zero-p-code-op instructions. A branch or a nop run leaves
  through a stub; a call ends `NoReturn`, dropping what follows it. Any
  fall-through into a data range is reported; any other into bytes Sleigh
  rejects fails the build. The entry decodes even inside a data range.
- `PastFunctionEnd`: a fall-through, not-taken path or conditional
  instruction's skip of itself past a `FunctionBound::Size` end, at the first
  address after the last decoded instruction. It leaves through a stub; a call
  returning there ends `NoReturn`. Off a seeded arm the arm is dropped.
- `IsaModeConflict`: two edges reached an address in different ISA modes; the
  loser decodes in the winner's. Other committed vars are not compared.
- `InteriorTarget`: an off-boundary target, and a stepped-over region start.

Beside it: `undecodable_seeded_targets`, each arm dropped because it or code
it leads to (up to the next dispatch) would not decode, with its site; and
`link_register_seated` / `tail_call_seated`, live sites a seat consumed.
