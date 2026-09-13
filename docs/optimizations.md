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
  fixed point). Each can expose work for the others. A pass whose last run
  changed nothing is skipped until another pass changes the graph. The loop is
  capped at 1024 iterations and raises if it has not settled by then.
- **Post-passes** run once, after the loop has settled. They mostly record facts,
  but two of them edit: `CallStackArgCollect` wires a call's stack arguments in
  as extra `Call` inputs, and `FunctionArgDetect` moves an argument load's
  memory edge past the memory definitions that cannot overwrite it.

## Rewriting passes

In the order the default pipeline runs them:

**ConstantFold.** Computes anything whose inputs are all constant, applies
algebraic identities (`x + 0`, `x * 1`, and so on), and folds constant
truncations and extensions.

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

**PhiCollapse.** A phi or memory phi whose inputs other than itself are all one
value is replaced by that value, so a loop-carried `[x, itself]` collapses to
`x`. A strongly connected group of phis whose only input from outside the group
is one value folds to that value too, such as two loop phis feeding each other
plus `x`.

**RegionCollapse.** A region with exactly one control input is removed, and each
phi or memory phi over it is replaced by its single value input.

**DeadBranchElimination.** An `if` on a constant condition, or a `switch` whose
dispatch address is a constant naming one of its arms, can only take one arm.
The branch is removed, and the arms no longer reached go with it. It declines
when the surviving arm would reach no terminator, as for a loop whose only exit
was the dead arm. It also declines an `if` whose dead arm feeds an `Unreachable`
directly. A dead arm that calls a no-return function still folds, since it
reaches its `Unreachable` through the call.

**CfgDetach.** Removes control edges into a merge region that can never be taken,
along with the matching phi and memory-phi inputs.

**LoadForward.** When a value is stored to some location (a stack slot, a
constant address, a heap object) and later loaded back from exactly that
location with nothing overwriting it in between, the stored value is handed
straight to the load. A wider store is narrowed to the load's range; anything
short of an exact base-and-offset match blocks, as does a control merge whose
paths reach different definitions. So does an intervening call, unless its
convention declares `preserves_memory`, which is what `cc.preserves_all()` and
`per_address_ccs={callee_addr: cc}` buy for a transparent hook such as
`__fentry__`.

Memory is taken to be RAM under every option: a load reads back the last value
the function stored there, so a memory-mapped register polled after a write
folds to the value written.

Each load it examines also has its memory edge moved past the memory
definitions that cannot overwrite it. That move uses none of the
`AssumptionOptions`, so re-optimising under `AssumptionOptions.none()` inherits
no assumption from an earlier run.

With `AssumptionOptions(escape_analysis=True)` it also forwards across a call,
when no stack address escapes the frame anywhere in the function (as a call
argument, a stored value, a return value, an input to pointer arithmetic, or a
value in a register a call clobbers) and the slot is not one the call hands its
callee as an argument. With `noalias_allocators=[addr, ...]` naming pure
allocators, a load steps through a call to one when it reads a different
allocation, or a slot of a frame no stack address escapes that lies outside
that call's argument window; a load from a global does not. Under either of
those two, and only then, `callee_preserves_stack_args=True` empties the
outgoing-argument window, so a spill at the stack top, indistinguishable from a
pushed argument once lowered to memory, forwards too. Set on its own it changes
nothing.

## Post-passes

In the order the default pipeline runs them:

**StackOffsetDetect.** For every load and store whose address reduces to a base
plus a constant, it records the pair. The base is a stack pointer, or the result
of a call to a `noalias_allocators` entry. Offsets are comparable only against
another access sharing the same base: an alignment-masked `sp & -16` is its own
base, since its distance from the entry stack pointer depends on the caller.
These pairs are what a query for stack accesses, or for one exact slot, reads.
A pipeline run records them after its post-passes even when this pass is not
in the pipeline.

**CallStackArgCollect.** At each call, gathers the stores into the outgoing
argument window that reach it and attaches them to the call node, so they read
like ordinary call arguments. Once lowered to memory an argument push is
indistinguishable from an incidental write to the same area, so the collection
errs wide: a spill just above the arguments can come along.

**FunctionArgDetect.** Finds where the function reads its incoming *stack-passed*
arguments (loads off the entry stack pointer) and records which value carries
each one. It also moves the memory edge of each load it examines in an
incoming-argument slot past the memory definitions that cannot overwrite it,
using none of the `AssumptionOptions`. Register-passed arguments are recorded
at lift time, so they are already in place before this pass runs.

## Indirect-branch resolution

Jump tables, computed calls and returns are resolved by their own post-pass,
`IndirectBranchClassify`, which `analyze` appends to the pipeline it runs unless
that pipeline already lists it (the Rust API can; `strider.opt` does not expose
it). After optimizing, Strider classifies each unresolved indirect branch
against the clean IR, feeds any newly discovered targets back in, and re-lifts,
repeating until the set of edges stops changing. Whatever still cannot be
resolved comes back as the `unresolved` list from `analyze`, never as an
exception. It is one of the six channels
[python-api.md](python-api.md#12-the-cfg-stridercfg) describes, which
`cfg.is_complete()` reads together.

A return instruction lands in `unresolved` too when its folded target is not
provably the address the function was entered with: the entry link register,
or the entry-SP slot the call pushed it into, followed back through the store
that saved it.

A resolved target carries the ISA mode it decodes in, taken from the mode the
branch commits or else the one flowing into it, so an ARM/Thumb interworking
dispatch or a MIPS16 entry reaches the right decoder. A seated `switch` keeps
the mode its instruction commits, so arms found when its table is derived again
decode in that mode too.

`CfgOptions(known_targets={dispatch_addr: [target, ...]})` seats answers of your
own, which the loop then grows from; a site that ends up holding nothing but
your seed is reported by `cfg.unverified_seeded_sites()`, since seating can stop
the classifier deriving. `LifterOptions(resolve_indirect_branches=False)` turns
the classifier off and leaves every site for you to answer.

### Dispatch shapes that do not resolve

These shapes come back in `unresolved` rather than as an error:

- AArch64 big-endian stack-array dispatch whose table base is built through a
  `bfi` insert. Sleigh's lowering of `bfi` masks the SP-derived value, and the
  stack decomposition cannot see through that mask. The table is also written
  by one 16-byte `str q0`, so it arrives as one 128-bit constant holding two
  entries.
- MIPS64 GOT-indirect dispatch: the entries lift as `Add(Load[GOT], const)`
  rather than constants, with the GOT pointer derived from `t9`.
- PowerPC stack-array dispatch on ppc32le, ppc64be and ppc64le, where the
  entries are loaded from `.data.rel.ro` in a writable segment, which the
  read-only image leaves out. On ppc64 the address is also relative to the
  incoming `r12`, an unknown register as in the MIPS64 shape.
- A table index formed by a constant offset that takes a bounded value below
  zero, such as `(x & 7) - 2`. Enumerating `x` would name slots before the
  table that only the guards on the offset value exclude, so the site is left
  unresolved.
- A dispatch in a loop whose arms call a function that clobbers the register
  holding the table base. The arms are seated, but once the loop closes the
  selector no longer derives, so nothing re-proves the set. Overriding the
  callee with `per_address_ccs={callee: cc.preserves_regs()}` resolves it.

## Using a different pipeline

Build a custom set of passes with the `strider.opt` builders and pass it through
`LifterOptions(pipeline=...)`.
