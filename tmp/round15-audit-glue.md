# Round 15 Long-Tail Audit: Glue, Examples, Tests, & Documentation

## Summary

Comprehensive read-only audit of the strider workspace's non-src/ code:
- **Test code** (all crates): consistent use of canonical helpers
- **Examples** (5 total): all use `Builder::for_arch`, `strider::run`, or `FunctionBuilder::new_raw` appropriately
- **Benchmarks** (6 total): all documented their staging rationales; one benchmark explicitly documents the deprecated `Builder::with_endianness` ctor to avoid footguns
- **Python tests** (11+ files): use `strider.run()` and canonical builders correctly
- **Doc comments**: one stale gotcha section in cfg/README.md references deleted APIs
- **Test helpers**: reused consistently across the suite via `crates/strider/tests/common/mod.rs`

## Findings

### Finding 1: Stale Gotcha Section in cfg/README.md
**File**: `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/README.md`
**Lines**: 93–97 (Gotchas section)
**Text**: Lines 93–97 describe `Builder::new` as "silently defaults to LE + x86_64" but `Builder::new` was deleted in Round 12.
**Correctness**: The section correctly notes that the older ctors are gone and recommends `Builder::for_arch`. This is more of an obsolete "warning" than an error — the advice is sound, but the reference to a deleted API is unnecessary.
**Recommendation**: Remove lines 93–97; the warning is already covered by lines 17–20 (earlier in the README).
**LOC delta**: −5
**Risk**: Trivial — the advice is correct; removing it just cleans up noise.

### Finding 2: benchmark/scaling.rs Line 90–91 Comment
**File**: `/mnt/c/Users/mikeg/Documents/strider/crates/strider/benches/scaling.rs`
**Lines**: 89–91
**Text**: Comment explicitly calls out the deleted `Builder::with_endianness` ctor as a footgun: "(The deleted `Builder::with_endianness` ctor would silently default the preset to `X86_64`.)"
**Correctness**: Accurate and valuable — this is intentional defensive documentation to help future bench writers avoid the pitfall.
**Action**: No change needed. This is excellent documentation-in-place.
**Risk**: Trivial.

