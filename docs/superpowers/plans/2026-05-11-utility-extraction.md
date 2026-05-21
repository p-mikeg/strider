# Round 16+17 — Algorithmic generalization & utility extraction plan

> Audit date: 2026-05-11.  Branch: `simplification/ai1` (HEAD `5b0f7a8`).
> User directive: "i want more algorithmic generalization utilities."

## Scope

Eight parallel read-only audits surveyed the workspace for algorithm-level + data-structure-level utility extraction opportunities:

1. Fixed-point / iteration / worklist patterns
2. Side-table + entity-keyed map shapes
3. Per-NodeKind classification / dispatch tables
4. Phi-node abstractions
5. Graph mutation / in-place rewriting algorithms
6. Dataflow / propagation / lattice analyses
7. Coercion / type-promotion / sub-register slicing
8. Per-arch / per-CC specialisation patterns

Reports on disk:
- `/tmp/round16-audit-iteration.md` · `/tmp/round16-audit-data.md` · `/tmp/round16-audit-classification.md` · `/tmp/round16-audit-phi.md`
- `/tmp/round17-audit-mutation.md` · `/tmp/round17-audit-dataflow.md` · `/tmp/round17-audit-coerce.md` · `/tmp/round17-audit-arch.md`

## TL;DR

**13 concrete extraction candidates** across 8 categories, plus a documentation pass.  Eight are recommended; five are deferred for documented reasons.  Total Tier 1 + Tier 2 LOC delta: **~ −110 to −170**.  Closes 5 distinct foot-guns (silent-disagreement risk between sites that share an invariant).

The audits also surfaced 4 categories where extraction would be over-engineering today (generic dataflow framework, generic fixed-point driver, NodeKind property merge, MemPhi↔VarPhi unification).  Those are explicitly skipped.

---

## Tier 1 — Mechanical foot-gun mitigations (RECOMMENDED)

### T1-1 — `NodeKind::is_phi()` + `is_asm_fingerprint_exempt()` methods

- **Sites:** `validate/layer_c.rs:123-129` (inline `matches!`), `layer_c.rs:184-197` (standalone fn), `opt::redundant_phis:~181`, `opt::dead_branch:~172`.
- **Proposal:** add `NodeKind::is_phi(&self) -> bool` + `NodeKind::is_asm_fingerprint_exempt(&self) -> bool`.
- **Foot-gun closed:** when a new phi-shaped kind ships, 3+ audit sites today → 1 after.
- **LOC:** −15 · **Risk:** trivial · **Effort:** ~20 min.

### T1-2 — `Graph::remap_side_tables()` wrapper

- **Site:** `graph/compact.rs:237-240` — 4 repetitive remap calls.
- **Proposal:** `Graph::remap_side_tables(&mut self, &HashMap<NodeId, NodeId>)` drives all four.
- **Foot-gun closed:** new per-node side-table on `Graph` automatically picked up by compaction.
- **LOC:** −8 · **Risk:** trivial · **Effort:** ~10 min.

---

## Tier 2 — Algorithm extraction with substantial LOC win (RECOMMENDED)

### T2-1 — `Graph::replace_all_uses_and_detach(old_node, new_output)`

- **Sites (5):** constant_fold, redundant_phis, stack_store::detect, stack_load_forward, function_args — all do "redirect output uses + detach producer inputs (orphan it)".
- **Proposal:** atomic helper that combines `replace_all_uses` + `detach_node_inputs`.
- **LOC:** −12 · **Risk:** low · **Effort:** ~1 hour.

### T2-2 — `gather_call_outputs` helper for orchestrator splice sites

- **Sites:** `orchestrator.rs:689-708` (`apply_in_place_edit`) + `orchestrator.rs:795-826` (`build_anchor_calling_context`) + `ir::builder::call::build_call_with_cc`.
- **Proposal:** factor a free function in `strider::indirect_resolve` (or `target`) that takes `&BuiltCallingConvention` + per-region vn-to-value view → `AnchorCallingContext`.
- **LOC:** −30 · **Risk:** low · **Effort:** ~1.5 hours.

### T2-3 — `synthesize_endian_narrow(wider, wider_ty, narrow_ty, endianness)`

