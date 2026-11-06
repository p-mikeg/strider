# Optimizations

Strider optimizes IR for regularity, not speed: equivalent code is rewritten
into one shape, so one pattern matches every variant. `analyze` runs the
default set. When a pattern does not match, a pass has usually reshaped it.

## How the passes run

- **Rewriting passes** loop together to a fixed point. A pass whose last run
  changed nothing is skipped until another pass changes the graph. The loop is
  capped at 1024 iterations; a run that stops there keeps a valid, partly
  folded graph and sets `Function.optimizer_converged` to `False`.
- **Post-passes** run once afterwards (below).

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
constant stored there. `load_elf` derives the read-only image (the file minus
writable mappings), so an RWX segment is never folded; an image whose only
`PT_LOAD` is RWX folds nothing, silently. Check `prog.rom()`, whose repr counts
its regions, and pass your own `rom=` (see the [Python guide](python-guide.md)),
as for a raw blob.

**KnownBits.** Tracks known-0/known-1 bits; a fully known result becomes a
constant. A mask keeping every bit its operand may set is dropped:
`zext32(x8) & 0xff` is `zext32(x8)`.

**FlagCmpCanonicalize.** Rewrites flag-bit trees (carry, zero, sign,
overflow) back into a plain comparison: `sign != overflow` is signed `a < b`.

**IfCondInversion.** An `if` on a logical NOT drops the NOT and swaps the arms,
so you only match the positive form.

