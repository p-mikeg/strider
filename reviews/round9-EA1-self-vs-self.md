# Round 9 — EA1: Code-vs-code self-consistency

**Branch:** `feature/ai`. Coverage: all 10 paired-site investigations specified.

## Findings

### Finding 1 (HIGH): `ConstantFold` `rule_and_dist` and other multi-node rules emit fresh intermediate nodes with no asm-fingerprint

**Severity:** HIGH.

**Where:** `crates/opt/src/constant_fold/rules.rs:62-71` (rewrite rule) and `crates/pattern/src/rewrite.rs:91-92` (attribution mechanism).

The `((a & C1) | (b & C2)) & C3 → (a & (C1&C3)) | (b & (C2&C3))` rule is implemented via `pattern::rewrite_rule`, which calls `extend_asm_fingerprint_from(new_node, root_node)` only for the **outermost** RHS node (`Or`). The two inner `And(a, IntConst(C1&C3))` and `And(b, IntConst(C2&C3))` are fresh nodes (when `C3` narrows masks) with no fingerprint attribution. Both `IntBinaryOp::And` and `IntConst` are non-exempt. Calling `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` after this rewrite flags both as `MissingAsmFingerprint`.

Same gap affects: `(x + C1) + C2 → x + (C1+C2)` (fresh `IntConst(C1+C2)` unattributed), and other multi-node reassociation rules.

**Fix:** Extend `rewrite_rule` to walk the freshly-built RHS subtree and attribute each new node from the root, OR introduce a multi-node variant, OR replace each multi-node rule with a custom closure using `create_node_attributed`.

**Regression test:** Build a graph where `a` and `b` carry known fingerprints, run `ConstantFold`, call `validate_with_options(..., check_asm_fingerprints: true)`, assert no `MissingAsmFingerprint` errors.

### Finding 2 (MED): `apply_elf_relocations` GOT-PLT path and generic path disagree on Section-relocation error classification

**Severity:** MED.

**Where:** `crates/reader/src/elf.rs:533-540` (GOT-PLT) and `crates/reader/src/elf.rs:581-587` (generic).

GOT-PLT path: any `reloc.target()` not `RelocationTarget::Symbol` → `skipped_malformed_target` (line 523, 538). Generic path: `RelocationTarget::Section(idx)` with `obj.section_by_index(idx)` `Err` → `skipped_unresolved_target` (line 584).

Per `RelocationStats` doc-comments (lines 374-392): `skipped_malformed_target` = "ELF structurally suspect"; `skipped_unresolved_target` = "legitimately unresolvable (weak/extern)". A section-index-out-of-range error is structurally malformed in both paths; the generic path's bucketing is wrong.

**Fix:** Change generic path's `section_by_index(idx)` `Err` arm from `skipped_unresolved_target` to `skipped_malformed_target`.

**Regression test:** Mock object with `RelocationTarget::Section(bad_idx)`; assert `stats.skipped_malformed_target == 1` and `stats.skipped_unresolved_target == 0`.

## Coverage summary (all 10 paired sites)

1. **SP decomposition** (`decompose_sp` × 3 callers): all use same entry-point. `SpExprMemo` scoped per call (shape-match) or shared (CallStackArgCollect) correctly. Cycle-guard correct. No issue.
2. **`CallOther` dispatch preset**: both call sites read same `arch.preset` set via `Builder::for_arch`. No issue.
3. **`read_vn`/`write_vn` mutual-inverse**: shift/mask formulas verified for all widths 1/2/4/8/10/16. Widths 32/64 symmetrically disallow sub-register aliasing. No issue.
4. **Commutativity tables vs free constructors**: `Add|Mul|And|Or|Xor` (int), `And|Or|Xor` (bool), `Add|Mul` (float), `Equal|Carry|Scarry` (int_cmp), `Equal` (float_cmp). All free constructors consistent. No issue.
5. **Lift-time canonicalisations vs pattern aliases**: all 6 canonical shapes match exactly between lifter and pattern alias. No issue.
6. **Asm-fingerprint contract in opt passes**: `ConstantFold` multi-node rules — **VIOLATION** (Finding 1). All other passes verified correct (KnownBits, IfCondInversion, StackStoreDetect, StackLoadForward, FunctionArgDetect, LoadReadOnly, FlagCmpCanonicalize, RedundantPhis, DeadBranchElimination, CallStackArgCollect).
7. **Validator Layer A/B/C reachability scoping**: all checks correctly scoped (Layer A reachability-gated, Layer B explicit, Layer C uniqueness arena-wide by design, others reachability-scoped).
8. **Memory chain monotonicity**: `build_store` always advances; `build_call_with_cc` honours `no_memory_clobber`; `build_call_other_modeled` honours `memory_edge`; `build_call_other_terminal` terminates region. All consistent.
9. **Phi-token ↔ ControlState ownership**: `FunctionBuilder` always wires phi token from owning `ControlState`; `check_layer_c_phis` verifies. All variants (VarPhi, MemPhi, ValuePhi, StackStorePhi) correct.
10. **`apply_elf_relocations` error classification**: divergence — Finding 2.

## Files referenced

- `/mnt/c/Users/mikeg/Documents/strider/crates/opt/src/constant_fold/rules.rs:59-71`
- `/mnt/c/Users/mikeg/Documents/strider/crates/pattern/src/rewrite.rs:83-96`
- `/mnt/c/Users/mikeg/Documents/strider/crates/reader/src/elf.rs:505-545`, `:547-594`
