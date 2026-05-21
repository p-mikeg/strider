# Round 12 — Ask 8 pass 4: boundary / edge-case correctness audit

Branch: `review/ai6` · Trust model: strict (no prior-round reviews; no other round-12 reports read).

## Verdict

**No HIGH findings.** 1 MED + 2 LOW findings. Most boundary classes defended; remaining ones documented below.

## Findings

### EC-1 — `fn_max_size = Some(0)` decodes past the zero-byte function bound
- **Severity:** MED (would be HIGH if a real caller could trigger it; current callers all set non-zero bounds, so latent)
- **Where:** `crates/cfg/src/cfg/query.rs:41-46`, `crates/cfg/src/cfg/builder/region_builder.rs:625-672`
- **Edge case:** `OptionsBuilder::set_function_max_size(0)`.
- **What's wrong:** `is_addr_tail_call(target, start, Some(0), _)` computes `upper = start.saturating_add(0) = start`, so `target >= upper` is true for every `target >= start`. The fallthrough bound check at line 667 fires after the first machine instruction with pcode ops and emits a `TailCall` terminator correctly. But when the first (and every subsequent) machine instruction produces zero pcode ops (AArch64 `nop`, `paciasp`, etc.), `self.insns.is_empty()` is true and the guard at line 667 is skipped — comment at line 660: "the next iteration will keep lifting." With `fn_max_size=0`, lifting continues into memory following the supposed zero-byte function until Sleigh hits unmapped memory or a pcode-terminating insn. The CFG covers arbitrary code attributed to the wrong address range with no diagnostic.
- **Fix:** Validate in `OptionsBuilder::build()` that `max_size > 0` when provided, returning a caller error for `Some(0)`. Alternatively early-exit in `Builder::build` with a single-region `RegionTerminator::Return`.
- **Regression test:** `OptionsBuilder::new().set_function_max_size(0).build()` should reject 0, or the resulting CFG must have exactly 1 region and must not lift into memory beyond `start_addr`.

### EC-2 — `start_addr = u64::MAX` with any nonzero `fn_max_size` saturates upper bound
- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/query.rs:42`
- **Edge case:** `start_addr = u64::MAX` + `fn_max_size = Some(n>=1)`.
- **What's wrong:** `start_addr.saturating_add(n)` clips to `u64::MAX = start_addr`, so `is_addr_tail_call(u64::MAX, u64::MAX, Some(n), _)` returns `true` — the entry address is classified as a tail call. The first instruction's fallthrough to `start_addr + insn_len` (overflowing) would also be misrouted.
- **Fix:** Replace with `let upper = start_addr.checked_add(sz).unwrap_or(...);` — when overflow occurs, treat the entire `[start, u64::MAX]` range as in-range. Alternatively `OptionsBuilder` validates `start_addr + fn_max_size <= u64::MAX`.
- **Regression test:** Unit test asserts entry address at `u64::MAX` is NOT classified as a tail call.

### EC-3 — `write_reg_vn` shift-overflow guard is `debug_assert!`-only
- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/vn_io.rs:321-329` (guard), `:354` (shift); parallel guard in `read_reg_vn` at `:247-256`.
- **Edge case:** A Sleigh varnode whose `addr_off` places it outside its container (malformed SLA spec, future >128-bit containers, fuzzer input).
- **What's wrong:** The invariant `shift_bits < container_bit_width` is enforced only via `debug_assert!`. In release builds, `vn_mask(reg)? << shift_bits` (a `u128 << u64`) on x86 hardware silently applies `shift % 128`, producing the wrong `reg_mask` and silently corrupting the container value. Diagnostic-free corruption.
- **Fix:** Replace both `debug_assert!` blocks with a real check that returns `Err(anyhow!(...))` so the invariant holds in all build modes. Consistent with the existing wide-container guard at lines 223-231 / 308-315 which already uses `return Err(...)`.
- **Regression test:** Synthetic Vn structurally outside its container; `write_reg_vn` returns `Err` in release.

## Categories defended

✓ **Empty pattern sets** (`int_const_any_of([])`, `at_any([])`, `offset_any([])`): all vacuously fail. Documented behaviour pinned by implementation.

✓ **Float boundaries (NaN / ±0 / ±inf in constant folding)** — `crates/opt/src/constant_fold/eval_float.rs`: Rust's native `f32`/`f64` IEEE 754 arithmetic handles all special values correctly. NaN comparisons return false. F80 short-circuits to `None` preventing precision loss.

✓ **INT_MIN / -1 in `Sdiv` and `Srem`** — `crates/opt/src/constant_fold/eval_int.rs:86-93, 111-119`: both arms detect and return `None`. `int_min` formula computes `-(1i128 << (bits-1))` without overflow for `bits <= 128`.

✓ **INT_MIN sign-extension via `Extend`** — `crates/ir/src/node/output_type.rs:222-243`: `get_signed_int` sign-extends correctly. Tests at `:328-341` cover U32/U128.

✓ **`addr = u64::MAX` arithmetic in `next_pcode_addr`** — `crates/cfg/src/cfg/builder/region_builder.rs:35-43`: `checked_add` returns `anyhow::Error` on overflow.

✓ **`MemRegion::new` with overflow** — `crates/reader/src/lib.rs:127-133`: `start_addr.checked_add(len)` rejects overflow.

✓ **Wide-constant boundaries** — `crates/ir/src/graph/compact.rs:248-297`: `gc_wide_consts` correctly GCs at compaction. Layer-C checks `DanglingWideConstId` and `WideConstWidthMismatch`. `IntConst(u128)` rejects > u128 at build time.

✓ **`StackStorePhi` with equal-offset predecessors** — `crates/opt/src/stack_store/tests.rs:204-258` pins that equal-offset phis collapse to plain `StackStore`.

✓ **Sub-byte (1-bit) varnodes** — Carry-flag varnodes use `size=1`. `vn_mask(size=0)` errors; `vn_mask(size=1)` returns u8::MAX (8-bit mask). No supported ISA emits `size=0`.

✓ **`addr == start_addr` boundary** — strict-less comparison; entry is in-range for `n > 0`.

✓ **`locate_and_write` with `site_addr = u64::MAX`** — `crates/reader/src/elf.rs:1042`: `checked_add` + `is_some_and`, increments `skipped_no_region` safely.

✓ **Single-predecessor ControlState (RedundantPhis target)** — pass detects and eliminates; Layer-C catches mismatches.

✓ **Empty bytes to `ElfFileMemReader`** — `object::File::parse(&[])` fails before regions load; empty sections are skipped.

## Files reviewed

- `crates/cfg/src/cfg/{query.rs,builder/{region_builder,mod}.rs,options.rs}`
- `crates/pcode-lift/src/{vn_io.rs,value/*.rs}`
- `crates/opt/src/constant_fold/{eval_int.rs,eval_float.rs}`
- `crates/opt/src/{stack_store/{detect,tests}.rs,redundant_phis/mod.rs}`
- `crates/ir/src/{node/output_type.rs,graph/compact.rs,validate/layer_c.rs}`
- `crates/reader/src/{lib.rs,elf.rs}`
- `crates/pattern/src/{pat/ctor/wildcards.rs,pat/builders/{call,memory}.rs}`
