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
| `strider-opt` | Optimization passes, `OptimizerPipeline`, indirect-branch resolution logic |
| `strider-orchestrator` | Orchestrator (`run`), lift driver, cfg-time indirect-resolver stub; re-exports `strider-opt` as `opt` |
| `strider-ir-test-utils` | Mock-IR helpers with sentinel asm-fingerprint stamping |
| `strider-py` | **Python bindings — the primary user-facing query interface** |
| `dot` | Generic Graphviz / dark-themed HTML renderer |
| `entity-utils` | `cranelift-entity` helpers (`DenseEntitySet`, `Worklist`) |
| `graphwalk` | Generic preorder / postorder graph traversal |

Each crate carries its own `README.md` with details. The full architecture (including invariants and gotchas) lives in the per-crate READMEs and in [`CLAUDE.md`](CLAUDE.md).

---

## Install (Python)

The uv project lives at the workspace root, so run from there:

```bash
uv sync --group dev          # .venv + dev deps
uv run maturin develop       # builds the Rust extension (crates/strider-py)
uv run pytest                # runs the test suite
```

The wheel is `abi3` (Python 3.9+). A pip-based legacy install path is documented in [`crates/strider-py/README.md`](crates/strider-py/README.md).

---

## Quickstart

```python
import strider
from strider.pattern import Capture, var, add, load

# 1. Load an ELF.  `strider.lift.load_elf` returns an `ElfLifter`: one
#    object that IS the loaded binary (`isinstance(prog, strider.lift.Lifter)`
#    is true) — arch + calling convention auto-detected, code + ROM
#    readers wired internally, symbols/entry-point ready.
prog = strider.lift.load_elf("fixtures/out/x86/memory.elf")
#    (kernel / syscall / custom-ABI: pass arch=… / cc=… / apply_relocations=True)

# 2. Lift + optimize one function (symbol name or address).  Pass
#    opts.cfg.function_max_size=N to bound the lift to [entry, entry+N)
#    on stripped binaries; the symbol's recorded size is used by default.
#    Returns (Cfg, Function, unresolved_addrs) — `cfg` is the FINAL
#    resolved CFG `function` was actually lifted from, so no rebuild is
#    needed to render it (see step 4).
cfg, function, unresolved = prog.analyze(
    "array_sum",
    opts=strider.lift.LifterOptions(
        cfg=strider.cfg.CfgOptions(allow_code_before_start_addr=True)
    ),
)

# 3. Query the optimized IR.  Pattern queries live directly on `Function`.
ptr, off = Capture(), Capture()
for hit in function.find_all(load(addr=add(var(ptr), var(off))), ignore_casts=True):
    off_val = hit.const_uint(off)
    print(f"load at {hit.const_uint(ptr)} + {off_val if off_val is not None else '<symbolic>'}")

# 4. Visualise.  The pretty renderer needs a Sleigh (for register names),
#    which only the `Lifter` (`prog`, here an `ElfLifter`) owns.
prog.to_html(function, "graph.html")   # the IR graph
cfg.to_html("cfg.html")                # the control-flow graph (already
                                        # the final resolved CFG from step 2)
```

`prog` also exposes `prog.symbol(name)`, `prog.symbol_size(name)`,
`prog.symbols()`, `prog.entry_point()`, `prog.read(addr, size)`, and
`prog.functions()`.  For non-ELF / firmware / custom data sources, build a
low-level `strider.reader.BufferReader` (raw byte regions) and drive
`strider.lift.lifter(arch, mem, rom=None)` directly — its
`.analyze(addr, cc, opts=...)` returns the same
`(Cfg, Function, unresolved_addrs)` tuple.

### Analyze many functions with one setup

The `Lifter` / `ElfLifter` handle itself is the frozen setup — arch,
Sleigh, and the code/ROM readers are all built once at construction.
Analyse as many functions as you want by calling `.analyze(...)`
repeatedly; `cc` is a required argument of every call, so one handle
can even mix calling conventions across functions:

```python
for fn in prog.functions():
    cfg, function, unresolved = prog.analyze(fn)

# Standalone (non-ELF / firmware) form over a raw BufferReader
# (address targets only — no ELF symbol table):
lft = strider.lift.lifter(arch, mem)
cfg, function, unresolved = lft.analyze(0x8000, cc)
```

---

## Pattern features

The pattern crate covers every IR node kind the lifter emits.  Below are the highest-leverage features when querying a real graph.  In the snippets below `g` is the lifted IR queried — i.e. `g = function` from the quickstart's step 2.

### Set-membership target queries

Match a call against any of N known callees in one pass:

```python
from strider.pattern import call, int_const_any_of

hits = g.find_all(call().at_any([0x1000, 0x2000, 0x3000]))
# Equivalent:
hits = g.find_all(call().target(int_const_any_of([0x1000, 0x2000, 0x3000])))
```

### Multi-pattern joins on shared captures

