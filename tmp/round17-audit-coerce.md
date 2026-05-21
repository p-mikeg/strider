# Coercion Unification Audit — Strider IR

## Executive Summary

The codebase has **multiple overlapping coercion sites** with shared algorithmic patterns but no unified extraction. Key opportunities exist at:

1. **Endianness-aware narrowing** (Truncate/ShiftRight synthesis for LE/BE)
2. **Sub-register bit manipulation** (mask + shift + merge)
3. **Type masking and sign-extension** centralization
4. **Narrower-read synthesis from wider stores**

The patterns are load-bearing but **three or more sites reimplement similar logic**.

---

## Finding 1: Endianness-Aware Narrowing Pattern

**Sites:**
- `crates/pcode-lift/src/vn_io.rs::read_reg_vn` (lines 211–275)
  - Sub-register read: shift down by LE offset, **no BE path** (LE only)
  - Truncates to sub-register width
  
- `crates/opt/src/stack_load_forward/mod.rs::realize` (lines 367–407)
  - **Narrow-load-from-wider-store synthesis**
  - LE: `Truncate(data)` only
  - BE: `Truncate(ShiftRight(data, (store_size - load_size) * 8))`
  - Both paths create fresh nodes with `create_node_attributed`

**Pattern:**
```rust
// LE: take low bytes
let result = Truncate(data);

// BE: take high bytes
let shift_bits = (store_size - load_size) * 8;
let shifted = ShiftRight(data, shift_bits);
let result = Truncate(shifted);
```

**Observation:**
- The LE/BE fork is identical in both sites (lines 261–275 vn_io vs. 383–398 stack_load_forward)
- Endianness decision via `self.endianness` (vn_io) vs. parameter `endianness` (stack_load_forward)
- Shift amount calculation is **identical formula** but computed independently
- **Missing**: A `Graph::synthesize_endian_narrow(wider, wider_ty, narrow_ty, endianness) -> NodeOutputId` helper

**Proposed Utility:**
```rust
pub fn synthesize_endian_narrow(
    &mut self,
    wider_out: NodeOutputId,
    wider_ty: NodeOutputType,
    narrow_ty: NodeOutputType,
    endianness: Endianness,
) -> Result<NodeOutputId> {
    // Shift only on BE
    let shifted = match endianness {
        Endianness::Little => wider_out,
        Endianness::Big => {
            let shift_bits = ((wider_ty.byte_size() - narrow_ty.byte_size()) as u64) * 8;
            let shift_const = self.build_int_const(shift_bits, wider_ty)?;
            self.build_int_binary_operation(wider_out, shift_const, IntBinaryOp::ShiftRight, wider_ty)?
        }
    };
    self.truncate_if_needed(shifted, narrow_ty)
}
```

**Delta:** ~15 LOC, eliminates duplication at 2 call sites, one in perf-critical path (stack-load-forward).
**Risk:** Endianness parameter threading; ensure both callers have it available (both already do).

---

## Finding 2: Sub-Register Write Bit-Splice Pattern

**Site:**
- `crates/pcode-lift/src/vn_io.rs::write_reg_vn` (lines 294–389)
  - Splices a narrow value into a wider register container
  - Lines 336–387: "merge" logic

**Explicit Steps (lines 336–387):**
1. Extend narrow value to container width (line 342)
2. Shift into container position (lines 344–354)
3. Mask the positioned value (lines 359–366)
4. Preserve other bits via inverted container mask (lines 371–378)
5. OR them together (lines 381–386)

**Pattern (simplified):**
```rust
// Extend to container width
let val_extended = extend_if_needed(val, container_ty, ZeroExtend)?;

// Shift into position
let shifted_val = ShiftLeft(val_extended, shift_bits);

// Positioned mask for reg's bits
let reg_mask = vn_mask(reg) << shift_bits;
let reg_val = And(reg_mask, shifted_val);

// Preserve bits outside reg
let preserve_mask = vn_mask(container) & !reg_mask;
let preserved = And(preserve_mask, container_val);

// Merge
let result = Or(preserved, reg_val);
```

**Observation:**
- This is the **only site** that performs this operation
- Highly specialized to register aliasing (needs `vn_mask`)
- Not duplicated elsewhere
- Complex but load-bearing — cannot extract without careful API design

**Assessment:** Localized to one site. No immediate unification opportunity, but the pattern is **stable and testable** (has comprehensive positioned_mask_tests at lines 474–548). Candidate for future `Graph::splice_bits(wider, narrower, shift, container_mask)` if needed elsewhere.

---

## Finding 3: Type Masking & Sign-Extension — Centralized But Scattered Access

**Central source:**
- `crates/ir/src/node/output_type.rs` (lines 169–243)
  - `bit_mask_u128()` — all-ones mask for type's width
  - `get_unsigned_int(val) -> Option<u128>` — masks to width
  - `get_signed_int(val) -> Option<i128>` — sign-extends then masks

