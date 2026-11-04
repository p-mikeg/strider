# Vocabulary

The terms Strider uses, in plain language.

## Reading the binary

**Binary / ELF.** The compiled program on disk. ELF is Linux's format for
executables, shared libraries and unlinked object files.

**Sleigh / p-code.** Sleigh is GHIDRA's instruction decoder. It turns one CPU
instruction (say x86 `add rax, rbx`) into *p-code*, generic operations that mean
the same on every CPU, so the rest of Strider is architecture-neutral.

**Varnode.** Sleigh's storage location: a register, a slice of memory, or a
constant, as (space, offset, size). `RAX` is a varnode; a memory read such as
`[rsp + 8]` is a `LOAD` through a runtime-computed address.

**Calling convention.** Where a function's arguments and result live (which
registers, which stack slots). `load_elf` picks it from the ELF header.

**ISA mode.** Some architectures mix two instruction sets in one binary (ARM
and Thumb, MIPS32 and MIPS16). The mode is not in the bytes but carried by the
branch that reached them, so each region decodes in the mode of an edge that
reached it. An address reached in two modes is listed in `cfg.incomplete` as
`"isa_mode_conflict"`.

## Control flow

**Region.** A straight run of instructions entered at the top and left at the
bottom; elsewhere called a "basic block". Strider says region in both the CFG
and the IR.

**CFG (control flow graph).** The regions and the jumps between them.

**Dominator.** Region A *dominates* B if every path from entry to B goes
through A. It decides where phi nodes are needed.

## The IR

**IR (intermediate representation).** Strider's representation of the
function, between the CFG and your query. Every analysis runs on it.

**Sea of nodes.** The IR's shape: a graph, not an instruction list. Each
operation is a **node**; **edges** carry values from producer to users. Order
comes from dependencies, not line numbers.

**NodeId and ValueId.** The graph is bipartite: wires run
`node -> value -> node`. A **NodeId** names a node (a computation); a
**ValueId** names one of its outputs. In Python you mostly hold `Node` handles
(`function.node(id)`, `node.inputs()`, `node.outputs()`); ids are valid until
the graph is compacted, optimized or rewritten, and `function.node_ids()`
lists them.

**What edges carry (control, memory, phi token, data).** Each edge carries one
of:

- **Control (ctrl).** Execution order; the CFG skeleton.
- **Memory token (mem).** The whole state of memory. Loads, stores and calls
  chain through it; a MemPhi merges it at a region.
- **Phi token.** From a region to each of its phis; the phi's value inputs
  follow that region's predecessors in order.
- **Data value.** A computed value, the only kind with a *type*.

**Value type.** Width and integer-or-float. Integers are `I1, I8, I16, I24,
I32, I40, I48, I56, I64, I72, I80, I96, I112, I128, I256, I512`; floats are
`F16, F32, F64, F80, F128`. A boolean is `I1`. The odd widths exist because
some Sleigh register or temporary has that size
(`crates/strider-ir/src/node/value_type.rs` names each source). In Python:
`node.value_type()`.

**Walks (cfg, data, memory).** Traversals from the entry. A **cfg walk**
follows control edges (`function.cfg_walk()`). A **data walk** (reachable walk)
visits every node reachable through data and control edges, in pre-order
(`function.data_walk()`, or `function.walk(node_id)` from one node). A
**memory walk** follows the memory token through loads, stores and calls
(`function.mem_walk()`).

**SSA (static single assignment).** Each value is written exactly once, so
"where did this come from?" has one answer.

**Region node.** A CFG region as a node in the IR, where control merges (e.g.
the two arms of an `if`) and where phis live.

**Phi.** At a merge, picks the value of the path taken:
`phi(token, a, b, ...)`, the region's phi token then one value per incoming
edge in predecessor order. `x = cond ? a : b` becomes `phi(token, a, b)`. A phi
the lifter placed for a register is tagged with it, which `phi_for(vn)`
matches; an untagged phi is anonymous.

**MemPhi.** A phi over memory state.

**CallOther.** A node for an instruction the lifter cannot express as data
operations (`cpuid`, `rdtsc`, a syscall, a coprocessor access). It behaves like
a call; `node.call_other_name()` reads its name.

## Analysis

**Optimization.** Passes that simplify the IR (constant folding, dead-branch
removal, store-to-load forwarding, ...) before you query.

**Canonicalization.** Rewriting equivalent shapes into one form so one pattern
matches all of them: `a - b` is always `a + (-b)`. The
[python guide](python-guide.md) lists the surprising ones.

**Assumption.** A claim about the code the IR cannot prove. Seven help the
optimizer tell memory accesses apart (the stack and globals do not overlap, a
named allocator returns fresh storage, ...); two are about the processor (an
ARM register `msr` keeps the mode, float conversions round to nearest).
`AssumptionOptions` holds all nine; a wrong one makes the answer wrong, not
imprecise. [python-api.md](python-api.md#2-analyzing-a-function) covers each.

**Noalias allocator.** A function you name as returning storage nothing else
points at, typically `malloc`. Its result is a distinct base, so a load from a
different allocation, or from a non-escaping frame's stack slot, steps through
a call to it; a load from a global does not.

**Stack offset.** For a load or store at "stack pointer plus a constant",
Strider records the constant, so you can ask for stack accesses or one exact
slot.

**Asm-fingerprint.** The addresses of the machine instructions whose lift, or a
later rewrite, produced an IR node, mapping a match back to assembly. Every
reachable node has at least one, except the six synthesised kinds (`Entry`,
`InitialMemory`, `InitialVar`, `Region`, `Phi`, `MemPhi`), which may have none
until a rewrite replaces another node with them.

## Querying

**Pattern.** The shape you look for, built from constructors like `load(...)`,
`int_add(...)`, `call(...)`, leaving out what you do not care about.

**Capture.** A named hole in a pattern that matches anything and remembers it.
Write it as `Capture("name")`. A bare string is not a pattern operand; it names
a capture only as a rewrite's `replace` or a `Match` key.

**Alternation.** One pattern for several shapes: `one_of` reports every
matching alternative, `first_of` only the first. Either can sit in any slot.

**Join / constraint.** Several patterns matched together on shared captures. A
`constraints=` entry narrows the pairings: `dominates(a, b)` keeps those where
`a` dominates `b`, and a `JoinPredicate` runs your own test.

**Match.** One query result, carrying every capture's value: `hit.uint(off)`,
`hit.node(base)`, `hit.asm_fingerprint(base)`.
