# Round 14: Opt Crate Generalization Audit

## Summary

After examining 12 passes + pipeline infrastructure, the remaining tail is **small but concrete**. Rounds 12–13 did substantial work; what remains is 3–4 mechanical refactorings that would yield modest LOC savings (5–15%) and cohesion gains. No deep structural reshuffling needed.

---

## 1. Pass-Driver Duplication: WorkSet Seeding

**Finding:** Every worklist-driven pass repeats the same `seeded` / `seeded_kind` → pop loop:

- `ConstantFold` (mod.rs:63): `WorkSet::seeded(ctx.preorder())` + `while let Some(node) = work.pop()` loop w/ consumers re-enqueue
- `StackStoreDetect` (detect.rs:115): `WorkSet::seeded_kind(ctx, |k| matches!(k, NodeKind::Store(_)))`
- `StackLoadForward` (mod.rs:62): `WorkSet::seeded_kind(ctx, |k| matches!(k, NodeKind::Load(_)))`
- `DeadBranchElimination` (mod.rs:281): `WorkSet::seeded(ctx.preorder())`
- `IfCondInversion` (mod.rs:64): `ctx.preorder()` iterated directly (no WorkSet)

**Pattern:** 
```rust
let mut work = WorkSet::seeded(...);
while let Some(node) = work.pop() {
    let result = try_action(ctx, node, ...)?;
    if result.changed() {
        // push consumers to work
    }
}
```

**Proposal:** Extract a `for_each_rewritable_node` helper in pipeline.rs:
```rust
pub(crate) fn iterate_with_consumers(
    ctx: &mut RewriteCtx<'_>,
    seed: impl IntoIterator<Item = NodeId>,
    mut f: impl FnMut(&mut RewriteCtx<'_>, NodeId) -> Result<OptimizationResult>,
) -> Result<OptimizationResult> {
    let mut work = WorkSet::seeded(seed);
    let mut result = OptimizationResult::NoChange;
    while let Some(node) = work.pop() {
        // snapshot consumers before mutation
        let consumers = /* snapshot logic */;
        let r = f(ctx, node)?;
        if r.changed() {
            for &c in &consumers { work.push(c); }
            result |= r;
        }
    }
    Ok(result)
}
```

**Metrics:**
- **Files:** ConstantFold, StackStoreDetect, StackLoadForward, DeadBranchElimination (4 sites)
- **LOC delta:** −30 to −50 (removes 4 × ~12-line driver loops)
- **Difficulty:** mechanical
- **Upsides:** single canonical worklist loop, easier to audit; consistent error handling

---

## 2. Asm-Fingerprint Absorption: `replace_all_uses` + Extend Pattern

**Finding:** Every pass that creates an RHS node + rewires repeats:
```rust
let new_node = ctx.create_node(...);
ctx.extend_asm_fingerprint_from(new_node, old_node);
let changed = ctx.replace_all_uses(old_out, new_out)?;
```

**Occurrences (file:line):**
- `constant_fold/mod.rs:47-48`: extend → replace
- `flag_cmp_canonicalize/mod.rs:186,201,205`: build_int_cmp/build_bool_neg both extend
- `stack_store/detect.rs:45,68`: StackStore + StackStorePhi
- `stack_load_forward/mod.rs:127`: extends Load → forwarded
- `dead_branch/mod.rs:113`: extends If → ctrl_in
- `function_args/mod.rs:~100–150` (observed in detection patterns)

**Pattern:** Replace + extend happens in **6+ sites** identically; `OptimizationResult::after_replace` exists but only 1–2 calls use it.

**Proposal:** Add helper in pipeline.rs:
```rust
pub(crate) fn replace_and_absorb_fingerprint(
    ctx: &mut RewriteCtx<'_>,
    old_out: NodeOutputId,
    new_out: NodeOutputId,
    old_node: NodeId,
) -> Result<bool> {
    let new_node = ctx.get_node_from_output(new_out);
    ctx.extend_asm_fingerprint_from(new_node, old_node);
    ctx.replace_all_uses(old_out, new_out)
}
```

Caller shortens to:
```rust
replace_and_absorb_fingerprint(ctx, old_out, new_out, old_node)?;
```