**Call sites (126 total in codebase):**

1. **Constant-fold evaluation** (constant_fold/eval_int.rs, rules.rs):
   - Lines 25, 61, 76–77, 103–104, 130, 137, 141 (eval_int.rs)
   - Lines 304, 317, 415, 449, 467, 474, 483, 486, 499, 517 (rules.rs)
   - **Purpose:** Mask constants during folding, sign-extend for signed comparisons
   - **Centralized:** Yes; all call `ty.get_unsigned_int()` or `ty.get_signed_int()` directly

2. **Pattern matching** (pattern/pat/ctor/wildcards.rs):
   - Lines 65–66, 118–122 (int_const, int_const_any_of)
   - **Purpose:** Width-aware IntConst matching with masking
   - **Centralized:** Yes; calls `ty.bit_mask_u128()` directly

3. **Coerce builder** (ir/src/builder/coerce.rs):
   - Lines 81, 105 (get_as_unsigned_int, get_as_signed_int)
   - **Purpose:** Extract constant values respecting width
   - **Centralized:** Yes; wraps `get_unsigned_int()` / `get_signed_int()`

**Assessment:** These are **properly centralized**. `NodeOutputType` is the single source of truth. No unification opportunity; the pattern is already unified and well-tested.

---

## Finding 4: Truncate Construction Patterns

**Sites:**

A. `coerce.rs::truncate_if_needed` (lines 152–168)
   - Guard: `if curr_size ≤ target_size → Ok(output_id)`
   - Const-fold: `if const value → build_int_const(masked, target_ty)`
   - Node create: `Truncate(output_id, target_ty)`

B. `stack_load_forward/mod.rs::realize` (lines 399–406)
   - Always creates `Truncate` node
   - Trusts caller to have determined narrowness

C. `vn_io.rs::read_reg_vn` (lines 274–275)
   - Always creates `Truncate` node
   - Trusts caller to have determined sub-register narrowness

**Observation:**
- `truncate_if_needed` has **guards and optimizations** (const-fold, no-op on same width)
- Callers at B and C **assume narrowness** and skip guards
- B and C do not constant-fold; they rely on `ConstantFold` pass later
- Pattern is intentional: `truncate_if_needed` is a **builder helper**; direct `Truncate` creation in passes is a **rewrite operation**

**Assessment:** **No unification needed.** `truncate_if_needed` is the unified builder API; passes (B, C) use lower-level `create_node` when they already know narrowness. This is correct layering.

---

## Finding 5: Extend Construction Patterns

**Sites:**

A. `coerce.rs::extend_if_needed` (lines 177–218)
   - Guards: checks width relationships
   - Const-fold: evaluates `sign_extend(val)` or `zero_extend(val)` inline (lines 189–192)
   - Non-integer input → insert `CastToInt` first (lines 204–205)
   - Node create: `Extend(op, output_id, target_ty)` (line 217)

B. `vn_io.rs::write_reg_vn` (line 342)
   - Calls `extend_if_needed(val, container_ty, ZeroExtend)`
   - Uses builder API directly

C. `function_args/mod.rs::detect_stack_args` (lines 337–349)
   - **Narrower read path**: Inserts `Truncate` when `load_ty != max_type` (line 346)
   - `Truncate` is created with `create_node_attributed` for asm-fingerprint tracking

**Observation:**
- `extend_if_needed` is **the unified entry point** (builder method)
- Non-integer input handling (CastToInt insertion) is **unique to builder** (loads are always integer at IR level)
- `write_reg_vn` reuses it correctly via builder

**Assessment:** **No unification needed.** `extend_if_needed` is already the canonical API. Stack_args coercion (Finding 5C) is **separate concern** (asm-fingerprint attribution) and does not compete with builder coercion.

---

## Finding 6: Narrow-Read Synthesis from Wider Store — Three-Path Pattern

**Primary site:**
`crates/opt/src/stack_load_forward/mod.rs::realize` (lines 358–434)

**Sub-cases:**

1. **Existing** (line 366)
   - Load type matches store type exactly
   - Return store's data slot directly
   - **One call site**

2. **Narrow** (lines 367–407)
   - Load type is narrower than store type
   - Both must be integer
   - LE vs BE endianness (Finding 1 pattern)
   - **Only call site**: `probe()` lines 230–239

3. **Phi** (lines 408–434)
   - MemPhi predecessors may each resolve differently
   - Recursively realize each predecessor
   - Deduplicate if all resolve to same output
   - **Only call site**: `probe()` lines 256–299

**Assessment:**
- **Specialized to stack-load-forward** — unlikely to migrate
- **No external call sites** — internal to one pass
- **No duplication** — only `realize` implements this three-way dispatch

---

