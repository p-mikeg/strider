# Round 11 — CLAUDE.md diff (R7 consolidation)

Report 3A (`round11-3A-doc-verify.md`) sampled 11 CLAUDE.md claims against the
live source tree. **All 11 were confirmed correct.** No CLAUDE.md edits are
required this round.

## Confirmed CLAUDE.md claims (no edit needed)

| Claim | CLAUDE.md location | Code location | Verdict |
|-------|-------------------|---------------|---------|
| Side-table field names/types (`stack_phi_offsets`, `call_other_names`, `asm_fingerprints`) | line 63 | `crates/ir/src/graph/mod.rs:60,76,101` | CONFIRMED |
| Public fingerprint API (`asm_fingerprint`, `set_asm_fingerprint`, `extend_asm_fingerprint`, `extend_asm_fingerprint_from`) | line 64 | `crates/ir/src/graph/store.rs:132,144,154,184` | CONFIRMED |
| `validate_with_options` opt-in Layer-C check; default `validate` unchanged | lines 70-74 | `crates/ir/src/validate/mod.rs:71-115`, `layer_c.rs:199-222` | CONFIRMED |
| 9-kind asm-fingerprint exempt set | line 64 | `crates/ir/src/validate/layer_c.rs:184-197` | CONFIRMED |
| `NodeOutputType` variant enumeration | line 67 | `crates/ir/src/node/output_type.rs:9-39` | CONFIRMED |
| `IntConst(u128)` rejects U256/U512; wide types via `IntConstWide` | line 67 | `crates/ir/src/builder/nodes.rs:86-99` | CONFIRMED |
| `vn_mask` width enumeration (1/2/4/8/10/16 + degraded >16) | line 147 | `crates/pcode-lift/src/vn_io.rs:38-46` | CONFIRMED |
| Optimizer pipeline composition (`default=6 passes, stable=4, destructive=2`) | line 89 | `crates/opt/src/lib.rs:123-202` | CONFIRMED |
| `Decision { FixedPoint, StableOnly, Rebuild }` enum | line 82 | `crates/strider/src/orchestrator.rs:187-197` | CONFIRMED |
| `IntCmpOp` variant set (no LessEqual/SlessEqual/Borrow) | line 139 | `crates/ir/src/ops/op_kinds.rs` | CONFIRMED |
| `CallOther` classification API (`classify(preset, name) → CallOtherClass`) | line 95 | `crates/target/src/call_other_abi.rs:35-61` | CONFIRMED |

## Source-file doc changes needed (not CLAUDE.md)

These are not CLAUDE.md changes but are documented here for completeness.  See `round11-readme-diffs.md` for the full per-crate README diff and `round11-summary.md` H-18 for the source-comment fix.

1. **`crates/target/README.md:10-12`** — `ArchPreset` variant casing (HIGH; see readme-diffs Change 1).
2. **`crates/pcode-lift/README.md:31-33`** — `require_output_vn` listed as public, actually `pub(crate)` (LOW; see readme-diffs Change 2).
3. **`crates/strider/src/strider/pipeline.rs:182-183`** — `build_optimizer_pipeline` doc undercounts passes (HIGH; H-18 in summary).