Find the `K` such that two patterns simultaneously match with the same
binding for a shared capture.  Passing a `list` of patterns to `find_all`
(instead of one) joins them on shared captures — there is no separate
`find_joined` method.  Each result is a single merged `Match` (not a
per-pattern tuple), so every capture from every pattern in the list is
readable straight off it:

```python
from strider.pattern import Capture, add, any_int_const, load, var

# Field-offset recovery: `p->x + p->y` lifts to two loads off the SAME
# base pointer — one bare (`p->x`, offset 0) and one at `p + off` (`p->y`).
# The shared Capture `base` is what joins the two patterns: only a
# pointer that feeds BOTH loads is reported, and `off` reads back the
# second field's byte offset.
_, g, _ = prog.analyze("struct_field_load")
base, off = Capture(), Capture()
for m in g.find_all(
    [load(addr=var(base)), load(addr=add(var(base), any_int_const(off)))],
    ignore_casts=True,
):
    print(f"struct field at offset {m.const_uint(off):#x}")
```

### Stack-only filter

Restrict a `Load` / `Store` match to SP-relative accesses (i.e. nodes
that `StackOffsetDetect` has stamped with an offset).  Use `.stack_only()`
to gate the match without pinning a specific offset, or `.stack_offset(k)`
to require exactly `sp + k`:

```python
from strider.pattern import store

for hit in g.find_all(store().stack_only()):
    print(f"stack store: {hit}")
```

### Asm-fingerprint attribution

Every IR node carries the sorted, deduped list of machine-instruction addresses whose lift contributed to its value.  Use it to map a matched value back to source assembly:

```python
c = Capture()
for hit in g.find_all(call().capture(c)):
    addrs = hit.asm_fingerprint(c)
    print(f"call {hit.const_uint(c):#x} contributed by asm at: "
          + ", ".join(f"{a:#x}" for a in addrs))
```

`Match.asm_fingerprint` returns `[]` for "structural" node kinds (Entry, InitialMemory, Region, MemPhi, Phi, InitialVar) whose existence is synthesised by the IR builder rather than tied to a specific asm instruction.

### Predicate guards

```python
from strider.pattern import var

# Match any int that is divisible by 16.
def is_aligned(m):
    v = m.const_uint(c)
    return v is not None and v % 16 == 0

c = Capture()
hits = g.find_all(var(c).when(is_aligned))
```

The predicate proxy is short-lived: it's only valid during the predicate call.  Storing it for later use silently returns `None` from accessors.

### Per-node introspection (no pattern needed)

```python
g.node_ids()                  # [0, 1, 2, ...] every reachable node
n = g.node(node_id)           # a `Node` handle on that node
n.kind()                      # "IntConst", "Call", "Phi", ...
n.asm_fingerprint()           # [0x1000, 0x1004, ...]
n.call_other_name()           # "cpuid" or None
g.validate()                  # None on success, error string on failure
g.compact()                   # drop unreachable nodes
```

### Raw graph dump (debugging the real shape)

`Lifter.to_html`/`Lifter.to_dot` (used in step 4 of the quickstart) render
a *pretty* view (constants inlined, virtual nodes for Call clobbers / If
branches) and need a Sleigh, which only the `Lifter` owns. To see the
graph **exactly as stored** — one node per `NodeId` reachable from entry,
one edge per input edge, side-tables (stack offset, phi tag, asm
fingerprints, …) shown inline, no inlining or virtual nodes — call the
same-named methods directly on `Function` instead (no Sleigh needed):