## Finding 7: Function-Argument Narrowing Path

**Site:**
`crates/opt/src/function_args/mod.rs::detect_stack_args` (lines 337–349)

**Logic:**
- Collects widest load type per stack offset (line 322)
- Emits one `FunctionArg` with that width (lines 327–334)
- For each narrower load, inserts `Truncate` (lines 337–349)

**Pattern (lines 344–349):**
```rust
if load_ty == max_type {
    // Direct replacement
    ctx.replace_all_uses(old_out, new_out)?;
} else {
    // Narrower read: insert a Truncate with asm-fingerprint
    let truncate = ctx.create_node_attributed(NodeKind::Truncate, ...);
    ctx.replace_all_uses(old_out, truncate_out)?;
}
```

**Observation:**
- **Similar to stack_load_forward's Narrow case** but for `FunctionArg` instead of `StackStore` data
- No shared utility exists; both sites inline their truncate logic
- Both use `create_node_attributed` for fingerprint tracking
- **Duplication:** Minimal (4–5 lines each), but **intent is identical**

**Proposed Utility:**
```rust
pub fn synthesize_truncate_with_fingerprint(
    &mut self,
    src_out: NodeOutputId,
    src_ty: NodeOutputType,
    target_ty: NodeOutputType,
    fingerprint_sources: &[NodeId],
) -> Result<NodeOutputId> {
    if src_ty == target_ty {
        return Ok(src_out);
    }
    if src_ty.byte_size() <= target_ty.byte_size() {
        return Ok(src_out); // no truncate needed
    }
    let truncate = self.create_node_attributed(
        NodeKind::Truncate,
        [src_out],
        [NodeOutputKind::OutputType(target_ty)],
        fingerprint_sources,
    );
    let [out] = self.node_outputs_exact::<1>(truncate)?;
    Ok(out)
}
```

**Delta:** ~15 LOC in passes, eliminates 2 inline patterns, clarifies intent.
**Risk:** Requires RewriteCtx; integration depends on whether both callers are in pass contexts (they are).

---

## Finding 8: Positional Register Mask Computation

**Site:**
`crates/pcode-lift/src/vn_io.rs::write_reg_vn` (lines 359–378)

**Pattern:**
```rust
// vn_mask(reg) is in low-bits domain; shift by container position
let reg_mask = vn_mask(reg)? << shift_bits;
let preserve_mask = vn_mask(container)? & !reg_mask;
```

**Observation:**
- **Only call site** within the codebase
- Highly specialized to register-aliasing invariant
- Thoroughly tested (lines 474–548)
- No other code path manipulates positioned masks

**Assessment:** **No unification needed.** Localized, tested, unlikely to recur elsewhere. The `vn_mask` helper (lines 38–48) is already unified and used in both read and write paths.

---

## Finding 9: Lift-Time Canonicalization Rules

**Stated in CLAUDE.md (lines 111–121):**
- `IntSub(a, b) → Add(a, Neg(b))`
- `IntLessEqual(a, b) → BoolNeg(IntLess(b, a))`
- `IntSlessEqual(a, b) → BoolNeg(IntSless(b, a))`
- `IntNotEqual(a, b) → BoolNeg(IntEqual(a, b))`
- `FloatSub(a, b) → FloatAdd(a, Neg(b))`
- `FloatNotEqual(a, b) → BoolNeg(FloatEqual(a, b))`
- `FloatLessEqual(a, b) → Or(FloatLess(a, b), FloatEqual(a, b))`

**Implementation:**
- Rules are hand-coded in `pcode_lift` lifting routines (not found in this audit)
- Pattern aliases exist in `pattern::sub`, `pattern::int_le`, etc. (lines 121)
- **Documented as single source of truth** in CLAUDE.md

**Assessment:**
- These are **architectural choices**, not duplication opportunities
- Canonicalization happens at **lift time** (pcode → IR), not at coercion
- Out of scope for this audit (focuses on value-type adjustment, not opcode canonicalization)

---

## Finding 10: Signed vs Unsigned Int Extraction — Centralized

**Source:**
`crates/ir/src/node/output_type.rs` (lines 210–243)
- `get_unsigned_int(val) -> Option<u128>` — masks to width (lines 210–215)
- `get_signed_int(val) -> Option<i128>` — sign-extends (lines 222–243)

**Call sites:**
- Constant-fold: dozens (Finding 3)
- Builder coerce: lines 81, 105
- Pattern matching: lines 65–66, 118–122

**Assessment:**
- **Properly centralized**
- No duplication
- Well-tested (lines 292–463)
- No unification opportunity

---

## Finding 11: Call Return-Value Coercion

**Site:**
`crates/ir/src/builder/call.rs::build_call_with_cc` (lines 47–167)

**Relevant section:**
- Lines 159–165: Post-call SP adjustment
  - Reads `pre_call_SP`, computes `pre_call_SP + ret_stack_pop`
  - Builds an `Add` node