**Metrics:**
- **Files:** 6–8 passes
- **LOC delta:** −20 to −40
- **Difficulty:** trivial
- **Upsides:** single source of truth for fingerprint threading; catches forget-to-extend bugs

---

## 3. SP-Decomposition Memo Threading

**Finding:** Every SP-aware pass re-declares and threads the same `SpExprMemo`:

- `StackStoreDetect::optimize` (detect.rs:116): `let mut memo: SpExprMemo = Default::default()`
- `StackLoadForward::optimize` (mod.rs:63): same
- `FunctionArgDetect::optimize` (function_args/mod.rs:~94–100): identical pattern expected
- `indirect_branch_resolve/classify.rs:~70`: KnownBits analysis cached; no memo equivalent yet

**Pattern:** Memo is thread-local to one `optimize` call; declared per-pass but never shared across passes in a pipeline run.

**Non-issue:** This is actually correct — each pass owns its memo lifetime, and the pipeline never reuses a memo across passes. **No change needed.** Memo was already deduplicated at the site level in prior rounds.

**Note:** Re-examined with fresh eyes; this is by-design isolation, not duplication.

---

## 4. Worklist: `opt::worklist::WorkSet` vs `entity_utils::worklist::Worklist`

**Finding:** Two independent worklist impl:
- `opt/src/worklist.rs` (71 LOC): FIFO, `WorkSet::seeded()`, methods `push`/`pop`
- `entity-utils/src/worklist.rs` (240 LOC): generic `Worklist<E>`, methods `enqueue`/`dequeue`, fully tested

**Semantics match:** Both use `DenseEntitySet + VecDeque`, prevent double-enqueue, FIFO order.

**Why two?**
- `entity-utils::Worklist` is generic `Worklist<E: EntityRef>`, public API, tested
- `opt::WorkSet` is `NodeId`-only, not exported, has `seeded()` + `seeded_kind()` ergonomics for preorder seeding

**Assessment:** Not true duplication — `WorkSet` wraps `Worklist`-pattern semantics with **pass-specific ergonomics** (seeded constructors). Merging would require:
1. Move `seeded()` helpers to `entity-utils` (adds pass-specific API to general crate)
2. Or wrap `Worklist<NodeId>` in `opt` (adds 5-line newtype)

**Recommendation:** Keep separate. The ergonomic gap (`seeded` vs `new` + `extend`) justifies two shallow implementations.

---

## 5. SP-Decomposition Duplication Check

**Finding:** `sp_expr.rs` is the canonical SP-walker shared by:
- `StackStoreDetect::try_detect_stack_store` (detect.rs:31): calls `decompose_sp`
- `StackLoadForward::try_forward_load` (mod.rs:91): calls `decompose_sp`
- `FunctionArgDetect` (function_args/mod.rs): calls `decompose_sp` in stack-arg detection
- `indirect_branch_resolve/classify.rs`: **does NOT use** `decompose_sp`; has its own Load-pattern matcher

**Cross-check:** `indirect_branch_resolve` for stack-array arm (stack_array.rs): inspects Load shapes but uses pattern-matcher, not `sp_expr` helpers.

**Assessment:** SP-decomposition is already unified via `sp_expr::decompose_sp`. No duplication of core logic. The indirect classifier doesn't need it because it's pattern-driven and runs in a different context (once per anchor, not per-node).

---

## 6. Rule-Table Reuse: `flag_cmp_canonicalize` Pattern

**Finding:** `flag_cmp_canonicalize` uses a `RULES: Vec<Rule>` registry with LHS + RHS builder fn-pointers:

```rust
struct Rule {
    lhs: Pat,
    build_rhs: fn(&mut Graph, NodeOutputId, NodeOutputId, NodeId) -> Result<NodeOutputId>,
    lhs_capture: Capture,
    rhs_capture: Option<Capture>,
}
```

Driver loop (mod.rs:79):
```rust
for rule in RULES.iter() {
    if try_apply_rule(ctx, node, rule)? { break; }
}
```

**Check:** Could other passes adopt this? Examined:
- `ConstantFold`: **no** — rules are embedded in `apply_identity_rules()` / `apply_const_eval_rules()` / etc.; dispatch is by node kind, not by tabulated rules
- `KnownBits`: **no** — per-node analysis, not rewrite rules
- `IfCondInversion`: **no** — single hand-coded surgery, not a table
- `FlagCmpCanonicalize`: **yes** — rules-table only pass