- **Sites (2):** `pcode-lift::vn_io.rs:261-275` (read_vn sub-register read), `opt::stack_load_forward::realize` (lines 383-398, narrow-load-from-wider-store path).
- **Proposal:** centralise the LE/BE-aware Truncate + ShiftRight composition.
- **LOC:** −15 · **Risk:** low · **Effort:** ~45 min.

### T2-4 — `synthesize_truncate_with_fingerprint(src_out, src_ty, target_ty, sources)`

- **Sites (2):** `function_args/mod.rs:347-357` (narrower-read Truncate), `stack_load_forward/mod.rs:367-407`.
- **Proposal:** consolidate "build a Truncate node + thread asm-fingerprint via create_node_attributed".
- **LOC:** −15 · **Risk:** trivial · **Effort:** ~30 min.

### T2-5 — `ConventionView` accessor bag

- **Sites:** four `from_convention(cc)` impls — `StackStoreDetect`, `StackLoadForward`, `FunctionArgDetect`, `CallStackArgCollect`.  Each extracts a different field subset.
- **Proposal:** `pub struct ConventionView { stack_ptr_vn, arg_passing_regs, stack_arg_offsets, endianness, link_register_vn, … }` with one canonical `from_convention_and_arch(cc, arch)` builder.  Each pass takes the view; field access is by accessor.
- **Foot-gun closed:** when a new CC field ships, one site updates instead of N.
- **LOC:** −15 · **Risk:** low · **Effort:** ~1 hour.

### T2-6 — `PhiAnchor` newtype for ControlState → phi connection

- **Site:** `validate/layer_c.rs:139-159` runtime check.
- **Proposal:** `pub struct PhiAnchor(NodeOutputId)` returned from `ControlState` construction; phi ctors require it.  Type-system enforces what Layer C runtime-checks.
- **Foot-gun closed:** invalid phi-token wiring becomes a compile error.
- **LOC:** ~0 (size-neutral; win is type-level) · **Risk:** low · **Effort:** ~1 hour.

---

## Tier 3 — Optional / deferred (DO NOT LAND unless explicitly approved)

### T3-1 — `Graph::drop_cs_predecessor(cs, idx)` helper

DBE's bookkeeping (drop a CS predecessor + walk consuming phis + patch StackStorePhi offsets) is the most subtle mutation in the workspace.  Consolidating saves ~15 LOC but the StackStorePhi side-table mutation is fragile.

**Recommendation:** defer.  Land if a second consumer needs this shape.

### T3-2 — Typed slot accessors (`node.control_in()`, `node.memory_in()`, `node.variadic_args()`)

~20 sites currently use bare `inputs[0]` / `inputs[1]`.  Typed accessors would improve readability but the audit found no actual bugs; pure ergonomic improvement.

**Recommendation:** defer.  Land if a slot-shape change ever bites us.

### T3-3 — `PerPredecessor` trait for phi input indexing

Abstracts "what's pred i's value?" across VarPhi/MemPhi/ValuePhi (input[i+1]) and StackStorePhi (side-table offsets[i]).  Phi audit flagged medium risk.

**Recommendation:** defer.  Reconsider if a fifth phi kind ships.

### T3-4 — `RegionState` consolidation (orchestrator)

Three per-region carriers (`RegionIndex`, `RegionLiftHandles`, `per_address_ccs`) overlap.  ~20 LOC, low-medium risk.

**Recommendation:** defer until the in-flight `incremental-indirect-resolve` plan lands — that work changes per-iteration handle threading.

### T3-5 — `extend_subgraph_fingerprint(root, derives_from)` public helper

