# Round 13 — Ask-8 pass 4: boundary / edge-case correctness audit

Branch: `review/ai7`.

## Verdict

**1 LOW finding (test-coverage gap on R12 EC-1 sibling), all other edge cases defended.**

## Finding

### EC13-1 — `set_function_boundary(Bounded { max_size: 0 })` lacks release-mode pin test
- **Severity:** LOW (confidence 85)
- **Where:** `crates/cfg/src/cfg/options.rs:218-227` (the `set_function_boundary` overload) vs `crates/cfg/tests/options.rs:40-45` (only tests `set_function_max_size`).
- **What:** Both `set_function_max_size(0)` and `set_function_boundary(Bounded { max_size: 0 })` use `debug_assert!(false, ...)` to flag the invalid input.  In release builds the `debug_assert!` is compiled out but the corrective assignment (`self.options.fn_max_size = None`) still executes — behaviour is correct in both modes.  `set_function_max_size` has a release-mode pin test (`#[cfg_attr(debug_assertions, ignore)]`) at options.rs:40-45.  `set_function_boundary` has no equivalent pin.  The skill `crates/strider/.claude/skills/strider-public-api-encapsulation/SKILL.md:60` flags `debug_assert!` in error paths as an anti-pattern.
- **Fix:** Add a release-mode pin test for `set_function_boundary(Bounded { max_size: 0 })` paralleling the existing `options_builder_set_function_max_size_zero_falls_back_to_unbounded_in_release`.  Longer-term: convert both setters to `Result` per the skill guide.

## Categories defended

✓ **`addr == start_addr` boundary** — strict `target < lower` in `is_addr_tail_call` (`query.rs:38`); entry is in-range.  Pinned at `region_builder_tail_call.rs:64-71`.

✓ **u64::MAX addresses / overflow arithmetic** — `next_pcode_addr` `checked_add` with `Err` on overflow; `MemRegion::new` `checked_add`; `is_addr_tail_call` `saturating_add`.  Pinned by `fn_max_size_plus_start_addr_overflow_treats_inside_range_as_non_tail_call`.

✓ **Empty `Vec<u8>` to `ElfFileMemReader::from_bytes`** — `object::File::parse(&[])` returns `Err`; clean propagation.

✓ **Empty pattern sets** — `find_all_requirements(&[])` and `find_all_multi(&[])` early-return empty vec.

✓ **INT_MIN sign-extension** — `eval_int_binary` guards `Sdiv`/`Srem` for `INT_MIN / -1`; `int_const_signed` uses `wrapping_neg`.

✓ **Float boundaries (NaN / ±inf / signed zero)** — tests `fold_f64_equal_nan_false`, `fold_f64_nan_plus_one_stays_nan`, `fold_f64_inf_minus_inf_is_nan` cover IEEE 754 cases.

✓ **Wide-const boundaries (U256/U512)** — fixed-size `[u64; 4]` / `[u64; 8]` limbs; `check_layer_c_wide_consts` catches dangling IDs + width mismatches; round-trip + dedup + LE-byte serialization pinned.

✓ **`StackStorePhi` lifetime-zero overlap** — `Match::stack_phi_offsets` collapses empty side-table to `None`.

✓ **Empty fingerprints (opt-in)** — `check_asm_fingerprints` flag at `layer_c.rs:202-222`; exempt kinds enumerated at lines 184-197.

✓ **Self-referential graph** — `walk_graph` uses `DenseEntitySet`; cycles terminate at second visit.

✓ **Single-pred ControlState** — `RedundantPhis` collapses single-pred join nodes.  Layer C `EmptyControlStatePredecessors` fires only on zero preds; single-pred is structurally valid.

✓ **Sub-byte widths (1-bit carry flag)** — `NodeOutputType::Bool` is 1-bit; `eval_int_binary` masks via `ty.bit_mask_u128()`.

✓ **`fn_max_size = Some(0)` release behaviour** — corrective `self.options.fn_max_size = None` assignment is OUTSIDE the `debug_assert!`, runs in both modes.  Behaviour correct.

## Summary

The R12 EC-1 release-mode behaviour is provably correct (the corrective assignment executes in both debug and release; the `debug_assert!` only adds debug-mode noise).  The single residual is the test-coverage asymmetry between the two zero-bound setters.  All other boundary classes are defended.
