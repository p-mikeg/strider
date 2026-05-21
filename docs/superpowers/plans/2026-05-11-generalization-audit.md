# Generalization Audit — Round 14 Plan

> Audit date: 2026-05-11.  Branch: `review/ai7` (HEAD `877593f`).
> Scope: every crate in the workspace, with both within-crate and cross-crate dimensions.

## Executive summary

Six parallel read-only audits surveyed the workspace looking for *algorithmic logic / edge cases that are handled separately but could be merged*.  Rounds 11–13 already removed most large-scale duplication.  What remains is a **small tail of 12 actionable items**, almost entirely mechanical, plus a few intentional designs that should stay separate.

| Category | Items | Net LOC delta (combined) | Effort |
|----------|------:|------:|-------:|
| Within-crate generalisations | 8 | −215 to −335 | ~10–12 h |
| Cross-crate generalisations  | 4 | −85 to −145  | ~5–8 h |
| Already optimal (no-op) | 11 | — | — |
| Deferred (major scope) | 2 | varies | — |

The single highest-leverage change is **G-1: `create_derived` API on `ir::Graph` + `pattern::RewriteCtx`**, which unifies the asm-fingerprint propagation idiom across every opt pass that builds RHS nodes.  It removes a documented foot-gun (a new pass author forgetting to call `extend_asm_fingerprint_from` silently breaks the superset contract — Layer-C only fires under `check_asm_fingerprints: true`) and saves 25–40 LOC across 6+ passes.

Three findings were promoted from “low priority” to “high priority” specifically because they remove a *foot-gun* (a way for a future pass author to silently violate an invariant): G-1, G-2, W-2.  Pure LOC savings without foot-gun mitigation rank lower.

Files dropped at synthesis time: `/tmp/round14-audit-{cross,opt,pattern,pyo3}.md` (on disk); `ir`, `cfg+strider`, `support` reports were in-conversation (captured below).

---

## Reading guide

Each finding is keyed: **W-N** for within-crate, **G-N** for cross-crate (general).  Each has:
- **Sites:** file:line citations from at least one (W-) or two (G-) crates
- **Shape:** the shared pattern in 1–2 sentences
- **Proposal:** the unified abstraction + where it lives
- **Difficulty:** trivial / mechanical / moderate / major (the last = cross-crate signature change with downstream churn)
- **LOC delta:** estimated net change
- **Risk:** migration impact on consumers
- **Foot-gun:** is there an existing way to silently break an invariant?  (Used for prioritisation.)

---

## Within-crate findings

### W-1 — Validate Layer C: repeat full-arena iteration pattern

- **Sites:** `crates/ir/src/validate/layer_c.rs` — 6 checks each iterate `graph.nodes.keys()` with distinct early-exits (lines 27–51, 61–96, 113–177, 207–221, 242–259, 277–326)
- **Shape:** `for node in graph.nodes.keys() { if !reachable.contains(node) { continue; } if !matches!(node_kind, …) { continue; } … }` recurs 6×.
- **Proposal:** Extract `fn iter_reachable_of_kind<F: Fn(&NodeKind) -> bool>(graph, reachable, F) -> impl Iterator<NodeId>`.  All six checks become one-liners on top of it.
- **Difficulty:** mechanical (1 file, lifetimes need care)
- **LOC delta:** −60 to −80
- **Risk:** none (private helper)
- **Foot-gun:** Low — a new Layer-C check is more likely to forget reachability scoping; the helper enforces it.

### W-2 — Asm-fingerprint propagation per pass

- **Sites (ir + opt; cross-cutting; primary owner is **opt**):**
  - `crates/opt/src/constant_fold/mod.rs:47-48`
  - `crates/opt/src/flag_cmp_canonicalize/mod.rs:179, 186, 195, 201`
  - `crates/opt/src/dead_branch/mod.rs:113`
  - `crates/opt/src/redundant_phis/mod.rs:103, 114, 144`
  - `crates/opt/src/function_args/mod.rs:~162`
  - `crates/opt/src/pipeline.rs:42-60` (`OptimizationResult::after_replace` already encapsulates *some* of this, but only after-`replace_all_uses`)
