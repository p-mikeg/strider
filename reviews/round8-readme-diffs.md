# Round 8 — README rewrite proposals

This is a proposal-only document.  No README files are modified by this round.

## Root `README.md` rewrite

The user's ask 10 is "rewrite root `README.md` to focus on the Python API with more examples."  The proposed structure:

### 1. Headline section (10 lines)

```
# strider — binary analysis toolkit

Lift native binaries to a sea-of-nodes IR; query the IR with arbitrary
patterns from Rust or Python.

  Binary → CFG → IR → Optimizations → Pattern Queries (Python)

Strider is built on top of [GHIDRA's Sleigh](...) for instruction decoding.
Architectures: x86, x86_64, AArch64 (LE/BE), ARM (LE/BE/Thumb), MIPS (BE/LE,
32/64), PowerPC (BE/LE, 32/64).
```

### 2. Quick start (Python) — 30 lines

A complete end-to-end example using `strider.run(...)` on a real ELF, using `pattern.call().at(0x...).arg(0, ...)` to find a call site, with output:

```python
import strider
from strider import pattern as pat
from strider.pattern import Capture

mem = strider.MemoryMap.from_elf("/path/to/binary.elf")
arch = strider.SleighArch.x86_64()
cc = strider.CallingConvention.x86_64_systemv()  # NOTE: rename pending; currently `x86_64_systemv_abi`
g = strider.run(arch=arch, cc=cc, mem=mem, entry=0x401000)

# Find every call to malloc(N) where N is a constant
malloc_addr = mem.symbol("malloc")
size_cap = Capture("size")
matches = g.find_all(
    pat.call()
       .at(malloc_addr)
       .arg(0, pat.int_const_any_of([1, 2, 4, 8, 16, 32, 64, 128]).capture(size_cap))
)
for m in matches:
    print(f"malloc({m['size']}) at {m.asm_fingerprint(size_cap)}")
```

### 3. Pattern recipes (Python) — 5-7 short examples

Each ~10 lines — drawn from `crates/strider-py/tests/python/test_pattern_match.py` and `crates/strider-py/tests/python/test_run.py`:

- "Find every store to a stack slot at SP-K with a constant value" (uses `stack_store().offset_any([...]).data(int_const_any_of([...]))`).
- "Find every conditional branch on a comparison with zero" (uses `if_node().cond(int_cmp(IntCmpOp.Equal, any_(), int_const(0)))`).
- "Find call argument flowing from another call's return value" (uses `find_all_requirements` with shared captures).
- "Match an x86_64 syscall and extract the syscall number" (uses `call_other('syscall').arg(0, int_const_any_of([...]))`).
- "Trace a load address back to a stack offset" (uses `load().addr(stack_store_addr_pattern)` and `match.stack_offset(c)`).
- "Verify a value passes through a `__fentry__` instrumentation hook" (uses `per_address_ccs={fentry_addr: x86_64_all_preserving()}`).

### 4. Architecture diagram (10 lines)

ASCII / mermaid diagram of the pipeline, with hyperlinks to per-crate READMEs.

### 5. Build & development (10 lines)

```bash
# Rust workspace
cargo test --workspace
cargo run -p strider --example strider  # rewrites the broken example fixture per round8-3B

# Python bindings (uv-based)
uv sync --group dev
uv run maturin develop
uv run pytest crates/strider-py/tests/python/
```

### 6. Status badges + license footer

(Standard badges + repo license.)

**Total target length:** ~140 lines.  Replace the existing root README (which leans toward Rust-side internals) with this Python-API-first structure.

## Per-crate `README.md` outlines

12 stub READMEs to author (one per crate except `strider-py`, which already has one).  Each ~120-180 lines, structured:

1. **Purpose** (3-5 lines): what the crate does in one paragraph.
2. **Public surface** (10-30 lines): bullet list of every `pub fn` / `pub struct` / `pub trait` an external caller would touch.
3. **Internal architecture** (20-40 lines): module map, key types, how data flows.
4. **Key invariants** (10-20 lines): assumptions that must hold for the crate to function.
5. **Tests** (5-10 lines): where they live, how to run a single test.
6. **Gotchas** (10-20 lines): non-obvious behavior, footguns, asymmetries.

### Per-crate stubs

#### `crates/cfg/README.md`

- **Purpose:** Builds a CFG from a binary using rsleigh; basic blocks (`Region`) and edges (`Branch` / `Fallthrough` / `IfCaseTrue` / `IfCaseFalse`).
- **Public surface:** `Cfg<R>`, `Region`, `RegionBuilder`, `BuilderOptions`, `RegionTerminator` enum.
- **Internal architecture:** `cfg/builder/region_builder.rs` is the engine; `region_id_at_start` ordering invariant; `decode_cache` interns Sleigh `LiftRes`.
- **Key invariants:** `start_addr_to_region_id` is consistent with `BTreeMap` ordering; bounded-lift `is_addr_tail_call` semantics.
- **Gotchas:** Indirect branches lifted as placeholder `BranchIndirect` regions; resolved by strider's orchestrator.

#### `crates/dot/README.md`

- **Purpose:** Renders graphs to `.dot` and `.html` for visual debugging.
- **Public surface:** `GraphDotDumper` trait, `dump_html`, `dump_dot`.
- **Gotchas:** `json_quote` and `escape_dot_label` for safe HTML/SVG output.

#### `crates/entity-utils/README.md`

- **Purpose:** Entity-keyed sets/worklists for `cranelift-entity` IDs.
- **Public surface:** `DenseEntitySet<T>`, `Worklist<T>`.
- **Gotchas:** `DenseEntitySet` is `O(1)` via bit-vector indexed by raw u32; prefer over `FxHashSet<NodeId>` (cross-ref `round8-16-perf.md`).

#### `crates/graphmock/README.md`

- **Purpose:** Mock graph for tests of generic graph utilities.
- **Public surface:** `MockGraph`, `MockNode`.
- **Gotchas:** Recommend folding into `graphwalk/tests/` per `round8-simplifications.md`.

#### `crates/graphwalk/README.md`

- **Purpose:** Generic preorder / postorder traversal utilities.
- **Public surface:** `walk_graph`, `PreOrder`, `PostOrder`.
- **Key invariants:** Cycle detection via visited set.

#### `crates/ir/README.md`

- **Purpose:** Sea-of-nodes IR with dedup-cache, validate, and asm-fingerprint side-table.
- **Public surface:** `Graph`, `NodeKind`, `NodeOutputKind`, `NodeOutputType`, `BuiltFunctionGraph`, `FunctionBuilder`, `validate`, `validate_with_options`.
- **Key invariants:** Asm-fingerprint superset contract (CLAUDE.md spec); dedup cache structural equivalence; reachability scoping in Layer A/B.
- **Gotchas:** `BuiltFunctionGraph::from_graph_and_entry_for_rewrite` returns a partial-state form (`round8-2D-types.md`).  `Graph::asm_fingerprints` exempt set lists `Entry, InitialMemory, InitialVar, FunctionArg, ControlState, MemPhi, VarPhi, ValuePhi, StackStorePhi` (per `validate/layer_c.rs::asm_fingerprint_exempt`).
- **Required correction (per round8-3B-comments.md):** Field-doc on `Graph::asm_fingerprints` lists nonexistent `IfCase` and omits four phi kinds.

#### `crates/opt/README.md`

- **Purpose:** Optimization passes operating on the IR.
- **Public surface:** `OptimizerPipeline`, `Optimizer` trait, every concrete pass.
- **Key invariants:** Fixed-point loop with `MAX_ITERS=1024`; end-of-run `validate` call; asm-fingerprint extension contract per pass.
- **Gotchas:** `default_pipeline` excludes `LoadReadOnly` (config-dependent); `stable_default_pipeline` is the iteration-safe subset; `destructive_default_pipeline` runs once at fixed-point exit.

