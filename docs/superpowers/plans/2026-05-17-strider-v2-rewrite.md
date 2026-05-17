# Strider v2 Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. Each phase is an independent merge boundary — Phase N+1 starts on `main` after Phase N has landed.

**Goal:** Rewrite strider from 12 tangled crates into 5 focused crates with an egraph-based optimizer, lazy CFG construction, a macro-generated pattern DSL, and a Python-first API — keeping pattern queries / IR analysis / optimization / binary support / compactness.

**Architecture:** Sea-of-nodes IR + egraph saturation for optimization + Salsa-style incremental queries for re-analysis after indirect-branch discovery + attribute-macro pattern DSL that is the single source of truth for both Rust and Python builders + Python-first ergonomic API (`Strider.load().analyze().find()`) backed by the existing Rust building blocks.

**Tech Stack:** Rust 2024 edition, `egg` (egraph engine), `salsa-2022` (incremental query), `pyo3 + maturin + abi3-py39`, `proptest` (Rust) + `hypothesis` (Python) for property-based testing, `insta` for snapshot tests, existing `rsleigh` (Sleigh/GHIDRA lifter), `object` (binary formats).

**Skills to use throughout:**
- `superpowers:test-driven-development` — every task is test-first, no exceptions. The existing code is the spec; new code must reproduce it.
- `superpowers:systematic-debugging` — when IR diffs against v1 don't match, debug from evidence, not hunches.
- `superpowers:requesting-code-review` — at the end of every phase, request review against this plan and the v1 behavior contract.
- `superpowers:subagent-driven-development` — dispatch fresh subagents per task across phases 2–6; the orchestrator (you) keeps the architectural picture.
- `superpowers:verification-before-completion` — no phase is "done" without (a) full v1 parity tests passing, (b) all snapshot baselines reviewed.

---

## Design Principles

The single number that matters: **lines of source per unit of behavior**. v1 is ~30 KLOC across 12 crates; the target is ~15 KLOC across 5 crates with strictly more capability (egraph saturation, incremental re-analysis, lazy CFG, property-based invariants).

