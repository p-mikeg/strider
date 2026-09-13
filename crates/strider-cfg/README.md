# strider-cfg

Decodes one function's bytes into a control-flow graph of straight-line regions,
through GHIDRA's Sleigh (`rsleigh`). IR-free: a `Cfg` is a graph of pcode
instruction runs plus a terminator per run, and knows nothing of the
sea-of-nodes IR `strider-lift` builds from it.

## What's here

- `Builder::for_arch(arch, sleigh, start_addr, opts).build()` -> `Cfg`. A work
  queue seeded with the entry drives decoding: each item either decodes a new
  region or routes an edge to an existing one. `with_flow_context` lends the
  sla's flowing context vars plus the function's own decode mode;
  `with_per_address_ccs` supplies calling conventions for call targets.
- `Region`, the graph's node: maximal straight-line pcode entered only at
  `start_addr`, ending at its `RegionTerminator`. `contains_addr` decides which
  bytes it owns, down to the pcode index, since a region can end mid-pcode
  sequence.
- `CfgOptions`: `fn_max_size` / `allow_code_before_start_addr` bound the
  function, `known_targets` seats indirect-branch answers a caller derived, and
  `call_other_overrides` classifies user-op names.
- `FlowVars` / `FlowContext`: the sla's flowing decode context, discovered once
  per sla and lent to every build.
- Queries on `Cfg` (`regions`, `region_ids`, `region_predecessors`,
  `region_if`, `switch_arm_regions`) and dark-themed Graphviz output
  (`dot_dumper`, `neighborhood_dot`).
- `insn_text` / `insn_plain_text`: one decoded p-code op as text, a LOAD /
  STORE space id named by its space (`Cfg::space_ids`) rather than printed as
  the host address Sleigh encodes it as.

## Every address decodes once

Region ownership is by address. A target an existing region owns routes to that
region, splitting it in two when the target is one of its instruction
boundaries; the second half keeps the region id so existing edges need no
fixup. Those bytes are never decoded again. The first edge to arrive
therefore fixes the ISA mode the bytes decode in; a later edge carrying a
different mode is recorded rather than decoding a second copy of them.

Overlapping code is the exception, and the only one. Sequential decoding steps
over a region start interior to an instruction it decodes, the fall-through
check being by exact start address, and those bytes then have two owners with
two different instruction streams. The stepped-over start is reported on
`interior_branch_targets`.

Within a region decoding is strictly sequential. `Sleigh::lift_one` takes
`&mut self` and carries context-register state (ARM/Thumb mode, x86 operand and
address size, MIPS16) that a decoded instruction can itself set, so lifting out
of order returns the wrong instructions.

That state is *flowing* context: as in GHIDRA's `ContextDatabase` a flowing var
is a value committed per address, holding forward until the next change point.
Each queued edge captures the context reaching its target and `FlowVars::pin_at`
commits it back before that target decodes, so a region Sleigh never
straight-line-flowed into (a caller-seated indirect target, a backward edge)
still decodes in the mode that reaches it. An interworking branch bakes its own
ISA-mode bit into the context it hands the target.

## How a region ends

`Unconditional` (an explicit branch, a fall-through into an already-discovered
region, a zero-pcode-op instruction such as `nop` or `endbr64`, or the first
half of a split), `CondBranch` carrying the taken target, `Return`, `NoReturn`
(a CallOther classified as a `BUG()`-class trap, a call whose target has a
no-return calling convention, or a call whose return address is out of range),
`TailCall` carrying the callee, `Switch` carrying a jump table's arms and the
dispatch address, and `UnresolvedIndirectBranch`. Only `Unconditional`, `CondBranch` and `Switch`
ever have an outgoing edge.

Classifying a `BranchIndirect` needs the IR, which sits above this crate, so a
site is either seated from `known_targets` or deferred as
`UnresolvedIndirectBranch`. A seat maps `LinkRegister` to `Return`, an
out-of-range `Single` to `TailCall`, and anything else to a `Switch`, an
in-function `Single` included, so a later round can widen the site instead of
latching its first answer. A deferral keeps the `target_vn` and
address anchor the value a resolver inspects before feeding the answer back.

A target interior to a region but off every pcode boundary is neither: no split
can express it. It comes from overlapping code, or from a jump-table entry read
past the end of the table. For a direct branch the edge is wired to the region
that owns those bytes, whose stream starts earlier, so the arm is not the stream
the branch jumps to.

A table's arms are judged at the seal. One arm off every boundary, or one out of
the function bound, and the WHOLE site defers as `UnresolvedIndirectBranch`: a
bad arm is evidence the bound the classifier derived is wrong, so the rest of
the answer is worth no more than that arm. Dropping arms individually instead
costs functions outright, seating the rest to decode whatever else the bound
over-approximated into. Only an arm the site learns about
after it was seated is dropped on its own, and a `Switch` left with no arm
degrades to `UnresolvedIndirectBranch` too. So does one whose bytes turn out to
be decoded in the other ISA mode: a direct edge proves the mode it carries, a
classifier-committed `isa_bit` only claims one, so the direct decode keeps the
bytes and the arm goes.

## Nothing is dropped silently

A completed `Cfg` may be incomplete, and says so through six getters. Read them
all: they overlap in cause and not in content.

- `undecodable_seeded_targets`: a seeded target that would not decode, with the
  dispatch site it was an arm of. A seeded target is a misclassification, so its
  edge is dropped and reported.
- `unmapped_branch_targets`: a DIRECT branch to an address the reader has no
  bytes for. It leaves through an empty `TailCall` stub, so the regions that did
  decode survive. An unmapped ENTRY is still an `Err` on the whole build.
- `isa_mode_conflicts`: two edges reached an address carrying different ISA
  modes, and the losing path decodes in the winner's. A direct edge always
  wins; between two direct ones the winner is work-queue order, so neither
  decode can be trusted.
- `interior_branch_targets`: the off-boundary targets above, whose edge is
  inexact (a direct branch) or absent (a dropped `Switch` arm), plus a region
  start a later decode stepped over.
- `link_register_seated` and `tail_call_seated`: sites a seated answer consumed
  at build time. No placeholder and no `Switch` anchor survives, so nothing
  later can tell a genuine `bx lr` or tail call from a dispatch collapsed to its
  first derived answer.

Depends only on `strider-target`, `dot`, `rsleigh`, `anyhow`, `petgraph` and
`rustc-hash`.
