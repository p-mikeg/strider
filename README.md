# strider

> *"He's one of them Rangers. Dangerous folk they are — wandering the Wilds."*
> — Barliman Butterbur, The Fellowship of the Ring

**Strider** is named after Aragorn's ranger alias in Tolkien's Middle-earth — a tracker who moves quietly through dark places, finding what others miss. Here, Strider hunts through binary code, letting you ask any question about how a function behaves — without source code or debug symbols.

---

## What It Does

Strider lifts native binaries to a sea-of-nodes IR and exposes it for arbitrary pattern queries from Python. You can ask things like: what offset does this function access off a pointer? what value does it pass to `malloc`? what does it return when the input matches this condition?

**Pipeline:**

```
Binary → CFG → IR → Optimizations → Pattern Queries
```

1. **Read** — loads an ELF binary and exposes its memory to the lifter
2. **Lift** — GHIDRA's Sleigh engine lifts machine instructions to p-code
3. **CFG** — builds a Control Flow Graph of basic blocks from the p-code
4. **IR** — translates the CFG into a sea-of-nodes IR graph with SSA-like variable tracking
5. **Optimize** — runs optimization passes to simplify the IR before querying
6. **Query** — write patterns in Python; captures extract the values you care about

---

## Architecture

```
reader ──→ cfg ──→ analyzer ──→ ir
                                └──→ opt
                                └──→ pattern
dot   (visualization)
```

| Crate | Role |
|-------|------|
| `reader` | ELF loader, memory reader for rsleigh |
| `cfg` | Control flow graph construction from p-code |
| `ir` | Sea-of-nodes IR graph (`Graph`, `FunctionBuilder`) |
| `analyzer` | CFG → IR translation, register aliasing (x86 `rax`/`eax`/`ax`/`al` etc.) |
| `opt` | IR optimization passes (see below) |
| `pattern` | IR graph pattern matching with named captures |
| `dot` | Renders CFG and IR graphs to `.dot` / `.html` for visualization |
| `strider-py` *(planned)* | Python bindings — the primary user-facing query interface |

---

## Optimizations

The `opt` crate runs all passes in a shared fixed-point loop via `default_pipeline()`. A simplification made by one pass (e.g. folding a branch condition to `false`) is immediately visible to later passes in the same iteration.

| Pass | What it does |
|------|-------------|
| `ConstantFold` | Evaluates constant arithmetic, comparisons, booleans, truncation, and extension. Also applies algebraic identities: `x+0→x`, `x^x→0`, nested AND-mask merging `(a&C1)&C2 → a&(C1&C2)`, etc. |
| `KnownBits` | Propagates statically known zero/one bits through the graph to fold partially-known expressions. |
| `RedundantPhis` | Eliminates `ControlPhi` and `MemPhi` nodes and `ControlState` nodes that have only one reachable predecessor. Detaches inputs of CFG-unreachable nodes. |
| `DeadBranchElimination` | Removes `If` nodes whose condition is a compile-time boolean. Strips the dead control edge from successor nodes. Works together with `RedundantPhis`. |
| `LoadReadOnly` | Resolves `Load` nodes with a constant address into constants by reading from a caller-supplied read-only memory region (e.g. `.rodata`, `.text`). |

---

## Usage

```bash
# Build
cargo build --workspace

# Run the example (reads binary_tests/binary_test, writes cfg.html + graph.html)
cargo run --example analyzer

# Tests
cargo test --workspace

# Lint
cargo clippy --workspace
```

---

## Python API *(planned)*

`strider-py` will be the primary interface. You write patterns in Python with named captures; strider evaluates them over the lifted IR and hands back the matched values.

Patterns can express any question about a function's behavior — memory accesses, call arguments, return values, branch conditions, anything representable in the IR:

```python
import strider

binary = strider.load("target_binary")
func   = binary.analyze_function(0x401234)
func.optimize()

# Named captures bind to the matched sub-expressions.
# Any part of the pattern can be captured.
ptr    = strider.capture("ptr")
offset = strider.capture("offset")

# "Find every load where the address is (something + a constant)"
pat = strider.load(addr=strider.add(ptr, offset))

for match in func.find(pat):
    print(f"load from {match['ptr']} + {match['offset']:#x}")

# "What constant does this function pass as the first argument to malloc?"
size = strider.capture("size")
pat  = strider.call(addr=0x..., arg(0, size))

for match in func.find(pat):
    print(f"malloc({match['size']})")

# "What value does the function return after a specific call?"
retval = strider.capture("retval")
pat    = strider.ret(preceded_by=strider.call(addr=0x...), ret_val=retval)

for match in func.find(pat):
    print(f"return value: {match['retval']}")
```

---

## Rust API

The `pattern` crate is the underlying engine.

### Capture variables

`Var` binds a data-flow value (`NodeOutputId`). `NodeVar` binds a control-flow node (`NodeId`). Create one with `Var::new()` / `NodeVar::new()` and embed it in a pattern. The same variable may appear multiple times — the matcher enforces that all occurrences bind to the **same** output.

```rust
use pattern::{Var, var};

let x = Var::new();
// var(x) matches any output and captures it.
// If x appears twice, both must resolve to the same node output.
```

### Pattern constructors

Every free function (`add`, `load`, `call`, …) returns a builder value that converts to `Pat` via `.into()`. Builders compose freely: any function that accepts `impl Into<Pat>` accepts another builder directly.

```rust
use pattern::{add, load, int_const, any, var, Var};

let offset = Var::new();

// load whose address is (anything + a constant)
let pat: Pat = load().addr(add(any(), var(offset))).into();
```

### Commutative matching

