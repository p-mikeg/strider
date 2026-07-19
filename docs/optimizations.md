# Optimizations

Before you query a function, Strider optimizes its IR. The point is not speed:
it is regularity. Optimization folds away clutter and rewrites equivalent code
into one agreed shape, so a single pattern matches every case instead of a dozen
near-misses. This guide lists what each pass does, in plain terms.

You rarely call these yourself. `analyze` runs the default set for you. They are
worth knowing because when a pattern does not match, it is usually because a pass
already reshaped what you were looking for.

## How the passes run

There are two kinds:

- **Rewriting passes** run together in a loop until nothing changes anymore (a
  fixed point). Each can expose work for the others, so they keep going round
  until the graph settles.
- **Post-passes** run once, after the loop has settled. They annotate the final
  graph rather than reshape it.

## Rewriting passes

In the order the default pipeline runs them:

**ConstantFold.** Computes anything whose inputs are all constant, applies
algebraic identities (`x + 0`, `x * 1`, and so on), and folds constant
truncations and extensions. The workhorse: most simplification traces back here.

**LoadReadOnly.** When a load reads from a fixed address that lands in read-only
memory you supplied (see the ROM reader in the [Python guide](python-guide.md)),
it replaces the load with the constant value stored there.

**KnownBits.** Tracks which bits of each value are known to be 0 or 1. When every
bit of a result is pinned down, it becomes a constant.

**FlagCmpCanonicalize.** CPUs compute a comparison as a little tree of flag bits
(carry, zero, sign). This recognizes those trees, including the shapes left after
a negated branch is normalized, and rewrites them back into a plain comparison
like `a < b`.

**IfCondInversion.** Makes every `if` test a non-negated condition. When the
condition is a logical NOT, it drops the NOT and swaps the two branches. So you
only ever have to match the positive form.

**PhiCollapse.** A phi whose incoming paths all carry the same value, or that has
only one live path, is not a real choice. It is replaced by that single value.

**RegionCollapse.** Likewise a region (or memory phi) with a single live
predecessor is not a real merge, so it is removed and folded away.

**DeadBranchElimination.** When an `if`'s condition is a constant, only one arm
can ever run. The branch is removed and the unreachable arm goes with it.

**CfgDetach.** Removes control edges into a merge region that can never be taken,
along with the matching phi and memory-phi inputs.

**LoadForward.** When a value is stored to a stack slot and later loaded back
from the same slot with nothing overwriting it in between, the stored value is
handed straight to the load.

## Post-passes

These run once on the settled graph and record facts on it:

**StackOffsetDetect.** For every load and store whose address works out to "stack
pointer plus a fixed amount", it records that amount. This is what lets a query
ask for stack accesses, or for one exact slot.

**CallStackArgCollect.** At each call, gathers the arguments that were pushed onto
the stack beforehand and attaches them to the call node, so they read like
ordinary call arguments.

**FunctionArgDetect.** Finds where the function reads its own incoming arguments
(registers at entry, stack slots via loads) and records which value carries each
one.

## Indirect-branch resolution

Jump tables, computed calls, and returns are not resolved by the passes above.
They are handled separately: after optimizing, Strider classifies each unresolved
indirect branch against the clean IR, feeds any newly discovered targets back in,
and re-lifts, repeating until the set of edges stops changing. Whatever still
cannot be resolved comes back as the `unresolved` list from `analyze`, which is
information, not an error.

## Using a different pipeline

`analyze` uses the default pipeline. If you need a custom set of passes, build one
with the `strider.opt` builders and pass it through
`LifterOptions(pipeline=...)`. Most work never needs this.
