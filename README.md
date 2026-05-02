# Strider

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
reader ──→ cfg ──→ strider ──→ ir
                               └──→ opt
                               └──→ pattern
dot   (visualization)
```

| Crate | Role |
|-------|------|
| `reader` | ELF loader, memory reader for rsleigh |
| `cfg` | Control flow graph construction from p-code |
| `ir` | Sea-of-nodes IR graph (`Graph`, `FunctionBuilder`) |
| `strider` | CFG → IR translation, register aliasing (x86 `rax`/`eax`/`ax`/`al` etc.) |
| `opt` | IR optimization passes (see below) |
| `pattern` | IR graph pattern matching with named captures |
| `dot` | Renders CFG and IR graphs to `.dot` / `.html` for visualization |
| `strider-py` | Python bindings — the primary user-facing query interface (PyO3 + maturin, abi3-py39) |

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
| `StackStoreDetect` | Converts `Store(InitialVar(SP) + K, …)` into a dedicated `StackStore { offset: K }` (or `StackStorePhi` at join points), with per-predecessor offsets in a side table. |
| `StackLoadForward` | Forwards values from `StackStore`s to subsequent same-offset `Load`s, eliminating the round-trip through memory. |
| `IndirectBranchResolve` | Producer-shape classifier for `BranchIndirect` placeholders — recognises link-register returns, tail calls, jump tables, and stack-array dispatch. |
| `CallStackArgCollect` *(post-pass)* | Collects positional stack arguments at `Call` sites using the calling convention's stack-arg offsets. |
| `FunctionArgDetect` *(post-pass)* | Canonicalises register- and stack-passed argument reads at the function boundary into `FunctionArg` nodes, so patterns can match on argument position. |

---

## Usage

```bash
# Build
cargo build --workspace

# Run the example (reads fixtures/binary_test, writes cfg.html + graph.html)
cargo run --example strider

# Tests
cargo test --workspace

# Lint
cargo clippy --workspace
```

---

## Python API

`strider-py` is the primary interface. You write patterns in Python with named captures; strider evaluates them over the lifted IR and hands back the matched values.

### Install (uv)

```bash
cd crates/strider-py
uv sync --group dev          # creates .venv + installs dev deps
uv run maturin develop       # builds the Rust extension
uv run pytest                # runs the test suite
```

A pip-based legacy path is documented in [`crates/strider-py/README.md`](crates/strider-py/README.md). The wheel is `abi3` (Python 3.9+).

### Quickstart

```python
import strider
from strider.pattern import Capture, var, add, load, call, int_const

# 1. Load a binary into a MemoryMap.
mem = strider.MemoryMap()
mem.add_region_from_elf("fixtures/out/x86/memory.elf")
mem.apply_elf_relocations("fixtures/out/x86/memory.elf")  # autoloads .got.plt etc.

# 2. Run the full pipeline (CFG → IR → optimize, including the
#    indirect-branch fixed-point loop) in one call.
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem, rom=mem,
    entry=mem.symbol("array_sum"),
    allow_code_before_start_addr=True,
)

# 3. Query the optimized graph.
ptr, off = Capture(), Capture()
for hit in result.graph.find_all(load(addr=add(var(ptr), var(off))), ignore_casts=True):
    print(f"load at {hit.uint(ptr)} + {hit.uint(off):#x}")

# 4. Visualize.
result.cfg.to_html("cfg.html")
result.graph.to_html("graph.html")
```

### Pattern features beyond plain `find_all`

**Set-membership target queries** — match a call against any of N known callees in one pass:

```python
from strider.pattern import call, int_const_any_of

# Either form below works:
hits = g.find_all(call().at_any([0x1000, 0x2000, 0x3000]))
hits = g.find_all(call().target(int_const_any_of([0x1000, 0x2000])))
```

**Multi-pattern joins on shared captures** — find the K such that two patterns simultaneously match with the same binding for a shared capture:

```python
from strider.pattern import Capture, add, any_int_const, call, load, initial_var_for, var