Binary operations that are mathematically commutative (`add`, `mul`, `and`, `or`, `xor`, `bool_and`, `bool_or`, `bool_xor`) automatically try both operand orderings. Non-commutative operations (`sub`, `div`, `shl`, …) are always ordered.

```rust
use pattern::{Matcher, add, int_const};

// Matches add(5, x) AND add(x, 5) — commutative by default.
let pat = add(int_const(5), any()).into();

// Force operand order with .ordered():
let pat_ordered = add(int_const(5), any()).ordered().into();
// Only matches if 5 is literally the left operand in the IR.
```

### Capturing any output with `.capture(v)`

Any pattern builder or `Pat` can have `.capture(v)` chained to it. After the structural match succeeds, the matched output is bound to `v`. This lets you capture the result of any subexpression — not just leaves.

```rust
use pattern::{Matcher, Var, var, add, any, load};

let add_out = Var::new();  // the output of the add node itself
let addr_v  = Var::new();  // the load's address operand

// Capture the entire add node's output:
let pat = add(any(), any()).capture(add_out).into();

// Capture a nested field (the load's address):
let pat2 = load().addr(any().capture(addr_v)).into();

// Equivalent shorthand for field capture — var(v) is any().capture(v):
let pat3 = load().addr(var(addr_v)).into();
```

### Predicate guards with `.when(f)`

Any pattern builder or `Pat` can have `.when(f)` chained. After the structural match succeeds, `f` is called with `(&BuiltFunctionGraph, NodeOutputId)`. The match fails if `f` returns `false`. This lets you add arbitrary constraints without writing a new `PatKind` variant.

```rust
use pattern::{Matcher, add, any, predicate};
use ir::node::{NodeKind, NodeOutputKind, NodeOutputType};

// Match any add whose result is U64:
let pat = add(any(), any()).when(|fg, out| {
    fg.graph.output_kind(out) == NodeOutputKind::OutputType(NodeOutputType::U64)
}).into();

// Match any IntConst node with value ≥ 0x1000:
let pat2 = predicate(|fg, out| {
    let node = fg.graph.get_node_from_output(out);
    match fg.graph.node_kind(node) {
        NodeKind::IntConst(v) => *v >= 0x1000,
        _ => false,
    }
}).into();

// Predicate as a sub-pattern — load whose address satisfies a custom check:
let pat3 = load().addr(predicate(|fg, out| {
    let node = fg.graph.get_node_from_output(out);
    matches!(fg.graph.node_kind(node), NodeKind::IntConst(_))
})).into();
```

`predicate(f)` is shorthand for `any().when(f)` and can appear anywhere a `Pat` is accepted.

---

### Example: load from a computed address

```rust
use pattern::{Matcher, Var, var, load, add, any};

let ptr    = Var::new();
let offset = Var::new();

// Match any load whose address is (anything + anything)
let pat = load().addr(add(var(ptr), var(offset))).into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(off) = m.get_int_const(offset, &graph) {
        println!("load at ptr + {off:#x}");
    }
    println!("  base: {:?}", m.get(ptr));
}
```

### Example: call argument — what size does this function pass to `malloc`?

```rust
use pattern::{Matcher, Var, var, call};

const MALLOC: u64 = 0x401080;

let size = Var::new();
let pat  = call().at(MALLOC).arg(0, var(size)).into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(n) = m.get_int_const(size, &graph) {
        println!("malloc({n})");
    }
}
```

### Example: return value after a specific call

```rust
use pattern::{Matcher, Var, var, call, ret};

const PARSE_FN: u64 = 0x402000;

let retval = Var::new();
let pat    = ret()
    .preceded_by(call().at(PARSE_FN))
    .ret_val(0, var(retval))
    .into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    println!("return value after parse_fn: {:?}", m.get(retval));
}
```

### Example: branch condition — find compares against a constant threshold

```rust
use pattern::{Matcher, Var, var, if_node, int_lt, any};

let threshold = Var::new();
let pat = if_node()
    .cond(int_lt(any(), var(threshold)))
    .into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(t) = m.get_int_const(threshold, &graph) {
        println!("branch: x < {t}");
    }
}
```

### Example: complex — call in the true branch of `(x & 4) == 0`, arg 2 is a load from a table

This shows how patterns compose: a branch condition, a call inside one branch, and a specific memory access inside that call's argument — all in a single query.

```rust
use pattern::{Matcher, Var, var, if_node, call, load, add, and, int_eq, int_const};

let x      = Var::new(); // the value tested in the branch condition
let offset = Var::new(); // the constant offset added to 0x1000

let cond = int_eq(and(var(x), int_const(4)), int_const(0));
let arg2 = load().addr(add(int_const(0x1000), var(offset)));

let pat = if_node()
    .cond(cond)
    .true_branch(call().arg(2, arg2))
    .into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    println!("branch variable:  {:?}", m.get(x));
    if let Some(off) = m.get_int_const(offset, &graph) {
        println!("load offset:      {off:#x}  →  address {:#x}", 0x1000u64 + off);
    }
}
```

A single `find_all` call returns every site in the function where all constraints hold simultaneously.

---

## Dependencies

- [rsleigh](https://github.com/p-mikeg/rsleigh) — Rust bindings to GHIDRA's Sleigh p-code lifter (local path dep at `../rsleigh`)
- [petgraph](https://github.com/petgraph/petgraph) — graph data structures for the CFG
- [cranelift-entity](https://github.com/bytecodealliance/wasmtime/tree/main/cranelift/entity) — typed entity indices for IR nodes

---

## Status

The IR lifter, optimizer, and pattern matcher are functional. The Python bindings (`strider-py`) are planned as the primary user-facing interface.
