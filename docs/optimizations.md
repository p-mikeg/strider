# Optimizations

Before you query a function, Strider optimizes its IR for regularity rather than
for speed: clutter folds away and equivalent code is rewritten into one agreed
shape, so a single pattern matches every case instead of a dozen near-misses.

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
truncations and extensions. Most other passes depend on it having run.

**LoadReadOnly.** When a load reads a fixed address that lands in read-only
memory, it replaces the load with the constant stored there. `load_elf` derives
that image for you (the loaded file minus its writable mappings), so a load out
of an RWX segment is fetched from but never folded. For a raw blob you pass one
in as `rom=` (see the ROM reader in the [Python guide](python-guide.md)).

An image whose only `PT_LOAD` is RWX, which is how a Linux `vmlinux` is normally
laid out, therefore has no read-only part at all: every load fails to fold and
the pass goes quiet rather than reporting anything. Check `prog.rom()`, whose
repr counts its regions, when folding you expected does not happen; the fix is
to hand in a `rom=` covering the constant data yourself.

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
short of an exact base-and-offset match blocks, as does an intervening call or
control merge.

With `AssumptionOptions(escape_analysis=True)` it also forwards across a call,
when no stack address escapes to the callee and the slot is not one the call
hands it as an argument. With `noalias_allocators=[addr, ...]` naming pure
allocators, a load also steps through such a call, whose result is a fresh
object disjoint from everything else. `callee_preserves_stack_args=True` empties
the outgoing-argument window, so a spill at the stack top, indistinguishable
from a pushed argument once lowered to memory, forwards too.

## Post-passes

These run once on the settled graph. They record facts on it, and two of
them also edit it:

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
`IndirectBranchClassify`, which `analyze` appends to whatever pipeline it runs
(the Rust API can list it itself; `strider.opt` does not expose it).
After optimizing, Strider classifies each unresolved
indirect branch against the clean IR, feeds any newly discovered targets back in,
and re-lifts, repeating until the set of edges stops changing. Whatever still
cannot be resolved comes back as the `unresolved` list from `analyze`, never as
an exception. That list is one of the four channels a converged CFG reports its
own incompleteness through; see
[python-api.md](python-api.md#12-the-cfg-stridercfg).

A resolved target carries the ISA mode it decodes in, taken from the mode the
branch commits or else the one flowing into it, so an ARM/Thumb interworking
dispatch or a MIPS16 entry reaches the right decoder.
`CfgOptions(known_targets={dispatch_addr: [target, ...]})` seats answers of your
own, which the loop then grows from; a site that ends up holding nothing but
your seed is reported by `cfg.unverified_seeded_sites()`, since seating can stop
the classifier deriving. `LifterOptions(resolve_indirect_branches=False)` turns
the classifier off and leaves every site for you to answer.

## Using a different pipeline

Build a custom set of passes with the `strider.opt` builders and pass it through
`LifterOptions(pipeline=...)`. `analyze` appends `IndirectBranchClassify` to
whatever you build, so `resolve_indirect_branches=False` is how you leave it
out.
