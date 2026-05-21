# Cross-Crate Generalization Audit – Round 14

## Finding 1: NodeKind ↔ PatKind Mirror Gaps

**Files:**
- `crates/ir/src/node/kind.rs` (lines 27–243: 50+ NodeKind variants)
- `crates/pattern/src/pat/builders/mod.rs` (re-exported builders)
- `crates/pattern/src/pat/ctor/mod.rs` (free constructors)

**Issue:** NodeKind enumerates 50+ operation types; pattern builders cover the common path but omit niche kinds:
- `IntConstWide` — no dedicated `wide_const()` constructor (must use `.when()` predicate)
- `ValuePhi` — has `ValuePhiPat` builder but no ergonomic ctor (only `phi()`)
- `CPoolRef`, `SegmentOp`, `New` — no pattern builders (opaque to queries)
- `StackStorePhi` — has `StackStorePhiPat` builder but incomplete API (no offset-set matching)

**Concrete Gaps:** When `ir` adds a new NodeKind, pattern crate doesn't auto-mirror. Last 3 kinds added (`ValuePhi`, `StackStorePhi`, `StackStore`) required manual pattern builder additions.

**Proposal:** Introduce sealed procedural macro `#[mirror_node_kind]` that generates both `NodeKind` enum variant and corresponding `PatKind` builder boilerplate. Single source of truth in `crates/target/node_kinds.toml` or similar manifest. Removes mechanical duplication; designer still chooses query-ability per kind.

**Difficulty:** Moderate (macro design + build-script phase, but low semantic complexity)
**LOC delta:** -50 to -80 LOC pattern (gains in ir insignificant vs macro infrastructure)
**Migration risk:** High — changes node-definition authority; all downstream pattern builders must adapt

---

## Finding 2: Asm-Fingerprint Propagation API Fragmentation

**Files:**
- `crates/ir/src/graph/store.rs` (lines 440–480: `extend_asm_fingerprint`, `extend_asm_fingerprint_from`)
- `crates/opt/src/constant_fold/mod.rs` (line 137: manual `ctx.extend_asm_fingerprint_from(new_node, node_id)`)
- `crates/opt/src/flag_cmp_canonicalize/mod.rs` (lines 146, 162, 179: explicit per-node calls)
- `crates/opt/src/dead_branch/mod.rs` (line 91: `ctx.extend_asm_fingerprint_from(ctrl_in_node, if_node)`)
- `crates/opt/src/function_args/mod.rs` (line 162: `ctx.extend_asm_fingerprint_from(new_node, initial_var)`)
- `crates/opt/src/pipeline.rs` (line 28: `function.extend_asm_fingerprint_from(new_node, old_node)`)

**Issue:** Every opt pass that builds intermediate nodes manually calls `extend_asm_fingerprint_from` at construction. No automatic union. When a pass rewrites `A → [B, C, D]`, author must:
1. Create B, C, D individually via `create_node`
2. Explicitly union each via `extend_asm_fingerprint_from(new_node, old_node)`

Pattern rewriter provides built-in absorption only for the outermost node, not intermediate construction chains.

**Proposal:**
- Add optional `derives_from: NodeId` parameter to `ir::Graph::create_node()` (or separate `create_derived()` method)
- When set, auto-union the fingerprint at creation time
- Update 6 opt passes to use the new path (reduces 5–10 LOC per pass)
- `pattern::RewriteCtx` already has `create_node` access; expose `create_derived(kind, inputs, derives_from)` helper

**Difficulty:** Trivial (local API addition + 6 mechanical call-site updates)
**LOC delta:** -25 to -40 LOC (net savings in passes, small cost in ir builder)
**Migration risk:** Trivial — additive API, existing code unaffected (but should migrate for hygiene)

---

## Finding 3: Register Aliasing Logic Split (pcode-lift ↔ ir ↔ opt)

**Files:**
- `crates/pcode-lift/src/vn_io.rs` (lines 141–300: `find_largest_fitting_register`, `read_vn`, `write_vn` — full container/sub-register math)
- `crates/ir/src/builder/mod.rs` (lines 134, 488–525: `largest_container: OnceCell<HashMap<Vn, Vn>>` cache + `largest_container_for()` O(V²) recomputation)
- `crates/opt/src/stack_load_forward/mod.rs` (uses builder cache to resolve partial-overlap reads)

