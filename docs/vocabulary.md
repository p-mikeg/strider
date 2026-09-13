# Vocabulary

The terms Strider uses, each in plain language.

## Reading the binary

**Binary / ELF.** The compiled program on disk. ELF is the file format Linux
uses for executables, shared libraries, and unlinked object files.

**Sleigh / p-code.** Sleigh is GHIDRA's engine for decoding machine
instructions. It turns one CPU instruction (say x86 `add rax, rbx`) into
*p-code*, a small set of generic operations that mean the same thing on every
CPU. Strider builds on p-code so the rest of the pipeline does not care whether
the binary is x86, ARM, or MIPS.

**Varnode.** Sleigh's name for a storage location: a register, a slice of
memory, or a constant. It is a triple of (space, offset, size). `RAX` is a
varnode; a memory read such as `[rsp + 8]` is a `LOAD` through an address
computed at runtime.

**Calling convention.** The rules for how arguments are passed to a function
and how the result comes back (which registers, which stack slots). Strider
needs it to know where a function's inputs live. `load_elf` picks it from the
ELF header.

**ISA mode.** Some architectures encode two instruction sets in one binary and
switch between them at runtime: ARM and Thumb, MIPS32 and MIPS16. Which one an
address decodes as is not in the bytes, it is carried by the branch that
reached it, so each region is decoded in the mode of an edge that reached it.
`cfg.isa_mode_conflicts()` reports an address two edges reached in different
modes, which only one of them can win.

## Control flow

**Region.** A straight run of instructions with no jumps in or out of the
middle: execution enters at the top and leaves at the bottom. Other tools often
call this a "basic block"; Strider calls it a region throughout, in both the
CFG and the IR.

**CFG (control flow graph).** The map of regions and the jumps between them.

**Dominator.** Region A *dominates* region B if every path from the function's
entry to B goes through A first. It is how the tools reason about "this always
happens before that", and it is what decides where phi nodes are needed.

## The IR

**IR (intermediate representation).** Strider's own representation of the
function, sitting between the raw CFG and your query. Every analysis runs on it.

**Sea of nodes.** The shape of that IR: a graph, not a list of instructions.
Each operation is a **node**, and **edges** carry values from the node that
produces one to the nodes that use it. Order is expressed by these data
dependencies, not by line numbers.

**NodeId and ValueId.** The graph is bipartite: nodes and values alternate, and a
node never links straight to another node. A **NodeId** names a node (a
computation); a **ValueId** names one output that a node produces. A node reads
other nodes' outputs through its input slots, so the wires run
`node -> value -> node`. In Python you mostly hold `Node` handles
(`function.node(id)`, `node.inputs()`, `node.outputs()`); the ids are the
integer names underneath, valid until the graph is compacted, optimized or
rewritten, and `function.node_ids()` lists them all.

**What edges carry (control, memory, phi token, data).** Not every wire carries a
computed number. Each edge carries one of four things:

- **Control (ctrl).** The token that threads execution order through the graph.
  Control edges are the CFG skeleton: they say which node runs, not what it
  computes.
- **Memory token (mem).** One token standing for the whole state of memory. Loads,
  stores, and calls chain through it so their ordering stays explicit, and a
  MemPhi merges it at a region. One token covers every address.
- **Phi token.** The edge from a region to each of its phis. It ties a phi to
  its region; the phi's value inputs follow that region's predecessors in order
  (see the `phi(token, ...)` shape below).
- **Data value.** An actual computed value, and the only kind that carries a
  *type*.

**Value type.** The width of a data value and whether it is integer or float.
Integers are `I1, I8, I16, I24, I32, I40, I48, I56, I64, I72, I80, I96, I112,
I128, I256, I512`; floats are `F16, F32, F64, F80, F128`. A boolean is the
1-bit integer `I1`. The odd widths exist because some Sleigh register or
temporary has that size; `crates/strider-ir/src/node/value_type.rs` names the
source of each. Read a node's type in Python with `node.value_type()`.

**Walks (cfg, data, memory).** Ways to traverse the graph from the entry. A
**cfg walk** follows control edges only, giving the region skeleton
(`function.cfg_walk()`). A **data walk**, also called the reachable walk, visits
every node reachable from entry through both data and control edges, in pre-order
(`function.data_walk()`, or `function.walk(node_id)` from one node). A **memory
walk** follows the memory token forward through the loads, stores, and calls that
touch memory (`function.mem_walk()`).