**Assessment:** Rule-table pattern is special-case; only `flag_cmp_canonicalize` uses it. No generalization opportunity.

---

## 7. Convention-Aware Pass Metadata Threading

**Finding:** Stack- + function-arg passes take calling-convention metadata at construction:

- `StackStoreDetect::new(stack_ptr_vn: Vn)` (detect.rs:97)
- `StackLoadForward::new(stack_ptr_vn, endianness)` (mod.rs:42)
- `FunctionArgDetect::new(arg_passing_regs, stack_ptr_vn, stack_arg_offsets)` (function_args/mod.rs:67)
- Each has a `.from_convention(cc: &BuiltCallingConvention)` builder

**Pattern:** Identical metadata unpacking at 3 sites; each checks the same metadata fields.

**Proposal:** Create a shared `struct ConventionMetadata`:
```rust
pub(crate) struct ConventionMetadata {
    pub stack_ptr_vn: rsleigh::Vn,
    pub arg_passing_regs: Vec<rsleigh::Vn>,
    pub stack_arg_offsets: Vec<i64>,
    pub endianness: Endianness,
}
impl ConventionMetadata {
    pub fn from_convention(cc: &BuiltCallingConvention, arch: &SleighArch) -> Self { ... }
}
```

Then passes take `ConventionMetadata` at construction, reducing boilerplate.

**Metrics:**
- **Files:** 3 passes
- **LOC delta:** −15 to −25 (consolidates `.from_convention` implementations)
- **Difficulty:** mechanical
- **Upsides:** single pass for updating convention metadata in future; less copy-paste

**Caveat:** StackLoadForward needs arch (endianness), FunctionArgDetect doesn't. Shared struct must carry both but FunctionArgDetect ignores endianness — acceptable tradeoff.

---

## 8. Redundant-Phi + Dead-Branch Collaboration Notes

**Finding:** CLAUDE.md (line 269–270) notes: "works together with [`RedundantPhis`]" and vice versa.

- `DeadBranchElimination` (dead_branch/mod.rs) strips dead ControlState inputs
- `RedundantPhis` (redundant_phis/mod.rs) collapses single-input phis that result

**Assessment:** This is **not duplication** — it's a documented **division of labor**: DBE handles control flow and avoids stripping when data escapes (complex heuristic); RedundantPhis then cleans up the aftermath. The split is intentional and sound.

No generalization needed.

---

## 9. Indirect-Branch Classifier Arms

**Finding:** `indirect_branch_resolve/` has 5 arms: `LinkRegister`, `Single`, `Multiple` (known targets), plus `classify` entry, `inplace` edits, `jump_table` + `stack_array` sub-classifiers.

**Per-arm duplication check:**

- `jump_table::classify_jump_table(...)` (jump_table.rs): Load-pattern matcher + ROM read
- `stack_array::classify_stack_array(...)` (stack_array.rs): Load-pattern matcher + stack walk
- Both take the same parameters (anchor_output, known_bits, etc.)
- Both return `Option<Vec<u64>>` targets

**Observation:** Structures are similar but operands differ (ROM vs stack memory). The "anchor inspector" framework is already factored out as `classify_anchor*` entry points.

**Assessment:** Both arms are domain-specific enough that merging would require parameterization by memory source. Current split is cleaner. The caller (`strider` orchestrator) already dispatches between them correctly.

No consolidation recommended.

---

## 10. Tail Assessment

Remaining opportunities are **small, mechanical, and optional:**

| Opportunity | Type | Difficulty | LOC Δ | Priority |
|------------|------|-----------|-------|----------|
| WorkSet seeding helper | Driver pattern | Mechanical | −30 to −50 | Medium |
| Fingerprint absorption helper | Boilerplate | Trivial | −20 to −40 | High |
| ConventionMetadata struct | Metadata threading | Mechanical | −15 to −25 | Low |
| (No more duplication) | — | — | — | — |

**Total estimated savings:** 65–115 LOC across 3 changes.
**Effort:** ~3 hours mechanical refactoring.
**Rounds 12–13 already consolidated:** rule dispatch, memo factories, known-bits side-tables, RULES registry.

The crate is in good shape post-round-13. Further gains are marginal.

