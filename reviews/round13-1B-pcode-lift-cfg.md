# Round 13 — 1B: `pcode-lift` + `cfg` audit

Branch: `review/ai7` · Scope: `crates/pcode-lift/{src,tests}/**`, `crates/cfg/{src,tests}/**`, `target/src/call_other_abi.rs` (focus area 7), `strider/tests/indirect_branch.rs` (focus area 9).

## Verdict

**No HIGH findings.** 1 MED + 2 LOW.

## Findings

### EC-1-FOLLOWUP — `OptionsBuilder::set_function_max_size(0)` is silent in release builds
- **Severity:** MED
- **Where:** `crates/cfg/src/cfg/options.rs:194-201` and `:220-227` (the `set_function_boundary(Bounded { max_size: 0 })` sibling).
- **What's wrong:** Round-12 EC-1 added a guard against `fn_max_size = Some(0)` but uses `debug_assert!(false, …)` only.  In release builds `debug_assert!` is compiled out, so the code falls through to `self.options.fn_max_size = None` and returns the builder in unbounded mode silently.  A caller passing a computed-zero size in production gets no error, no panic, and different semantics than intended.  CLAUDE.md / W1 commit message describe the intent as "reject `Some(0)`"; the code only rejects in debug.
- **Verified against:** `options.rs:193-204`, `:218-237`.
- **Fix:** Either (a) replace `debug_assert!(false, …)` with a hard `panic!` so both debug and release fail loudly, OR (b) convert `OptionsBuilder::build` to `build() -> Result<Options>` and surface the zero-bound as `Err` at build time, OR (c) accept the silent fallback as the intentional release contract and update the doc-comment to say so explicitly.
- **Regression test:** `cargo test --release` should pin the chosen behaviour.

### IRA-2-FOLLOWUP — `indirect_branch_resolved_aarch64be` still `#[ignore]`d
- **Severity:** LOW (documented known gap, not a regression)
- **Where:** `crates/strider/tests/indirect_branch.rs:215`.
- **What's wrong:** The ignore reason precisely describes the gap: "aarch64-be: stack-array dispatch unresolved — lifter emits `Or(SP,K)` instead of `Add(SP,K)` and wraps stored labels in Truncate; resolver matches `Add(SP,K)`+raw-`IntConst` only."  Round 12 IRA-2 said "fixed, just re-enable" — actually the resolver arm in `opt::indirect_branch_resolve::stack_array` matches `Add(SP, K)`, not `Or(SP, K)`.  So the gap is real and the ignore is accurate, not stale.  My round-12 finding was wrong.
- **Fix:** Extend `classify_stack_array`'s SP-offset recognition to accept both `Add(SP, K)` and `Or(SP, K)` (the bitwise-OR is semantically equivalent to addition when `K` is a power-of-two-aligned offset; AArch64-BE Sleigh emits the OR form).  Then remove the `#[ignore]`.

### ARCH-IND-TEST-3 — `arch_independent_call_entries_have_empty_register_channels` test lists `sysret` in the arch-independent group (misleading comment)
- **Severity:** LOW
- **Where:** `crates/target/src/call_other_abi.rs:803`.
- **What's wrong:** The test's `arch_independent_names` list contains `"sysret"` under a `// NoReturn` comment, implying `sysret` is arch-independent.  After round 12 W1 it lives in `classify_arch_specific` for `X86 | X86_64`.  The test passes because it queries with `X86_64` (arch-specific path returns `Some(NoReturn)` first) and `NoReturn` hits an early-continue before the ABI-invariant assertion.  But a future reader will mistakenly believe `sysret` resolves identically on all arches.  The companion test `sysret_and_swapgs_are_x86_only` correctly asserts the arch-specific placement.
- **Fix:** Remove `"sysret"` from the `arch_independent_names` list, or move it to a separate comment block clearly noting it is checked via the arch-specific path.

## Categories verified clean

✓ **`vn_io` register aliasing** — round-12 EC-3 runtime `Err` checks are in place at `vn_io.rs:250-259` (read) and `:326-334` (write).  BE shift formula `8*(container.size - reg.size - (reg.off - container.off))` at `:197-202`.  Positioned-mask correctly shifts `vn_mask(reg)` by `shift_bits`.  Width set 1/2/4/8/10/16/32/64 via `vn_mask`; widths > 16 use `u128::MAX` (degraded) with the wide-container `Err` guard.