**Issue:** Three separate aliasing-aware lookups:
1. **pcode-lift's full scan** — every read/write traverses all registered variables to find the container (O(V) per call, but amortised over a function's lifts)
2. **ir builder's cache** — O(V²) upfront map built on first `largest_container_for` call; reused for every subsequent lookup
3. **opt's partial-overlap logic** — queries builder cache + applies endianness-aware slicing

The three paths are functionally equivalent but isolated: pcode-lift doesn't consult the builder cache, and opt doesn't go back to pcode-lift. When a new container family (e.g., new x87 variants) ships, all three sites must update independently.

**Proposal:** Expose a shared `ArchAliasing` registry in `target` crate that both pcode-lift and ir-builder query:
- `ArchAliasing::new(arch_preset, sleigh_regs) → AliasMap`
- `AliasMap::find_container(vn) → Option<Vn>` (memoized internally)
- Both pcode-lift and ir-builder use this single source

**Difficulty:** Mechanical (move alias data to target, inject into both constructors, validate parity)
**LOC delta:** -40 to -60 LOC (removes duplicate cache logic; pcode-lift stays O(V) per call but now consults shared map)
**Migration risk:** Low — internal refactor, no API change at opt/strider level

---

## Finding 4: Endianness Threading (target ↔ pcode-lift ↔ opt ↔ cfg)

**Files:**
- `crates/target/src/arch.rs` (owns `Endianness` type)
- `crates/cfg/src/cfg/builder/mod.rs` (lines 60–61: `endianness: target::Endianness` stored per builder)
- `crates/cfg/src/cfg/builder/indirect_resolve.rs` (passes endianness to value-lifter)
- `crates/opt/src/stack_load_forward/mod.rs` (takes `endianness: target::Endianness` as parameter for partial-overlap masking)
- `crates/pcode-lift/src/lib.rs` (ValueLifter carries it for sub-register slicing)

**Issue:** Endianness is threaded inconsistently:
- cfg builder stores it (line 61) and threads to indirect_resolve (implicit)
- opt::StackLoadForward is passed it explicitly at construction
- pcode-lift's ValueLifter carries it
- opt::LoadReadOnly also takes it

No single "CCContext" or "ArchContext" that bundles {preset, endianness, calling_convention}. Each consumer extracts what it needs from different sources.