The `pattern::rewrite_rule` DFS walk that absorbs fingerprints into every fresh interior node could be made public for non-rewrite-rule callers (e.g. `apply_tail_call`'s splice chain).  But `apply_tail_call` already does the absorption via three `create_node_attributed` calls; no current second consumer.

**Recommendation:** defer.  No second consumer today.

---

## Tier 4 — Documentation updates (RECOMMENDED, zero risk)

CLAUDE.md additions:
1. **Worklist seeding invariant** — `WorkSet::seeded` / `seeded_kind` is the canonical pass driver; document consumer-push contract.
2. **Determinism contract** — all stable + destructive pipeline passes must be deterministic; the orchestrator's fixed-point relies on this.  Document.
3. **Convergence-guard strategy** — pipeline `MAX_ITERS = 1024`, orchestrator stall budget, cfg mini-resolver runs to local convergence.  Document why each strategy fits its loop.
4. **Side-table compaction contract** — all per-NodeId side-tables move together via `remap_side_tables`.  Document at the new helper.

**LOC:** N/A (docs) · **Risk:** zero · **Effort:** ~30 min.

---

## Categories explicitly verified clean

These were investigated and **NOT** recommended for extraction:

1. **Generic `FixedPointDriver<S, D>`** over opt pipeline / strider orchestrator / cfg mini-resolver — three loops have similar structure but **semantically distinct** convergence rules (pipeline is exhaustive; orchestrator has three-way Decision; cfg builder is "drive-to-terminator").  Unifying would shuffle code without reducing complexity.
2. **`IterStep` trait** abstracting `OptimizationResult` + `Decision` + `BuildOutcome` — the orchestrator's three-way decision is *semantically* different from the pipeline's binary; intermediate trait would be cognitive overhead.
3. **Dataflow `Lattice` / `Analysis<L>` framework** — workspace has only 2.5 analyses (KnownBits, SpExpr, asm-fingerprints); framework infra would exceed LOC savings.  Reconsider if a 3rd lattice analysis ships.
4. **NodeKind property table merge** (cacheability + signature + fingerprint exemption) — each property has distinct ownership; merging sacrifices architectural clarity.
5. **MemPhi ↔ VarPhi(memory_vn) merge** — `Region::memory_node` vs `Region::variables` lifetimes are intentionally distinct.
6. **ValuePhi ↔ VarPhi(synthetic_vn)** — pollutes `Vn` semantics for marginal win.
7. **Region ↔ ControlState derivation** — CFG syntactic vs IR semantic; intentionally decoupled.
8. **StackStorePhi → StackStoreJoin rename** — clarity win real, but public API churn (NodeKind variant rename touches pattern builder, strider-py mirror, docs).  Defer.
9. **Lazy `OnceCell<HashMap<...>>`** abstraction — singleton (`largest_container`); no recurrence.
10. **CC preset constructor unification beyond ConventionView** — 22 CC ctors use mutate-base inheritance (verified optimal by Round 14 W-5 + Round 17 arch audit).
11. **Per-arch CallOther dispatch** — already centralised in `target::call_other_abi::classify(preset, name)`.
12. **Endianness propagation** — already correctly threaded through every consumer; no scatter.

---

## Phasing recommendation

| Phase | Items | Effort | LOC delta | Risk |
|-------|-------|-------:|----------:|------|
| Tier 1 | T1-1, T1-2 | ~30 min | −23 | trivial |
| Tier 2 | T2-1 … T2-6 | ~5 hours | −85 to −105 | low |
| Tier 4 | T4 docs | ~30 min | N/A | zero |
| **Total** | 8 items + docs | **~6 hours** | **~−110 to −130** | low |

If Tier 3 is also landed: +~4 hours, +~−45 LOC, medium risk.

---

## Foot-guns closed by Tier 1 + Tier 2

1. **Phi-kind audit-site count** (T1-1): 3 → 1.
2. **Side-table audit-site count** (T1-2): 4 → 1.
3. **CC field-extraction divergence across opt passes** (T2-5): 4 → 1.
4. **Layer C runtime check on phi-token wiring** (T2-6): runtime → compile-time.
5. **Output-use redirection + producer detachment two-step** (T2-1): 5 ad-hoc sites → 1 atomic helper.

---

## Artifacts

- `/tmp/round16-audit-iteration.md` (~270 lines)
- `/tmp/round16-audit-data.md` (~360 lines)
- `/tmp/round16-audit-classification.md` (in-conversation key finding: T1-1)
- `/tmp/round16-audit-phi.md` (key findings: T2-6, T3-3)
- `/tmp/round17-audit-mutation.md` (~400 lines; key findings: T2-1, T3-1, T3-2, T3-5)
- `/tmp/round17-audit-dataflow.md` (~400 lines; verdict: defer all)
- `/tmp/round17-audit-coerce.md` (~440 lines; key findings: T2-3, T2-4)
- `/tmp/round17-audit-arch.md` (key findings: T2-5)
