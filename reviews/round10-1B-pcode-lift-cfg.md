# Round 10 — `pcode-lift` + `cfg`

Reviewing all source files in `crates/pcode-lift/src/` (10 files) and `crates/cfg/src/` (12 files) with their test suites.

---

## CRITICAL

### C-1: `resolve_const_loads` silently drops loads wider than 8 bytes, never folds them
- **Severity:** MED (re-classified after analysis — see below)
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:302`
- **What's wrong:** The chain `ty.get_unsigned_int(u128::from(loaded)).and_then(|v| u64::try_from(v).ok())` returns `None` whenever the masked value > `u64::MAX`. `rom.read` already returns `Option<u64>`, so any `loaded` value fits in u64. For `U80`/`U128` typed loads, the masked u128 from `get_unsigned_int` applied to a value already ≤ u64::MAX always fits in u64 — the `u64::try_from` always succeeds. So the chain is correct for the actual ROM-read path. The latent issue: `make_int_const` accepts only `u64`, so wider types are silently confined to the low-64 bits. Low risk because `BranchIndirect` targets are machine pointers (≤64-bit). **Real-world risk: medium.**
- **Verified against:** `crates/ir/src/ops/consts.rs:82` — `make_int_const` takes `u64`.
- **Fix:** Either gate the fold on `ty.fits_u64()` and skip wider types explicitly, or use `make_int_const_wide` for U128 outputs.

---

## IMPORTANT

### I-1: `Region.insns` doc says "Never empty" but code creates empty-Branch regions; `contains_addr` returns false for them
- **Severity:** HIGH (confidence: 95)
- **Where:** `crates/cfg/src/cfg/types.rs:221-223`, `crates/cfg/src/cfg/builder/mod.rs:184-192`
- **What's wrong:** `Region` doc says "Never empty" (`types.rs:221`). `add_region` was relaxed to allow empty `Branch`-terminated regions. `contains_addr` (`types.rs:233-236`) uses `self.insns.last()` — when `insns` is empty it returns `false` for any address, including `start_addr`. This means `find_region_containing_addr` will never return this empty region even at `start_addr`. If the work queue later pushes `start_addr` again (e.g., a second edge to the same empty-Branch region), `explore` sees no existing region and attempts to build a new one — yielding a duplicate region.
- **Verified against:** `crates/cfg/src/cfg/builder/mod.rs:205-215` — `find_region_containing_addr` calls `region.contains_addr(addr)`, fails on empty regions.
- **Fix:** `contains_addr` must handle the empty case: when `insns` is empty, return `start_addr == addr`. Alternatively fix the doc and route start-address queries through `start_addr_to_region_id` exclusively.

---

### I-2: `PcodeInsnAddr` and `MachineInsnAddr` fields remain `pub` despite accessor migration intent
- **Severity:** MED (confidence: 82)
- **Where:** `crates/cfg/src/cfg/types.rs:31,65`, `crates/cfg/src/cfg/mod.rs:72`
- **What's wrong:** Round 9 V3 added `.as_u64()` / `.machine_addr()` / `.insn_index()` accessors as a migration-path; fields stayed `pub`. External code can construct ad-hoc `MachineInsnAddr { addr: x }` or mutate `Cfg::start_addr_to_region_id` directly. A bogus key inserted there desyncs `region_id_at_start`.
- **Fix:** File a TODO with a milestone to flip the fields to `pub(crate)`. In the interim, document the invariant explicitly on each `pub` field: "Direct write desyncs `start_addr_to_region_id`."

---

### I-3: `handle_int_sub` — `neg_ty` from `inputs[1].size` may mismatch `lhs` width
- **Severity:** HIGH (confidence: 87)
- **Where:** `crates/pcode-lift/src/value/arithmetic.rs:161-167`
- **What's wrong:** The IMP-4 wave-31 fix uses `insn.inputs[1].size` for `Neg`'s width, while the downstream `Add` uses `out_ty`. If Sleigh emits `IntSub` with `inputs[0].size != inputs[1].size`, the IR builder's `build_int_binary_operation` will reject the width mismatch — but the comment claims "input_size == output_size" always, while the code accepts a divergent input width. The OLD code was actually correct for the typical case; the IMP-4 substitution makes it robust for an unrealistic future edge case but introduces a new failure mode for the (theoretically illegal) mixed-width `IntSub`.
- **Fix:** Either (a) add an explicit assertion `inputs[0].size == inputs[1].size == out_vn.size`, or (b) revert to using `out_ty` for the `Neg` since Sleigh guarantees equal widths.

---

### I-4: `resolve_const_loads` walks a snapshot of `preorder()` but mutates during iteration
- **Severity:** LOW
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:279-310`
- **What's wrong:** `let nodes: Vec<_> = fg.preorder().collect()` snapshots `NodeId`s; `replace_all_uses(data_out, new_out)` rewrites uses but does not remove the old Load. If the new IntConst already exists (dedup), no NodeId is recycled, so iteration is sound — sea-of-nodes graphs don't reuse NodeIds. Practical risk: zero. Worth a comment explaining why the snapshot is safe.
- **Fix:** Add a comment: `replace_all_uses` never recycles the visited `NodeId`; the zombie Load remains at its original ID.

---