**Proposal:** Create `target::ArchContext`:
```rust
pub struct ArchContext {
    pub preset: ArchPreset,
    pub endianness: Endianness,
}
```
Thread this single struct through cfg → opt construction instead of extracting fields piecemeal. Reduces error surface (can't thread wrong endianness to a preset mismatch).

**Difficulty:** Trivial (new struct + 4–5 call-site signatures update)
**LOC delta:** +20 LOC (new struct, minor refactor), net neutral or slightly positive
**Migration risk:** Low — additive wrapper around existing fields

---

## Finding 5: CallingConvention Threading (target ↔ ir ↔ opt ↔ strider)

**Files:**
- `crates/target/src/lib.rs` (re-exports `BuiltCallingConvention`)
- `crates/ir/src/builder/mod.rs` (FunctionBuilder consumes it)
- `crates/strider/src/orchestrator.rs` (lines 124, 320, 670, 786: passes `per_address_built_ccs` HashMap through every iteration)
- `crates/strider/src/strider/mod.rs` (lines 29, 45: stores as `&'a HashMap<u64, BuiltCallingConvention>`)
- `crates/opt/src/function_args/mod.rs` (consumes CC metadata for arg detection)
- `crates/opt/src/stack_store_detect/mod.rs` (consumes stack_ptr from CC)
- `crates/opt/src/stack_load_forward/mod.rs` (consumes endianness, calls into opt pipeline)

**Issue:** Calling convention is passed as:
1. **By reference** in strider orchestrator's LoopState (HashMap<addr, BCC>)
2. **By reference** in IrStrider per-iteration context
3. **By value** in opt pipeline constructors (e.g., `StackStoreDetect::new(&cc)`)
4. **Deconstructed** in passes (e.g., `StackLoadForward` takes just `endianness` + `stack_ptr`)

No uniform "calling-convention consumer contract." Some passes need the full BCC; others need specific fields. Strider's `per_address_ccs` is HashMap but most lookup is single-shot (one function per run).

**Proposal:** Create `opt::CallConventionCtx`:
```rust
pub struct CallConventionCtx {
    pub stack_ptr: rsleigh::Vn,
    pub endianness: target::Endianness,
    pub ret_val_regs: &'static [rsleigh::Vn],
}
```
Extract this upfront in strider, thread it to opt passes. Passes take what they need; no deconstructed fields scattered across call sites.

**Difficulty:** Mechanical (new struct + 4–6 pass signatures)
**LOC delta:** -15 to -25 LOC (consolidates field threading)
**Migration risk:** Low — internal refactor

---

## Finding 6: Builder::for_arch + ArchPreset Dispatch Consistency

**Files:**
- `crates/cfg/src/cfg/builder/mod.rs` (lines 92–109: `for_arch(arch, ...) → Builder` sets preset + endianness atomically)
- `crates/target/src/call_other_abi.rs` (owns `classify(preset: ArchPreset, name) → CallOtherClass` table)
- `crates/cfg/src/cfg/builder/region_builder.rs` (line ~210: calls `classify(builder.preset, name)`)
- `crates/strider/src/strider/insn/mod.rs` (presumably also calls `classify` with same preset)

**Issue:** Preset → CC and preset → CallOther dispatch are independent tables. No "PresetCapabilities" registry that ensures both lookups return compatible results. When x86_64 arch object ships, cfg calls `for_arch(x86_64)` which sets preset correctly; later CallOther name lookup also uses preset. But there's no shared invariant enforcing both use the same source.

**Proposal:** Add `target::PresetCapabilities` registry:
```rust
pub struct PresetCapabilities {
    pub preset: ArchPreset,
    pub ccs: &'static [CallingConvention],  // all known CCs for this arch
    pub call_others: &'static CallOtherMap,  // dispatch table
}
pub fn capabilities_for(preset: ArchPreset) -> &'static PresetCapabilities
```
Both cfg builder and strider lift driver query this single registry, ensuring consistency.

**Difficulty:** Mechanical (new registry + 2–3 lookup sites)
**LOC delta:** +30 to +50 LOC (new registry module), net small positive
**Migration risk:** Trivial — purely internal organization

---

## Finding 7: GraphRef Implementation Gap (graphwalk ↔ cfg ↔ ir)

**Files:**
- `crates/graphwalk/src/lib.rs` (lines 33–53: trait `GraphRef` with `try_successors`)
- `crates/ir/src/walk.rs` (uses graphwalk via `ir::walk::GraphWalkSuccs` wrapper)
- `crates/ir/src/function.rs` (walk_graph uses it)
- `crates/cfg/src/cfg/mod.rs` (uses petgraph StableDiGraph directly; no GraphRef impl)

**Issue:** `graphwalk::GraphRef` is the abstract graph interface. `ir::Graph` is accessed via custom `GraphWalkSuccs` wrapper that implements the trait. `cfg::Cfg` holds a `StableDiGraph` internally (petgraph) but doesn't expose a GraphRef implementation. Any code wanting to run reachability or dominance algorithms on both IR and CFG regions must write separate traversals or use petgraph's algorithms directly.

**Proposal:** Add `impl GraphRef for cfg::Cfg`:
```rust
impl graphwalk::GraphRef for Cfg<R> {
    type NodeId = NodeIndex;
    fn try_successors(&self, node: NodeIndex, f: impl FnMut(NodeIndex) -> ControlFlow<()>) -> ControlFlow<()> {
        self.graph.neighbors(node).try_for_each(|succ| f(succ))
    }
}
```
Then reachability / dominance algorithms in graphwalk become available to cfg code without reimplementation.

**Difficulty:** Trivial (one impl block, ~10 LOC)
**LOC delta:** -10 to -20 LOC (future reuse)
**Migration risk:** None — pure addition

---

## Finding 8: PyO3 Boilerplate Uniformity (strider-py ↔ all crates)

**Files:**
- `crates/strider-py/src/lib.rs` (60+ `#[pyclass]` definitions)
- `crates/strider-py/src/graph.rs` (PyGraph, PyValue, etc.)
- `crates/strider-py/src/matcher.rs` (PyMatch, PyCapture, etc.)
- `crates/strider-py/src/opt.rs` (PyOptimizerPipeline, PyConstantFold, etc.)
- `crates/strider-py/src/pattern.rs` (PyPat, PyCallPat, PyLoadPat, PyIntBinaryOpPat, …)

**Issue:** Every pub Rust type crossing the PyO3 boundary needs a Py wrapper. No macro uniformity — each wrapper is hand-written with:
- `#[pyclass]` or `#[pyclass(subclass)]`
- Unique method naming (`#[pymethods]` block)
- Custom `From<T>` impls where needed

Boilerplate is mostly mechanical but varies per type (some are subclass-able, some frozen, some carry mutable state). No proc-macro reducing the copy-paste.

**Proposal:** Introduce `#[pyo3_wrap]` macro:
```rust
#[pyo3_wrap(name = "Graph", module = "strider")]
pub struct Graph { … }
```
Expands to `#[pyclass(name = "Graph", module = "strider")]` + auto-delegation of common accessors. Reduces ~200 LOC of boilerplate but doesn't eliminate hand-written method pairs (e.g., Match accessors need custom extraction logic).

**Difficulty:** Mechanical (macro design, moderate scope)
**LOC delta:** -80 to -120 LOC (pattern types especially verbose)
**Migration risk:** Low — macro is purely additive; existing code unaffected

---

## Finding 9: Reader Type Hierarchy (reader ↔ opt ↔ strider-py)

**Files:**
- `crates/reader/src/lib.rs` (owns `ReadOnlyMemory` trait + `MemoryMap` impl)
- `crates/opt/src/load_readonly/mod.rs` (consumes `ReadOnlyMemory`)
- `crates/strider-py/src/reader.rs` (lines 87–300+: defines 6 wrapper types)

**Issue:** strider-py defines:
1. `PyMemoryMap` (data-only)
2. `PyMemReader` + `PyMemReaderAdapter` (callback into Python MemReader subclass)
3. `PyReadOnlyMemory` + `PyReadOnlyMemoryAdapter` (callback for optimizer)
4. `AnyMemReader` (enum over both fast + callback paths)
5. `PyMemoryMapReader` (implements `rsleigh::MemReader` for PyMemoryMap)

That's 6 types for 2 concepts (fast in-process + callback). Reader owns `ReadOnlyMemory` trait; strider-py reimplements callback adapter. Layering is clean (separation of fast/callback paths) but the API surface is verbose.

**Proposal:** Consolidate in strider-py: `AnyMemReader` is the unifying type. Expose it as the primary interface in `build_cfg` / `run` signatures. Retire the individual types from public API (keep as implementation detail). Reduces public surface from 6 to 1 type; callers always use `AnyMemReader::*` ctors.

**Difficulty:** Mechanical (type merging, public API refactor)
**LOC delta:** -50 to -80 LOC (fewer pub definitions, no behavioral change)
**Migration risk:** High — breaking change to strider-py public API (users must switch to AnyMemReader ctors)

---

## Finding 10: Typed Error Variants (all crates ↔ strider-py)

**Files:**
- `crates/ir/src/error.rs` (line 30: `UnknownCallOtherError` typed error)
- `crates/strider-py/src/errors.rs` (lines 42–46: downcast for UnknownCallOtherError; string-match heuristic for `LiftError`)

**Issue:** Each crate uses `anyhow::Result` workspace-wide. strider-py translates errors to typed PyErrs at the boundary:
- `ir::error::UnknownCallOtherError` has a typed downcast (line 45)
- Other errors are identified via string-match heuristics (e.g., "lift" in message → `LiftError`)

String matching is fragile (false positives if an unrelated error message contains "lift"). No shared error type hierarchy in Rust that strider-py could downcast more broadly.

**Proposal:** (Deferred to a future audit.) Introduce typed error newtypes in each crate:
```rust
// ir/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum IrError { UnknownCallOther(String), … }
pub type Result<T> = anyhow::Result<T, IrError>;

// pcode-lift/src/error.rs
pub enum LiftError { Sleigh(…), Decode(…), … }
pub type Result<T> = anyhow::Result<T, LiftError>;
```
Then strider-py can downcast these typed roots instead of string-matching. Large change; deferred.

**Difficulty:** Major (affects every error site in 4+ crates)
**LOC delta:** +200 to +400 LOC (error boilerplate + downcast sites)
**Migration risk:** Very high — changes error model workspace-wide

---

## Summary: Most Concrete Opportunities

**Immediate (trivial):**
1. Add `create_derived()` parameter to ir::Graph (Finding 2)
2. Implement GraphRef for cfg::Cfg (Finding 7)

**Near-term (mechanical):**
3. Extract ArchAliasing registry (Finding 3)
4. Create ArchContext wrapper (Finding 4)
5. Consolidate CallConventionCtx (Finding 5)
6. Merge MemReader types in strider-py (Finding 9)

**Medium (design):**.
7. Mirror NodeKind with sealed proc-macro (Finding 1)
8. Introduce #[pyo3_wrap] macro (Finding 8)

**Deferred (major scope):**
9. Typed error hierarchy across crates (Finding 10)

**No Action (already well-isolated):**
- CallOther dispatch consistency — preset flows atomically through cfg::for_arch (Finding 6 is already good)