**SSA (static single assignment).** A property the IR keeps: each value is
written exactly once. Instead of overwriting a variable, the code produces a
new value. This makes "where did this value come from?" always have one answer.

**Region node.** In the IR, a region shows up as a node that marks where control
flow merges, for example where the two arms of an `if` join back together. It is
the same region from the CFG, seen from inside the graph, and it is where phi
nodes live.

**Phi.** Where control can reach a region by more than one path, a phi node
chooses the value belonging to the path actually taken. It is written
`phi(token, a, b, ...)`: the first input is the phi token from the merge
region, and the rest are one candidate value per incoming edge, in the same
order as that region's predecessors. So `x = cond ? a : b` becomes
`phi(token, a, b)`, which yields `a` when control arrived on the first edge and
`b` on the second. A phi the lifter placed for a register carries a tag naming
it, which `phi_for(vn)` matches on; a phi with no tag is anonymous.

**MemPhi.** The same idea as a phi, but for memory instead of a register value:
it merges the state of memory coming from different paths.

**CallOther.** A node for a special instruction the lifter cannot express as
plain data operations: `cpuid`, `rdtsc`, a syscall, a coprocessor access, and the
like. It behaves like a call and carries the operation's name, readable with
`node.call_other_name()`.

## Analysis

**Optimization.** Passes that simplify the IR: folding constant arithmetic,
removing dead branches, forwarding stored values to later loads, and so on.
They run before you query so the graph is small and regular.

**Canonicalization.** Rewriting equivalent shapes into one agreed form so a
single pattern matches all of them. For example subtraction `a - b` is always
stored as `a + (-b)`, so you never have to write both. The
[python guide](python-guide.md) lists the ones that most often surprise people.

**Assumption.** A claim about the code being analysed that the IR cannot
prove, and that the optimizer needs before it can rule two memory accesses
apart: that the stack and globals do not overlap, that a named allocator
returns fresh storage, and four more. `AssumptionOptions` holds them, and a
wrong one makes the answer wrong rather than imprecise.
[python-api.md](python-api.md#2-analyzing-a-function) says what each buys and
which values are sound.

**Noalias allocator.** A function you name as returning storage nothing else
points at, `malloc` being the usual one. Its return value becomes a base
distinct from every other, so a load from a different allocation, or from a
stack slot of a frame no stack address escapes, steps through a call to it. A
load from a global does not.

**Stack offset.** When a load or store addresses memory as "stack pointer plus
a fixed amount", Strider records that amount. It lets you ask for stack accesses
specifically, or for one exact slot.

**Asm-fingerprint.** The addresses of the machine instructions whose lift, or a
later rewrite, contributed to an IR node, so given a match you can map the
value back to the assembly it came from. Every reachable node carries at least
one, except the six kinds the lifter synthesises rather than lifts (`Entry`,
`InitialMemory`, `InitialVar`, `Region`, `Phi`, `MemPhi`). Those may carry
none, and pick addresses up when a rewrite replaces another node with them.

## Querying

**Pattern.** The shape you are looking for, built from constructors like
`load(...)`, `int_add(...)`, `call(...)`. A pattern describes a piece of the IR
without pinning down the parts you do not care about.

**Capture.** A named hole in a pattern. Where you write a capture, the pattern
matches anything and remembers what it matched, so you can read it back. Write
it as a `Capture("name")` object. A bare string is not a match-pattern operand;
it names a capture in a rewrite template or on a `Match`.

**Alternation.** One pattern standing for several shapes: `one_of` matches any
of them and reports every hit, `first_of` takes the first that matches and
stops. Either can sit in any slot, so an alternation of two address forms is
still one query.

**Join / constraint.** Two patterns searched together, matched up on the
captures they share, so you can ask about a relationship rather than a single
shape. A `constraints=` entry narrows which pairings count: `dominates(a, b)`
keeps only those where every control path from entry to `b` passes through `a`
(see **Dominator**), and a `JoinPredicate` runs your own test over the pair.

**Match.** One result of a query. It carries every capture's value: index it
with the capture and read the aspect you want, `hit[off].uint`,
`hit[base].node`, `hit[base].asm_fingerprint`.