✓ **Lift-time canonicalisations (all 8)**:
- `IntSub` → `Add(a, Neg(b))` (`arithmetic.rs:151-180`)
- `IntNotEqual` → `BoolNeg(IntEqual)` (`arithmetic.rs:74-95`)
- `IntLessEqual(a,b)` → `BoolNeg(IntLess(b,a))` (`arithmetic.rs:97-117`)
- `IntSlessEqual(a,b)` → `BoolNeg(IntSless(b,a))` (`arithmetic.rs:119-138`)
- `FloatSub` → `FloatAdd(a, FloatNeg(b))` (`float.rs:92-107`)
- `FloatNotEqual` → `BoolNeg(FloatEqual)` (`float.rs:109-122`)
- `FloatLessEqual` → `Or(FloatLess, FloatEqual)` NaN-aware (`float.rs:124-140`)
- `FloatNan(x)` → `BoolNeg(FloatEqual(x, x))` (`float.rs:78-90`)

✓ **Sub-register partial-write** — `write_reg_vn` reads container, masks old bits out, shifts new in, ORs, writes back.  Preserves all non-target bits including phi-live container values.  End-to-end pin in `vn_io_partial_write.rs`.

✓ **`RegionBuilder::build` bounded-lift OOB terminator** — `region_builder.rs:666-671` checks `is_branch_tail_call_nocheck(cur_addr)` after every advance.  Empty-`insns` guard prevents rejecting zero-pcode-op stretches.

✓ **Single-insn CondBranch one-OOB** — `region_builder.rs:348-382` pops the trailing `CondBranch`, emits `Branch` to in-range successor; `add_region`'s relaxed empty-Branch invariant accepts the resulting empty region.

✓ **`is_addr_tail_call` half-open semantics** — `query.rs:36-47`.  Lower bound strict when bounded or `!allow_code_before_start_addr`; upper bound uses `saturating_add` preventing overflow.

✓ **CallOther classification dispatch** — `monitor`/`mwait`/`monitorx`/`mwaitx` arch-specific X86/X86_64.  `sysret`/`swapgs` arch-specific.  Arch-independent invariant (no register channels) enforced by test.  ARM `swi` ≠ x86 `swi` correctly split.

✓ **Region semantics** — `contains_addr` empty-region `start_addr == addr` (`:268-273`).  `split_region` uses `rposition` fallback for zero-pcode-op holes.

✓ **CONST-space guards** — `ensure_const_space` called from `Subpiece`/`Extract`/`Insert`/`PtrAdd` at `cast.rs:58, 157, 158, 206, 207, 273`.  `PtrSub` correctly has no guard (no literal-width operand).  `Piece` size-sum invariant check in place.

✓ **`vn_mask` widths** — exact for 1/2/4/8/10; degraded `u128::MAX` for 16/32/64; `Err` for unsupported.  Tests pin all widths.

## Coverage table

| File | Status |
|---|---|
| `pcode-lift/src/lib.rs`, `vn_io.rs` | Fully read |
| `pcode-lift/src/value/{mod,arithmetic,float,cast,integer}.rs` | Fully read |
| `pcode-lift/src/value/{boolean,mem_load,misc_value}.rs` | Skipped (not focus area) |
| `pcode-lift/tests/vn_io_partial_write.rs` | Fully read |
| `pcode-lift/tests/value_lifter.rs` | Partial (prologue + helpers) |
| `cfg/src/lib.rs`, `cfg/mod.rs`, `cfg/types.rs`, `cfg/options.rs`, `cfg/query.rs` | Fully read |
| `cfg/src/cfg/builder/{mod,region_builder,split,indirect_resolve}.rs` | Fully read |
| `cfg/src/cfg/decode_cache.rs`, `dot.rs`, `test_api.rs` | Skipped (not focus area) |
| `cfg/tests/indirect_resolve.rs`, `region_builder_tail_call.rs` | Fully read |
| `cfg/tests/*.rs` (remaining 16) | Skipped (behaviour covered by source reading) |
| `target/src/call_other_abi.rs` | Fully read (focus area 7) |
| `strider/tests/indirect_branch.rs` | Partial (focus area 9: `#[ignore]` audit) |
