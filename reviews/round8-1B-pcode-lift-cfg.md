# Round 8 / 1B — `pcode-lift` + `cfg` audit

**Branch:** `review/ai2`.  Independent audit; round-7 reviews not consulted.

## Coverage

| File | Status |
|------|--------|
| `crates/pcode-lift/src/lib.rs` | full |
| `crates/pcode-lift/src/vn_io.rs` (634 lines) | full |
| `crates/pcode-lift/src/value/{mod,arithmetic,boolean,cast,float,integer,mem_load,misc_value}.rs` | full |
| `crates/pcode-lift/tests/{value_lifter,vn_io_partial_write}.rs` | full |
| `crates/pcode-lift/Cargo.toml` | full |
| `crates/cfg/src/lib.rs` | full |
| `crates/cfg/src/test_api.rs` | full |
| `crates/cfg/src/cfg/{mod,types,options,query,decode_cache}.rs` | full |
| `crates/cfg/src/cfg/builder/{mod,region_builder,split,indirect_resolve}.rs` (region_builder = 797 lines) | full |
| `crates/cfg/Cargo.toml` | full |
| All 25 `crates/cfg/tests/*.rs` files | full |
| `../rsleigh/src/{ffi,core_types}.rs` | consulted |

## Findings

### IMPORTANT: `handle_insert` u64 mask truncation corrupts upper bits of ≥128-bit destination types

- **Severity:** MED (architecturally wrong; only triggered if pcode `Insert` is emitted on U128).
- **Where:** `crates/pcode-lift/src/value/cast.rs:168-176`
- **What's wrong:** `mask_raw`, `mask_shifted`, `not_mask_shifted` all computed as `u64`.  When `out_ty` is `U128` (16-byte SIMD register), `not_mask_shifted = !mask_shifted` is a `u64` complement.  Passed to `build_int_const(not_mask_shifted, out_ty)` which zero-extends u64 → u128: bits 64–127 become **zero**, not one, so the AND mask zeros the upper 64 bits of the destination unconditionally.

  Concrete failure: `Insert(q0, d0, 64, 64)` (insert 64-bit value into bits 64–127 of q0) — `mask_raw = u64::MAX` (len ≥ 64), `mask_shifted = u64::MAX.wrapping_shl(64) = 0`, `not_mask_shifted = u64::MAX`, zero-extended `0x0000_0000_0000_0000_FFFF_FFFF_FFFF_FFFF`.  AND'ing the destination with this zeros bits 64–127, corrupting them rather than preserving them.  The inserted value is also at the wrong position (shift-left-64 in u64 space is 0).
- **Verified against:** `build_int_const` in `crates/ir/src/builder/nodes.rs:86` accepts `impl Into<u128>` and `u64: Into<u128>` zero-extends.
- **Fix:** Compute masks in u128:
  ```rust
  let mask_raw_u128: u128 = if len >= 128 { u128::MAX } else { (1u128 << len) - 1 };
  let mask_shifted = mask_raw_u128.wrapping_shl(lsb as u32);
  let not_mask_shifted = !mask_shifted;
  ```
- **Test gap:** No tests exercise `handle_insert` at all.

### IMPORTANT: `handle_extract` u64 mask truncates for >64-bit narrow output

- **Severity:** MED (only triggered if narrow_ty is U128 with 64 ≤ len < 128).
- **Where:** `crates/pcode-lift/src/value/cast.rs:131-146`
- **What's wrong:** `mask_val` computed as u64; for `narrow_ty = U128` with `64 ≤ len < 128`, `u64::MAX` is too narrow.  Same pattern as Insert.
- **Fix:** Same u128 arithmetic as Insert.

## Areas verified correct (not findings — recorded so future rounds know)

- **`vn_mask` for 32/64-byte regs.**  Returns `u128::MAX` (degraded mask).  Sound because `read_reg_vn` early-exits at line 214 (`container_reg == *reg`) and `write_reg_vn` early-exits at lines 277–289.  Wide-container guard (lines 223-231 / 292-300) rejects sub-register aliasing within >16-byte containers with a clear error.
- **`region_id_at_start` returns `range.next()`.**  `BTreeMap::range((Included((addr,0)), Included((addr,u64::MAX))))` ordered by `(machine_addr, insn_index)`.  First entry has `insn_index = 0` which is what `add_region` always inserts as the canonical region.  Correct per the docstring.
- **`Indirect` opcode (61) and `MultiEqual` (60).**  Both are GHIDRA decompiler-internal; never emitted by `rsleigh::Sleigh::lift_one`.  The `_ => Ok(false)` wildcard is correct.
- **`is_addr_tail_call` overflow.**  Uses `saturating_add` — overflow doesn't trigger off-by-one bug.
- **All 8 lift-time canonicalisations** (`IntSub`, `IntLessEqual`, `IntSlessEqual`, `IntNotEqual`, `FloatSub`, `FloatNotEqual`, `FloatLessEqual`, `FLOAT_NAN`).  Code matches CLAUDE.md spec.

## Coverage summary

All `pcode-lift` and `cfg` source + test files inspected fully.  rsleigh `Opcode` enum + `core_types.rs` consulted.

**Real bugs found: 2** (both in `handle_insert` / `handle_extract` for ≥128-bit output).
**False positives avoided: 5** (Indirect/MultiEqual fallthrough, vn_mask wide degraded mask, region_id_at_start ordering, is_addr_tail_call overflow, decode_branch_target CONST sign-extend bail).
