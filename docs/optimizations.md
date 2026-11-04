# Optimizations

Strider optimizes IR for regularity, not speed: clutter folds away and
equivalent code is rewritten into one shape, so one pattern matches every
variant. `analyze` runs the default set. When a pattern does not match, a pass
has usually reshaped what you were looking for.

## How the passes run

- **Rewriting passes** loop together to a fixed point. A pass whose last run
  changed nothing is skipped until another pass changes the graph. The loop is
  capped at 1024 iterations and raises if it has not settled.
- **Post-passes** run once afterwards. Most record facts; `CallStackArgCollect`
  adds a call's stack arguments as extra `Call` inputs, and `FunctionArgDetect`
  moves argument loads' memory edges past definitions that cannot overwrite
  them.

## Rewriting passes

In default pipeline order:

**ConstantFold.** Computes constant expressions, applies algebraic identities
(`x + 0`, `x * 1`, ...) and folds constant truncations and extensions. Any sum
or difference of `x`, `x*C` and `x<<C` becomes one `x * K`: `x + x*2` and
`x + (x<<1)` are both `x*3`; search for `int_mul(x, int_const_any_width(K))`.
Also:

- a truncate of an add, mul, and, or or xor of two extends (or an extend and a
  constant) from the truncate's width is that operation at the narrow width:
  `trunc8(zext32(a) + zext32(b))` is `a + b` at 8 bits;
- constant shifts compose: `(x << 2) << 3` is `x << 5`; a signed right shift
  saturates at width minus one;
- constant xors merge: `(x ^ 1) ^ 3` is `x ^ 2`;
- a sign extend of a widening zero extend is that zero extend.

**LoadReadOnly.** A load from a fixed address in read-only memory becomes the
constant stored there. `load_elf` derives the image (the file minus writable
mappings), so an RWX segment is never folded; for a raw blob pass `rom=` (see
the [Python guide](python-guide.md)). An image whose only `PT_LOAD` is RWX
folds nothing, silently. Check `prog.rom()`, whose repr counts its regions, and
pass your own `rom=` covering the constant data.

**KnownBits.** Tracks known-0/known-1 bits; a fully known result becomes a
constant. A mask keeping every bit its operand may set is dropped:
`zext32(x8) & 0xff` is `zext32(x8)`.

**FlagCmpCanonicalize.** Rewrites flag-bit trees (carry, zero, sign, overflow),
including those left after a negated branch is normalized, back into a plain
comparison like `a < b`. Signed `a < b` is `sign != overflow`.

**IfCondInversion.** An `if` on a logical NOT drops the NOT and swaps the arms,
so you only match the positive form.

**PhiCollapse.** A phi or memory phi whose inputs other than itself are all one
value becomes that value (`[x, itself]` is `x`). A strongly connected group of
phis with a single outside input folds to it too.

**RegionCollapse.** A region with one control input is removed; its phis and
memory phis become their single value input.

**DeadBranchElimination.** An `if` on a constant, or a `switch` whose dispatch
address is a constant naming one arm, is removed along with the arms no longer
reached. It folds only if the surviving arm still reaches a terminator. Every
run also removes never-taken control edges into merge regions, with their phi
and memory-phi inputs.

**LoadForward.** A load from exactly the location an earlier store wrote, with
nothing overwriting it between, takes the stored value; a wider store is
narrowed to the load's range. Forwarding is blocked by anything short of an
exact base-and-offset match, a merge whose paths reach different definitions,
and a call unless its convention declares `preserves_memory`
(`cc.preserves_all()`, or `per_address_ccs={callee_addr: cc}` for a hook such
as `__fentry__`). Memory is taken to be RAM: a memory-mapped register polled
after a write folds to the value written.

Each examined load's memory edge is also moved past definitions that cannot
overwrite it, using `stack_global_disjoint` and `noalias_allocators`' heap
identity, but never through a call a claim makes transparent
(`escape_analysis`, a listed allocator): the jump-table walk and return-target
verification read that edge without those claims. Claims accumulate on a
function and are never withdrawn; to drop one, analyse again without it.

