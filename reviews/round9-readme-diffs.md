# Round 9 — Per-crate README and SKILL.md correctness diffs

Concrete edit proposals from R9-3A (doc-verify) and R9-3B (stale-comment sweep) for files outside CLAUDE.md.

---

## ir/README.md

### Edit — Line 50: `NodeOutputType` variant list incomplete

**Source:** R9-3A Issue A (conf 100).

**Current:**
```
`NodeOutputType` (`Bool` / `U8`–`U256` / `F32` / `F64`)
```

**Proposed:**
```
`NodeOutputType` (`Bool` / `U8` / `U16` / `U32` / `U64` / `U80` (x87 80-bit) /
 `U128` / `U256` / `U512` (AVX-512 zmm) / `F32` / `F64` / `F80` (x87 extended-precision))
```

**Rationale:** Round-8 added U80/U512/F80; the README description was not updated. The actual enum has 12 variants.

---

## pcode-lift/README.md

### Edit — Line 49: `vn_mask` width list omits 32 and 64

**Source:** R9-3A Issue C (conf 100).

**Current:**
```
Width support: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register) bytes.
```

**Proposed:**
```
Width support: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register),
32 (YMM), 64 (ZMM) bytes. Widths 32 and 64 use a degraded `u128::MAX` mask;
sub-register aliasing within > 16-byte containers raises an error.
```

**Rationale:** `crates/pcode-lift/src/vn_io.rs:45` — `16 | 32 | 64 => Ok(u128::MAX)`. CLAUDE.md is correct (line 147); only the per-crate README lagged.

---

## opt/README.md

### Edit — Line 9: opening sentence implies all passes implement `Optimizer`

**Source:** R9-3A Claim 21.

**Current:**
```
`Optimizer` — every pass implements this trait. `run(&mut graph) -> Result<OptimizationResult>`
```

**Proposed:**
```
`Optimizer` — most passes implement this trait. `run(&mut graph) -> Result<OptimizationResult>`.
`IfCondInversion` implements the related `OptimizerOnBuilt` trait (defined on the line below)
because it needs `BuiltFunctionGraph`-level access for control-flow surgery.
```

---

## README.md (root)

### Edit — Line 256: quickstart uses deprecated CC alias

**Source:** R9-3B HIGH (conf 100).

**Current:**
```
strider.run(arch=..., cc=strider.cc.x86_64_systemv_abi(), ...)
```

**Proposed:**
```
strider.run(arch=..., cc=strider.cc.x86_64_systemv(), ...)
```

---

## SKILL.md updates

### strider-fingerprint-audit/SKILL.md — Line 24

**Source:** R9-3A Issue D (conf 90).

**Current:**
```
Public API on `Graph` (`crates/ir/src/graph/store.rs:108-160`)
```

**Proposed:**
```
Public API on `Graph` (`crates/ir/src/graph/store.rs:127-200`)
```

(Or remove the line range entirely.)

### strider-opt-pass-author/SKILL.md — Line 30

**Source:** R9-3A Issue E (conf 90).

**Current:**
```
`Graph::extend_asm_fingerprint_from(new, contributor)` (`crates/ir/src/graph/store.rs:160`)
```

**Proposed:**
```
`Graph::extend_asm_fingerprint_from(new, contributor)` (`crates/ir/src/graph/store.rs:184`)
```

### strider-opt-pass-author/SKILL.md — add multi-node rewrite_rule trap

**Source:** EA1 F1 (HIGH).

**Proposed insert** under "Propagate asm-fingerprints (REQUIRED)" section:
```
**Trap: multi-node rewrite_rule rules.** `pattern::rewrite_rule` calls
`extend_asm_fingerprint_from(new_node, root_node)` only for the **outermost**
RHS node. If your rule's RHS produces additional fresh interior nodes
(e.g. `((a&C1)|(b&C2))&C3 → (a&(C1&C3)) | (b&(C2&C3))`), the inner `And`
nodes will have empty fingerprints and will fail
`validate_with_options(check_asm_fingerprints: true)`. For multi-node RHS rules:
either use `create_node_attributed(... &[contributor])` directly, or ensure
your rule's RHS reuses existing nodes.
```