#### `crates/pattern/README.md`

- **Purpose:** Pattern matching over IR.
- **Public surface:** `Pat`, `Capture`, `Match`, every builder type, every free constructor.
- **Key invariants:** Commutative ops (Add/Mul/And/Or/Xor/IntCmp::{Equal,Carry,Scarry}/FloatCmp::Equal) try both operand orderings; `*_any` empty-set vacuously fails.
- **Gotchas:** `IfPat` direct-layout-only (relies on `IfCondInversion`); lift-time canonicalisation aliases (`sub`, `int_le`, etc.) construct lowered shapes.

#### `crates/pcode-lift/README.md`

- **Purpose:** Pure value-producing pcode → IR lifter; owns register-aliasing logic.
- **Public surface:** `ValueLifter`, `vn_io::{find_largest_fitting_register, vn_mask}`.
- **Key invariants:** All reads/writes through largest containing register; widths 1/2/4/8/10/16/32/64 supported; widths > 16 use degraded mask.
- **Required correction (per round8-3A):** Replace "(planned)" cfg integration text — it shipped.

#### `crates/reader/README.md`

- **Purpose:** Loads ELF binaries, provides `MemReader` for rsleigh, applies dynamic relocations.
- **Public surface:** `ElfFileMemReader`, `apply_elf_relocations`, `apply_elf_relocations_autoload`, `MemRegion`.
- **Gotchas:** Autoload variant scans dynamic relocs and lazily extends `regions` to cover missing site sections (e.g. `.got.plt`).

#### `crates/strider/README.md`

- **Purpose:** Top-level orchestrator: CFG → IR → optimization fixed-point → indirect-branch resolution.
- **Public surface:** `strider::run`, `RunConfig`, `Strider::analyze_cfg`, `Strider::build_optimizer_pipeline`, `GraphRewriter`.
- **Key invariants:** Fixed-point loop with stall budget = `unresolved.len()` post-progress; cap = `2 * pending_at_iter_0 + 4`.
- **Gotchas:** `Strider::analyze_cfg` returns `AnalyzeOutcome { graph, unresolved_branches, region_handles }`; the orchestrator drives the fixed-point and returns `BuiltFunctionGraph`.
- **Required correction (per round8-correctness-cross-arch.md):** `strider::run` currently uses `Builder::with_endianness` which hardcodes `ArchPreset::X86_64`; fix is `Builder::for_arch(opts.strider.arch(), ...)`.

#### `crates/target/README.md`

- **Purpose:** Pure target-description data: `SleighArch`, `CallingConvention`, `BuiltCallingConvention`, `call_other_abi`.
- **Public surface:** Every `SleighArch` preset; every `CallingConvention` preset (userland + kernel + syscall); `call_other_abi::classify`.
- **Key invariants:** `BuiltCallingConvention` resolves register names through Sleigh; unknown names produce `Err`.
- **Required correction (per round8-3A):** Variant list typos (`X8664`, `Mipsbe32`); add `Ppc*` variants, Linux-kernel CC presets, Linux-syscall CC presets, `x86_64_all_preserving`.
- **Gotchas:** `aarch64_aapcs64`, `arm_aapcs`, and PowerPC presets list LR/x30/lr in `callee_saved_regs` — deliberate deviation from ABI specs (`round8-17-graph-soundness.md` B-1/B-2/B-3).

## Recommended sequencing

1. Author root `README.md` first (highest user-facing impact).
2. Author per-crate READMEs in dependency order: `entity-utils`, `graphwalk`, `graphmock`, `dot` (leaves) → `target`, `reader` → `ir`, `pcode-lift` → `cfg`, `pattern`, `opt` → `strider`.
3. Update `crates/strider-py/README.md` to align with the new root README's example flow.

Estimated effort: ~3-4 person-days for all 13 README files combined.
