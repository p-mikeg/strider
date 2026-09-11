# Optimizations

Before you query a function, Strider optimizes its IR for regularity rather than
for speed: clutter folds away and equivalent code is rewritten into one agreed
shape, so a single pattern matches every case instead of each near-miss.

`analyze` runs the default set for you. The passes are worth knowing because when
a pattern does not match, it is usually because one of them already reshaped what
you were looking for.

## How the passes run

There are two kinds:

- **Rewriting passes** run together in a loop until nothing changes anymore (a
  fixed point). Each can expose work for the others. The loop is capped at 1024
  iterations and raises if it has not settled by then.
- **Post-passes** run once, after the loop has settled. They mostly record facts,
  but two of them edit: `CallStackArgCollect` wires a call's stack arguments in
  as extra `Call` inputs, and `FunctionArgDetect` shortens a load's memory edge
  onto the store it proved reaches it.

## Rewriting passes

In the order the default pipeline runs them:

**ConstantFold.** Computes anything whose inputs are all constant, applies
algebraic identities (`x + 0`, `x * 1`, and so on), and folds constant
truncations and extensions. `LoadReadOnly`, `FlagCmpCanonicalize` and
`IfCondInversion` are ordered after it and say so.

It also collects a value against itself: `x + x*2` becomes `x*3`, and the same
for a shift standing in for a multiply, so `x + (x<<1)` folds too. Thirteen
pairings of add / subtract with multiply / shift reduce to one `x * K`, which
means a pattern written against the shape in the source will not match. Search
for the folded form, or for `int_mul(x, int_const_any_width(K))`.

**LoadReadOnly.** When a load reads a fixed address that lands in read-only
memory, it replaces the load with the constant stored there. `load_elf` derives
that image for you (the loaded file minus its writable mappings), so a load out
of an RWX segment is fetched from but never folded. For a raw blob you pass one
in as `rom=` (see the ROM reader in the [Python guide](python-guide.md)).

An image whose only `PT_LOAD` is RWX therefore has no read-only part at all:
every load fails to fold and the pass goes quiet rather than reporting anything.
Check `prog.rom()`, whose repr counts its regions, when folding you expected does
not happen; the fix is to hand in a `rom=` covering the constant data yourself.

**KnownBits.** Tracks which bits of each value are known to be 0 or 1. When every
bit of a result is pinned down, it becomes a constant.

**FlagCmpCanonicalize.** CPUs compute a comparison as a little tree of flag bits
(carry, zero, sign, overflow). This recognizes those trees, including the shapes
left after a negated branch is normalized, and rewrites them back into a plain
comparison like `a < b`. The signed relations need the overflow bit: `a < b` is
`sign != overflow`.

**IfCondInversion.** Makes every `if` test a non-negated condition. When the
condition is a logical NOT, it drops the NOT and swaps the two branches. So you
only ever have to match the positive form.

**PhiCollapse.** A phi whose incoming paths all carry the same value, or that has
only one live path, is not a real choice. It is replaced by that single value. A
phi that refers to itself counts as agreeing, so a loop-carried `[x, itself]`
collapses to `x`.

**RegionCollapse.** Likewise a region (or memory phi) with a single live
predecessor is not a real merge, so it is removed and folded away.

**DeadBranchElimination.** When an `if`'s or `switch`'s selector is a constant,
only one arm can ever run. The branch is removed and the unreachable arms go with
it. It declines when folding would strand a loop with no path to a terminator,
which would leave the loop body unanchored.

**CfgDetach.** Removes control edges into a merge region that can never be taken,
along with the matching phi and memory-phi inputs.

**LoadForward.** When a value is stored to some location (a stack slot, a
constant address, a heap object) and later loaded back from exactly that
location with nothing overwriting it in between, the stored value is handed
straight to the load. A wider store is narrowed to the load's range; anything
short of an exact base-and-offset match blocks, as does an intervening control
merge. So does an intervening call, unless its convention declares
`preserves_memory`, which is what `cc.preserves_all()` and
`per_address_ccs={callee_addr: cc}` buy for a transparent hook such as
`__fentry__`.

Each load walks the chain from its own cursor and the memo is keyed on the
probed location, so loads at different offsets share nothing, but that does not
compound: the per-function address-decomposition memos, and `narrow_load_to`
shortening each load's memory edge onto its clobber for good, hold the marginal
cost of one more load flat as the chain grows. Measured in `--release` on the
workspace's own bench shape, N SP-relative stores at distinct offsets read back
by N loads (`crates/strider-orchestrator/benches/scaling.rs`), the pass grows
about 2.0x per doubling of N, the same as every other pass, and costs roughly a
27th of what `ConstantFold` costs on that same shape. That is synthetic IR
timed on one machine, but the exponent holds across three shape variants and a
512x range of N.

