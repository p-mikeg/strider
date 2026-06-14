# Deep code audit — `strider-target`

Date: 2026-06-14
Scope: `crates/strider-target/src` (arch presets, calling conventions, stack-arg
layout math, CallOther ABI classification). READ-ONLY audit.

Files reviewed:
- `src/lib.rs`
- `src/arch.rs` (`Endianness`, `ArchPreset`, `SleighArch` presets)
- `src/calling_convention/mod.rs` (`CallingConvention`, `BuiltCallingConvention`,
  `StackArgs`, `PositionalArgLayout`, `CC_PRESETS`)
- `src/call_other_abi.rs` (`classify`, dispatch tables, `CallOtherAbi`)
- `src/call_descriptor.rs`
- tests: `calling_convention/tests.rs`, `arch.rs#endianness_tests`,
  `tests/{cc_validation,callother_dispatch,linux_cc_presets,arch_smoke}.rs`

Overall the crate is in good shape: the layout math is correct for the strides
and base offsets every supported ABI actually uses, the preset register sets and
link-register/`ret_stack_pop` divergences match their ABIs, `try_new` enforces
the substantive ABI invariants, and the test suite (case-table driven) pins both
the happy path and most boundaries. Findings below are mostly hardening and
minor robustness items; no high-severity correctness defect was found.

---

## TGT-1 — `StackArgs` slot math can integer-overflow on attacker-controlled offsets

- Dimension: SOUNDNESS / hardening (edge case)
- Severity: LOW
- Confidence: HIGH
- Location: `src/calling_convention/mod.rs:327` (`offset_of`),
  `:346` (`index_of`, `rel = offset - self.base_offset` and `offset + size`),
  `:373` (`slot_of`, `offset - self.base_offset`); consumer
  `crates/strider-opt/src/post_opt/function_args/mod.rs:199`
  (`offset + load_size - 1`).

What & why (verified):
`offset` is an `i64` and, per the consumer, comes from
`SpDecomposer::decompose(addr)` on *analyzed binary content* — i.e. it is
derived from values decoded out of the target binary, not a trusted/bounded
input. Every arithmetic op in the slot math (`offset - base_offset`,
`offset + size`, `base_offset + idx*increment`, and the consumer's
`offset + load_size - 1`) is unchecked `i64` arithmetic. A crafted/garbage
SP-relative offset near `i64::MAX` (or `offset + load_size - 1` overflowing)
panics in a debug build and wraps in release. The release wrap is worse: a
wrapped-negative `offset + load_size - 1` makes `slot_of` return `None`, which
trips the `.expect("end byte >= start byte …")` at `function_args/mod.rs:200`,
turning a hardening gap into a crash with a misleading message. `offset_of` is
exercised with a runaway `cursor` in `call_stack_args` (`offset_of(cursor)`),
which has the same wrap exposure if the cursor walk ever runs away.

These offsets are realistically small (stack frames), so this is LOW, but the
crate's own docs frame `slot_of`/`index_of` as the soundness primitive shared
with the indirect-branch stack-array classifier — they should not trust their
argument's magnitude.

Proposed fix: make the math saturating/checked at the `StackArgs` boundary —
e.g. `offset.checked_sub(self.base_offset)` returning `None` on overflow, and
`offset.checked_add(size)` in `index_of`. Have the consumer use
`offset.checked_add(load_size - 1)` and `continue` (not `.expect`) when it
overflows. This keeps the "garbage offset → not a stack arg" semantics without a
panic/wrap.

---

## TGT-2 — `index_of` documents "straddles a slot boundary → None" but returns `Some` for zero-size accesses

- Dimension: SOUNDNESS — code vs doc / edge case
- Severity: LOW
- Confidence: HIGH
- Location: `src/calling_convention/mod.rs:332-350` (doc + `then_some`).