# ni_vp-style field-offset recovery:
#   vn_open(&nd, ...);
#   script_vp = nd.ni_vp;     // load nd.ni_vp = Add(rbp, K1+K_field)
rbp = sleigh.reg("RBP")  # or RSP for -fomit-frame-pointer builds
k_call, k_load = Capture(), Capture()
for tup in g.find_all_requirements([
    call().target(int_const(VN_OPEN)).arg(0,
        add(initial_var_for(rbp), any_int_const(k_call)).ordered()),
    load().addr(add(initial_var_for(rbp), any_int_const(k_load)).ordered()),
]):
    field_offset = (tup[1].uint(k_load) - tup[0].uint(k_call)) & 0xFFFFFFFFFFFFFFFF
    print(f"recovered field offset = {field_offset:#x}")
```

**Stack-offset recovery** — capture a `StackStore` and read its compile-time SP-relative offset:

```python
from strider.pattern import Capture, stack_store

c = Capture()
for hit in g.find_all(stack_store().offset_any([-8, -16, -24]).capture(c)):
    print(f"matched stack store at offset {hit.stack_offset(c)}")
```

See [`crates/strider-py/README.md`](crates/strider-py/README.md) for the full Python surface and [`crates/strider-py/examples/python/`](crates/strider-py/examples/python/) for runnable end-to-end walkthroughs.

---

## Rust API

The `pattern` crate is the underlying engine.

### Capture variables

`Capture` is the single capture type — bindings store both the matched `NodeId` and (for value-producing patterns) the matched `NodeOutputId`. Create one with `Capture::new()` and embed it in a pattern. The same capture may appear multiple times in a pattern — the matcher enforces that every occurrence binds to the **same** value.

```rust
use pattern::{Capture, var};

let x = Capture::new();
// var(x) matches any output and captures it.
// If x appears twice in one pattern, both occurrences must agree.
```

For cross-pattern equality on a shared capture (e.g. "find the K such that pattern A(K) AND pattern B(K) match with the same `<base>` binding"), use [`Matcher::find_all_requirements`](crates/pattern/src/matcher/mod.rs) — it runs N patterns and returns only the joined tuples whose shared captures agree on `Binding` (node + value output).

### Pattern constructors

Every free function (`add`, `load`, `call`, …) returns a builder value that converts to `Pat` via `.into()`. Builders compose freely: any function that accepts `impl Into<Pat>` accepts another builder directly.

```rust
use pattern::{add, load, int_const, any, var, Capture};

let offset = Capture::new();

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

### Capturing any output with `.capture(c)`

Any pattern builder or `Pat` can have `.capture(c)` chained to it. After the structural match succeeds, the matched value is bound to `c`. This lets you capture the result of any subexpression — not just leaves.

```rust
use pattern::{Matcher, Capture, var, add, any, load};

let add_out = Capture::new();  // the output of the add node itself
let addr_c  = Capture::new();  // the load's address operand

// Capture the entire add node's output:
let pat = add(any(), any()).capture(add_out).into();

// Capture a nested field (the load's address):
let pat2 = load().addr(any().capture(addr_c)).into();

// Equivalent shorthand for field capture — var(c) is any().capture(c):
let pat3 = load().addr(var(addr_c)).into();
```

### Predicate guards with `.when(f)`

Any pattern builder or `Pat` can have `.when(f)` chained. After the structural match succeeds, `f` is called with `(&BuiltFunctionGraph, NodeOutputType, NodeOutputId)`. The match fails if `f` returns `false`. This lets you add arbitrary constraints without writing a new `PatKind` variant.

```rust
use pattern::{add, any, predicate};
use ir::node::{NodeKind, NodeOutputType};

// Match any add whose result is U64:
let pat = add(any(), any()).when(|_fg, ty, _out| ty == NodeOutputType::U64).into();

// Match any IntConst node with value ≥ 0x1000:
let pat2 = predicate(|fg, _ty, out| {
    let node = fg.graph.get_node_from_output(out);
    matches!(fg.graph.node_kind(node), NodeKind::IntConst(v) if *v >= 0x1000)
}).into();
```

`predicate(f)` is shorthand for `any().when(f)` and can appear anywhere a `Pat` is accepted.

### Set-membership constructors

For "any of these N constants / addresses" queries, use the dedicated set-membership helpers:

