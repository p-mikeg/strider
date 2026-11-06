# strider-cfg

Decodes one function's bytes into a control-flow graph of straight-line p-code
regions through GHIDRA's Sleigh (`rsleigh`). IR-free: `strider-lift` builds the
IR from a `Cfg`.

## API

- `DecodeEngine::new(arch, reader)?.build_cfg(entry, &opts)` -> `Cfg`. Every
  build decodes as on a fresh engine. On an arch with an ISA mode the entry's
  low bit selects it (Thumb, MIPS16e).
- `Region`: a straight-line run of p-code from `start_addr`, ending at its
  `RegionTerminator`, not necessarily maximal. It can end mid-instruction.
- `CfgOptions<'a>`: the function `bound` and `fragment_ranges`, borrowed
  target tables (`known_targets`, `derived_targets`, `no_return_targets`,
  `call_other_overrides`, `local_entries`), and `data_ranges`, bytes never
  decoded.
- Queries on `Cfg` (`regions`, `region_containing`, `region_if`,
  `switch_arm_regions`, ...), Graphviz output (`dot_dumper`,
  `neighborhood_dot`), and `insn_text` for one p-code op.

## Decoding

A target inside an existing region splits it at the target's instruction or
p-code boundary rather than decoding those bytes again. The first decode fixes
the ISA mode of those bytes; a later edge in another mode is reported. A
target off every boundary (overlapping code) cannot be split out and is
reported as `InteriorTarget`.

A region decodes sequentially, because `Sleigh::lift_one` carries context
(ARM/Thumb and IT state, MIPS16, ppc `linkreg`) that an instruction can commit
at a later address. Each queued edge carries the context reaching its target,
so a seated indirect target or a backward edge decodes in the right mode.

A `BranchIndirect` is seated from `derived_targets` / `known_targets` or
deferred as `UnresolvedIndirectBranch` for a resolver above this crate. A
table with an arm off a boundary or out of the bound defers whole. An arm
found off-boundary after the seal, or lost to a direct edge's ISA mode, is
dropped alone; a `Switch` left with no arm defers. Direct edges are explored
before any arm, so a direct decode wins an ISA-mode clash.

## Incompleteness

`Cfg::incomplete()` is a sorted set of `(PcodeInsnAddr, Reason)`; each
reason's contract is on `Reason`. A build resolves nothing, so every deferred
site is `UnresolvedBranch` and every seat `UnverifiedSeed`. An unmapped entry,
or a fall-through into bytes Sleigh rejects that no call explains, fails the
build.