### I-5: `Builder::with_endianness` and `Builder::new` lack `#[deprecated]` despite the documented X86_64 preset trap
- **Severity:** MED (confidence: 88)
- **Where:** `crates/cfg/src/cfg/builder/mod.rs:103-120`
- **What's wrong:** CLAUDE.md documents the trap. `Builder::for_arch` is the safe alternative. There is no compile-time signal to steer callers away from `Builder::new` / `Builder::with_endianness`. An AArch64 binary lifted via the old API silently gets X86_64 CallOther classification. NoReturn CallOthers (`brk`) on AArch64 are misclassified as non-terminating, letting the builder decode past a trap.
- **Fix:** Add `#[deprecated(since = "...", note = "Use Builder::for_arch to set endianness and ArchPreset atomically.")]` to `Builder::new` and `Builder::with_endianness`.

---

### I-6: `read_reg_vn` / `write_reg_vn` shift constants — no validation that `shift < container_bits`
- **Severity:** MED (confidence: 85)
- **Where:** `crates/pcode-lift/src/vn_io.rs:246-253` (and `write_reg_vn` mirror)
- **What's wrong:** For sub-register access in 16-byte (XMM) containers, the shift constant is built at `container_reg.size` width. The maximum legitimate shift is `8 * 15 = 120` bits, well within U128 range. However there is no defensive check that `shift_value < container_reg.size * 8`. A malformed Sleigh spec (sub-register at offset 17 inside a 16-byte container) would produce shift = 136 ≥ 128 and the IR's ShiftRight/ShiftLeft would be UB.
- **Fix:** `debug_assert!(shift_value < (container_reg.size as u64) * 8, "sub-register shift {shift_value} >= container bit width")`.

---

### I-7: `split_region` doesn't guard against `split_index == insns.len()` — could create an empty second region with non-Branch terminator
- **Severity:** MED (confidence: 80)
- **Where:** `crates/cfg/src/cfg/builder/split.rs:48-91`
- **What's wrong:** `split_index` is computed from a position search that can land at `insns.len()` (rounded-down fallback `i + 1`). The early-return at line 68 only handles `split_index == 0`. If `split_index == insns.len()`, the second region's `insns` becomes empty and retains the original terminator (which may not be `Branch`). `add_region`'s rejection of empty non-Branch regions does NOT apply here because `split_region` mutates in place. `Cfg::region_id_at_start` would return the second-region ID but `contains_addr` on it would return false for any address.
- **Fix:** Add `if split_index >= insns.len() { return Ok(region_id); }` after line 67.

---

## LOW

### L-1: `vn_mask` — 16-byte container's `u128::MAX` is exact, but lumped with 32/64-byte degraded cases
- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/vn_io.rs:45`
- **Fix:** Add a comment to the `16 | 32 | 64` arm clarifying: 16-byte is exact (128 bits = all 1s); 32/64-byte is degraded (actual width exceeds u128).

### L-2: `handle_float_sub` derives `float_ty` from `out_vn` for both Neg and Add — input width mismatch theoretically possible
- **Severity:** LOW
- **Where:** `crates/pcode-lift/src/value/float.rs:103-106`
- **Fix:** Use `Self::float_type_from_vn(&insn.inputs[1])` for the `Neg` and `float_ty` (from `out_vn`) for the `Add`, mirroring the recommended `handle_int_sub` pattern.

### L-3: `make_resolver_pipeline` includes `RedundantPhis` despite mini-graph having no phi nodes
- **Severity:** LOW (confidence: 88)
- **Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:252-258`
- **What's wrong:** The mini-graph is a single basic block — no `CondBranch`, no `VarPhi`. `RedundantPhis` runs its full sweep and finds nothing. Wasted work proportional to node count on every indirect-branch site.
- **Fix:** Remove `pipeline.add(opt::RedundantPhis)` and add a docstring note explaining "no phi nodes in the mini-graph — RedundantPhis intentionally omitted."

---

## Coverage

| File | Status |
|------|--------|
| `crates/pcode-lift/src/lib.rs` | Fully |
| `crates/pcode-lift/src/vn_io.rs` | Fully |
| `crates/pcode-lift/src/value/mod.rs` | Fully |
| `crates/pcode-lift/src/value/arithmetic.rs` | Fully |
| `crates/pcode-lift/src/value/boolean.rs` | Fully |
| `crates/pcode-lift/src/value/cast.rs` | Fully |
| `crates/pcode-lift/src/value/float.rs` | Fully |
| `crates/pcode-lift/src/value/integer.rs` | Fully |
| `crates/pcode-lift/src/value/mem_load.rs` | Fully |
| `crates/pcode-lift/src/value/misc_value.rs` | Fully |
| `crates/cfg/src/cfg/mod.rs` | Fully |
| `crates/cfg/src/cfg/types.rs` | Fully |
| `crates/cfg/src/cfg/query.rs` | Fully |
| `crates/cfg/src/cfg/options.rs` | Fully |
| `crates/cfg/src/cfg/decode_cache.rs` | Fully |
| `crates/cfg/src/cfg/dot.rs` | Partially (visualisation only) |
| `crates/cfg/src/cfg/builder/mod.rs` | Fully |
| `crates/cfg/src/cfg/builder/region_builder.rs` | Fully |
| `crates/cfg/src/cfg/builder/split.rs` | Fully |
| `crates/cfg/src/cfg/builder/indirect_resolve.rs` | Fully |
| `crates/cfg/src/lib.rs` | Fully |
| `crates/cfg/src/test_api.rs` | Fully |
| `crates/cfg/tests/*.rs` | Partially |
| `crates/pcode-lift/tests/*.rs` | Partially |
