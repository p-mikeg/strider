<p align="center">
  <img src="docs/strider.png" alt="Strider" width="320">
</p>

# Strider

> *"He's one of them Rangers. Dangerous folk they are — wandering the Wilds."*
> — Barliman Butterbur, *The Fellowship of the Ring*

**Strider** is named after Aragorn's ranger alias in Tolkien's Middle-earth — a tracker who moves quietly through dark places, finding what others miss. Here, Strider hunts through binary code, letting you ask any question about how a function behaves — without source code or debug symbols.

---

## What It Does

Strider lifts native binaries to a sea-of-nodes IR and exposes it for arbitrary pattern queries from Python. You can ask things like: what offset does this function access off a pointer? what value does it pass to `malloc`? what does it return when the input matches this condition?

**Pipeline:**

```
Binary → CFG → IR → Optimizations → Pattern Queries (Python)
```

1. **Read** — loads an ELF binary and exposes its memory to the lifter
2. **Lift** — GHIDRA's Sleigh engine lifts machine instructions to p-code
3. **CFG** — builds a Control Flow Graph of basic blocks from the p-code
4. **IR** — translates the CFG into a sea-of-nodes IR graph with SSA-like variable tracking
5. **Optimize** — runs optimization passes to simplify the IR before querying
6. **Query** — write patterns in Python; captures extract the values you care about

---

## Architecture (high level)

| Crate | Role |
|-------|------|
| `strider-reader` | ELF loader, memory reader for rsleigh |
| `strider-lift` | Pcode → IR value lifter (register aliasing) + CFG construction |
| `strider-ir` | Sea-of-nodes IR graph (`Graph`, `FunctionBuilder`) |
| `strider-target` | SleighArch + CallingConvention presets, CallOther ABI table |
| `strider-analyze` | Orchestrator, optimizer pipeline, pattern matcher, indirect-branch resolver |
| `strider-pattern-macros` | Proc-macro that emits the PyO3 mirror of each hand-written Rust pattern builder |
| `strider-ir-test-utils` | Mock-IR helpers with sentinel asm-fingerprint stamping |
| `strider-py` | **Python bindings — the primary user-facing query interface** |
| `dot` | Generic Graphviz / dark-themed HTML renderer |
| `entity-utils` | `cranelift-entity` helpers (`DenseEntitySet`, `Worklist`) |
| `graphwalk` | Generic preorder / postorder graph traversal |

Each crate carries its own `README.md` with details. The full architecture (including invariants and gotchas) lives in the per-crate READMEs and in [`CLAUDE.md`](CLAUDE.md).

---

## Install (Python)

```bash
cd crates/strider-py
uv sync --group dev          # .venv + dev deps
uv run maturin develop       # builds the Rust extension
uv run pytest                # runs the test suite
```

The wheel is `abi3` (Python 3.9+). A pip-based legacy install path is documented in [`crates/strider-py/README.md`](crates/strider-py/README.md).

---

## Quickstart

```python
import strider
from strider.pattern import Capture, var, add, load, call, int_const

# 1. Load a binary into a MemoryMap.  apply_elf_relocations is
#    autoload-by-default — it lazily extends with .got.plt etc.
mem = strider.MemoryMap()
mem.add_region_from_elf("fixtures/out/x86/memory.elf")
mem.apply_elf_relocations("fixtures/out/x86/memory.elf")

# 2. Run the full pipeline in one call.
#    `function_max_size` bounds the lifter to [entry, entry+N) — set
#    it on stripped binaries where the function boundary is unknown.
result = strider.run(
    arch=strider.SleighArch.x86(),
    cc=strider.CallingConvention.x86_cdecl(),
    mem=mem, rom=mem,
    entry=mem.symbol("array_sum"),
    function_max_size=None,        # or e.g. 0x200
    allow_code_before_start_addr=True,
)

# 3. Query the optimized graph.  `result.function` is the lifted IR.
ptr, off = Capture(), Capture()
for hit in result.function.find_all(
    load(addr=add(var(ptr), var(off))),
    ignore_casts=True,
):
    print(f"load at {hit.uint(ptr)} + {hit.uint(off):#x}")

# 4. Visualise.
result.cfg.to_html("cfg.html")
result.function.to_html("graph.html")
```