- **Shape:** every pass that builds an RHS node calls `extend_asm_fingerprint_from(new_node, old_node)` immediately after `create_node`; multi-node RHSs (e.g. `flag_cmp_canonicalize::rhs_ls` building `BoolNeg(IntLess(...))`) must touch every intermediate or Layer-C with `check_asm_fingerprints: true` fires.
- **Proposal (W-2a, internal to opt):** add `pipeline::replace_and_absorb_fingerprint(ctx, old_out, new_out, old_node)` helper that wraps the canonical sequence (extend + replace + result-fold).  Call-sites shrink to one line.
- **Proposal (W-2b, also see G-1):** push this further upstream by adding `Graph::create_node_derived(kind, inputs, output_kinds, derives_from: NodeId)` — see G-1.
- **Difficulty:** trivial
- **LOC delta:** −20 to −40
- **Risk:** zero (additive)
- **Foot-gun:** **HIGH** — Layer-C is opt-in via `ValidateOptions { check_asm_fingerprints: true }`, so a forgotten propagation slides through default `validate` silently.

### W-3 — Side-table remap loop duplication

- **Sites:** `crates/ir/src/graph/compact.rs:210-240` — 4 side-tables (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`, `call_clobbered_overrides`) remapped with nearly identical 5-line loops differing only in empty-check predicate.
- **Proposal:** generic `fn remap_side_table<T: Default, F: Fn(&T) -> bool>(table, old_to_new, is_empty: F)`.
- **Difficulty:** mechanical
- **LOC delta:** −40 to −50
- **Risk:** none (private helper inside one fn)

### W-4 — Worklist-driven pass driver

- **Sites (opt):**
  - `ConstantFold::optimize` (mod.rs:63)
  - `DeadBranchElimination::optimize` (mod.rs:281)
  - `StackStoreDetect::optimize` (detect.rs:115)
  - `StackLoadForward::optimize` (mod.rs:62)
- **Shape:** `let mut work = WorkSet::seeded(ctx.preorder()); while let Some(n) = work.pop() { … if changed { push consumers } }` recurs.
- **Proposal:** `pub(crate) fn iterate_with_consumers(ctx, seed, |n| -> Result<OptResult>) -> Result<OptResult>` in opt's `pipeline.rs`.
- **Difficulty:** mechanical (worklist lifetime needs care)
- **LOC delta:** −30 to −50
- **Risk:** none (private helper)

### W-5 — Convention-aware pass metadata threading

- **Sites (opt):**
  - `StackStoreDetect::new(stack_ptr_vn)` (detect.rs:97)
  - `StackLoadForward::new(stack_ptr_vn, endianness)` (mod.rs:42)
  - `FunctionArgDetect::new(arg_passing_regs, stack_ptr_vn, stack_arg_offsets)` (function_args/mod.rs:67)
  - Each pass has a `.from_convention(&BuiltCallingConvention)` builder.
- **Shape:** three passes unpack the same fields out of the same CC and store them.
- **Proposal:** `opt::ConventionMetadata { stack_ptr_vn, arg_passing_regs, stack_arg_offsets, endianness }` + `ConventionMetadata::from(&BuiltCallingConvention, &SleighArch)`.  Each pass holds one field of that shape.
- **Difficulty:** mechanical
- **LOC delta:** −15 to −25
- **Risk:** very low (private to opt; pass constructors take the new struct)

### W-6 — Match accessor dispatch (pattern)

- **Sites:** `crates/pattern/src/matcher/match_result.rs:64-165` — 8 typed accessors (`get_int`, `get_uint`, `get_bool`, `get_float_bits`, `get_int_binary_op`, `get_vn`, `stack_offset`, `stack_phi_offsets`) all delegate to `Bindings` via 4–6 lines each.
- **Shape:** capture → binding → graph-fetch → extract.
- **Proposal:** trait `TypedExtractor` with one method `extract(&Bindings, c, &Graph) -> Option<Self::Target>` and 8 impls.  `Match` delegates via `m.extract::<IntConst>(c, &g)`.
- **Difficulty:** moderate (trait + 8 impls; reduces readability per the pattern-crate auditor)
- **LOC delta:** −30 to −50 (offset by trait infrastructure)
- **Risk:** **medium** — call-site ergonomics change (turbofish), and the pattern auditor explicitly recommends *not* doing this.
- **Verdict:** **DEFER** — readability of the explicit per-method form is load-bearing for Python-binding mirroring (each accessor maps to a Py method).

### W-7 — Bound-check + OOB classification in cfg::region_builder

- **Sites:** `crates/cfg/src/cfg/builder/region_builder.rs:238-384`
- **Shape:** the CondBranch OOB-collapse (both-in-range / both-OOB → TailCall / one-OOB → Branch) is tightly coupled to the per-insn loop.
- **Proposal:** `struct CondBranchClassification { terminator, edges }` + a stateless `classify(cond, taken, not_taken, bound) → CondBranchClassification`.
- **Difficulty:** trivial (pure data-shape refactor)
- **LOC delta:** ~−5 net (+20 in types, −25 in region_builder)
- **Risk:** very low
- **Verdict:** **OPTIONAL** — works fine today; only worth doing if region_builder needs to grow another bound-check site.

### W-8 — Reader wrapper: inline `PyMemoryMapReader` into `PyMemoryMap`

- **Sites:** `crates/strider-py/src/reader.rs:652-725` (PyMemoryMapReader: 75 LOC wrapper struct) + `reader.rs:87-450` (PyMemoryMap).
- **Shape:** `PyMemoryMapReader` exists solely to bridge `PyMemoryMap → rsleigh::MemReader`.
- **Proposal:** `impl rsleigh::MemReader for PyMemoryMap` directly (or, if the orphan-rule blocks it, `impl … for &PyMemoryMap` plus an `Arc<PyMemoryMap>` instance).
- **Difficulty:** mechanical
- **LOC delta:** −75
- **Risk:** very low (internal type)
- **Foot-gun:** none.

---

## Cross-crate findings

### G-1 — `create_derived` for asm-fingerprint propagation (ir ↔ opt ↔ pattern)

- **Owner:** **ir** (Graph creation primitive); consumed by opt + pattern.
- **Sites:** every opt pass under W-2 above, plus `pattern::rewrite_rule` (currently absorbs only the outermost RHS node — `crates/pattern/src/rewrite.rs:43-160`).
- **Shape:** the “create node + immediately call `extend_asm_fingerprint_from`” pair recurs in 8+ places.  In `pattern::rewrite_rule`, the equivalent operation is *missing* for interior RHS nodes — which is the precise reason `flag_cmp_canonicalize` had to roll its own RHS-builder fn-pointer mechanism (see CLAUDE.md note about the bespoke pattern vs `pattern::rewrite_rule`).
- **Proposal:**
  - **G-1a:** add `Graph::create_node_derived(kind, inputs, output_kinds, derives_from: NodeId) -> NodeId` to `ir`.  Equivalent to `create_node` + `extend_asm_fingerprint_from`.
  - **G-1b:** expose the same shape on `pattern::RewriteCtx` (which already derefs to `Graph`).
  - **G-1c:** extend `pattern::rewrite_rule` to walk fresh interior RHS nodes and absorb the matched-root fingerprint into each.  This retires the bespoke `flag_cmp_canonicalize::Rule { build_rhs: fn-pointer }` machinery — that pass becomes a normal `rewrite_rule`-driven set.
- **Difficulty:** mechanical for G-1a/b; **moderate** for G-1c (reverse-walk from RHS output back to LHS captures, careful not to double-absorb on cached nodes).
- **LOC delta:** −60 to −100 cumulative (G-1a/b save ~25–40; G-1c retires ~70 LOC of `flag_cmp_canonicalize` bespoke RHS-builder infrastructure).
- **Risk:** G-1c needs heavy validation against existing rules — the failure mode is silent (Layer-C only catches with `check_asm_fingerprints: true`).
- **Foot-gun:** **HIGH** (the primary reason this is the #1 priority).
- **Verdict:** **Phase 1 priority.**

### G-2 — Register-aliasing logic split (pcode-lift ↔ ir)

- **Owner:** new home should be **`target`** (with the architecture data).
- **Sites:**
  - `crates/pcode-lift/src/vn_io.rs:141-300` (full container/sub-register math: `find_largest_fitting_register`, `read_vn`, `write_vn`)
  - `crates/ir/src/builder/mod.rs:134, 488-525` (cached map `largest_container: OnceCell<HashMap<Vn, Vn>>`)
  - `crates/ir/src/builder/mod.rs:51-93` (`upgrade_to_tracked_for` — cover + sub-register fallback over the *tracked-variable* subset)
- **Shape:** three separate-but-overlapping aliasing computations.  When a new container family (e.g. an AMX tile register) ships, all three sites need updating.
- **Proposal:** new `target::aliasing::AliasMap` (or a name in that vein) that owns the container→sub-register relation for a given Sleigh `regs` table.  pcode-lift consumes it; ir-builder consumes it; the builder cache becomes a thin memoising view.  `upgrade_to_tracked_for` consults the same data restricted to its tracked-set.
- **Difficulty:** moderate (cross-crate sig change; needs careful validation against existing test fixtures)
- **LOC delta:** −40 to −60
- **Risk:** moderate — aliasing logic is correctness-critical (any divergence here causes wrong-register IR).  Must keep the existing per-call O(V) scan + cache semantics for performance.
- **Foot-gun:** moderate — three places that must stay aligned.

### G-3 — `target::ArchContext` (target ↔ cfg ↔ opt ↔ strider)

- **Owner:** **`target`**.
- **Sites:**
  - `cfg::Builder::for_arch(arch, …)` — `crates/cfg/src/cfg/builder/mod.rs:60-109`
  - `opt::LoadReadOnly::new(rom, endianness)` — takes endianness explicitly
  - `opt::StackLoadForward::new(stack_ptr_vn, endianness)` — takes endianness explicitly
  - `pcode-lift::ValueLifter` — carries it internally
- **Shape:** every consumer re-extracts `{preset, endianness}` from a different upstream source.  No shared `(preset, endianness)` bundle.
- **Proposal:** `struct target::ArchContext { preset: ArchPreset, endianness: Endianness }` + a single point of construction (probably `SleighArch::context()`).  Thread it instead of piecemeal extracts.
- **Difficulty:** trivial (new struct + 4–5 call-site signature touches)
- **LOC delta:** ~+15 (new struct) − 10 (consolidations) ≈ neutral, but kills a class of "wrong endianness for this preset" bugs.
- **Risk:** low
- **Foot-gun:** medium — currently nothing prevents a caller from passing `Endianness::Big` together with `ArchPreset::X86_64`.

### G-4 — `cfg::Cfg` implements `graphwalk::GraphRef` (graphwalk ↔ cfg)

- **Owner:** **cfg** (consumer-side impl block).
- **Sites:**
  - `graphwalk::GraphRef` trait — `crates/graphwalk/src/lib.rs:33-53`
  - `ir::walk::GraphWalkSuccs` — uses it
  - `cfg::Cfg` — currently uses petgraph::StableDiGraph directly; no GraphRef impl
- **Shape:** the abstract graph-traversal interface is implemented for IR but not for cfg.  Any future need (reachability over regions, dominance analysis on the CFG) requires duplicating algorithms.
- **Proposal:** `impl<R: rsleigh::MemReader> graphwalk::GraphRef for cfg::Cfg<R>` — ~10 LOC pass-through to `self.graph.neighbors(node)`.
- **Difficulty:** trivial
- **LOC delta:** +10 (now) → −20 to −40 future savings if/when a pass needs CFG reachability
- **Risk:** none (pure addition)
- **Foot-gun:** none.
- **Verdict:** **easy win, do now even if no immediate caller** — it makes the abstraction promise honest.

---

## Already optimal (no-op verdicts)

These were investigated and found to be intentional, not duplicative:

1. **CC presets in `target`** — kernel/syscall variants use mutate-and-return over a base preset (`x86_linux_kernel` = mutate `x86_cdecl`).  Optimal given Rust's lack of named-struct inheritance.
2. **call_other_abi register entries** — `mfence`/`sfence`/`lfence` share a `PURE` const; the audit found no further extractable constants worth the indirection.
3. **`Worklist<E>` (entity-utils) vs `WorkSet` (opt)** — different abstraction levels; `WorkSet` has opt-specific seeded ctors that don't belong in entity-utils.
4. **ELF relocation autoload wrapper** — `apply_elf_relocations_autoload` wraps `apply_elf_relocations`; two-phase design is semantically clear, refactoring to share would *add* complexity.
5. **`DBE` + `RedundantPhis` division of labor** — documented co-design; not duplication.
6. **`find_all_multi` vs `find_all_requirements`** — the latter already calls the former and adds a cross-product filter; no extractable shared logic.
7. **Lift-time canonicalisation aliases** (`pattern::sub`, `int_le`, `int_sle`) — each is intentionally explicit to document the IR shape after lift-time lowering.
8. **Pattern-builder `From<>` per builder** — type safety dominates ~30 LOC savings.
9. **`SpExprMemo` per pass** — memo lifetime is correctly per-`optimize` call; no cross-pass sharing makes sense.
10. **CallOther dispatch table** — `target::call_other_abi::classify(preset, name)` is the single source of truth and both consumers (cfg terminator + strider lift) consult it.  The 3-way `(NoOp|NoReturn|Call(abi))` split is correct.
11. **Endianness threading** — *individually* threaded sites are correct; G-3 above would tidy the bundling, but each site's threading is sound.

---

## Deferred (major scope)

### D-1 — `NodeKind` ↔ `PatKind` mirror via proc-macro

- **Sites:** `crates/ir/src/node/kind.rs` (50+ NodeKind variants) ↔ `crates/pattern/src/pat/builders/` (mostly mirrors them).
- **Shape:** when `ir` adds a NodeKind, `pattern` requires manual mirror updates.  Recent additions (`ValuePhi`, `StackStorePhi`, `IntConstWide`) all required hand-written pattern builders.
- **Proposal:** proc-macro `#[mirror_node_kind]` driven by a manifest (e.g. `crates/target/node_kinds.toml`) that generates both enum + builders.
- **Difficulty:** **major** (macro design, build-script phase, downstream churn)
- **LOC delta:** −50 to −80 in pattern; +macro infrastructure
- **Risk:** high — changes node-definition authority
- **Verdict:** **DEFER** — gain is real but the proc-macro carries its own complexity tax.  Re-evaluate if the gap between NodeKind additions and pattern mirroring becomes a recurring maintenance pain point.

### D-2 — Typed error hierarchy across crates

- **Sites:** every crate uses `anyhow::Result`; `strider-py::errors::into_strider_err` and `into_lift_err` downcast via 1–2 typed-error variants plus a string-match heuristic.
- **Shape:** typed-PyErr translation at the boundary needs richer downcastable error types in each Rust crate.
- **Proposal:** introduce `thiserror`-driven typed error enums per crate (`IrError`, `LiftError`, `PatternError`, …) and downcast against the typed roots in strider-py.
- **Difficulty:** **major** (every error site in 4+ crates)
- **LOC delta:** +200 to +400 (error boilerplate)
- **Risk:** very high — changes error model workspace-wide
- **Verdict:** **DEFER** — only worth doing if user-facing Python error UX becomes a priority.

---

## Phasing recommendation

### Phase 1 — Foot-gun mitigations (highest priority)

| Item | Crate(s) | LOC Δ | Effort | Foot-gun |
|------|----------|------:|-------:|:-------:|
| G-1a/b: `create_node_derived` in ir + RewriteCtx | ir, opt, pattern | −25 to −40 | ~2 h | HIGH |
| W-2: `pipeline::replace_and_absorb_fingerprint` helper | opt | −20 to −40 | ~1 h | HIGH (companion to G-1) |
| G-3: `target::ArchContext` bundle | target, cfg, opt | ~0 (neutral) | ~2 h | MEDIUM |

Phase-1 changes are mostly *additive* (new helpers + opt-in migration); existing code continues to work.  They primarily remove a class of silent bug.

### Phase 2 — Mechanical LOC wins (no foot-gun)

| Item | Crate(s) | LOC Δ | Effort |
|------|----------|------:|-------:|
| W-1: `iter_reachable_of_kind` helper in validate | ir | −60 to −80 | ~2 h |
| W-3: `remap_side_table` generic helper | ir | −40 to −50 | ~1 h |
| W-4: `iterate_with_consumers` helper | opt | −30 to −50 | ~1.5 h |
| W-5: `ConventionMetadata` struct | opt | −15 to −25 | ~1 h |
| W-8: inline `PyMemoryMapReader` into `PyMemoryMap` | strider-py | −75 | ~1 h |
| G-4: `impl GraphRef for cfg::Cfg` | cfg | +10 (future-facing) | ~30 min |

### Phase 3 — Cross-crate refactor (needs validation)

| Item | Crate(s) | LOC Δ | Effort |
|------|----------|------:|-------:|
| G-1c: extend `rewrite_rule` to absorb fingerprints into interior nodes, retire `flag_cmp_canonicalize` bespoke RHS-builder | pattern, opt | −70 (net, after retiring bespoke RHS) | ~4 h + heavy testing |
| G-2: `target::aliasing::AliasMap` shared by pcode-lift + ir | target, pcode-lift, ir | −40 to −60 | ~4–6 h + correctness validation |

Phase 3 has the most upside but needs the most test coverage — both items affect correctness paths (asm-fingerprint contract, register aliasing) where the failure mode is silent.

### Skip / Defer

- W-6 (match-accessor dispatch trait) — readability cost > 30–50 LOC savings.
- W-7 (CondBranchClassification struct) — works fine; only worth doing if a second consumer appears.
- D-1, D-2 (NodeKind mirror macro, typed error hierarchy) — major scope, low immediate return.

---

## Per-finding action checklist (for an executor agent)

Each Phase-1 / Phase-2 / Phase-3 row above is one self-contained patch.  For each:

1. **Write the failing test** (or red-line clippy lint) that demonstrates the current foot-gun / duplication.
2. **Add the new helper / struct / impl** in the owning crate.
3. **Migrate one call site** end-to-end; verify the test goes green and the existing test suite stays green.
4. **Migrate the remaining call sites** mechanically.
5. **Delete the old per-site logic** in a separate commit (so the rollback story is clean).
6. **Run `cargo build/test/clippy --workspace --all-targets -- -D warnings`** between each commit.

For Phase 3 items (G-1c, G-2), step 1 is mandatory and must include a `validate_with_options { check_asm_fingerprints: true }` regression test, since the failure mode is silent under default validation.

---

## Artifacts

- `/tmp/round14-audit-cross.md` (cross-crate audit — 304 lines)
- `/tmp/round14-audit-opt.md` (opt crate — 271 lines)
- `/tmp/round14-audit-pattern.md` (pattern crate — 243 lines)
- `/tmp/round14-audit-pyo3.md` (strider-py — 234 lines)
- IR + pcode-lift audit (in-conversation; key findings folded into W-1, W-2, W-3, G-1, G-2 above)
- cfg + strider audit (in-conversation; key findings folded into W-7, G-3 above)
- support/target/reader audit (in-conversation; key findings folded into G-3, G-4, "already optimal" above)

## Honest assessment

The workspace is in good shape.  Twelve rounds of cleanup have removed nearly every large-scale duplication; what remains is a small mechanical tail plus two design questions (D-1, D-2) that are real but not load-bearing today.  The single highest-value change is **G-1** (the `create_derived` propagation primitive) — not because of LOC savings, but because it retires a documented foot-gun that lives behind an opt-in validator flag.

After Phase 1 lands, the next-round audit should focus on test coverage of the asm-fingerprint contract under `check_asm_fingerprints: true`, since the propagation invariant is currently the costliest invariant to verify by hand.