```python
g.to_dot()                    # Graphviz DOT, 1:1 with the stored graph
g.to_html()                   # same, wrapped in self-contained HTML
g.to_dot("raw.dot")           # write DOT to a file
g.to_html("raw.html")         # write HTML to a file
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
| `IfCondInversion` | Canonicalises `If(Xor(C, IntConst(1)):I1){A}{B}` into `If(C){B}{A}` so every `If` has a non-negated cond (logical NOT is `Xor(_, IntConst(1)):I1` since bitwise complement is `Xor(x, all_ones)`). |
| `RedundantPhis` | Eliminates `Phi`/`MemPhi`/`Region` with a single reachable predecessor.  (The phi's optional source-varnode tag lives in `Function::value_vn`, read via `get_vn_for_value`.) |
| `DeadBranchElimination` | Removes `If` whose condition is constant; strips dead control edges. |
| `LoadReadOnly` | Folds `Load`s of constant addresses against a caller-supplied ROM. |
| `StackOffsetDetect` | Populates `Function::stack_offsets` with the SP-relative offset of every Store/Load whose address resolves to `sp + K`. |
| `LoadForward` | Forwards stack-tagged `Store` values to subsequent same-offset `Load`s. |
| `CallStackArgCollect` (post-pass) | Collects positional stack args at `Call` sites. |
| `FunctionArgDetect` (post-pass) | Detects stack-passed arg reads (`Load[sp + K]`) and records their carrier values in `Function::arg_index_to_values`.  Register-passed args are recorded at builder entry (`FunctionBuilder::set_entry_region`), not by this pass (carrier `NodeId` is `InitialVar` for register args, `Load` for stack args).  There is no `FunctionArg` `NodeKind` variant. |

`opt::indirect_branch_resolve` is a module of free-function classifiers (link-register-return, tail call, jump table, stack-array dispatch) and in-place IR editors (`apply_link_register`, `apply_tail_call`).  A constant target reached through cast/extend chains is resolved by the prior `ConstantFold` pass rather than a dedicated arm here.  There is no `Optimizer`-implementing struct — the strider orchestrator calls them directly, outside any pipeline.

---

## Troubleshooting: why didn't my pattern match?

A few common surprises when a pattern that "should obviously match" returns no hits:

1. **`If(Xor(C, IntConst(1)):I1)` doesn't exist in optimised IR.**  `IfCondInversion` rewrites it to `If(C){B}{A}` (logical NOT is `Xor(_, IntConst(1)):I1` since bitwise complement is `Xor(x, all_ones)`).  Write your `if_node()` pattern against the canonical (non-negated) form.

2. **Lift-time canonicalisation aliases.**  `IntSub`/`IntLessEqual`/`IntSlessEqual`/`IntNotEqual`/`FloatSub`/`FloatNotEqual`/`FloatLessEqual`/`FloatNan` are NOT IR primitives — the lifter lowers them at lift time.  Use the alias constructors (`pattern::sub`, `pattern::int_le`, `pattern::int_sle`, `pattern::float_sub`, `pattern::float_ne`, `pattern::float_le`) rather than the raw cmp ops.  The `FLOAT_NAN(x)` shape lowers to `Xor(FloatEqual(x, x), IntConst(1)):I1` — match it in Rust by composing `bool_not(float_eq(x, x))` (the `bool_not` builder emits the Xor-with-1 shape).  The Python binding exposes a `pattern.float_is_nan(x)` convenience constructor that builds the same shape.

3. **Commutativity.**  `add` / `mul` / `and` / `or` / `xor` (and the boolean equivalents) and `IntCmpOp::{Equal,Carry,Scarry}` plus `FloatCmpOp::Equal` automatically try both operand orderings.  Non-commutative ops (`sub`, `div`, `shl`, `int_lt`, …) keep stated order.  Use `int_binary("Add", l, r).ordered()` to force left-to-right matching on a typed binary builder.  `.ordered()` on a finalised `Pat` (returned by free constructors like `add(x, y)`) raises `PatternError` because commutativity is baked in at construction.

4. **`phi()` matches a tagged `Phi` only** (one whose `Function::get_vn_for_value` on its output is `Some`, i.e. the lifter-emitted SSA φ for a register-aliased read).  Use `mem_phi()` for the memory-token phi at join points.  There is currently no pattern builder for an anonymous value phi (one with no `value_vn` tag).

5. **Optimisation level.**  Patterns generally run on the post-`default_pipeline` graph.  Pre-optimisation IR may contain shapes (multi-input `MemPhi`, single-pred `Region`, `Or(IntConst(0):I1, x)`, etc.) that `RedundantPhis` / `ConstantFold` would have collapsed.

6. **Width mismatch / signedness.**  `int_const(42)` matches a constant whose value equals 42 at the node's own width, so a `42` lifted as `IntConst(42 : U32)` and one lifted as `IntConst(42 : U64)` both match.  The subtlety is *signed* values: a negative constant narrowed to U32 (e.g. `-50` as `0xFFFFFFCE`) is a different bit pattern from its 64-bit sign-extension, so `int_const(-50)` won't match the narrowed form.  Use `signed_int_const(-50)`, which matches the value sign-correctly at whatever width the node carries.

When stuck, dump the IR (`prog.to_html(function, "graph.html")` and open in a browser) and walk forward from `entry` looking for the shape you expected.

---

## Rust API

The Rust crates are usable directly when scripting in Rust is a better fit than Python.  Each crate has a top-level `README.md` documenting its public surface; below is an end-to-end skeleton.

```rust
use std::collections::HashMap;
use strider_orchestrator::{run, Config, Strider};
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
[`crates/strider-orchestrator/examples/orchestrator_demo.rs`](crates/strider-orchestrator/examples/orchestrator_demo.rs).
For pattern-construction details see
[`crates/strider-pattern/src/`](crates/strider-pattern/src/).
For per-pass details see
[`crates/strider-opt/src/`](crates/strider-opt/src/).

---

## Build & test

```bash
# Build the workspace
cargo build --workspace

# Run all Rust tests
cargo test --workspace

# Lint (treats warnings as errors)
cargo clippy --workspace -- -D warnings

# Python tests (rebuild the wheel first if Rust changed) — uv project is at the repo root
uv run maturin develop --release
uv run pytest
```

`fixtures/` contains test binaries (some via Git LFS — install with `git lfs install && git lfs pull`).

---

## Project status

The 11-crate workspace is internally consistent; `cargo test --workspace` and `cargo clippy --workspace -- -D warnings` are part of CI.  Per-crate READMEs in each `crates/<name>/README.md` document the per-crate surface.
