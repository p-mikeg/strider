---
name: strider-pcode-lift-vn-aliasing-extend
description: Extend pcode-lift's vn_mask / register-aliasing layer to support a new container width (e.g. RISC-V vector regs, future SVE register slots) without breaking existing arches.
---

# strider-pcode-lift-vn-aliasing-extend

## When to use

User wants to add register-aliasing support for a new width that
`vn_mask` does not currently accept. Triggers include:

- "Add support for register width N bytes."
- "`vn_mask` doesn't include W byte width."
- "RISC-V V-extension regs are 16/32/64/128/… bytes — extend `vn_mask`."
- "x86 AVX-512 ZMM is in but I need a 96-byte slot for an extension."
- A lift fails with `"vn_mask: unsupported width <N>"`.

## When NOT to use

- The width is already in the table (1, 2, 4, 8, 10, 16, 32, 64) — the
  failure is elsewhere (sub-register aliasing, sleigh register table).
- You want to add aliasing logic *between* arbitrary widths beyond the
  largest-containing-register rule. That's a redesign of the aliasing
  layer; route through `feature-dev` first.
- The width belongs to a register Sleigh does not actually emit. Confirm
  with a real lift before extending.

## Inputs the skill expects

- The new width in bytes.
- The arch / register family motivating the addition (so the test names a
  concrete register).
- Whether the new width should use a precise mask (widths ≤ 16 bytes use
  exact masks; > 16-byte containers use a degraded `u128::MAX` and
  reject sub-register aliasing).

## Procedure

1. Locate `vn_mask` at `crates/pcode-lift/src/vn_io.rs:38`. The function
   matches on `reg.size` and returns the bit-mask of valid value bits.
   Add the new width arm. For widths ≤ 16 bytes use `(1u128 << bits) - 1`;
   for widths > 16 bytes use `u128::MAX` and document the degraded
   semantics.
2. Cross-check `find_largest_fitting_register` at line 141 of the same
   file. The largest-container search walks `vn_overlapping_registers`
   and asks `vn_mask` for each candidate; new widths should "just work"
   if `vn_mask` accepts them, but verify by tracing one read of a
   register at the new width.
3. Confirm `read_vn` (line 68) and `write_vn` (line 105) handle the new
   width. The shift / extract / insert formulas are size-parametric;
   widths > 16 bytes guard against sub-register aliasing with a clear
   error so the existing reject path stays intact.
4. Add a unit test in `crates/pcode-lift/tests/value_lifter.rs` that
   constructs an `rsleigh::Insn` reading or writing a register of the
   new width and asserts the lifted IR has the expected `Truncate` /
   `Extend` / direct-read shape. Mirror the style of
   `vn_mask_accepts_32_bytes_for_avx2_ymm` /
   `vn_mask_accepts_64_bytes_for_avx512_zmm`.
5. Update the width list in `crates/pcode-lift/README.md` (the bullet
   under "Public surface" naming `vn_io`) and the aliasing summary in
   `CLAUDE.md` so the supported-widths invariant stays documented.

## Verification

- `cargo test --package pcode-lift` — covers the existing aliasing tests
  plus the new width.
- `cargo test --workspace` — confirms no downstream lift surprised by
  the wider mask.
- `cargo clippy --workspace --all-targets -- -D warnings`.

## Exit criteria

- `vn_mask(reg)` returns `Ok(_)` for the new width.
- A regression test in `crates/pcode-lift/tests/value_lifter.rs`
  exercises a register of the new width.
- README + CLAUDE.md width list mention the new size.
- Existing reject path still fires for widths the table does not list
  (`vn_mask_still_rejects_unsupported_widths` test stays green).

## Pitfalls

- **Reaching for `u128::MAX` on a width that fits** — widths ≤ 16 bytes
  must use the exact `(1u128 << bits) - 1` mask. Degrading silently to
  `u128::MAX` lets stale high bits survive a partial write.
- **Forgetting the wide-container guard.** Any width > 16 bytes must
  reject sub-register aliasing in `find_largest_fitting_register`; the
  existing arms set this up automatically, but a hand-rolled new arm can
  miss it.
- **Adding a width that Sleigh doesn't actually emit.** Confirm by lift,
  not by spec — sleigh's register file may collapse multiple ISA-level
  views into a smaller container.
- **Skipping the README / CLAUDE.md update.** The width list is the
  documented invariant; readers grep it before extending the table.

## Related skills

- `strider-target-arch` — when the new width is part of a brand-new arch
  preset.
- `strider-callother-abi` — when the new width's registers appear as
  implicit reads / writes on a CallOther entry.
- `strider-doc-line-number-refresh` — to refresh README / CLAUDE.md
  after the table edit.