---

## Pattern features

The pattern crate covers every IR node kind the lifter emits.  Below are the highest-leverage features when querying a real graph.  In the snippets below `g` is the lifted IR queried — i.e. the `result.function` from the quickstart.

### Set-membership target queries

Match a call against any of N known callees in one pass:

```python
from strider.pattern import call, int_const_any_of

hits = g.find_all(call().at_any([0x1000, 0x2000, 0x3000]))
# Equivalent:
hits = g.find_all(call().target(int_const_any_of([0x1000, 0x2000, 0x3000])))
```

### Multi-pattern joins on shared captures

Find the `K` such that two patterns simultaneously match with the same binding for a shared capture:

```python
from strider.pattern import (
    Capture, add, any_int_const, call, load, initial_var_for, int_const,
)

# Field-offset recovery: vn_open(&nd, ...); script_vp = nd.ni_vp;
rbp = sleigh.reg("RBP")
k_call, k_load = Capture(), Capture()
for tup in g.find_all_requirements([
    call().target(int_const(VN_OPEN))
        .arg(0, add(initial_var_for(rbp), any_int_const(k_call))),
    load().addr(add(initial_var_for(rbp), any_int_const(k_load))),
]):
    field_offset = (tup[1].uint(k_load) - tup[0].uint(k_call)) & 0xFFFFFFFFFFFFFFFF
    print(f"field offset = {field_offset:#x}")
```

### Stack-offset recovery

Capture an SP-relative `Store` and read its compile-time offset (`g` is
the `result.function` from the quickstart):

```python
from strider.pattern import OffsetCapture, store

c = OffsetCapture()
for hit in g.find_all(store().offset_capture(c)):
    print(f"stack store at offset {hit.captured_offset(c)}")
```

### Asm-fingerprint attribution

Every IR node carries the sorted, deduped list of machine-instruction addresses whose lift contributed to its value.  Use it to map a matched value back to source assembly:

```python
c = Capture()
for hit in g.find_all(call().capture(c)):
    addrs = hit.asm_fingerprint(c)
    print(f"call {hit.uint(c):#x} contributed by asm at: "
          + ", ".join(f"{a:#x}" for a in addrs))
```

`Match.asm_fingerprint` returns `[]` for "structural" node kinds (Entry, InitialMemory, Region, MemPhi, Phi, InitialVar) whose existence is synthesised by the IR builder rather than tied to a specific asm instruction.

### Predicate guards

```python
from strider.pattern import any_, var

# Match any int that is divisible by 16.
def is_aligned(m):
    return m.uint(c) % 16 == 0

c = Capture()
hits = g.find_all(var(c).when(is_aligned))
```

The predicate proxy is short-lived: it's only valid during the predicate call.  Storing it for later use silently returns `None` from accessors.

### Per-node introspection (no pattern needed)

```python
g.node_kind(node_id)          # "IntConst", "Call", "Phi", ...
g.node_ids()                  # [0, 1, 2, ...] every reachable node
g.asm_fingerprint(node_id)    # [0x1000, 0x1004, ...]
g.call_other_name(node_id)    # "cpuid" or None
g.validate()                  # None on success, error string on failure
g.compact()                   # drop unreachable nodes
```

### Raw graph dump (debugging the real shape)

`to_html`/`html_str` render a *pretty* view (constants inlined, virtual
nodes for Call clobbers / If branches). To see the graph **exactly as
stored** — one node per `NodeId` reachable from entry, one edge per input
edge, side-tables (stack offset, phi tag, asm fingerprints, …) shown
inline, no inlining or virtual nodes — use the raw renderer:

```python
g.raw_dot_str()               # Graphviz DOT, 1:1 with the stored graph
g.raw_html_str()              # same, wrapped in self-contained HTML
g.to_raw_dot("raw.dot")       # write DOT to a file
g.to_raw_html("raw.html")     # write HTML to a file
```

---

## Bounded vs unbounded lifts

Set `function_max_size=N` to constrain the lifter to `[entry, entry+N)`.  Branches and `bl`/`call` targets that fall outside this window are classified as tail calls.  Useful when:

