# Round 9 / 3A — Trust-only-the-code doc verification

**Branch:** `review/ai3`. Independent audit; sampled ≥30 doc claims, verified each against current code.

## Coverage

Doc files: CLAUDE.md, all 10 crate READMEs, 9 SKILL.md files. Code files cross-referenced: ir/{node/output_type,node/kind,graph/store,validate/{mod,layer_c}}.rs, pcode-lift/src/vn_io.rs, target/src/{call_other_abi,calling_convention/mod}.rs, opt/src/{lib,pipeline,if_cond_inversion/mod}.rs, strider/src/strider/pipeline.rs, strider/examples/strider.rs.

## Critical issues (confidence ≥ 80)

### Issue A — ir/README.md: `NodeOutputType` description omits U80, U512, F80

**Confidence:** 100.

**Where:** `crates/ir/README.md:50`.

Claim: "`NodeOutputType` (`Bool` / `U8`–`U256` / `F32` / `F64`)". Actual enum has 12 variants: Bool, U8, U16, U32, U64, **U80**, U128, U256, **U512**, F32, F64, **F80**.

**Fix:** "`NodeOutputType` (`Bool` / `U8` / `U16` / `U32` / `U64` / `U80` (x87 80-bit) / `U128` / `U256` / `U512` (AVX-512 zmm) / `F32` / `F64` / `F80` (x87 extended-precision))."

### Issue B — CLAUDE.md: `x86_64_systemv_abi` listed as canonical CC name

**Confidence:** 100.

**Where:** `CLAUDE.md:77`.

`x86_64_systemv_abi` is `#[deprecated(since = "0.1.0", note = "renamed to x86_64_systemv")]` in calling_convention/mod.rs:304. Canonical name is `x86_64_systemv`.

**Fix:** Change `x86_64_systemv_abi` → `x86_64_systemv` on CLAUDE.md:77.

### Issue C — pcode-lift/README.md: `vn_mask` width list omits 32 and 64

**Confidence:** 100.

**Where:** `crates/pcode-lift/README.md:49`.

Claim: "Width support: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register) bytes." Actual `vn_io.rs:45`: `16 | 32 | 64 => Ok(u128::MAX)`.

**Fix:** "Width support: 1, 2, 4, 8, 10 (x87 80-bit extended), 16 (XMM/q-register), 32 (YMM), 64 (ZMM) bytes. Widths 32 and 64 use a degraded `u128::MAX` mask."

## Important issues

### Issue D — strider-fingerprint-audit SKILL.md: stale line numbers

**Confidence:** 90.

**Where:** SKILL.md:24.

Claim: "`crates/ir/src/graph/store.rs:108-160`". Actual: `asm_fingerprint` at line 132, `extend_asm_fingerprint_from` at line 184. Range covers wrong functions.

**Fix:** Update to `:127-200` or remove specific line numbers.

### Issue E — strider-opt-pass-author SKILL.md: stale line number

**Confidence:** 90.

**Where:** SKILL.md:30.

Claim: "`extend_asm_fingerprint_from(...)` (`crates/ir/src/graph/store.rs:160`)". Actual: line 184.

**Fix:** Change `:160` → `:184`.

## Verified correct (37 sample claims)

CLAUDE.md `NodeOutputType` variant list with U80/U512/F80 ✓; `vn_mask` width list 1/2/4/8/10/16/32/64 ✓; `classify(preset, name)` two-arg signature ✓; `IndirectBranch` and `IntConstWide` in IR Node Model ✓; dependency graph `cfg → opt` and `strider → pattern` edges ✓; LR-in-CC deliberate tradeoff note ✓; mfence/sfence/lfence as `PURE_WITH_MEM_EDGE` arch-independent ✓; `validate_with_options`/`ValidateOptions{check_asm_fingerprints: true}` ✓; example binary path `fixtures/out/x86/arithmetic.elf` exists ✓; target/README.md `classify(preset, name)` signature ✓; target/README.md `x86_64_systemv` (new name) ✓; strider-fingerprint-audit exempt kind list ✓ (against layer_c.rs); strider-cc-preset-extend rename note ✓; strider-orchestrator-extend `Builder::for_arch` guidance ✓; opt/README.md `stable_default_pipeline` pass composition ✓; opt/README.md `IfCondInversion` semantics ✓; CLAUDE.md `Strider::new(arch, sleigh_regs, cc)` signature ✓; CLAUDE.md `LoadReadOnly` not in default_pipeline rationale ✓; canonicalisation aliases (sub, int_le, int_sle, float_*) all match lifter shapes ✓.

## Notes

- opt/README.md line 9 says "every pass implements [Optimizer] trait" but `IfCondInversion` implements `OptimizerOnBuilt`. Minor inconsistency — README does mention `OptimizerOnBuilt` separately on line 11. Update opening sentence.
- strider-pattern-author SKILL.md cites `crates/pattern/src/call.rs` but actual path may be `src/pat/call.rs` or similar. Worth confirming.