What & why (verified):
The check is `(offset + size <= slot_start + self.increment).then_some(idx)`.
For `size == 0`, `offset + 0 <= slot_start + increment` holds for any in-slot
offset, so `index_of(off, 0)` is `Some` for every `off >= base_offset`. The test
`stack_args_index_and_slot_boundaries_per_increment` explicitly *pins* this
("zero-size at base" → `Some(0)`), so it is intended — but the method doc only
describes the non-zero cases ("fully contains a `size`-byte access" / "straddles
a slot boundary"), never the zero-size degenerate. A future caller passing a
`size` derived from a possibly-zero IR width would get a slot attribution for a
read that touches no bytes. Not a defect today (no consumer passes `size == 0`),
but the contract is under-specified vs. the pinned behaviour.

Proposed fix: add one sentence to the `index_of` doc stating that a zero-size
access trivially fits and yields `Some(slot-of-start)`, OR decide zero-size is
nonsensical and early-return `None`. Pick one and make doc + code agree.

---

## TGT-3 — `try_new` skips the `ret_val_regs ∩ ret_val_regs_float` / float-list-vs-arg disjointness it could cheaply assert

- Dimension: SOUNDNESS — invariant completeness
- Severity: LOW
- Confidence: MED
- Location: `src/calling_convention/mod.rs:221-268`.

What & why (verified):
`try_new` checks: arg ∩ callee-saved, (ret ∪ ret_float) ∩ callee-saved, SP not
in any of the four lists, and within-list uniqueness. It does NOT check
`ret_val_regs ∩ ret_val_regs_float` or `arg_passing_regs ∩ ret_val_regs_float`.
The integer-vs-float-return overlap in particular is almost certainly a CC-author
bug if it ever occurred (an integer return reg should not also be the float
return reg — they are physically different register files on every supported
arch). The omission is benign today because no preset violates it and the
distinctness *within* `ret_val_regs_float` is already covered by the per-list
loop. This is a "tighten the validating constructor" item, not a live bug.

Note (verified non-finding): NOT checking `arg ∩ ret_val_regs` is correct — on
x86_64 SysV `RDX` is legitimately both the 3rd arg and the 2nd return register,
so an arg∩ret check would wrongly reject the real ABI.

Proposed fix: add a `ret_val_regs ∩ ret_val_regs_float` disjointness check (one
more loop in the existing style). Leave arg∩ret unchecked (intentionally legal).

---

## TGT-4 — `Endianness::read_uint` lives in `arch.rs` and panics on `len > 16`; it is decode logic, not an arch description

- Dimension: GENERALIZE / soundness-of-placement
- Severity: LOW
- Confidence: MED
- Location: `src/arch.rs:25-42`.

What & why (verified):
`read_uint` is a fully self-contained byte-decode helper (the optimizer's
ROM-decode path) that happens to hang off `Endianness`. It has an `assert!(n <=
16)` documented panic. Two observations:
(1) It is the only panicking production path in the crate and the only piece of
*behaviour* (vs. pure data) in `arch.rs`, which the crate-level docs describe as
"pure target descriptions." Its placement blurs the crate's stated "pure data"
contract.
(2) The `len > 16` panic is a hard crash on a caller that asks for a >16-byte
decode (e.g. a future I256/I512 ROM load). The IR already models I256/I512, so a
`LoadReadOnly` over a wide constant region is plausibly reachable; today it would
panic rather than degrade.

Proposed fix: either (a) document this as a genuine "callers must pre-clamp"
invariant and leave it (acceptable per the project's panic-on-invariant policy
if a validator guarantees `len <= 16` upstream — confirm that guarantee exists),
or (b) return `Option<u128>` for the over-wide case so a wide ROM load degrades
to "unfoldable" instead of crashing. No need to move the function unless the
"pure data" framing is load-bearing.

---

## TGT-5 — Missing edge-case tests (names + scenarios; not written here)

- Dimension: EDGE CASES (test coverage)
- Severity: LOW
- Confidence: HIGH
- Location: `src/calling_convention/tests.rs`, `tests/cc_validation.rs`.

The existing suite is strong but leaves these gaps uncovered:

1. `stack_args_index_of_overflow_returns_none_not_panic` — call
   `index_of(i64::MAX, 8)` and `slot_of(i64::MAX)` on an 8/8 layout; pins that
   adversarial offsets degrade rather than panic/wrap (see TGT-1). Currently no
   test exercises near-`i64::MAX` offsets on `index_of`/`slot_of` (only
   `offset_of(1<<40)` is pinned, and only for `offset_of`).

2. `index_of_below_base_with_negative_offset` — `index_of(-8, 8)` /
   `slot_of(-8)` on a `base_offset: 0` layout (AArch64/ARM/MIPS-n64). Pins the
   `offset < base_offset → None` guard for negative offsets specifically (a
   below-base byte from a decoded negative SP delta). Existing `base - 1` probes
   only cover positive-base presets.

3. `try_new_rejects_ret_int_overlapping_ret_float` — construct a CC with the
   same Vn in `ret_val_regs` and `ret_val_regs_float`; assert `try_new` errors
   (drives TGT-3). No current test covers float-return-list overlaps at all.

4. `read_uint_rejects_or_degrades_over_16_bytes` — assert the documented
   behaviour for `len == 17` (panic today; would be `None` after TGT-4 (b)).
   The decode tests stop at exactly 16 bytes.

5. `mips_o32_first_stack_arg_at_sp_plus_16` (positional layout) — assert
   `positional_arg_layout().stack_offset_of(4) == Some(16)` for `mips_o32`,
   pinning that the O32 shadow-space `base_offset: 16` flows through the
   register-then-stack indexing (4 reg args → first stack slot at +16). The
   `positional_arg_layout_*` tests only cover x86_64 (+8) and x86 cdecl (+4),
   not the shadow-space arch.

---

## Verified non-findings (do not re-flag)

- `CC_PRESETS` linear lookup (`lookup_preset`) and `try_new`'s O(n²) within-list
  dup check are both fine: ~22 rows / ≤32 regs, both run once at build time.
- `index_of`/`slot_of` `debug_assert!(increment > 0)` plus the `try_new`
  `increment > 0` enforcement together prevent divide-by-zero in practice.
- The x86_64 SysV `RDX` arg∩ret overlap is correct ABI (see TGT-3 note).
- `link_register_vn` ⊆ `callee_saved_regs` is enforced in `try_new` and pinned
  by `link_register_vn_resolves_to_callee_saved_lr` across every LR preset.
- MIPS O32 `base_offset: 16` (shadow space with register args) and the PPC
  linkage-area offsets (48 ELFv1 / 32 ELFv2 / 8 SysV32) match their ABIs.
- `CallOtherClass` is intentionally unexported from `lib.rs` (consumers reach it
  via `call_other_abi::CallOtherClass`); the CallOther tables' arch-specific vs
  arch-independent split and the "arch-independent Call entries have empty
  register channels" invariant are correct and test-enforced. Missing CallOther
  entries are intentional per project policy.