- The function boundary isn't known a priori (stripped binary).
- The next function's first instruction is reachable via fall-through and you want to stop there.
- You want a tail-call to be modelled as a tail call rather than as in-function code.

Without `function_max_size`, set `allow_code_before_start_addr=True` to accept backward branches as in-function (e.g. for jump tables that target a switch prologue *before* the function entry per the compiler's layout).

---

## Optimizer pipelines

`opt::default_pipeline()` runs every rewriting pass in a shared fixed-point loop.  `opt::stable_default_pipeline()` runs only those whose rewrites survive subsequent phi-input growth (used while the indirect-branch resolver is still iterating).  `opt::destructive_default_pipeline()` runs node-removal passes safely only at the fixed point.

| Pass | What it does |
|------|-------------|
| `ConstantFold` | Constant arithmetic, comparisons, booleans, truncation, extension; algebraic identities. |
| `KnownBits` | Bit-level zero/one propagation. Folds outputs whose every bit is determined to a constant. |
| `FlagCmpCanonicalize` | Recognises CPU-flag-tree comparisons (AArch64 NZCV / x86 EFLAGS / ARM+Thumb) — both the raw flag trees and the decomposed shapes left after an inverted-sense branch is normalised — and rewrites them to high-level `IntCmpOp`. |
| `IfCondInversion` | Canonicalises `If(BitNot(C)){A}{B}` into `If(C){B}{A}` so every `If` has a non-negated cond (logical NOT is the 1-bit `BitNot`). |
| `RedundantPhis` | Eliminates `Phi`/`MemPhi`/`Region` with a single reachable predecessor.  (The phi's optional source-varnode tag lives in `Graph::phi_var_tag`.) |
| `DeadBranchElimination` | Removes `If` whose condition is constant; strips dead control edges. |
| `LoadReadOnly` | Folds `Load`s of constant addresses against a caller-supplied ROM. |
| `StackOffsetDetect` | Populates `Function::stack_offsets` with the SP-relative offset of every Store/Load whose address resolves to `sp + K`. |
| `LoadForward` | Forwards stack-tagged `Store` values to subsequent same-offset `Load`s. |
| `CallStackArgCollect` (post-pass) | Collects positional stack args at `Call` sites. |
| `FunctionArgDetect` (post-pass) | Canonicalises register- and stack-passed arg reads at the function boundary by populating `Function::arg_index_to_nodes` (carrier `NodeId` is `InitialVar` for register args, `Load` for stack args).  There is no `FunctionArg` `NodeKind` variant. |

`opt::indirect_branch_resolve` is a module of free-function classifiers (link-register-return, tail call, jump table, stack-array dispatch) and in-place IR editors (`apply_link_register`, `apply_tail_call`).  A constant target reached through cast/extend chains is resolved by the prior `ConstantFold` pass rather than a dedicated arm here.  There is no `Optimizer`-implementing struct — the strider orchestrator calls them directly, outside any pipeline.

---

## Troubleshooting: why didn't my pattern match?

A few common surprises when a pattern that "should obviously match" returns no hits:

1. **`If(BitNot(C))` doesn't exist in optimised IR.**  `IfCondInversion` rewrites it to `If(C){B}{A}` (the 1-bit `BitNot` is logical NOT).  Write your `if_node()` pattern against the canonical (non-negated) form.

2. **Lift-time canonicalisation aliases.**  `IntSub`/`IntLessEqual`/`IntSlessEqual`/`IntNotEqual`/`FloatSub`/`FloatNotEqual`/`FloatLessEqual`/`FloatNan` are NOT IR primitives — the lifter lowers them at lift time.  Use the alias constructors (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`) rather than the raw cmp ops.  The `FLOAT_NAN(x)` shape lowers to `BitNot(FloatEqual(x, x))` at `I1` — match it in Rust by composing `bool_not(float_eq(x, x))`.  The Python binding exposes a `pattern.float_is_nan(x)` convenience constructor that builds the same shape.

3. **Commutativity.**  `add` / `mul` / `and` / `or` / `xor` (and the boolean equivalents) and `IntCmpOp::{Equal,Carry,Scarry}` plus `FloatCmpOp::Equal` automatically try both operand orderings.  Non-commutative ops (`sub`, `div`, `shl`, `int_lt`, …) keep stated order.  Use `int_binary("Add", l, r).ordered()` to force left-to-right matching on a typed binary builder.  `.ordered()` on a finalised `Pat` (returned by free constructors like `add(x, y)`) raises `PatternError` because commutativity is baked in at construction.

4. **`phi()` matches a tagged `Phi` only** (one whose `Graph::phi_var_tag` entry is `Some`, i.e. the lifter-emitted SSA φ for a register-aliased read).  Use `mem_phi()` for the memory-token phi at join points; `value_phi()` for the anonymous value phi `LoadForward` synthesises (its `phi_var_tag` is `None`).

5. **Optimisation level.**  Patterns generally run on the post-`default_pipeline` graph.  Pre-optimisation IR may contain shapes (multi-input `MemPhi`, single-pred `Region`, `Or(IntConst(0):I1, x)`, etc.) that `RedundantPhis` / `ConstantFold` would have collapsed.

6. **Width mismatch / signedness.**  `int_const(42)` matches a constant whose value equals 42 at the node's own width, so a `42` lifted as `IntConst(42 : U32)` and one lifted as `IntConst(42 : U64)` both match.  The subtlety is *signed* values: a negative constant narrowed to U32 (e.g. `-50` as `0xFFFFFFCE`) is a different bit pattern from its 64-bit sign-extension, so `int_const(-50)` won't match the narrowed form.  Use `signed_int_const(-50)`, which matches the value sign-correctly at whatever width the node carries.

When stuck, dump the IR (`result.graph.to_html("graph.html")` and open in a browser) and walk forward from `entry` looking for the shape you expected.

---

## Rust API

The Rust crates are usable directly when scripting in Rust is a better fit than Python.  Each crate has a top-level `README.md` documenting its public surface; below is an end-to-end skeleton.

```rust
use std::collections::HashMap;
use strider_analyze::{run, Config, Strider};
use strider_target::{CallingConvention, SleighArch};

let arch = SleighArch::x86_64();
let obj = strider_reader::load_elf("path/to/binary.elf")?;
let mem = strider_reader::ElfFileMemReader::from_object(&obj)?;
let sleigh = rsleigh::Sleigh::new(arch.sla_spec(), arch.pspec(), mem)?;

// `Strider::new` takes the raw `CallingConvention` and resolves it
// against `sleigh.regs()` internally.
let strider = Strider::new(
    arch,
    sleigh.regs()?,
    CallingConvention::x86_64_systemv()?,
)?;

// `run` returns the fully-optimised `strider_ir::Function`.  Use
// `function.entry()` for the IR graph entry, `function.to_html(...)` for
// debug rendering, etc.
let function = run(Config {
    strider: &strider,
    start_addr: 0x1000_u64.into(),  // MachineInsnAddr
    sleigh,
    rom: None,
    fn_max_size: None,
    allow_code_before_start_addr: false,
    compact: true,
    per_address_ccs: HashMap::new(),
})?;
```

For a runnable end-to-end example see
[`crates/strider-analyze/examples/orchestrator_demo.rs`](crates/strider-analyze/examples/orchestrator_demo.rs).
For pattern-construction details see
[`crates/strider-analyze/src/pattern/`](crates/strider-analyze/src/pattern/).
For per-pass details see
[`crates/strider-analyze/src/opt/`](crates/strider-analyze/src/opt/).

---

## Build & test

```bash
# Build the workspace
cargo build --workspace

# Run all Rust tests
cargo test --workspace

# Lint (treats warnings as errors)
cargo clippy --workspace -- -D warnings

# Python tests (rebuild the wheel first if Rust changed)
cd crates/strider-py
uv run maturin develop --release
uv run pytest tests/python/
```

`fixtures/` contains test binaries (some via Git LFS — install with `git lfs install && git lfs pull`).

---

## Project status

The 11-crate workspace is internally consistent; `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` are part of CI.  Per-crate READMEs in each `crates/<name>/README.md` document the per-crate surface; the design specs that drove major refactors live under `docs/superpowers/specs/` and `docs/superpowers/plans/`.