**Pattern:**
```rust
let sp_ty: NodeOutputType = sp.size.try_into()?;
let const_id = self.build_int_const(ret_stack_pop as u64, sp_ty)?;
let adjusted = self.build_int_binary_operation(pre, const_id, IntBinaryOp::Add, sp_ty)?;
```

**Observation:**
- This is **not a coercion site** (SP is always integer)
- Post-call register updates (lines 145–147) directly write updated values without coercion
- No type mismatch expected (calling convention enforces register widths)

**Assessment:** **No unification needed.** This is straightforward arithmetic, not coercion.

---

## Finding 12: Cast-Kind Uniformity

**Five distinct cast kinds** (CLAUDE.md lines 142):
- `CastToInt` — any → integer
- `CastToBool` — any → bool
- `CastToFloat` — any → float
- `IntBitsToFloat` — int bits → float bits
- `FloatBitsToInt` — float bits → int bits

**Builders:**
- `crates/ir/src/builder/coerce.rs`:
  - `convert_to_int_if_needed` (lines 226–239) → `CastToInt`
  - `convert_to_bool_if_needed` (lines 49–66) → `CastToBool`
  - `cast_to_float_if_needed` (lines 246–255) → `CastToFloat`
  - No builders for `IntBitsToFloat` / `FloatBitsToInt`

**Assessment:**
- Cast kinds are **semantically distinct** (not interchangeable)
- Builders are **appropriately separated** — each handles its own type conversion
- No unification opportunity — these are fundamentally different operations

---

## Summary Table

| Finding | Pattern | Primary Site | Duplicates | Utility Candidate | Delta | Risk |
|---------|---------|--------------|-----------|-------------------|-------|------|
| 1 | Endian-narrow truncate | stack_load_forward | vn_io | `synthesize_endian_narrow` | ~15 | Threading endianness param |
| 2 | Sub-register bit-splice | vn_io | none | N/A (specialized) | — | — |
| 3 | Type masking & sign-ext | output_type.rs | 126 sites | Already unified ✓ | — | — |
| 4 | Truncate construction | coerce.rs | 2 sites | Already unified (truncate_if_needed) ✓ | — | — |
| 5 | Extend construction | coerce.rs | 1 site | Already unified (extend_if_needed) ✓ | — | — |
| 6 | Narrow-read from wide store | stack_load_forward | none | Single-site specialization | — | — |
| 7 | Function-arg narrowing | function_args | 1 site | `synthesize_truncate_with_fingerprint` | ~15 | RewriteCtx dependency |
| 8 | Positional mask compute | vn_io | none | Single-site specialization | — | — |
| 9 | Lift-time canonicalization | CLAUDE.md spec | hand-coded | Already documented ✓ | — | — |
| 10 | Signed/unsigned extract | output_type.rs | 126 sites | Already unified ✓ | — | — |
| 11 | Call SP adjustment | call.rs | none | N/A (arithmetic, not coercion) | — | — |
| 12 | Five cast kinds | coerce.rs | varies | Semantically distinct; correct | — | — |

---

## Honest Tail: Intrinsic vs Incidental Complexity

**Intrinsic unification opportunities (genuine algorithmic duplication):**
1. **Endianness-aware narrowing** (Finding 1): Identical LE/BE fork at vn_io and stack_load_forward. A small helper (`synthesize_endian_narrow`) saves 2× the BE branch computation and makes the operation explicit.
2. **Function-arg truncation with fingerprint** (Finding 7): Two passes independently insert Truncate nodes when reads are narrower than the canonical width. A `synthesize_truncate_with_fingerprint` utility clarifies the shared intent and reduces duplication by ~4 lines per site.

**Already-unified sites (no opportunity):**
- Type masking (`bit_mask_u128`, `get_unsigned_int`, `get_signed_int`) — centralized in `NodeOutputType`, properly used throughout.
- Builder coercion APIs (`truncate_if_needed`, `extend_if_needed`, `convert_to_int_if_needed`) — are themselves the unified extraction point for complex coercion logic.

**Incidental uniqueness (load-bearing per-site logic):**
- Sub-register write-merge sequence (vn_io) — depends on positioned-mask invariant specific to register aliasing.
- MemPhi / ValuePhi realization (stack_load_forward) — depends on forward-path graph construction in a rewrite pass.
- Narrow-load detection (stack_load_forward probe) — depends on memory-chain decomposition via SpExpr.

**Recommendation:** Extract utilities 1 and 7 if refactoring for clarity; skip the rest. The codebase already follows **sound layering** (builder APIs vs. pass-specific rewrites) and the remaining duplication is **justifiably per-site** due to different ownership contexts (builder vs. passes) and different attribution models (asm-fingerprint vs. no-op).