With `AssumptionOptions(escape_analysis=True)` it also forwards across a call,
when no stack address escapes to the callee and the slot is not one the call
hands it as an argument. With `noalias_allocators=[addr, ...]` naming pure
allocators, a load also steps through such a call, whose result is a fresh
object disjoint from everything else. Under either of those two, and only then,
`callee_preserves_stack_args=True` empties the outgoing-argument window, so a
spill at the stack top, indistinguishable from a pushed argument once lowered
to memory, forwards too. Set on its own it changes nothing.

## Post-passes

In the order the default pipeline runs them:

**StackOffsetDetect.** For every load and store whose address reduces to a stack
terminal plus a fixed amount, it records the pair. Offsets are comparable only
against another access sharing the same terminal: an alignment-masked `sp & -16`
is its own base, since its distance from the entry stack pointer depends on the
caller. This is what lets a query ask for stack accesses, or for one exact slot.

**CallStackArgCollect.** At each call, gathers the stores into the outgoing
argument window that reach it and attaches them to the call node, so they read
like ordinary call arguments. Once lowered to memory an argument push is
indistinguishable from an incidental write to the same area, so the collection
errs wide: a spill just above the arguments can come along.

**FunctionArgDetect.** Finds where the function reads its incoming *stack-passed*
arguments (loads off the entry stack pointer) and records which value carries
each one. It also shortens each such load's memory edge onto the store it
proved reaches it. Register-passed arguments are recorded at lift time, so they
are already in place before this pass runs.

## Indirect-branch resolution

Jump tables, computed calls and returns are resolved by their own post-pass,
`IndirectBranchClassify`, which `analyze` appends to the pipeline it runs unless
that pipeline already lists it (the Rust API can; `strider.opt` does not expose
it). After optimizing, Strider classifies each unresolved
indirect branch against the clean IR, feeds any newly discovered targets back in,
and re-lifts, repeating until the set of edges stops changing. Whatever still
cannot be resolved comes back as the `unresolved` list from `analyze`, never as
an exception. It is one of the five channels
[python-api.md](python-api.md#12-the-cfg-stridercfg) describes, which
`cfg.is_complete()` reads together.

A resolved target carries the ISA mode it decodes in, taken from the mode the
branch commits or else the one flowing into it, so an ARM/Thumb interworking
dispatch or a MIPS16 entry reaches the right decoder.
`CfgOptions(known_targets={dispatch_addr: [target, ...]})` seats answers of your
own, which the loop then grows from; a site that ends up holding nothing but
your seed is reported by `cfg.unverified_seeded_sites()`, since seating can stop
the classifier deriving. `LifterOptions(resolve_indirect_branches=False)` turns
the classifier off and leaves every site for you to answer.

### Dispatch shapes that do not resolve

Four shapes come back in `unresolved` rather than as an error:

- AArch64 big-endian stack-array dispatch built through a `bfi` insert. The
  frame and the table base are plain (`sub sp,sp,#0x30`, `add x8,sp,#0x10`);
  the mask the SP decomposition cannot spell out is the one Sleigh's lowering
  of `bfi` puts on the SP-derived value the insert itself makes. The table is a
  single 16-byte `str q0`, so it also arrives as one `IntConst:I128` that the
  classifier would have to slice into two entries.
- MIPS64 GOT-indirect dispatch, where the entries lift as
  `Add(Load[GOT], const)` rather than a raw constant, the GOT pointer derived
  from `t9` (`$25`) rather than read out of `gp`.
- PowerPC stack-array dispatch on ppc32le, ppc64be and ppc64le. The stack base
  lifts cleanly on all three; it is the entries that do not fold. On ppc32le
  they are `Load(RAM, 0x100201FC)` and `Load(RAM, 0x10020200)`, addresses in
  `.data.rel.ro` inside a writable `PT_LOAD`, which the read-only image rejects,
  so `LoadReadOnly` leaves them alone. On ppc64be and ppc64le, whose IR is
  identical despite one being clang and one gcc, they are `Load(RAM, r12 + K)`
  off the ELFv2 TOC prologue `addis r2,r12,2`: an unknown incoming register
  plus the same writable `.data.rel.ro` / `.got`, which is the MIPS64 shape
  again. ppc32be resolves.
- MIPS32 `switch_masked_loop`, where the six real arms ARE seated but the site
  is still reported. The arms' `Call` clobbers the register holding the table
  base, so the selector stops deriving once the loop closes and nothing
  re-proves the seated set; the mode one arm was seated on stays a guess, so
  the site reaches `unresolved` and `unverified_seeded_sites` at once. A
  per-`Call` `preserves_regs` override supplies the callee's real clobber set.

## Using a different pipeline

Build a custom set of passes with the `strider.opt` builders and pass it through
`LifterOptions(pipeline=...)`. `analyze` appends `IndirectBranchClassify` unless
the pipeline already lists it, which `strider.opt` gives you no way to do;
`resolve_indirect_branches=False` turns it off rather than leaving it out, and
it still records its report.