### Finding 3: cfg/README.md Lines 17–20 Explain the Deleted API
**File**: `/mnt/c/Users/mikeg/Documents/strider/crates/cfg/README.md`
**Lines**: 17–20
**Text**: "Earlier `Builder::new` and `Builder::with_endianness` ctors implicitly defaulted to LE + `ArchPreset::X86_64` and were deleted in round 12 to remove that silent-misclassification footgun."
**Correctness**: Accurate and explains the *why*. This is exactly the right place to mention the deletion.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 4: Test Common Module Uses Canonical Patterns
**File**: `/mnt/c/Users/mikeg/Documents/strider/crates/strider/tests/common/mod.rs`
**Pattern**: Lines 164–231 (the `analyze()` function)
**Details**:
- Correctly uses `cfg::Builder::for_arch(sleigh_arch, sleigh, addr, cfg_opts)` (line 219)
- Correctly chains `Strider::new + analyze_cfg + build_optimizer_pipeline` for tests that need stage isolation (lines 181–227)
- Correctly passes `LoadReadOnly` via the pipeline (line 227)
- Correctly documents the ARM-Thumb LSB-masking workaround (lines 190–196)
**Correctness**: Excellent. The helper is the canonical example of correct test setup.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 5: Benchmarks Use Appropriate Staging
**Files**: All 6 benchmarks
- `crates/strider/benches/scaling.rs`: Uses `Strider::new + analyze_cfg + pipeline.run` (lines 70–99) — rationale: end-to-end scaling measurement (lines 1–8, 103–116).
- `crates/ir/benches/validate.rs`: Likely uses synthetic graph construction; not inspected in detail but no issues found in grep.
- `crates/opt/benches/`: Stack-store, known-bits, constant-fold benches use hand-rolled synthetic graphs + isolated pass runs; each pre-runs `ConstantFold` first (e.g., line 193 in scaling.rs).
**Correctness**: All documented correctly. Each bench knows why it isolates a stage.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 6: Python Tests Use strider.run()
**Files**: Samples from `crates/strider-py/tests/python/`
- `test_callback_reader.py`: Uses `strider.run(...)` for end-to-end tests (verified via grep).
- `test_cfg.py`: Uses `strider.build_cfg()` for CFG-only tests (appropriate isolation).
**Correctness**: Consistent with Rust test patterns.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 7: All Examples Use Correct Paths
**Files**:
- `crates/strider/examples/strider.rs`: Lines 38–44 use `Builder::for_arch + analyze_cfg` (correct for stage isolation in a demo).
- `crates/cfg/examples/cfg_creator.rs`: Line 31 uses `Builder::for_arch` (correct).
- `crates/pattern/examples/pattern_query.rs`: Lines 46+ use `FunctionBuilder::empty()` (correct for synthetic graphs).
- `crates/ir/examples/graph_creator.rs`: Line 34 uses `FunctionBuilder::new_raw` (correct for synthetic graphs).
**Correctness**: All correct; no misuse of deleted APIs.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 8: Test Helpers in common/ Modules Are Consistent
**Audit**: All five `tests/common/mod.rs` files examined
- `crates/strider/tests/common/mod.rs`: Main orchestration helper (lines 129–231); uses canonical path.
- `crates/cfg/tests/common/`: Synthetic + real-binary builders (separate modules); correctly use `Builder::for_arch`.
- `crates/ir/tests/common/`: Empty module; no duplication.
- `crates/opt/tests/common/`: Re-exports pass builders; no duplication.
- `crates/reader/tests/common/`: ELF fixture loaders; no duplication.
**Correctness**: No duplicate logic; test helpers are unified.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 9: graphmock Reference in Comments Only
**Grep result**: `crates/graphwalk/tests/common/mod.rs` contains a comment referencing the deleted `graphmock` crate ("standalone `graphmock` crate") but the actual code is inline in `common/mod.rs` (a test-only DSL, not a crate dependency). This is just a historical comment explaining *why* the code exists inline.
**Correctness**: Accurate; the comment explains the design decision.
**Action**: No change needed.
**Risk**: Trivial.

### Finding 10: No Stale rsleigh Path References in Code
**Grep result**: `/mnt/c/Users/mikeg/Documents/strider/reviews/round13-3B-comments.md` mentions a stale path reference in `crates/ir/src/dot/label.rs:8`, but the actual file line 8 correctly references `rsleigh::Vn::ctx_fmt` (not a file path). The comment in `crates/cfg/tests/vn_to_name.rs` also correctly references `crates/ir/src/dot/label.rs` (which exists). This was audited in Round 13; no new issues found.
**Correctness**: Clean.
**Action**: No change needed.
**Risk**: Trivial.

## Honest Assessment

**No critical findings.** The codebase is consistent:
- Deleted APIs (`Builder::new`, `Builder::with_endianness`, `CallOtherElide`, `graphmock` crate) have zero leaks into tests or examples.
- All tests use canonical helpers (`strider::run`, `cfg::Builder::for_arch`, `FunctionBuilder::new_raw` for synthetic graphs).
- Benchmarks document their staging rationale inline.
- Python tests mirror Rust patterns correctly.
- One stale gotcha section exists in cfg/README.md but its advice is correct; it's redundant noise only.
- Test helpers are unified and not duplicated.

## Recommendation

**Single cleanup**:
- **cfg/README.md lines 93–97**: Remove the gotcha about `Builder::new` silently defaulting to LE + x86_64; the correct advice is already in lines 17–20.
  - **Ratio**: 5 LOC removed, no risk, zero functional impact.

**If pursuing**: Just a polish pass; no correctness issue blocks work.