### strider-pattern-author/SKILL.md — Line 27

**Source:** R9-3A Claim 32.

**Current:**
```
Use `CallPat` (`crates/pattern/src/call.rs`) for call sites
```

**Proposed:** Verify and update the path. The actual pattern crate uses `crates/pattern/src/pat/ctor/` for builder ctors. Search for the actual file containing `CallPat` and cite the real path.

---

## target/README.md (already correct)

R9-3A Claim 17 confirms `x86_64_systemv` is correctly listed. No edit needed.

---

## Comment-level fixes inside `.rs` files (R9-3B)

These are not README edits but are listed here for completeness — apply in the next-round implementation phase:

### crates/ir/src/error.rs:10-16 (HIGH)

`UnknownCallOtherError` doc claims the error is raised by a non-existent `FunctionBuilder::build_call_other`. Update to state that the error is constructed in the strider crate.

### crates/ir/src/builder/mod.rs:545-550 (HIGH)

`FunctionBuilder::build` `# Errors` section names `ValidationFailed` variant that doesn't exist. Update to current error type.

### crates/ir/src/validate/mod.rs:5-6 (MED)

Stale "skeleton; concrete checks are added by later tasks" — validator is fully implemented across Layers A/B/C plus opt-in fingerprint check. Delete the sentence.

### crates/cfg/src/cfg/builder/indirect_resolve.rs:40-44 (MED)

Stale "Multiple is reserved for the future" — IR-level resolver constructs `Multiple` today via classify, jump-table, stack-array arms. Rewrite to scope the claim to *this* mini-graph.

### crates/opt/src/lib.rs:13-19 (MED)

Pass table lists 5 of 12+ public passes. Either expand or remove (the real list is exported via `pub use`).

### crates/opt/src/indirect_branch_resolve/stack_array.rs:617 (MED)

Stale "before R2's refactor" — refactor is in (lines 189, 210). Rewrite as regression-pin doc that doesn't reference rounds.

### crates/strider/src/test_utils.rs:5-12 (MED)

Doc claims feature gating; `crates/strider/src/lib.rs:42-50` explicitly explains the gate was removed. Reconcile.

### crates/strider/src/strider/insn/mod.rs:27-44 (LOW)

Two stacked block comments describe the same setup. Merge.

### crates/target/src/call_other_abi.rs:78-84 (LOW)

Two paragraphs say the same thing about ARM SWI ABI. Drop lines 78-80.

### `build_int_const` `# Errors` (MED)

Doc lists only `U256`; code rejects both `U256` and `U512`. Update to "U256 and U512".

### `Strider::build_stable_optimizer_pipeline` doc (MED)

Misses `FlagCmpCanonicalize` + `IfCondInversion` from the listed pipeline composition. Update.

---

## Application order (if user approves)

1. **Doc files (READMEs, SKILL.md)** — apply Edits 1-7 above; commit as "docs: round-9 README + SKILL correctness fixes."
2. **In-`.rs`-file comments** — apply during the next-round implementation phase (Phase B from the summary), each comment fix bundled with its related code change where relevant.

---

## Per-crate README inventory

Each crate has a README; only those needing edits are listed above. Inventory:

| Crate | README status | Edit needed? |
|-------|---------------|--------------|
| cfg | correct | — |
| dot | correct | — |
| entity-utils | correct | — |
| graphwalk | correct | — |
| ir | needs Edit 1 | yes |
| opt | needs Edit 3 | yes |
| pattern | correct | — |
| pcode-lift | needs Edit 2 | yes |
| reader | correct | — |
| strider | correct | — |
| strider-py | correct | — |
| target | correct | — |
| (root) `README.md` | needs Edit 4 | yes |

**Total README edits:** 4 files (ir, opt, pcode-lift, root).
**Total SKILL.md edits:** 3 files (fingerprint-audit, opt-pass-author, pattern-author).
