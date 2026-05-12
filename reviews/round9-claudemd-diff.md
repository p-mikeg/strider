# Round 9 — CLAUDE.md correctness diff

Concrete edit proposals derived from R9-3A (doc-verify) and R9-3B (stale-comment sweep). Each entry: location, current text, proposed text, source.

These are **proposed** edits — apply only after user approval.

---

## High-confidence corrections (conf ≥ 90)

### Edit 1 — Line 77: deprecated CC alias listed as canonical

**Source:** R9-3A Issue B (conf 100), R9-3B #3.

**Current:**
```
CC presets — userland: `x86_cdecl`, `x86_64_systemv_abi`, `x86_64_all_preserving` ...
```

**Proposed:**
```
CC presets — userland: `x86_cdecl`, `x86_64_systemv`, `x86_64_all_preserving` ...
```

**Rationale:** `x86_64_systemv_abi` is `#[deprecated(since = "0.1.0", note = "renamed to x86_64_systemv")]` in `crates/target/src/calling_convention/mod.rs:304`. The canonical name is `x86_64_systemv`.

---

### Edit 2 — Line 92: `IfCondInversion` implements `OptimizerOnBuilt`, not `Optimizer`

**Source:** R9-3A Claim 35 (partial).

**Current:**
The opt section opens with "Passes added via `OptimizerPipeline::add` run in a shared fixed-point loop" implying all passes implement `Optimizer`.

**Proposed clarification (insert after the existing pass list around line 91-92):**
```
Most passes implement `Optimizer`; `IfCondInversion` implements `OptimizerOnBuilt`
because it needs `BuiltFunctionGraph`-level access for control-flow surgery.
Both trait kinds participate in the same fixed-point loop.
```

---

## Verified-correct claims (no edit needed)

The 37 sampled claims listed below in R9-3A all match current code:

- `NodeOutputType` variant list with U80/U512/F80 (line 67) ✓
- `vn_mask` width list 1/2/4/8/10/16/32/64 (line 147) ✓
- `target::call_other_abi::classify(preset, name) -> Option<CallOtherClass>` (line 95) ✓
- `IndirectBranch` and `IntConstWide` in IR Node Model (lines 136, 139) ✓
- Dependency graph `cfg → opt` and `strider → pattern` edges (lines 36-44) ✓
- LR-in-CC deliberate tradeoff note (line 79) ✓
- `mfence`/`sfence`/`lfence` as `PURE_WITH_MEM_EDGE` arch-independent (line 95) ✓
- `validate_with_options(graph, entry, ValidateOptions { check_asm_fingerprints: true })` (line 70) ✓
- Example binary path `fixtures/out/x86/arithmetic.elf` (lines 10-14) ✓
- `Strider::new(arch, sleigh_regs, cc)` signature (line 81) ✓
- `Strider::build_stable_optimizer_pipeline()` includes `ConstantFold + KnownBits + FlagCmpCanonicalize + IfCondInversion + StackStoreDetect + StackLoadForward + FunctionArgDetect post-pass` (line 85) ✓
- `LoadReadOnly` not in `default_pipeline()` rationale (line 96) ✓
- All 6 lift-time canonicalisation aliases (line 113) match what lifter emits ✓
- `StackStorePhi` fixed arity 3, `VarPhi`/`MemPhi` per-predecessor arity (line 73) ✓
- Layer A reachability scoping, Layer B explicit, Layer C mixed (line 73) ✓

---

## Application instructions

If user approves these edits:

1. Apply Edit 1 with `Edit` tool — replace single occurrence on line 77.
2. Apply Edit 2 by inserting clarification after the existing opt-pass list.
3. Verify no other occurrences of `x86_64_systemv_abi` exist in CLAUDE.md (`grep -n x86_64_systemv_abi CLAUDE.md`).
4. Commit with message: "docs: round-9 CLAUDE.md correctness fixes — rename deprecated alias + clarify trait split."

If user wants the broader doc updates (per-crate READMEs), see `round9-readme-diffs.md`.

---

## Out of scope for CLAUDE.md (handled elsewhere)

- per-crate README corrections (R9-3B): see `round9-readme-diffs.md`
- SKILL.md stale line numbers (R9-3A Issues D, E): apply to 2 SKILL.md files directly
- Comment fixes inside `.rs` files (R9-3B): part of next-round implementation phase, not doc edits