**PhiCollapse.** A phi or memory phi whose inputs other than itself are all one
value becomes that value (`[x, itself]` is `x`). A strongly connected group of
phis with a single outside input folds to it too. An `and` of a phi with a
constant mask becomes the value every arm agrees on under that mask.

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
overwrite it (`stack_global_disjoint`, `noalias_allocators`' heap identity),
never through a call a claim makes transparent: the jump-table walk and
return-target verification read that edge without those claims.

With `escape_analysis`, when no stack address escapes the frame, a spill load
forwards across a call if the slot is outside the callee's argument window, and
past a store through a pointer strider cannot place (under
`stack_global_disjoint`). With `noalias_allocators`, a load steps through a
call to a listed allocator when it reads another allocation or such a frame
slot; a global load does not. `callee_preserves_stack_args` empties the
argument window under either. [python-api
§2](python-api.md#2-analyzing-a-function) says when each claim is safe.

## Post-passes

In default pipeline order:

**CallStackArgCollect.** Attaches the stores into each call's outgoing argument
window to the call as ordinary arguments, up to the first slot no whole store
reaches. A store of the entry value of a register the function's own
convention preserves (a prologue save), or a call between store and call, also
ends the arguments; other stores just above them can come along (clang
x86-64's `push rax` alignment slot, MIPS o32's `gp` save). A slot every
arm of a non-loop join stores whole is collected as the phi of those values.

**FunctionArgDetect.** Numbers the incoming stack slots that loads off the entry
stack pointer read, after the register arguments: one ordinal per run of slots
one load spans, up to the first slot no load reads. It narrows those loads'
memory edges under the same claims as `LoadForward`. Register arguments are
recorded at lift time.

Every run then records, for each load and store whose address is a base plus a
constant, that pair. The base is a stack pointer or a `noalias_allocators`
call result; offsets compare only within one base (an aligned `sp & -16` is its
own base). Stack-access and exact-slot queries read these. Rust's
`refresh_memory_classes` re-records them after an edit outside a pipeline.

## Indirect-branch resolution

After the pipeline, every round, `analyze` classifies each unresolved jump
table, computed jump and return on the optimized IR, feeds new targets back and
re-lifts until the edge set stops changing. This runs whatever the pipeline
(`strider.opt` has no pass for it) unless `resolve_indirect_branches` is off.
It reads IR optimized under the function's claims, so a wrong claim can seat a
wrong edge (under `escape_analysis` and `callee_preserves_stack_args`, a target
spilled to the stack forwards across a call that overwrote it). What stays
unresolved is listed in `cfg.incomplete`
([reasons](python-api.md#12-the-cfg-stridercfg)).

An indirect CALL is neither resolved nor reported; its target is the `Call`
node's input value. A return is reported when its folded target is not
provably the entry return address (the entry link register, or the entry-SP
slot it was pushed to).

A resolved target decodes in the ISA mode the branch commits, else the one
flowing into it (ARM/Thumb interworking, MIPS16). A seated `switch` keeps its
committed mode for arms found later.

`CfgOptions(known_targets={dispatch_addr: [target, ...]})` seeds answers the
loop grows from; a site holding only your seed is listed as
`"unverified_seed"`.

### Dispatch shapes that need an opt-in

- MIPS64 GOT-indirect dispatch (`Add(Load[GOT], const)` entries): needs a
  convention naming `t9` (the abicalls entry address) and
  `load_elf(relocated_got_is_rom=True)`. Local entries only; a global
  (imported-symbol) entry stays unmapped.
- A label table in writable `.data.rel.ro` (PIC tables on AArch64, ARM, MIPS
  and PowerPC): no ROM serves it without `load_elf(relocated_got_is_rom=True)`.
- A dispatch in a loop whose arms call a function the convention says
  clobbers the table-base register, which the compiler kept live across the
  call (GCC `-fipa-ra` on a same-unit callee): once the loop closes the
  selector no longer derives. Use
  `per_address_ccs={callee: cc.preserves_regs()}`.

### Dispatch shapes that do not resolve

Reported as `"unresolved_branch"`; the rest of the function lifts:

- AArch64 big-endian stack-array dispatch whose table base is built with
  `bfi`: stack decomposition cannot see through the masked SP value, and one
  16-byte `str q0` writes two entries as one 128-bit constant.
- An index with a constant offset that can go below zero, such as
  `(x & 7) - 2`.
- A mask naming entries past the table (MIPS32 gcc: `& 7` over six entries).
- An index or table entry read back from a stack slot through a memory phi or
  at a variable index (clang -O0 on aarch64be and arm_be, arm_be8's two-slot
  stack array).
- A table of more than 4096 entries.
- An unguarded index whose type already spans the table (`x >> 28` over 16
  entries).
- A 64-bit selector on a 32-bit target.
- A wrapped narrow range (`unsigned short` cases 65529..65535 on x86).
- PowerPC64 dispatch on a 32-bit parameter: the bound covers only the low
  word, and the ABI's argument extension is not assumed.

## Known limits

- Chained switches (a jump table whose arm holds another) are found one
  re-lift round per level; past 256 levels the next site is reported
  `"unresolved_branch"` and deeper levels are never decoded.
- PowerPC float code lifts to far more p-code than other ISAs because the
  FPSCR is modelled exactly: one `fadd` is about 90 p-code ops.
- A chain of about 500 constant guards held only in registers reaches the
  1024-iteration cap, and the analysis returns it partly folded. A chain whose
  guards each test a different stack slot, with stores between them, still
  costs quadratic settle work in the guard count.
- Reloads of `n` distinct stack slots cost quadratic climb work past `n`
  diamonds inside an irreducible loop, where the climb steps one dominator at
  a time, and past `n` calls under `escape_analysis`, where every call is a
  candidate for every slot.
- A pattern nesting commutative nodes `d` deep can cost up to 2^d match
  attempts, even when nothing matches; `.ordered()` pins the operand order.
- Sibling KnownBits folds over one unfolded chain of depth `d` each copy the
  chain's instruction addresses, quadratic in `d`.
- A return after a stack-clash probe loop (gcc `-fstack-clash-protection`,
  frames of 64 KiB or more) is reported: the loop's exit SP is not derived.

## Using a different pipeline

Build passes with the `strider.opt` builders and pass them as
`LifterOptions(pipeline=...)`.