1. **Compactness via fewer concepts.** Replace the stable-vs-destructive pipeline split, the side-table proliferation (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`), and the hand-rolled fixed-point with one model: an egraph that saturates under declarative rewrites. One pipeline, one convergence story.
2. **Correctness by construction over correctness by validation.** Today the 3-layer validator catches type errors, use-list desync, and graph invariants *after* a pass produces them. Where possible, encode the invariant in types so the bad state is unrepresentable (e.g. `PhiToken` already does this for which nodes can feed phi inputs — extend the principle).
3. **Provenance is non-negotiable.** Asm-fingerprints become a Layer-A invariant (always-on, not opt-in), maintained automatically by egraph e-class unions. Every reachable non-exempt node carries ≥1 source address. This is the proof-of-correctness substrate for pattern queries.
4. **Python is the API, Rust is the engine.** Today `strider-py` is a thin mirror that ships behind every Rust change. In v2 the public Python API (`Strider`, `Analysis`, `Pattern`) is designed first; Rust types serve it. The pattern DSL is defined once via attribute macro and emits both surfaces.
5. **Incremental over rebuild.** Indirect-branch resolution today triggers a full CFG rebuild + IR re-lift. v2 uses Salsa to invalidate only the regions that depend on the resolved target, dropping fixed-point cost from O(iter × full) to O(iter × delta).
6. **Lazy over eager.** Today every reachable basic block is decoded and lifted before any analysis runs. v2 decodes on demand — pattern queries against a 50 MB binary only pay for the functions touched.

## Target Architecture

```
   strider-binary         (ELF/PE/Mach-O loaders, relocations, MemReader)
        ↑
   strider-ir             (sea-of-nodes graph, validate, walk, dot, egraph adapter)
        ↑          ↑
   strider-lift   strider-analyze
   (target+sleigh (egraph optimization, pattern DSL,
    +cfg+lifter)  indirect-branch resolution, orchestrator)
                       ↑
                  strider             ← maturin crate, exposes Python API
                  (PyO3 bindings)
```

Five crates, dropping from twelve. Mapping:

| v1 crate | Lands in | Notes |
|---|---|---|
| `reader` | `strider-binary` | Rename + extend to PE/Mach-O later (not in scope this plan). |
| `ir` | `strider-ir` | Core sea-of-nodes. Validator extended with always-on asm-fingerprint check. |
| `graphwalk`, `entity-utils`, `dot`, `graphmock` | `strider-ir` (modules) | Internal utilities — no reason for separate crates. |
| `target`, `pcode-lift`, `cfg` | `strider-lift` | Single crate for "binary → IR". CFG built lazily; per-region driver collapses into `lift::region`. |
| `opt`, `pattern` | `strider-analyze` | Egraph-based optimizer; pattern DSL macro-generated. |
| `strider` (orchestrator + `Strider::run`) | `strider-analyze::orchestrator` | The fixed-point loop becomes a Salsa query graph. |
| `strider-py` | `strider` (maturin crate) | Promoted to the top-level public API. |

## Key Implementation Choices With Rationale

### A. Acyclic-slice egraph optimizer (egg without back-edges)

**Correction from the v0 draft.** Standard egg saturation assumes acyclic terms, but sea-of-nodes carries cycles via `VarPhi` / `MemPhi` at loop joins. You raised this. The fix is to e-graph only the *acyclic value-subgraph slices* between phi nodes, treating phi outputs as opaque variables. This is exactly the design of `cranelift-egraph` (aegraph) — used in Wasmtime production for the same shape of IR.

**Approach:**
- **Slice definition.** A region's value subgraph from `{ phi outputs, InitialVar, InitialMemory, FunctionArg, Load output }` (opaque leaves) to `{ branch cond, store inputs, call args, return values, phi inputs of successor regions }` (roots) is acyclic. This slice is what egg sees.
- **Phis are leaves.** Egg never matches LHS patterns that cross a phi boundary. Phi nodes are opaque variables in the egraph and merge points outside it.
- **Control flow stays pinned.** `Control` ports on `If`, `ControlState`, `Return`, `Call`, `CallOther` are not in the egraph at all. Optimization rewrites only the value-port subgraph.
- **Memory chain stays pinned.** Identical to control. Egg sees `Load` *values*, not the memory token threading them through `Store` / `Load` / `Call`.
- **One saturation per slice.** Results merged back into the unified `Graph` post-extraction. The "stable vs destructive" split disappears — saturation is non-destructive; extraction picks the canonical form.

**Destructive cleanup interleaves with saturation — does NOT run only at the end.** You pointed out that destructive passes unlock new nondestructive rewrites. Concrete cycle:

```
DeadBranchElimination removes If(BoolConst(true))
  → RedundantPhis collapses a now-single-pred VarPhi to its surviving input
  → if the surviving input is IntConst, ConstantFold propagates into the consumer
  → consumer becomes a new IntConst
  → may enable another DeadBranch on a downstream If
```

v1 runs destructive only at the final fixed-point exit because its per-iteration `RegionIndex` pins phi `NodeId`s and can't tolerate mid-iteration deletions. That constraint disappears with the Salsa orchestrator. v2 uses one outer loop:

```
loop {
    saturate_egraph(value_slice)        // non-destructive: adds e-class equivalences
    extract_canonical(value_slice)      // pick lowest-cost form per e-class
    cleanup = run_control_simplification()
    //   DeadBranchElimination + RedundantPhis + dead-store removal
    if !saturation_added_anything && !cleanup.changed { break }
}
```

The egraph crate (egg or cranelift-egraph) handles the saturation half; the destructive half is plain imperative graph edits. After destructive cleanup invalidates some e-classes, the next saturation iteration rebuilds them from the now-smaller graph. The v1 "stable vs destructive pipeline" split (audit finding G4) collapses to a single loop.

**What this means for v1 passes:**
- `ConstantFold`, `KnownBits`, `FlagCmpCanonicalize`, `IfCondInversion`, algebraic identities (`x+0→x`, `x^x→0`) — all live in the value slice. Trivially port to `egg::Rewrite<NodeKind, NodeAnalysis>`. Run as the *saturate* step.
- `RedundantPhis`, `DeadBranchElimination` — touch the *control* subgraph; they stay imperative, run as the *cleanup* step inside the outer loop, NOT once at the end.
- `StackStoreDetect`, `StackLoadForward` — contextual (need the calling convention). Implement as `egg::Analysis::Data` (per-eclass metadata maintained on union). Re-evaluated every saturation iteration since cleanup may have changed which stores are live.
- `CallStackArgCollect`, `FunctionArgDetect` — true post-passes (run once after the loop converges); they collect/canonicalize but don't rewrite, so they don't need to be in the loop.
- `LoadReadOnly` — analysis with access to `ReadOnlyMemory`; rewrites a `Load` whose address eclass is a constant. Inside the saturate step.

**Fallback paths if Task 3.1 spike reveals problems** (decision gate, not "we might do all four"):
1. **Switch to `cranelift-egraph`.** Same model as our slice approach but with phi-handling built in. Smaller ecosystem; tighter API fit.
2. **Hand-rolled saturation.** Apply rewrites + node-id union-find directly on the existing `Graph`. No external dep. Loses egg's confluence machinery but keeps the data model intact.
3. **Drop egraph entirely.** Keep v1's imperative optimizer; collapse the stable/destructive split via a `PassEffect` enum (Finding 4 from the generalization audit). Lose saturation benefit; keep every other v2 win (Salsa, lazy CFG, proc-macro patterns, Python-first API).

Phase 3 is decoupled from the rest of v2: phases 1, 2, 4, 5 land regardless of which optimizer path wins.

### B. Salsa-based orchestrator

**Why:** The indirect-branch fixed-point loop currently does up to N full CFG rebuilds + IR re-lifts. Most regions don't change between iterations — only those reachable from the newly-resolved jump table. Salsa lets us declare the query graph (`cfg(addr) → regions`, `lift(region) → ir_subgraph`, `optimize(ir) → eclass_set`) and have re-runs invalidate only what depends on the changed input.

**What changes:**
- `Strider::run` becomes a Salsa database driving these queries.
- `RegionLiftHandles` and the orchestrator's `RegionIndex` disappear — Salsa tracks dependencies automatically.
- `Decision { FixedPoint, StableOnly, Rebuild }` collapses into Salsa cache invalidation.

### C. Lazy CFG

**Why:** v1 lifts every reachable basic block eagerly via `cfg::Builder::build`. For large binaries (libc, kernel) this is wasteful when a user only queries one function. Lazy lifting also composes naturally with Salsa — each `region(addr)` query lifts on demand and caches.

**What changes:**
- `cfg::Builder` becomes `lift::Lifter` with `lifter.region(addr) → &Region`.
- Reachability is computed on the fly as queries fan out from the entry.
- The `function_max_size` bound becomes per-`lift_function(entry)` state.

### D. Macro-generated pattern DSL

**Why:** Today the Rust `pattern` crate and the Python `strider.pattern` module mirror each other by hand. Every new pattern (`StackStorePhiPat`, `CallPat::at_any`, …) must be implemented twice, kept in sync, and tested twice. The macro emits both surfaces from one source.

**Sketch:**
```rust
#[strider_pattern]
mod patterns {
    // Generates StackStorePat (Rust), strider.pattern.StackStorePat (Python),
    // builders for .offset() / .offset_any() / .data() / .space(),
    // and stub-gen for IDE completion.
    pub struct StackStore {
        #[field(offset)] offset: Option<i64>,
        #[field(offset_set)] offset_set: Option<BTreeSet<i64>>,
        #[field(data, accepts = Pat)] data: Option<Pat>,
        #[field(space)] space: Option<VnSpace>,
    }
}
```

The macro covers ~80% of the boilerplate. Custom matchers (commutative variants, lift-time canonical aliases like `sub` → `add(_, neg(_))`) stay hand-written but live in one place.

### E. Asm-fingerprint as Layer-A invariant

**Why:** Today `validate_with_options(ValidateOptions { check_asm_fingerprints: true })` is opt-in because v1 mock graphs in tests don't set fingerprints. In v2, mocks are generated by a `GraphBuilder` test helper that auto-stamps fingerprints with sentinel addresses. The validator always checks non-exempt nodes have ≥1 fingerprint. Egraph e-class union maintains the superset invariant automatically.

### F. Always-tested binary fixtures

**Why:** v1's `fixtures/` directory has per-arch Makefiles producing ELFs. Cross-arch tests use them but cross-binary regressions (e.g. "the same C function lifts to the same canonical IR on x86 vs arm64") are weak. v2 adds **shape-equality fixtures**: a `.c` source + per-arch ELFs + an expected canonical-IR snapshot (`insta` snapshot of the post-optimization IR). A pass break shows as a snapshot diff *per architecture*.

## Generalization Audit Findings

These 15 findings (from a read-only audit of v1 by the `feature-dev:code-explorer` subagent on 2026-05-17) name concrete repetition/correctness wins that v2 should absorb. Ordered by impact. Each finding lists the v1 location, the duplication pattern, the v2 generalization, the correctness benefit, and the cost.

### G1. Unify three `Graph` side-tables behind a single `SideTable<K, V>` type
- **v1:** `crates/ir/src/graph/mod.rs:60-101` — `stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, plus the four-field `call_clobbered_overrides`. Each adds a `get_*` / `set_*` / `extend_*` accessor trio, a compaction hook in `compact.rs`, a validator skip in `validate/layer_c.rs`, and a Clone shim.
- **v2:** A sealed `SideTable<K, V>` newtype that owns compaction (node-id remap), Clone, and accessor ergonomics in one place. In the egraph model the natural replacement is per-eclass `egg::Analysis::Data` — all side-data lives in `NodeAnalysis` and is maintained automatically on merge.
- **Correctness:** Forgetting to hook a new side-table into `compact.rs` today produces dangling references without any compile-time signal. A unified type makes the forget impossible.
- **Cost:** Low. The four existing tables are structurally homogeneous; extraction is mechanical.

### G2. Proc-macro pattern DSL absorbs the 16-type Rust×Python duplication
- **v1:** `crates/strider-py/src/pattern.rs` (16 `Py*Pat` types) mirrors `crates/pattern/src/pat/builders/` (16 Rust builders) by hand. `pat_builder_finalise!` macro halves the Python side; nothing halves the Rust side.
- **v2:** `#[strider_pattern]` proc-macro emits both surfaces from one annotated struct (Phase 4). `capture` / `when` / `into_pat` / `cap` handled universally.
- **Correctness:** A new field added on the Rust side is silently absent from Python today. The macro makes the gap a compile error.
- **Cost:** Medium. Macro dev cost is real; commutative variants and lift-time aliases (`sub`, `int_le`) stay hand-written. Reduces ~3000 LOC to ~500 LOC.

### G3. Make asm-fingerprint Layer-C check unconditional (biggest correctness win)
- **v1:** `crates/ir/src/validate/mod.rs:115-117` — Layer-C fingerprint check is `if options.check_asm_fingerprints`. Default `validate()` skips it. `opt::OptimizerPipeline::run` calls `validate(graph, entry)` *without* options → fingerprint violations survive undetected in production.
- **v2:** Remove `ValidateOptions` entirely; check is unconditional. `GraphBuilder` test helper auto-stamps sentinel fingerprints so existing mock-graph tests stay green.
- **Correctness:** Every optimization pass that forgets `extend_asm_fingerprint_from` currently produces silently invalid output. Always-on check catches it at the next `pipeline.run()`.
- **Cost:** Low once the helper exists. The only risk is discovering a currently-shipping pass with latent empty-fingerprint output — which is exactly what should be caught.

### G4. Collapse three pipeline builders behind a `PassEffect` enum
- **v1:** `crates/strider/src/strider/pipeline.rs:186-246` — `build_optimizer_pipeline` / `build_stable_optimizer_pipeline` / `build_destructive_optimizer_pipeline` each thread CC and arch into CC-aware passes. Adding a CC-aware pass means touching every applicable builder.
- **v2:** Single `Strider::build_pipeline(effect: PassEffect)` with `enum PassEffect { Stable, Destructive, Full }` — 60 LOC → 20 LOC and CC threaded once. (The egraph path makes the split moot entirely; this is the interim/fallback shape.)
- **Correctness:** A missed CC threading silently makes intermediate and final fixed-point iterations behave differently.
- **Cost:** Low for the enum; zero for the egraph path.

### G5. `CallingConventionBuilder` collapses 12 preset constructors
- **v1:** `crates/target/src/calling_convention/mod.rs:369-898` — 12 full 9-field `CallingConvention` literals. Kernel/syscall variants mutate fields from the base via `let mut cc = …; cc.arg_passing_regs = …; cc`.
- **v2:** Fluent `CallingConventionBuilder` with `with_*` methods. Derived presets become one-liner diffs: `Self::x86_64_systemv().with_syscall_number_reg("RAX")`.
- **Correctness:** Today, adding a 10th `CallingConvention` field requires touching every preset literal. With a builder, defaults handle it.
- **Cost:** Low. Builder is ~30 LOC; preset bodies become ~3 LOC each.

### G6. `NodeKind::is_commutative()` as single-source-of-truth for commutative matching
- **v1:** `crates/pattern/src/matcher/commutativity.rs:1-36` + `crates/pattern/src/pat/builders/binary_op.rs:31-53` — four separate `is_commutative_*` functions plus per-builder `BinaryOpKind::is_commutative` methods, all dispatching on different op enums. `IntCmpOp::Carry`/`Scarry` semantics live in a fourth function.
- **v2:** Single `NodeKind::is_commutative(&self) -> bool` method; proc-macro annotates `#[commutative]` on enum variants.
- **Correctness:** Adding a commutative op today requires updating four sites; missing one makes the matcher silently non-commutative.
- **Cost:** Low.

### G7. CC metadata leaves the IR crate — `BuiltFunctionGraph` doesn't carry it
- **v1:** `crates/ir/src/function.rs:57-104` — five `pub(crate)` fields record CC state on the final graph (clobber lists, ret-val regs, no-memory-clobber flag). `from_graph_and_entry_for_rewrite` is an escape hatch for rewrite paths that don't need CC. The `ir` crate depends on `rsleigh::Vn` purely for these fields.
- **v2:** CC metadata lives in `strider-analyze::FunctionAnalysis` (wrapping IR graph + CC). The `ir` crate stays CC-free and drops the `rsleigh` dep on this path.
- **Correctness:** Today, CC data has no validator; bad CC is silently shipped. Moving it out makes the IR validator's scope explicit and complete.
- **Cost:** Medium. Every caller reading `bfg.call_clobbered` directly must migrate.

### G8. `RegionLiftHandles` / `RegionIndex` deleted by Salsa
- **v1:** `crates/strider/src/strider/pipeline.rs:21-47` — 8-field `RegionLiftHandles` struct; orchestrator rebuilds `RegionIndex` per iteration to map placeholder anchors back to source regions. O(iter × full) cost.
- **v2:** Salsa's `region_ir(addr)` query is the cache; no manual index. The struct disappears.
- **Correctness:** Today the 8-field handle can desync from IR state if any pass rewrites a `ControlState` or `MemPhi` `NodeId` mid-iteration. Salsa makes desync structurally impossible.
- **Cost:** High for Salsa; low for an interim read-only `FunctionBuilder::regions() -> &HashMap<…>` accessor.

### G9. `cfg → opt` dependency inversion: cfg's mini-IR resolver moves into strider-analyze
- **v1:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:1-48` — builds a standalone IR graph at CFG-build time, runs `ConstantFold + KnownBits + RedundantPhis` to resolve a `BranchIndirect`. This is why `cfg` depends on `opt` (a surprise edge in the dependency diagram).
- **v2:** In Salsa, "what does `target_vn` evaluate to in this region?" is just a query on the egraph value slice — no standalone graph. As an interim, expose a `resolve_indirect_target_value(region_insns, target_vn, cc) -> Option<ResolvedTargets>` free function in `strider-analyze` that takes the opt pipeline as a parameter.
- **Correctness:** Mini-resolver and main resolver can silently diverge on which passes they run.
- **Cost:** Medium for the function-extraction; the Salsa path eliminates it entirely.

### G10. `FunctionBounds` value type — shared between `cfg` builder and orchestrator
- **v1:** `crates/cfg/src/cfg/query.rs:25-48` — `is_addr_tail_call(target, start_addr, fn_max_size, allow_code_before_start_addr) -> bool`. Both `cfg` and the orchestrator reconstruct these args independently.
- **v2:** `FunctionBounds { start_addr, fn_max_size, allow_code_before_start_addr }` with a `classify_branch_target(addr) -> BranchTargetClass` method. Both callers share one type.
- **Correctness:** Today, the cfg builder pulls these fields from `self`; the orchestrator reconstructs them manually. A field rename can silently desync them.
- **Cost:** Low.

### G11. `ArchContext` fully replaces `SleighArch`'s two separate `preset` + `endianness` fields
- **v1:** `crates/target/src/arch.rs:109-118` introduced `ArchContext { preset, endianness }` to bundle what used to be two pass-through params. But `SleighArch` still stores them as separate fields and exposes `SleighArch::context()` that constructs a fresh value.
- **v2:** `SleighArch` embeds `ArchContext` directly; `SleighArch::context()` returns `&self.context`.
- **Correctness:** Eliminates the residual way to pull `preset` from one `SleighArch` and `endianness` from another.
- **Cost:** Trivial mechanical refactor.

### G12. `NodeKind::CallOther { user_op_id, name: InternedStr }` deletes a side-table
- **v1:** `crates/ir/src/graph/mod.rs:63-76` — `call_other_names: SecondaryMap<NodeId, Option<String>>` populated at construction. `CallOther` is explicitly non-cacheable, so identical ops produce different `NodeId`s.
- **v2:** Embed the op name as an interned 32-bit ID directly in `NodeKind::CallOther`. `NodeKind` stays `Copy`. One side-table deleted.
- **Correctness:** A pass that creates a `CallOther` without populating the side-table today silently loses the name.
- **Cost:** Low.

### G13. `Graph::reachable_set(entry) -> NodeIdSet` replaces four `walk_graph` "drive to completion" call-sites
- **v1:** `validate/mod.rs:91-93`, `redundant_phis/mod.rs`, `dead_branch/mod.rs`, `function_args/mod.rs` — all four independently drive `walk_graph` to completion to harvest `walk.visited` as a reachability filter.
- **v2:** One helper. 3-line impl.
- **Correctness:** Eliminates the footgun where a caller partially drains the walk and reads `visited` as if complete.
- **Cost:** Trivial.

### G14. Lift-time canonical aliases enforce a doc-test contract
- **v1:** `crates/pcode-lift/src/value/integer.rs` lowers `IntSub → Add(_, Neg(_))`; `crates/pattern/src/pat/ctor/int.rs` re-implements `sub(l, r) → add(l, neg(r))`. Two layers, no contract.
- **v2:** Doc-test: `assert_eq!(lift_insn(IntSub_insn).shape(), pattern::sub(_, _).expected_shape())`. The proc-macro can auto-generate the alias from `#[lowered_to = "add(_, neg(_))"]` on the pattern struct.
- **Correctness:** A future lift-time canonicalization change silently breaks patterns today.
- **Cost:** Low for the doc-test; Medium for the macro.

### G15. `PyPatBuilder` trait collapses the 16-arm `PatLike::into_pat` dispatch
- **v1:** `crates/strider-py/src/pattern.rs:239-292` — 16-variant `PatLike` enum + 16-arm `into_pat` match. Every new builder type means a two-site edit.
- **v2:** `PyPatBuilder` trait with `finalise() -> Pat`; `PatLike` becomes a small enum + `Builder(Box<dyn PyPatBuilder>)`. Or eliminated entirely by the macro (G2).
- **Correctness:** Missing a variant falls through silently to a Python type-extraction error today.
- **Cost:** Subsumed by G2 (the proc-macro).

## Pre-Code Verification Plan

You said "verify all your plan before coding it." The verifications below run as read-only research subagents in parallel before Phase 0 begins. Each gates a specific architectural commitment; if a verification fails, the plan is updated (or the fallback path documented in section A is taken) *before* any rewrite code is written.

### V1. Egg + sea-of-nodes (acyclic slice) is feasible
- **Hypothesis:** Egg with phi-as-opaque + control-pinned + memory-pinned is a workable model. `cranelift-egraph` is a viable swap-in if not.
- **Verification:** Build a 30-line spike: take one v1 region, extract its value-slice, run `egg::Runner` with ConstantFold rewrites, extract back. Compare to v1 ConstantFold output. Document the per-region overhead.
- **Pass condition:** Output matches v1 + per-region cost < 5ms on typical regions.
- **Fail action:** Switch to `cranelift-egraph` in section A, or fall back to imperative+`PassEffect` per the documented fallbacks.

### V2. Salsa fits our query shape and is API-stable
- **Hypothesis:** salsa-3.0 (or whichever is current) supports our queries: `binary(path)` → `cfg(entry)` → `region_ir(addr)` → `optimized_eclass(entry)`.
- **Verification:** Read the salsa docs + Wasmtime / rust-analyzer use as production references. Sketch the query graph in `docs/superpowers/specs/2026-05-17-salsa-query-shape.md` (single page).
- **Pass condition:** Every v1 orchestrator state transition maps to a Salsa input change or query invalidation; no required feature is missing.
- **Fail action:** Drop incremental rebuilds (still keep lazy CFG); implement a hand-rolled invalidation map (cost: Finding G8's interim accessor + ~150 LOC).

### V3. rsleigh supports per-BB lazy decoding
- **Hypothesis:** Lazy `Lifter::region(addr)` can decode one basic block on demand without rsleigh refusing or requiring a whole-function context.
- **Verification:** Read `rsleigh`'s public API (it's a path dep at `../rsleigh`). Look for "lift one instruction" / "lift to next branch" entry points. If only "lift function" exists, lazy-by-region degrades to lazy-by-function — still a win for large binaries but worth knowing now.
- **Pass condition:** rsleigh exposes a per-instruction or per-BB lift entry point.
- **Fail action:** Lazy-by-function (still big win on multi-function binaries); update Phase 2 Task 2.4 accordingly.

### V4. PyO3 proc-macro generation of `#[pyclass]` works
- **Hypothesis:** A custom proc-macro can emit `#[pyclass]` impls + their methods such that pyo3-stub-gen picks them up.
- **Verification:** Build a 20-line spike: `#[strider_pattern] struct Foo { #[field] x: i64 }` emits `PyFoo` with `__init__` + getters + setters. Run pyo3-stub-gen against it; verify the stub matches a hand-written equivalent.
- **Pass condition:** Stubs match; importable from Python; `mypy --strict` passes on a small consumer file.
- **Fail action:** Keep hand-written Python mirror but introduce a `declarative_pattern!` macro_rules (less powerful but no proc-macro infrastructure). Phase 4 still wins by ~40% LOC.

### V5. Property-based IR generation strategy
- **Hypothesis:** `proptest` strategies can generate valid sea-of-nodes graphs (well-typed, walk-reachable, fingerprint-stamped) for property tests.
- **Verification:** Sketch a `prop_compose!` strategy that builds a graph from a random sequence of `FunctionBuilder` operations. Run 1000 iterations under v1 `validate()` (with always-on fingerprints from G3) and confirm zero violations.
- **Pass condition:** 1000/1000 generated graphs pass v1 validation.
- **Fail action:** Shrink to hand-authored fixture suite (still better than v1's mock graphs); defer property testing to a v2.1 milestone.

### V6. Crate dependency direction has no back-edges
- **Hypothesis:** `strider-binary → strider-ir → strider-lift → strider-analyze → strider` has no cycles.
- **Verification:** Map every v1 cross-crate call site (already covered in `feature-dev:code-explorer` audit Finding G9 — `cfg → opt` is the one known back-edge). Confirm v2 dependency direction holds for every other call site.
- **Pass condition:** Zero back-edges identified.
- **Fail action:** Document the back-edge; either invert dependency in v2 or carry the edge as a documented exception in CLAUDE.md.

**V1–V6 run as parallel subagent dispatches** before Phase 0 begins. Results recorded in a new `docs/superpowers/specs/2026-05-17-verification-results.md`. The plan is updated based on results before Phase 0 Task 0.1 starts.

## Migration Strategy

The rewrite proceeds bottom-up, one crate per phase. After each phase:

- The new crate exists alongside the old.
- An adapter (`crates/strider-shim/`) exposes the new crate's API under the old crate's name so downstream crates compile unchanged.
- The shim is deleted in the final phase.

This means at every checkpoint the test suite (including all `crates/strider-py/tests/python/`) is runnable. No "long branch" risk.

---

## Phase 0: Repo Hygiene & Baseline (1 day)

**Goal:** Lock in the v1 behavior contract so the rewrite can diff against it.

### Task 0.1: Pin v1 IR snapshots across all fixtures

**Files:**
- Create: `crates/strider/tests/snapshots/v1_baseline/<arch>/<binary>__<function>.snap`
- Create: `crates/strider/tests/v1_baseline.rs`

- [ ] **Step 1: Write a failing snapshot test that loads every binary in `fixtures/out/` and dumps the post-optimization IR for every exported function via `Graph::to_dot`**

```rust
#[test]
fn v1_baseline_snapshots() {
    for binary in walk_fixtures("fixtures/out") {
        let arch = arch_of(&binary);
        for func_name in exported_functions(&binary) {
            let cfg = build_cfg(&binary, &func_name).unwrap();
            let g = strider::run(RunConfig {
                cfg,
                arch,
                cc: cc_for(arch),
                rom: rom_from(&binary),
            }).unwrap();
            let dot = g.to_dot_string();
            insta::assert_snapshot!(
                format!("{}__{}", arch.preset_name(), func_name),
                dot,
            );
        }
    }
}
```

- [ ] **Step 2: Run it to verify it fails (snapshots don't exist yet)**

Run: `cargo test --package strider --test v1_baseline -- --include-ignored`
Expected: every assertion fails with "snapshot not found".

- [ ] **Step 3: Use `cargo insta review` to accept all snapshots**

Run: `cargo insta review`
Expected: every snapshot is reviewed and accepted; commit them.

- [ ] **Step 4: Re-run; verify all PASS**

Run: `cargo test --package strider --test v1_baseline`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/strider/tests/snapshots/v1_baseline crates/strider/tests/v1_baseline.rs
git commit -m "test: snapshot v1 IR baseline across all fixtures"
```

### Task 0.2: Add cross-arch shape-equality test

**Files:**
- Create: `crates/strider/tests/cross_arch_shape.rs`
- Create: `crates/strider/tests/snapshots/cross_arch_shape/`

- [ ] **Step 1: Write a test that picks one canonical fixture (`fixtures/cases/control.c`), lifts the same function on every arch, and snapshots a *structural fingerprint* (node-kind histogram) per arch**

The fingerprint elides register names so x86 `rax` and arm64 `x0` collapse to "register read at arg position 0".

```rust
fn structural_fingerprint(g: &Graph) -> String {
    // Walk reachable nodes; emit (kind, output_kinds, input_kinds_canonical) histogram.
    // Skip register names — they differ by arch but the shape should match.
}

#[test]
fn cross_arch_control_c_shape() {
    let mut by_arch = BTreeMap::new();
    for arch in [X86_64, AArch64, Arm, Mips64Le, Ppc64Le] {
        let g = lift_fixture("control.c", "branchy", arch);
        by_arch.insert(arch, structural_fingerprint(&g));
    }
    insta::assert_yaml_snapshot!(by_arch);
}
```

- [ ] **Step 2: Run, review snapshot, commit.**

(Three steps: run failing, `cargo insta review`, commit.)

```bash
git add crates/strider/tests/cross_arch_shape.rs crates/strider/tests/snapshots/cross_arch_shape
git commit -m "test: cross-arch shape-equality baseline for control.c"
```

### Task 0.3: Tag the v1 commit

- [ ] **Step 1: Tag**

```bash
git tag -a v1-final -m "v1 baseline before strider-v2 rewrite"
git push origin v1-final
```

---

## Phase 1: `strider-ir` Crate (1 week)

**Goal:** Consolidate `ir` + `graphwalk` + `entity-utils` + `dot` + `graphmock` into one crate, extend the validator to make asm-fingerprints Layer A, and introduce the egraph adapter type. Behavioral parity with v1.

### Task 1.1: Create the `strider-ir` crate skeleton

**Files:**
- Create: `crates/strider-ir/Cargo.toml`
- Create: `crates/strider-ir/src/lib.rs`
- Modify: `Cargo.toml:14-23` (add `strider-ir` to workspace)

- [ ] **Step 1: Add the crate to the workspace**

Edit `Cargo.toml`:
```toml
[workspace.dependencies]
strider-ir = { path = "crates/strider-ir" }
# ... existing entries unchanged
```

- [ ] **Step 2: Create `crates/strider-ir/Cargo.toml`**

```toml
[package]
name = "strider-ir"
edition.workspace = true

[dependencies]
rsleigh.workspace = true
cranelift-entity.workspace = true
cranelift-bitset.workspace = true
smallvec.workspace = true
thiserror.workspace = true
anyhow.workspace = true
ethnum.workspace = true
petgraph.workspace = true

[dev-dependencies]
proptest = "1.5"
insta = "1.40"
```

- [ ] **Step 3: Write a smoke test asserting the empty crate compiles**

`crates/strider-ir/src/lib.rs`:
```rust
//! Strider IR: sea-of-nodes graph, validation, traversal.
```

`crates/strider-ir/tests/smoke.rs`:
```rust
#[test]
fn crate_compiles() {}
```

- [ ] **Step 4: Run**

```bash
cargo test --package strider-ir
```
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/strider-ir
git commit -m "feat(strider-ir): create empty crate"
```

### Task 1.2: Move `entity-utils`, `graphwalk`, `dot`, `graphmock` modules in

For each of the four:

- [ ] **Step 1: Copy `crates/<old>/src/*` to `crates/strider-ir/src/<old>/`** (preserve module structure as a subdirectory).
- [ ] **Step 2: Add `pub mod <old>;` to `crates/strider-ir/src/lib.rs`.**
- [ ] **Step 3: Run that crate's existing test suite under `strider-ir`** — `cargo test --package strider-ir -- <old>` — all tests pass unchanged.
- [ ] **Step 4: Add a re-export shim in the old crate** so downstream still compiles:
  ```rust
  // crates/entity-utils/src/lib.rs
  pub use strider_ir::entity_utils::*;
  ```
- [ ] **Step 5: Commit per submodule** (`refactor(strider-ir): absorb entity-utils`, etc.).

### Task 1.3: Move `ir` crate contents in

**Files:**
- Move: `crates/ir/src/*` → `crates/strider-ir/src/*` (top-level, not under `ir/`)
- Modify: `crates/ir/src/lib.rs` to re-export from `strider-ir`

- [ ] **Step 1: Copy contents and update internal paths**
- [ ] **Step 2: Old `ir` crate becomes a shim:**

```rust
// crates/ir/src/lib.rs
pub use strider_ir::*;
```

- [ ] **Step 3: Run the full workspace test suite** — `cargo test --workspace`. Expected: PASS (the v1 baseline snapshots from Phase 0 still match).
- [ ] **Step 4: Commit.**

### Task 1.4: Always-on asm-fingerprint validation

**Files:**
- Modify: `crates/strider-ir/src/validate.rs` (default `validate()` now runs the Layer C asm-fingerprint check)
- Create: `crates/strider-ir/src/test_helpers.rs` (a `GraphBuilder` that auto-stamps fingerprints for tests)
- Modify: every existing test that builds a graph without fingerprints — switch to the helper.

- [ ] **Step 1: Write a failing test asserting `validate` (no options) flags an empty fingerprint on a non-exempt node**

```rust
#[test]
fn validate_flags_missing_asm_fingerprint() {
    let mut g = Graph::new();
    let entry = g.create_entry();
    // Create an Add node with NO fingerprint stamped.
    let a = g.create_int_const_u64(1, U64);
    let b = g.create_int_const_u64(2, U64);
    let _add = g.create_int_binary_op(IntBinaryOp::Add, a, b);
    let err = validate(&g, entry).unwrap_err();
    assert!(err.errors().iter().any(|e| matches!(e, ValidationError::MissingAsmFingerprint { .. })));
}
```

- [ ] **Step 2: Run; expect FAIL** (current `validate` is opt-in for fingerprints).
- [ ] **Step 3: Update `validate` to always check Layer C** — delete the `check_asm_fingerprints` field from `ValidateOptions`; the check is unconditional.
- [ ] **Step 4: Create `crates/strider-ir/src/test_helpers.rs`** with a `GraphBuilder` that wraps `Graph` and auto-stamps a sentinel address (`0xDEAD_BEEF_0000_0000 | counter`) on every created node.
- [ ] **Step 5: Migrate every existing IR test** that fails validation to use the helper. Run the workspace test suite; expect PASS.
- [ ] **Step 6: Commit.**

### Task 1.5: Introduce `EGraphAdapter`

**Why now:** Phase 3 needs to convert a `Graph` into an `egg::EGraph` and back. Defining the conversion in Phase 1 (with full v1 round-trip tests) means Phase 3 starts from a tested substrate.

**Files:**
- Create: `crates/strider-ir/src/egraph_adapter.rs`
- Create: `crates/strider-ir/tests/egraph_roundtrip.rs`

- [ ] **Step 1: Add `egg = "0.10"` to `strider-ir/Cargo.toml`**
- [ ] **Step 2: Define an `egg::Language` impl for `NodeKind`** — every variant becomes an `enum_node! { … }` arm. Control edges become typed child positions.
- [ ] **Step 3: Write a failing round-trip test**

```rust
#[test]
fn egraph_roundtrip_preserves_v1_snapshots() {
    for binary in walk_fixtures("fixtures/out") {
        for func in exported_functions(&binary) {
            let g_before = strider::run_v1(/*…*/).unwrap();
            let eg = EGraphAdapter::from_graph(&g_before);
            let g_after = EGraphAdapter::extract_lowest_cost(&eg);
            assert_eq!(g_before.canonical_form(), g_after.canonical_form());
        }
    }
}
```

- [ ] **Step 4: Implement until passing.** This is the longest task in Phase 1 — budget 2–3 days.
- [ ] **Step 5: Commit.**

### Phase 1 Checkpoint

- [ ] **Run full workspace test suite:** `cargo test --workspace` — every test passes.
- [ ] **Run v1 snapshot baseline:** `cargo test --package strider --test v1_baseline` — every snapshot matches.
- [ ] **Request code review** via `superpowers:requesting-code-review` against this plan's Phase 1 tasks and the v1 contract.
- [ ] **Delete the `ir`, `graphwalk`, `entity-utils`, `dot`, `graphmock` shim crates** — fix-up all downstream imports to point at `strider-ir`.
- [ ] **Commit:** `chore(strider-ir): delete shim crates`.

---

## Phase 2: `strider-lift` Crate (1 week)

**Goal:** Consolidate `target` + `pcode-lift` + `cfg` + the per-region driver from `strider` into one crate. Introduce lazy CFG via `Lifter::region(addr)`. Behavioral parity with v1.

### Task 2.1: Create `strider-lift` skeleton + move `target` in

Same shape as Tasks 1.1–1.2. Move `crates/target/src/*` into `crates/strider-lift/src/target/` (or flatten — `target` is small enough). Shim the old `target` crate to re-export.

### Task 2.2: Move `pcode-lift` in

- [ ] **Step 1: Copy contents.**
- [ ] **Step 2: Run `pcode-lift` tests under `strider-lift`** — all pass.
- [ ] **Step 3: Shim + commit.**

### Task 2.3: Move `cfg` in, **eagerly** (parity build)

This is the most invasive move. `cfg` has its own indirect-resolver mini-graph that depends on `opt`. Keep that dependency for now via `workspace.dependencies.opt` — `strider-lift` will temporarily depend on the old `opt` crate. Phase 3 inverts this.

- [ ] **Step 1: Copy.**
- [ ] **Step 2: Run cfg's full test suite, including indirect-branch resolution.** All pass.
- [ ] **Step 3: Shim + commit.**

### Task 2.4: Introduce `lift::Lifter` (lazy CFG)

**Files:**
- Create: `crates/strider-lift/src/lifter.rs`
- Create: `crates/strider-lift/tests/lazy_cfg.rs`

The lazy `Lifter` wraps the existing eager `Builder` — for now, on `lifter.region(addr)`, it lifts the *function* and caches per-region. Truly on-demand lifting (decode one BB at a time without lifting the rest) is a future optimization; the immediate win is the API surface for Phase 4 Salsa integration.

- [ ] **Step 1: Write a failing test asserting `Lifter::new` does no work and `lifter.region(entry)` triggers exactly one CFG build**

```rust
#[test]
fn lifter_defers_until_first_region_query() {
    let lifter = Lifter::new(reader, arch, cc, entry_addr);
    assert_eq!(lifter.regions_built(), 0);
    let _r = lifter.region(entry_addr);
    assert_eq!(lifter.regions_built(), 1);
    // Subsequent queries are cached.
    let _r2 = lifter.region(entry_addr);
    assert_eq!(lifter.regions_built(), 1);
}
```

- [ ] **Step 2: Implement.**
- [ ] **Step 3: Run, expect PASS, commit.**

### Task 2.5: Collapse the per-region driver from `strider`

**Files:**
- Move: `crates/strider/src/strider/per_region.rs` (the `process_insn` + `set_lift_addr` funnel) → `crates/strider-lift/src/region_driver.rs`
- Modify: `crates/strider-lift/src/lib.rs` to expose `RegionDriver`

- [ ] **Step 1: Identify the driver code in v1** — the `set_lift_addr(Some(addr)) … set_lift_addr(None)` wrapper around `process_insn`.
- [ ] **Step 2: Move, with tests.**
- [ ] **Step 3: Old `strider` crate now imports the driver from `strider-lift`.**
- [ ] **Step 4: Run v1 snapshot baseline.** All snapshots match.
- [ ] **Step 5: Commit.**

### Phase 2 Checkpoint

- [ ] Workspace tests pass; v1 snapshot baseline matches.
- [ ] Request code review.
- [ ] Delete the `target`, `pcode-lift`, `cfg` shim crates.
- [ ] Commit.

---

## Phase 3: `strider-analyze` Crate + Egraph Optimizer (3 weeks)

**Goal:** Replace the v1 optimizer (12 hand-written `Optimizer` impls split across stable/destructive/post-pass) with an egraph saturation pipeline. This is the highest-risk phase — budget time for snapshot drift and debugging.

**Skill use:** This phase requires `superpowers:systematic-debugging` at every divergence from v1 snapshots. Don't speculate why an IR diff appeared — bisect the rewrite set, identify the offending rule, fix the rule, re-saturate.

### Task 3.1: Spike — single pass on egraph (`ConstantFold` only)

Confirms the egraph model handles the sea-of-nodes shape before committing to the migration.

**Files:**
- Create: `crates/strider-analyze/Cargo.toml`
- Create: `crates/strider-analyze/src/optimize/constant_fold.rs`
- Create: `crates/strider-analyze/tests/constant_fold_egraph_parity.rs`

- [ ] **Step 1: Write a failing test asserting the egraph-based ConstantFold produces the same IR as v1 ConstantFold on every v1 snapshot fixture**

```rust
#[test]
fn constant_fold_egraph_matches_v1() {
    for fixture in v1_constant_fold_fixtures() {
        let before = load_ir(&fixture);
        let after_v1 = run_v1_constant_fold(&before);
        let after_v2 = run_egraph_constant_fold(&before);
        assert_eq!(after_v1.canonical_form(), after_v2.canonical_form());
    }
}
```

- [ ] **Step 2: Implement `ConstantFold` as `Vec<egg::Rewrite<NodeKind, NodeAnalysis>>`** — one Rewrite per algebraic identity (`(IntConst a) + (IntConst b) → IntConst(a+b)`, `x + 0 → x`, …).
- [ ] **Step 3: Run; debug divergences until PASS.**
- [ ] **Step 4: Commit.**

**Decision point:** if Task 3.1 reveals fundamental egraph/sea-of-nodes incompatibilities (e.g. memory-edge invariants violated by saturation), pause and reconsider. Fallback: keep v1's imperative optimizer but consolidate the stable/destructive split via a clearer `PassEffect` enum. Document the decision in the plan's "Decisions" appendix below.

### Tasks 3.2–3.7: Port remaining rewrite passes

One sub-task per pass, each following the same pattern as 3.1:

- **3.2 KnownBits** (rewrite + analysis hybrid — bit lattice as `egg::Analysis::Data`)
- **3.3 FlagCmpCanonicalize** (pure rewrites)
- **3.4 IfCondInversion** (pure rewrites — `If(BoolNeg(c), a, b) → If(c, b, a)`)
- **3.5 StackStoreDetect + StackLoadForward** (analysis-driven; needs CC)
- **3.6 LoadReadOnly** (analysis with ROM reader)
- **3.7 CallStackArgCollect + FunctionArgDetect** (analyses — produce side-table data on extraction)

Each task: parity test → implementation → bisect any drift → commit.

### Task 3.8: Move `pattern` crate into `strider-analyze`

Mechanical move + shim, like Phase 1/2.

### Task 3.9: Move orchestrator + indirect-resolver into `strider-analyze`

**Files:**
- Move: `crates/strider/src/orchestrator.rs` → `crates/strider-analyze/src/orchestrator.rs`
- Move: `crates/strider/src/indirect_resolve/` → `crates/strider-analyze/src/indirect_resolve/`
- Move: `crates/strider/src/rewrite.rs` → `crates/strider-analyze/src/rewrite.rs`

The old `strider` crate becomes a thin re-export of `strider-analyze`. Phase 5 promotes `strider-py` into the top-level `strider` crate; this intermediate state is short-lived.

### Task 3.10: Salsa orchestrator

**Files:**
- Modify: `crates/strider-analyze/src/orchestrator.rs`
- Add: `salsa = "0.18"` to `strider-analyze/Cargo.toml`

- [ ] **Step 1: Write a failing test asserting that resolving one new indirect-branch target re-lifts only the regions reachable from that target, not the whole CFG.**

```rust
#[test]
fn salsa_invalidates_only_affected_regions() {
    let mut db = StriderDb::new();
    db.set_binary(load("fixtures/out/x86/jump_table.elf"));
    let _ = db.optimized_ir("entry");
    let initial_lifts = db.lift_count();

    // Simulate the indirect-branch resolver discovering a new target:
    db.add_indirect_branch_target("jt_at_0x401234", 0x4015A0);
    let _ = db.optimized_ir("entry");

    let new_lifts = db.lift_count() - initial_lifts;
    assert!(new_lifts < db.total_regions(), "Salsa should not re-lift everything");
}
```

- [ ] **Step 2: Define the Salsa query graph** — `inputs: { binary, indirect_branch_targets }`, `queries: { cfg(addr), region_ir(addr), optimized_eclass(entry) }`.
- [ ] **Step 3: Implement; debug.**
- [ ] **Step 4: Run v1 snapshot baseline — every snapshot still matches.**
- [ ] **Step 5: Commit.**

### Phase 3 Checkpoint

- [ ] Workspace tests pass; v1 snapshot baseline matches; Python test suite passes (via the unchanged `strider-py` going through the shim).
- [ ] Request code review.
- [ ] Delete the `opt`, `pattern` shim crates.
- [ ] Commit.

---

## Phase 4: Pattern DSL Macro (1 week)

**Goal:** Single-source-of-truth pattern definitions emitting both Rust and Python builders.

### Task 4.1: Create `strider-pattern-macros` proc-macro crate

**Files:**
- Create: `crates/strider-pattern-macros/Cargo.toml` (`proc-macro = true`)
- Create: `crates/strider-pattern-macros/src/lib.rs`

The macro processes a `#[strider_pattern] struct Foo { #[field(name, accepts = Pat)] }` and generates:
- A Rust builder type with fluent methods.
- A `Pat` constructor.
- A PyO3 `#[pyclass]` mirror.
- A `pyo3-stub-gen` registration.

### Task 4.2: Migrate one pattern (`StackStorePat`)

- [ ] **Step 1: Write a parity test** — the macro-generated `StackStorePat` matches the same nodes as the hand-written one across the v1 snapshot fixtures.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Verify the Python side via `crates/strider-py/tests/python/test_stack_store.py` — unchanged.**
- [ ] **Step 4: Commit.**

### Tasks 4.3–4.N: Migrate remaining patterns

One pattern per task. Order by simplicity: leaf constructors (`IntConst`, `BoolConst`, `FloatConst`) → unary/binary ops → memory ops → call/return/control. Each: parity test → port → commit.

The lift-time-canonical aliases (`sub`, `int_le`, `int_sle`, `float_sub`, …) and the commutative-matching machinery stay hand-written but live in `crates/strider-analyze/src/pattern/aliases.rs`.

### Phase 4 Checkpoint

- [ ] Workspace tests pass; Python tests pass.
- [ ] Hand-written pattern code dropped from ~3000 LOC to ~500 LOC (the aliases + commutative machinery).
- [ ] Request code review.
- [ ] Commit.

---

## Phase 5: Python-First API (1 week)

**Goal:** Promote `strider-py` to the top-level `strider` crate. Introduce the high-level `Strider` / `Analysis` Python API. Keep the existing low-level surface for backward compatibility.

### Task 5.1: Rename `strider-py` → `strider`

After Phase 3, the old `strider` crate is a thin re-export of `strider-analyze` — delete it. Rename `crates/strider-py/` → `crates/strider/`. Update `Cargo.toml`, `pyproject.toml`, CI.

### Task 5.2: Introduce `strider.Strider`

**Files:**
- Modify: `crates/strider/src/lib.rs`
- Create: `crates/strider/python/strider/_api.py` (Python facade — small layer over the Rust bindings)
- Create: `crates/strider/tests/python/test_high_level_api.py`

- [ ] **Step 1: Write the failing test for the high-level API**

```python
import strider

def test_load_analyze_find():
    s = strider.load("fixtures/out/x86/arithmetic.elf")
    a = s.analyze("add")
    matches = a.find(strider.pattern.call().at(0x401200))
    assert len(matches) == 1
    assert matches[0].address() == 0x4011F0
```

- [ ] **Step 2: Implement `strider.load`, `Strider.analyze`, `Analysis.find` as a thin Python layer on top of `strider.run` + `MemoryMap` + `Graph.find_all`.**
- [ ] **Step 3: Run; expect PASS.**
- [ ] **Step 4: Commit.**

### Task 5.3: `Analysis.fingerprint(node)` proof-of-correctness helper

- [ ] **Step 1: Write failing test** — given a match, `analysis.fingerprint(match.root)` returns the list of source addresses (one or more) that contributed to that node.
- [ ] **Step 2: Implement.**
- [ ] **Step 3: Commit.**

### Task 5.4: Update all Python examples and docs

- [ ] Rewrite `README.md` examples to use `strider.load(...).analyze(...)`.
- [ ] Keep a "Low-level API" section pointing at the building blocks.

### Phase 5 Checkpoint

- [ ] All Python tests pass; the v1 snapshot baseline still matches; the new high-level API has its own snapshot tests.
- [ ] Request code review.
- [ ] Commit.

---

## Phase 6: Cleanup + Final Verification (2 days)

### Task 6.1: Delete all shim crates

- [ ] Audit `crates/` — only `strider-binary` (renamed `reader`), `strider-ir`, `strider-lift`, `strider-analyze`, `strider` (Python bindings), `strider-pattern-macros` should remain.
- [ ] Update `docs/` to reference the new layout.
- [ ] Delete the `superpowers/specs/` files made obsolete by the rewrite; keep the asm-fingerprint and CallOther classification specs (still load-bearing).

### Task 6.2: Update CLAUDE.md

Use the `claude-md-management:revise-claude-md` skill to rewrite the project memory against the new crate layout. The "Crate Dependency Flow" diagram and the "Key Crates" section need full rewrites; the "IR Node Model" and "Register Aliasing" sections survive mostly intact.

### Task 6.3: Property-based test suite

**Files:**
- Create: `crates/strider-ir/tests/proptest_invariants.rs`
- Create: `crates/strider-analyze/tests/proptest_optimizer.rs`

- [ ] **Properties to check:**
  - Every `Graph` produced by `GraphBuilder` passes validation (including Layer C).
  - Every optimization pass preserves validation.
  - Asm-fingerprints are monotonic: `extend_asm_fingerprint` never shrinks the set.
  - Egraph saturation is confluent: two random rewrite orders produce extractable graphs with equal canonical form.

### Task 6.4: Final cross-arch shape-equality verification

- [ ] Re-run the Phase 0 shape-equality test — every architecture's snapshot still matches.
- [ ] Re-run the Phase 0 v1 snapshot baseline — every snapshot still matches.

### Task 6.5: Performance benchmark

**Files:**
- Create: `crates/strider/benches/end_to_end.rs`

- [ ] Benchmark `strider.load().analyze().find(pattern)` on a 50 MB binary.
- [ ] Compare to v1. Target: ≥10× faster on pattern-only workflows (lazy CFG should drop most of the per-binary cost).

### Phase 6 Checkpoint

- [ ] **Final code review** via `superpowers:requesting-code-review` — request review of the cumulative diff against `v1-final`.
- [ ] **Verification before completion** — every test runs green, every snapshot matches, benchmarks documented.
- [ ] Merge to `main`. Tag `v2-final`.

---

## Decisions Appendix

Record decision points here as the rewrite progresses. Each entry: `Date | Phase | Decision | Reason`. Pre-seeded with the open decisions in this plan:

| Date | Phase | Decision | Rationale |
|---|---|---|---|
| — | 3.1 | Whether egraph adapter survives the spike or we fall back to imperative + clearer pass-effect enum. | Decide on first ConstantFold parity test pass/fail. |
| — | 3.5 | Where stack-offset side-table data lives in the egraph model (per-eclass analysis vs out-of-band map). | Decide while implementing `StackStoreDetect`. |
| — | 4 | Whether the proc-macro handles commutative variants automatically or we keep them in `aliases.rs`. | Decide after `IntBinaryOpPat` migration. |

## Risk Register

| Risk | Mitigation |
|---|---|
| Egraph slice (V1 verification) is impractical despite the phi-opaque framing. | Three documented fallback paths in section A: `cranelift-egraph`, hand-rolled saturation, or drop egraph entirely (collapse stable/destructive via `PassEffect` enum per G4). Phase 3 still delivers orchestrator/Salsa work. |
| Verification V1–V6 cumulative failure invalidates the architectural premise. | Phase 0 has no architectural commitment beyond pinning v1 snapshots — it's safe regardless of V1–V6 outcomes. Re-plan from V1–V6 results before Phase 1. |
| Salsa invalidation correctness bugs cause IR drift. | Phase 0 snapshot baseline catches drift on every CI run. |
| Proc-macro generates Python stubs that don't pass pyo3-stub-gen lint. | Migrate one pattern per task — stop and fix early. |
| Performance regression from egg's e-class union cost on huge graphs. | Bench in Task 6.5; if regressed, keep imperative optimizer and only adopt the Salsa orchestrator. |

## Out of Scope

- New binary formats (PE, Mach-O). The reader rename to `strider-binary` is mechanical only.
- New architectures beyond what `target` already exposes.
- Persistent on-disk analysis caches (Salsa is in-memory).
- JS bindings or C-FFI.

---

## Execution Notes

**Recommended approach:** Subagent-driven development. The orchestrator (you) holds the architectural picture and dispatches one subagent per task per phase. Two-stage review (subagent self-review → orchestrator review against this plan) catches drift early.

**Per-phase rhythm:** Land each phase to a branch (`v2/phase-1-ir`, `v2/phase-2-lift`, …), open a PR against `main`, request the `pr-review-toolkit:code-reviewer` agent, merge, tag (`v2-phase-1-done`), then start the next phase from a fresh worktree via `superpowers:using-git-worktrees`.

**Failure recovery:** If a phase's parity test diverges from v1 mid-task, do NOT proceed to the next task. Use `superpowers:systematic-debugging` to isolate the divergence — bisect rewrites, dump IR diffs, identify the broken rule. v1 is always the ground truth: every IR difference is a bug in v2 until proven a v1 bug.