With `AssumptionOptions(escape_analysis=True)` it forwards across a call when no
stack address escapes the frame anywhere in the function (as a call argument,
stored value, return value, pointer-arithmetic input, or in a call-clobbered
register) and the slot is not one of the callee's arguments. With
`noalias_allocators=[addr, ...]`, a load steps through a call to a listed pure
allocator when it reads a different allocation, or a non-escaping frame's slot
outside that call's argument window; a global load does not. A listed call's
result overlaps no stack slot or global; an allocator carving from a static or
caller-supplied arena does not qualify. Under either of those two,
`callee_preserves_stack_args=True` empties the outgoing-argument window so a
spill at the stack top forwards too; alone it does nothing.

## Post-passes

In default pipeline order:

**CallStackArgCollect.** Attaches the stores into each call's outgoing argument
window to the call node as ordinary arguments. A push is indistinguishable
from an incidental write there, so it errs wide: a spill just above the
arguments can come along.

**FunctionArgDetect.** Records which value carries each incoming *stack-passed*
argument (loads off the entry stack pointer), and narrows those loads' memory
edges under the same claims as `LoadForward`. Register arguments are recorded
at lift time.

Every run then records, for each load and store whose address is a base plus a
constant, that pair. The base is a stack pointer or a `noalias_allocators`
call result; offsets compare only within one base (an aligned `sp & -16` is its
own base). Stack-access and exact-slot queries read these. Rust's
`refresh_memory_classes` re-records them after an edit outside a pipeline.

## Indirect-branch resolution

Jump tables, computed jumps and returns are resolved by a classifier `analyze`
runs after the pipeline every round, whatever the pipeline, unless
`resolve_indirect_branches` is off (`strider.opt` has no pass for it). It
classifies each unresolved branch on the optimized IR, feeds new targets back
and re-lifts until the edge set stops changing. It reads IR optimized under
the function's claims, so a wrong claim can seat a wrong edge (under
`escape_analysis` and `callee_preserves_stack_args`, a target spilled to the
stack forwards across a call that overwrote it). What stays unresolved comes
back in `unresolved`, one of the seven reasons in
[python-api.md](python-api.md#12-the-cfg-stridercfg) that `cfg.incomplete`
lists.

An indirect CALL is neither resolved nor reported; its target is the `Call`
node's input value. A return is reported when its folded target is not
provably the entry return address (the entry link register, or the entry-SP
slot it was pushed to).

A resolved target decodes in the ISA mode the branch commits, else the one
flowing into it (ARM/Thumb interworking, MIPS16). A seated `switch` keeps its
committed mode for arms found later.

`CfgOptions(known_targets={dispatch_addr: [target, ...]})` seeds answers the
loop grows from; a site holding only your seed is listed as
`"unverified_seed"`. `LifterOptions(resolve_indirect_branches=False)` turns the
classifier off.

### Dispatch shapes that need an opt-in

- MIPS64 GOT-indirect dispatch (`Add(Load[GOT], const)` entries): needs a
  convention naming `t9` (the abicalls entry address) and
  `load_elf(relocated_got_is_rom=True)`. Local entries only; a global
  (imported-symbol) entry stays unmapped.
- PowerPC stack-array dispatch on ppc32le, ppc64be and ppc64le, whose table is
  in `.data.rel.ro`: needs `load_elf(relocated_got_is_rom=True)`.
- A dispatch in a loop whose arms call a function clobbering the table-base
  register: once the loop closes the selector no longer derives. Use
  `per_address_ccs={callee: cc.preserves_regs()}`.

### Dispatch shapes that do not resolve

Reported as `"unresolved_branch"`; the rest of the function lifts:

- AArch64 big-endian stack-array dispatch whose table base is built with `bfi`:
  Sleigh masks the SP-derived value, which stack decomposition cannot see
  through, and one 16-byte `str q0` writes two entries as one 128-bit constant.
- An index with a constant offset that can go below zero, such as
  `(x & 7) - 2`: enumerating `x` would name slots before the table that only
  the guards exclude.

## Using a different pipeline

Build passes with the `strider.opt` builders and pass them as
`LifterOptions(pipeline=...)`.
