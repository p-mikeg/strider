# Round 9 — Ask-8 R1: Typing / Signature / Arity Errors

**Branch:** `feature/ai`. Type-shape correctness audit across all `crates/*/src/`.

## Summary

**No HIGH-severity findings.** Two IMPORTANT doc/API consistency issues at confidence 80-82.

## Important

### I1 — `NodeOutputType::fits_u64` doc lists only two exclusions; impl excludes four

**Confidence:** 82.

**Where:** `crates/ir/src/node/output_type.rs:119-127`.

Doc: "Returns `false` for `U128` and `U256`". Implementation: `self.byte_size() <= 8` — also returns `false` for `U80` (10 bytes) and `U512` (64 bytes).

**Fix:** Update line 122 to: `/// Returns false for U80, U128, U256, U512, and float types F80.`

### I2 — `Bool` inconsistency between `bit_mask_u128` and `get_unsigned_int`

**Confidence:** 80.

**Where:** `crates/ir/src/node/output_type.rs:176-201`.

`bit_mask_u128` returns `1` for Bool (treats as 1-bit unsigned int). `get_unsigned_int` excludes Bool via `!is_integer()`. All 15 current call sites work around with separate `BoolConst` arm before reaching `get_unsigned_int`. New call site authors won't know about the exception.

**Fix A (minimal):** Add cross-reference note to `get_unsigned_int` doc: "Note: `Bool` is excluded even though `bit_mask_u128` returns `1` for it. For BoolConst nodes use the BoolConst match arm directly."

**Fix B (structural):** Return `Some(val & 1)` for Bool in `get_unsigned_int`, aligning with `bit_mask_u128`. Test at line 311 and any callers relying on `None` for Bool would need updating.

## Below threshold (verified safe)

| Location | Pattern | Why safe |
|----------|---------|----------|
| `coerce.rs:190` | `i64 as u128` | Sign-extends correctly per Rust Reference; comment accurate |
| `call.rs:161` | `ret_stack_pop as u64` (i64→u64) | All CC presets define `ret_stack_pop ∈ {0, 4, 8}` |
| `graph/uses.rs:97`, `dead_branch:163,169` | `len() as u32` | EntityList u32-bounded internally |
| `nodes.rs:443` | `bits as u64` (u128→u64) | Gated on `float_type != F80`; F32/F64 fit |
| `stack_array.rs:194,215,407` | `u128 as u64` | Annotated; documented invariant |
| `call_args.rs:178` | `slot as i32` | Bounded by CC's stack_arg_offsets.len() |
| `OptimizerPipeline::run` missing `#[must_use]` | — | `Result<()>` is `#[must_use]`; all callers `?`-propagate |

## Coverage

Crates fully or substantially read: ir/src (output_type, builder/{coerce,call,nodes,mod}, graph/{store,uses,access}, ops/consts, node_signature, function), opt/src (pipeline, constant_fold/{rules,eval_int}, sp_expr, dead_branch, stack_store/call_args, indirect_branch_resolve/stack_array, known_bits), pcode-lift/src/vn_io, target/src/{calling_convention/mod, call_other_abi}, pattern/src/matcher/{bindings, mod}.

Grep sweeps run across all crates: every `as i64`/`as i32`/`as u32`/`as u64`/`as usize`/`as u128`/`as i128` cast; all `get_unsigned_int`/`get_signed_int`/`fits_u64`/`bit_mask_u128` call sites; all `pipeline.run`/`run_on_built` call sites.