```rust
use pattern::{call, int_const_any_of, stack_store};

// Match a call to any of three known callees:
let pat = call().at_any([0x1000u64, 0x2000, 0x3000]);

// Match a stack-store at any of these field offsets:
let pat = stack_store().offset_any([-8i64, -16, -24]);

// Lower-level: an IntConst whose value is in a set:
let pat = int_const_any_of([0x1000u64, 0xDEADBEEF]);
```

Empty sets vacuously fail (match nothing).

---

### Example: load from a computed address

```rust
use pattern::{Matcher, Capture, var, load, add};

let ptr    = Capture::new();
let offset = Capture::new();

let pat = load().addr(add(var(ptr), var(offset))).into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(off) = m.get_uint(offset, &graph) {
        println!("load at ptr + {off:#x}");
    }
}
```

### Example: call argument — what size does this function pass to `malloc`?

```rust
use pattern::{Matcher, Capture, var, call};

const MALLOC: u64 = 0x401080;

let size = Capture::new();
let pat  = call().at(MALLOC).arg(0, var(size)).into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(n) = m.get_uint(size, &graph) {
        println!("malloc({n})");
    }
}
```

### Example: return value after a specific call

```rust
use pattern::{Matcher, Capture, var, call, ret};

const PARSE_FN: u64 = 0x402000;

let retval = Capture::new();
let pat    = ret()
    .preceded_by(call().at(PARSE_FN))
    .ret_val(0, var(retval))
    .into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    println!("return value after parse_fn: {:?}", m.output(retval));
}
```

### Example: branch condition — find compares against a constant threshold

```rust
use pattern::{Matcher, Capture, var, if_node, int_lt, any};

let threshold = Capture::new();
let pat = if_node()
    .cond(int_lt(any(), var(threshold)))
    .into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(t) = m.get_uint(threshold, &graph) {
        println!("branch: x < {t}");
    }
}
```

### Example: complex — call in the true branch of `(x & 4) == 0`, arg 2 is a load from a table

This shows how patterns compose: a branch condition, a call inside one branch, and a specific memory access inside that call's argument — all in a single query.

```rust
use pattern::{Matcher, Capture, var, if_node, call, load, add, and, int_eq, int_const};

let x      = Capture::new();
let offset = Capture::new();

let cond = int_eq(and(var(x), int_const(4u64)), int_const(0u64));
let arg2 = load().addr(add(int_const(0x1000u64), var(offset)));

let pat = if_node()
    .cond(cond)
    .true_branch(call().arg(2, arg2))
    .into();

let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat) {
    if let Some(off) = m.get_uint(offset, &graph) {
        println!("load offset {off:#x} → address {:#x}", 0x1000u64 + off as u64);
    }
}
```

### Example: stack-offset recovery from a captured `StackStore`

```rust
use pattern::{Matcher, Capture, stack_store};

let s = Capture::new();
let pat = stack_store().offset_any([-8i64, -16, -24]).capture(s);
let matcher = Matcher::new(&graph);
for m in matcher.find_all(&pat.into()) {
    if let Some(k) = m.stack_offset(s, &graph) {
        println!("stack store at SP + {k}");
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

The IR lifter, optimizer, pattern matcher, and Python bindings (`strider-py`) are all functional. The Python interface is the recommended way to use Strider; the Rust API stays available for embedding and for the `pattern` crate's authoring side. Recent additions:

- **Pattern queries**: `find_all_requirements` (multi-pattern join on shared captures), `int_const_any_of` / `CallPat::at_any` / `StackStorePat::offset_any` (set-membership queries), `Match::stack_offset` / `stack_phi_offsets` (read offsets off captured stack-store nodes).
- **Bounded lift**: `function_max_size` is now strictly enforced — `is_addr_tail_call` ignores `allow_code_before_start_addr` when a max-size is set, fall-through past the bound terminates as `TailCall`, and conditional branches whose successors leave the function collapse cleanly.
- **ELF relocations**: `MemoryMap.apply_elf_relocations` autoloads any missing site sections (e.g. `.got.plt`) so dynamic-relocation application works without staging the section by hand first.
- **uv install**: `uv sync --group dev` + `uv run maturin develop` + `uv run pytest` (PEP 735).
