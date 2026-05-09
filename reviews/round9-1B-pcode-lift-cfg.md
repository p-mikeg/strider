# Round 9 / 1B — `pcode-lift` + `cfg` audit

**Branch:** `feature/ai`. Independent audit; round-7/round-8 not consulted.

## Coverage

All `crates/pcode-lift/src/**/*.rs` (lib, vn_io, value/{mod,arithmetic,boolean,cast,float,integer,mem_load,misc_value}) and tests. All `crates/cfg/src/**/*.rs` (lib, test_api, cfg/{mod,types,options,query,decode_cache}, cfg/builder/{mod,region_builder,split,indirect_resolve}) and 25 test files. Cross-reference `opt/src/indirect_branch_resolve/{classify,mod}.rs`, `ir/src/builder/coerce.rs`, rsleigh `Opcode` enum.

## Critical

None found.

## Important

### Finding 1 — `Extend(_)` arm in `classify_anchor_with_rom_and_sp` is dead code in production

**Confidence:** 95.

**Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:252-269`.

Production `classify_anchor_with_rom_and_sp` runs after `build_stable_optimizer_pipeline()` (`crates/strider/src/orchestrator.rs:871-872`). The stable pipeline includes `ConstantFold`, whose rules 5/6 fold `ZeroExtend(IntConst)` and `SignExtend(IntConst)` (`crates/opt/src/constant_fold/rules.rs:456-492`). Additionally, `FunctionBuilder::extend_if_needed` folds eagerly (`crates/ir/src/builder/coerce.rs:185-193`). The only way to produce a live `Extend(IntConst)` is to bypass the builder — exactly what the unit test does.

**Fix:** Either remove the arm, or document that it's only reachable when bypassing the builder (test-only).

### Finding 2 — `Truncate(_)` arm in `classify_anchor_with_rom_and_sp` is also dead code in production

**Confidence:** 92.

**Where:** `crates/opt/src/indirect_branch_resolve/classify.rs:233-250`.

Same root cause as Finding 1: `ConstantFold` rule 4 folds `Truncate(IntConst)` (`crates/opt/src/constant_fold/rules.rs:440-455`). `FunctionBuilder::truncate_if_needed` folds eagerly (`crates/ir/src/builder/coerce.rs:159-162`).

**Fix:** Same as Finding 1.

### Finding 3 — `Builder::new` with non-x86_64 Sleigh silently installs wrong `ArchPreset::X86_64`

**Confidence:** 88.

**Where:** `crates/cfg/tests/indirect_dispatch.rs:159` and `crates/cfg/src/cfg/builder/mod.rs:94-95`.

`Builder::new` delegates to `Builder::with_endianness`, which hardcodes `preset: ArchPreset::X86_64`. Test uses ARM Sleigh + `bx lr` byte sequence. Currently harmless because `bx lr` lifts to `BranchIndirect` (not `CallOther`), so the wrong preset doesn't affect this test. But adding any ARM `CallOther`-emitting instruction (e.g. `BKPT`) would silently misclassify.

**Fix:** Change to `Builder::for_arch(&target::SleighArch::arm(), sleigh, base, opts)`.

### Finding 4 — `resolve_const_loads` single-pass design misses chained loads (Load-of-Load-const-addr)

**Confidence:** 80.

**Where:** `crates/cfg/src/cfg/builder/indirect_resolve.rs:264-300`.

`resolve_const_loads` collects all graph nodes via `fg.preorder().collect()` upfront and iterates once. `opt::LoadReadOnly` (`crates/opt/src/load_readonly/mod.rs:55`) uses `WorkSet` that re-queues nodes whose uses change. Comment at line 262 says "Must stay in lockstep with `opt::LoadReadOnly`'s impl" but the workset-strategy diverges. Multi-hop pointer chains in ROM remain unresolved.

**Fix:** Replace `preorder().collect()` loop with `WorkSet::seeded_kind(fg, |k| matches!(k, NodeKind::Load(_)))` mirroring `LoadReadOnly::optimize_built`.

## Special-Focus Verification

### vn_io register-aliasing all widths

| Width | `vn_mask` result | Sub-reg aliasing | IR type |
|-------|-----------------|------------------|---------|
| 1 | `0xFF` | Yes | U8 |
| 2 | `0xFFFF` | Yes | U16 |
| 4 | `0xFFFF_FFFF` | Yes | U32 |
| 8 | `0xFFFF_FFFF_FFFF_FFFF` | Yes | U64 |
| 10 | `(1<<80)-1` | Yes (x87 ST0) | U80 |
| 16 | `u128::MAX` | Yes (XMM/q) | U128 |
| 32 | `u128::MAX` (degraded) | Errors at line 223 | U256 |
| 64 | `u128::MAX` (degraded) | Errors at line 223 | U512 |

Wide-container guard rejects sub-register aliasing for >16-byte containers correctly.

### Sub-register partial-write traced

AL write (offset 0, size 1) into RAX (size 8) on LE: 7 IR nodes correctly emitted; subsequent EAX read truncates the merged value. Chain includes `InitialVar(RAX)` proving upper bits preserved. Confirmed by `vn_io_partial_write.rs` tests.

### Lift-time canonicalisations bit-exact

All 8 verified semantically correct: `IntSub`, `IntLessEqual`, `IntSlessEqual`, `IntNotEqual`, `FloatSub`, `FloatNotEqual`, `FloatLessEqual` (NaN-safe `Or`), `FloatNan(x) → BoolNeg(FloatEqual(x,x))`.

### Other axes verified

- `is_addr_tail_call` half-open semantics and saturating_add: correct.
- `Builder::for_arch` invariant: production correct; test bug isolated to Finding 3.
- CallOther classification dispatch site reads correct preset from `Builder`.

## Coverage Summary

42 source files, 25 cfg test files, all examined. Findings limited to Findings 1-4 above.
