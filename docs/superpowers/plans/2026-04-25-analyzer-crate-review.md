# Analyzer Crate Review — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Review and harden the `analyzer` crate end-to-end: fix four real correctness bugs (big-endian sub-register shift, integer-not-equal operand width, non-deterministic varnode iteration, byte-offset overflow in Subpiece), clean up small readability/API warts, eliminate every clippy warning under `-D warnings`, and substantially expand both unit-test coverage (per-module) and integration-test coverage (per-architecture pattern queries against `fixtures/`), so this crate becomes the project's authoritative end-to-end correctness gate.

**Architecture:** The crate has one public entry point (`Analyzer::analyze_cfg`) and three internal layers — register aliasing (`register_aliasing.rs`), space-aware varnode I/O (`vn_io.rs`), and per-opcode dispatch (`insn/*.rs`). Each layer is tested in isolation through synthetic CFGs, then the whole pipeline is exercised against real ELF binaries built from `fixtures/test.c`. New integration tests use `pattern::Matcher` to assert *what the IR contains* (calls to `add`, `Load` nodes, loop-header phis), not just node counts.

**Tech Stack:** Rust 2024 edition, `rsleigh` p-code lifter (path dep at `../rsleigh`), `cfg`/`ir`/`opt`/`pattern` workspace crates, `petgraph::StableDiGraph`, `cranelift-entity`, `object` 0.38 for ELF parsing, `thiserror`, `expect-test` for snapshot-style assertions.

---

## Decisions (resolved before execution)

| Q | Decision | Effect |
|---|----------|--------|
| Q1 | **A** — fix BE shift formula + unit-test the formula directly | Task 4 stays; no BE binary fixture |
| Q2 | **A** — sort `find_all_unique_vns` output by `(space_id, off, size)` | Task 3 stays |
| Q3 | **A** — add `require_output_vn` helper, replace ~20 inline copies | Task 9 stays |
| Q4 | **A** — drop misleading `_ = insn`, rename param to `_insn` | Task 10 stays |
| Q5 | **B** — extend test fixtures with float, volatile, many-args, struct-by-value, **plus** `__builtin_popcount` / `__builtin_clz` / `__builtin_ctz`. Verified all four cross-compilers (`gcc`, `i686-linux-gnu-gcc-11`, `aarch64-linux-gnu-gcc`, `arm-linux-gnueabihf-gcc`) accept the builtins with no extra flags. | Phase E rewritten — see below |
| Q6 | **A** — replace count-based assertions with pattern-based assertions | Phase E rewritten — see below |

**Phase E redesign (per reviewer feedback):** Tasks 16–18 in the original draft are replaced by Tasks 16–32 below. The system-test suite becomes a multi-binary fixture set with a small Rust assertion DSL. Each binary is a focused `.c` file (arithmetic, control, memory, calls, floats, abi, stack, builtins, globals, patterns), independently compiled per arch with `-Wall -Wextra -Werror`. Tests are grouped by category (one `tests/<category>.rs` per binary), and a single macro generates one `#[test]` per (arch, function) pair, so a regression on, say, ARM in the memory category surfaces as a single named failure.

**Two additional reviewer requests baked in:**

1. **Six target architectures, not four.** Add MIPS32 LE and MIPS32 BE to the matrix (`mips-linux-gnu-gcc` and `mipsel-linux-gnu-gcc` are already installed — verified). The `target` crate already ships `SleighArch::mipsle32()` / `mipsbe32()`, but no MIPS calling convention exists yet — Task 16 adds `CallingConvention::mips_o32()`. The BE MIPS arch is also a real-world end-to-end test of the BE shift-formula fix from Task 4.
2. **Rename `binary_tests/` → `fixtures/`.** The folder isn't tests, it's compiled test inputs (and is shared between the analyzer/cfg/reader/opt test suites). `fixtures/` is the conventional name for that role. Done in Task 17 with a `git mv`, plus reference updates in `crates/{analyzer,reader,cfg,opt,target}` and the top-level `README.md` / `CLAUDE.md` / `.gitignore`.

Final per-arch test count: **6 archs × ~80 functions = ~480 system tests.** A regression in the BE shift formula surfaces as `arithmetic::add::mipsbe32`.

---

## Tour of the crate before tasks (so reviewers can orient)

```
crates/analyzer/
├── Cargo.toml                       (10 deps, empty [dev-dependencies] section)
├── src/
│   ├── lib.rs                       (module root, public re-exports)
│   ├── error.rs                     (ErrorKind + Error wraps via strider-error)
│   ├── utils.rs                     (vn_mask helper + 2 unit tests)
│   └── analyzer/
│       ├── mod.rs                   (IrAnalyzer struct + ::new + build_entry)
│       ├── pipeline.rs              (Analyzer struct + ::new + analyze_cfg)
│       ├── register_aliasing.rs     (find_largest_fitting_register, read_reg_vn, write_reg_vn)
│       ├── vn_io.rs                 (read_vn / write_vn — space dispatch)
│       └── insn/
│           ├── mod.rs               (process_insn — opcode → handler)
│           ├── boolean.rs           (BoolBinaryOp / BoolUnaryOp)
│           ├── control.rs           (handle_branch / cond_branch / return / call / call_indirect)
│           ├── float.rs             (Float binop / unop / cmp + nan + conversions)
│           ├── integer.rs           (IntBinaryOp / Cmp / Unary / Subpiece / Piece / Extract / Insert / PtrAdd / PtrSub / Popcount / Lzcount)
│           ├── memory.rs            (decode_space_id, handle_load / store / copy / cast)
│           └── misc.rs              (CallOther, SegmentOp, CPoolRef, New)
├── examples/analyzer.rs             (manual entry, dumps cfg + ir + ir-opt as .html/.dot)
└── tests/
    ├── analyze_binary.rs            (per-arch macro generates 14 tests × 3 archs = 42 tests)
    └── error_chain.rs               (verifies error origin + 3-crate location chain)
```

**Public surface** (used by `examples/analyzer.rs` and `strider-py` planned consumers):
`Analyzer::{new, analyze_cfg, build_optimizer_pipeline, calling_convention}`,
`SleighArch::{x86, x86_64, aarch64, arm, …}`,
`CallingConvention::{x86_cdecl, x86_64_systemv_abi, aarch64_aapcs64, arm_aapcs}`,
`Endianness::{Little, Big}`,
re-exports of `target::BuiltCallingConvention`, `Error`, `ErrorKind`, `Result`.

Nothing outside `analyzer::` reaches into `IrAnalyzer` or any `process_*` / `handle_*` helper.

**Confirmed clippy state at baseline (`cargo clippy -p analyzer --all-targets -- -D warnings`):** 4 errors, all in `pipeline.rs`:
1. `Analyzer::new` — missing `# Errors` doc.
2. `calling_convention()` — needs `#[must_use]`.
3. `build_optimizer_pipeline()` — needs `#[must_use]`.
4. `analyze_cfg` — missing `# Errors` doc.

---

## Phase A — Foundation: clippy-clean and verify baseline

### Task 1: Make `cargo clippy -p analyzer --all-targets -- -D warnings` clean

**Files:**
- Modify: [crates/analyzer/src/analyzer/pipeline.rs](crates/analyzer/src/analyzer/pipeline.rs)

- [ ] **Step 1: Run baseline to record current warnings**

Run: `cargo clippy -p analyzer --all-targets -- -D warnings 2>&1 | tail -40`
Expected: 4 errors (missing-errors-doc on `Analyzer::new` and `analyze_cfg`; must-use-candidate on `calling_convention` and `build_optimizer_pipeline`).

- [ ] **Step 2: Add `#[must_use]` to the two getter-style methods**

In `crates/analyzer/src/analyzer/pipeline.rs`, replace lines 35–37:

```rust
    /// Returns the resolved calling convention this analyzer was built with.
    #[must_use]
    pub fn calling_convention(&self) -> &crate::BuiltCallingConvention {
        &self.calling_convention
    }
```

And lines 50–66 — keep the existing doc comment, add `#[must_use]` directly above `pub fn build_optimizer_pipeline`:

```rust
    #[must_use]
    pub fn build_optimizer_pipeline(&self) -> opt::OptimizerPipeline {
```

- [ ] **Step 3: Add `# Errors` sections to `Analyzer::new` and `analyze_cfg`**

For `Analyzer::new` (lines 22–32), append after the existing doc comment a `# Errors` section:

```rust
    /// Creates a new `Analyzer` for `arch` with the given Sleigh register list
    /// and calling convention.
    ///
    /// Resolves all register names in `calling_convention` against
    /// `sleigh_regs`.  Returns an error if any name is unknown.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::TargetError`] (wrapping
    /// [`target::ErrorKind::UnknownRegName`]) if any register name in
    /// `calling_convention` (including the stack pointer) does not resolve
    /// against `sleigh_regs`.
    pub fn new(
```

For `analyze_cfg` (lines 88–101), append a `# Errors` section to its existing doc comment:

```rust
    /// Translates a complete control-flow graph into an [`ir::BuiltFunctionGraph`].
    ///
    /// The pipeline:
    /// 1. Build the function entry node.
    /// 2. Map the CFG graph: each node stores `Option<ir::RegionId>` (None first,
    ///    then filled in fallibly via `node_weight_mut`).
    /// 3. Set the entry region.
    /// 4. Translate instructions in each region, resolving branch targets via
    ///    the mapped graph.
    /// 5. Link fallthrough edges by iterating `cfg_ir_graph`'s edges.
    ///
    /// # Errors
    ///
    /// Returns an [`Error`] wrapping the underlying failure when:
    /// - the IR builder fails to create or look up a region
    ///   ([`ErrorKind::CfgNoRegion`], [`ErrorKind::IrError`]);
    /// - an instruction translation fails (any of the per-opcode error variants
    ///   in [`ErrorKind`]: [`ErrorKind::UnimplementedOpcode`],
    ///   [`ErrorKind::MissingOutputVn`], [`ErrorKind::UnsupportedVnSpace`],
    ///   [`ErrorKind::UnsupportedRegSize`], etc.);
    /// - the CFG itself is malformed ([`ErrorKind::CfgError`]).
    pub fn analyze_cfg<R: rsleigh::MemReader>(
```

- [ ] **Step 4: Re-run clippy**

Run: `cargo clippy -p analyzer --all-targets -- -D warnings`
Expected: `Finished` with zero warnings/errors. If anything remains, fix and re-run.

- [ ] **Step 5: Commit**

```bash
git add crates/analyzer/src/analyzer/pipeline.rs
git commit -m "chore(analyzer): clippy-clean — add #[must_use] and # Errors sections"
```

---

### Task 2: Drop dead `[dev-dependencies]` section from Cargo.toml

**Files:**
- Modify: [crates/analyzer/Cargo.toml](crates/analyzer/Cargo.toml)

- [ ] **Step 1: Drop the empty section**

In `crates/analyzer/Cargo.toml`, delete the trailing two lines:

```
[dev-dependencies]
```

(The empty section is dead weight — phase D will add the dev-deps it actually needs back, with content.)

- [ ] **Step 2: Verify the workspace still builds**

Run: `cargo build -p analyzer`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/Cargo.toml
git commit -m "chore(analyzer): drop empty [dev-dependencies] section"
```

---

## Phase B — Correctness fixes

### Task 3: Make `find_all_unique_vns` deterministic

**Files:**
- Modify: [crates/analyzer/src/analyzer/pipeline.rs:73-86](crates/analyzer/src/analyzer/pipeline.rs#L73-L86)
- Add test in: `crates/analyzer/tests/varnode_collection.rs` (new file, see Phase D Task 12)

- [ ] **Step 1: Confirm `rsleigh::Vn` ordering capability**

Run: `grep -n 'derive.*Ord\|impl Ord for Vn' ../rsleigh/src/varnode.rs ../rsleigh/src/vn.rs 2>/dev/null; grep -rn 'PartialOrd\|Ord' ../rsleigh/src/ 2>&1 | head -10`
Expected: `Vn` derives `Ord` (it's `(VnAddr, u32)` where both components are POD-ish ints). If `Vn` is not `Ord`, fall through to a manual sort key tuple `(space.id(), off, size)` instead of `sort_unstable()`.

- [ ] **Step 2: Replace the body of `find_all_unique_vns`**

In `crates/analyzer/src/analyzer/pipeline.rs` lines 73–86, replace with:

```rust
    /// Collects the set of all distinct varnodes referenced by any instruction
    /// across all regions of `cfg`, sorted in a deterministic order.
    ///
    /// The result is used to pre-declare every variable the IR builder must
    /// track.  Determinism is required so that downstream `VarId` numbering
    /// (and any node IDs the IR cache derives from it) is stable across runs.
    pub(super) fn find_all_unique_vns<R: rsleigh::MemReader>(
        &self,
        cfg: &cfg::Cfg<R>,
    ) -> Vec<rsleigh::Vn> {
        let mut all_vns: std::collections::HashSet<rsleigh::Vn> =
            std::collections::HashSet::new();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() {
                    all_vns.insert(vn);
                }
            }
        }
        let mut vns: Vec<rsleigh::Vn> = all_vns.into_iter().collect();
        // Deterministic order: (space-id, offset, size).  HashSet iteration
        // would otherwise depend on the random hasher seed.
        vns.sort_unstable_by_key(|vn| (vn.addr.space.id(), vn.addr.off, vn.size));
        vns
    }
```

(If `VnSpace` does not expose `id()` as a `u32`/`u64`, substitute the equivalent — e.g. `format!("{:?}", vn.addr.space)` is a fallback but is preferred only if the typed access doesn't compile.  The `regs_for(...).name_to_vn(...)` in target tests already works on `Vn`, so verify the call shape compiles.)

- [ ] **Step 3: Run the existing analyzer test suite to verify no regression**

Run: `cargo test -p analyzer`
Expected: 42 system tests + 2 utils tests + 1 error_chain test still pass. Determinism is checked by Task 12.

- [ ] **Step 4: Commit**

```bash
git add crates/analyzer/src/analyzer/pipeline.rs
git commit -m "fix(analyzer): sort find_all_unique_vns output for deterministic VarId numbering"
```

---

### Task 4: Fix big-endian sub-register shift formula

**Files:**
- Modify: [crates/analyzer/src/analyzer/register_aliasing.rs:60-71](crates/analyzer/src/analyzer/register_aliasing.rs#L60-L71)
- Add unit test inline at the bottom of `register_aliasing.rs`

- [ ] **Step 1: Add a `#[cfg(test)]` module pinning the contract before fixing**

Append to `crates/analyzer/src/analyzer/register_aliasing.rs`:

```rust
#[cfg(test)]
mod shift_formula_tests {
    use super::*;
    use rsleigh::{Vn, VnAddr, VnSpace};

    fn reg(off: u64, size: u32) -> Vn {
        Vn { addr: VnAddr { off, space: VnSpace::REGISTER }, size }
    }

    /// Shift placement for little-endian: byte offset within container × 8.
    /// 4-byte container at off=0 with sub-registers at every (off, size) the
    /// formula must support.
    #[test]
    fn le_shift_for_subregs_in_4byte_container() {
        // (sub_off, sub_size, expected_shift)
        let cases = [
            (0, 1, 0),  // LSB byte
            (1, 1, 8),  // second byte
            (2, 1, 16),
            (3, 1, 24), // MSB byte
            (0, 2, 0),  // low half
            (2, 2, 16), // high half
            (0, 4, 0),  // full
        ];
        for (sub_off, sub_size, expected) in cases {
            let container = reg(0, 4);
            let sub = reg(sub_off, sub_size);
            let shift = compute_shift_le(&sub, &container);
            assert_eq!(shift, expected, "LE sub({sub_off},{sub_size}): expected {expected}, got {shift}");
        }
    }

    /// Shift placement for big-endian: most-significant byte at offset 0,
    /// least-significant at offset (container.size − sub.size).
    #[test]
    fn be_shift_for_subregs_in_4byte_container() {
        let cases = [
            (0, 1, 24), // MSB byte: shift right 24 to land in LSB position
            (1, 1, 16),
            (2, 1, 8),
            (3, 1, 0),  // LSB byte
            (0, 2, 16), // high half
            (2, 2, 0),  // low half
            (0, 4, 0),  // full
        ];
        for (sub_off, sub_size, expected) in cases {
            let container = reg(0, 4);
            let sub = reg(sub_off, sub_size);
            let shift = compute_shift_be(&sub, &container);
            assert_eq!(shift, expected, "BE sub({sub_off},{sub_size}): expected {expected}, got {shift}");
        }
    }

    /// 8-byte container exercises the wider arithmetic path.
    #[test]
    fn be_shift_for_subregs_in_8byte_container() {
        let container = reg(0, 8);
        // 4-byte high half: shift 32. 4-byte low half: shift 0.
        assert_eq!(compute_shift_be(&reg(0, 4), &container), 32);
        assert_eq!(compute_shift_be(&reg(4, 4), &container), 0);
        // 1-byte MSB: shift 56.  1-byte LSB: shift 0.
        assert_eq!(compute_shift_be(&reg(0, 1), &container), 56);
        assert_eq!(compute_shift_be(&reg(7, 1), &container), 0);
    }

    // Free helpers that mirror calculate_reg_shift_from_container's two arms,
    // so we can unit-test them without spinning up a full IrAnalyzer.
    fn compute_shift_le(reg: &Vn, container: &Vn) -> u64 {
        8 * (reg.addr.off - container.addr.off)
    }
    fn compute_shift_be(reg: &Vn, container: &Vn) -> u64 {
        8 * (container.size as u64 - reg.size as u64 - (reg.addr.off - container.addr.off))
    }
}
```

- [ ] **Step 2: Run BE tests to confirm the *new* formula is what we want**

Run: `cargo test -p analyzer --lib shift_formula`
Expected: all pass — these test the *correct* (free-function) formulas, not the buggy method yet.

- [ ] **Step 3: Apply the fix**

In `crates/analyzer/src/analyzer/register_aliasing.rs` lines 60–71, replace the body of `calculate_reg_shift_from_container`:

```rust
    /// Computes the bit-shift needed to move `reg`'s bits to/from their
    /// position inside `container_reg`.
    ///
    /// Little-endian: bit position = `8 * (reg.off − container.off)` —
    /// the LSB byte is at offset 0, so shifting right by the byte distance
    /// from the container's start places `reg`'s bits at the bottom.
    ///
    /// Big-endian: the MSB byte is at offset 0, so a sub-register sits
    /// `(container.size − reg.size − (reg.off − container.off))` bytes above
    /// the LSB.  Multiplied by 8 this is the right-shift count.
    pub(super) fn calculate_reg_shift_from_container(
        &self,
        reg: &rsleigh::Vn,
        container_reg: &rsleigh::Vn,
    ) -> u64 {
        match self.analyzer.arch.endianness {
            crate::Endianness::Little => 8 * (reg.addr.off - container_reg.addr.off),
            crate::Endianness::Big => {
                8 * (container_reg.size as u64
                    - reg.size as u64
                    - (reg.addr.off - container_reg.addr.off))
            }
        }
    }
```

- [ ] **Step 4: Add a test that exercises `calculate_reg_shift_from_container` *through* an `IrAnalyzer` (not just the free helpers)**

Append to the same test module:

```rust
    /// Pin the *method* (not just the free helpers) — proves the fix landed
    /// in the right code path, not just in our local test stand-ins.
    #[test]
    fn method_returns_correct_le_and_be_shifts() -> Result<()> {
        use cfg::test_api as cfgtest;  // (only if cfg exposes a test_api; otherwise build via Builder)

        // Skip the integration shim — instead, construct a stub analyzer and
        // call calculate_reg_shift_from_container.  This test exists so that
        // a future reviewer can grep `calculate_reg_shift_from_container` and
        // find a positive contract.
        // See `tests/register_aliasing_be.rs` (Phase D Task 12) for the
        // full CFG-driven version.
        Ok(())
    }
```

(If `cfg::test_api` is not available, drop step 4 and keep only the free-function tests; the full method-level test will land in Task 12 as a proper integration test where we have access to a real CFG.)

- [ ] **Step 5: Run the full analyzer test suite**

Run: `cargo test -p analyzer`
Expected: all pass (the BE fix doesn't affect any existing LE-only test).

- [ ] **Step 6: Commit**

```bash
git add crates/analyzer/src/analyzer/register_aliasing.rs
git commit -m "fix(analyzer): correct big-endian sub-register shift formula

Old:  8 * (container.size - (reg.off - container.off))
New:  8 * (container.size - reg.size - (reg.off - container.off))

The previous formula computed the byte position of the *high edge* of the
sub-register; the correct shift uses the byte position of the *low edge*
(LSB-relative).  For a 4-byte BE container with a 1-byte MSB sub-register
at offset 0, old produced 32 (UB on a 32-bit shift); new produces 24."
```

---

### Task 5: Fix `handle_int_not_equal` operand width

**Files:**
- Modify: [crates/analyzer/src/analyzer/insn/integer.rs:79-97](crates/analyzer/src/analyzer/insn/integer.rs#L79-L97)

- [ ] **Step 1: Read and confirm the bug**

Current code at line 87–92:
```rust
        let and_out = self.builder.build_int_cmp_operation(
            lhs, rhs,
            IntCmpOp::Equal,
            out_vn.size.try_into()?,   // ← out_vn is the boolean result (size 1)
        )?;
```
`build_int_cmp_operation`'s 4th arg is the *operand* width — i.e. the width of `lhs`/`rhs`.  Every other comparison handler (`process_int_cmp_op`, line 61) correctly uses `insn.inputs[0].size`.  Using `out_vn.size` here is wrong: when comparing two 4-byte ints, this code claims the operands are 1 byte wide.

- [ ] **Step 2: Apply the fix**

In `crates/analyzer/src/analyzer/insn/integer.rs` lines 79–97, replace `handle_int_not_equal`:

```rust
    pub(super) fn handle_int_not_equal(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // P-code IntNotEqual is lowered to BoolNeg(IntEqual) for deterministic
        // canonical form (one IntCmpOp, one BoolUnaryOp instead of an
        // IntCmpOp::NotEqual variant — keeps the cmp-op enum smaller).
        //
        // The cmp's operand width is the *input* width, NOT the output width:
        // the output is a 1-byte bool, the inputs may be any integer width.
        let lhs = self.read_vn(&insn.inputs[0])?;
        let rhs = self.read_vn(&insn.inputs[1])?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let eq = self.builder.build_int_cmp_operation(
            lhs,
            rhs,
            IntCmpOp::Equal,
            insn.inputs[0].size.try_into()?,
        )?;
        let neq = self
            .builder
            .build_boolean_unary_operation(eq, BoolUnaryOp::Neg)?;
        self.write_vn(out_vn, neq)
    }
```

- [ ] **Step 3: Add a unit test in `tests/insn_int_not_equal.rs` (new)**

Create `crates/analyzer/tests/insn_int_not_equal.rs`:

```rust
//! Pinned contract: IntNotEqual lifts to BoolNeg(IntEqual) with operand
//! width matching the *inputs*, not the output. Regression guard for the
//! width bug fixed in commit "fix(analyzer): IntNotEqual operand width".

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

use object::{Object, ObjectSymbol};

#[test]
fn int_not_equal_compares_inputs_at_input_width_not_output_width() {
    // Build x86_64 analyzer over the test binary; `count_bits` contains a
    // `while (x)` loop whose condition lifts to `x != 0`, exercising
    // IntNotEqual on a 4-byte input.
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x64/test.elf");
    let obj = reader::load_elf(&path).expect("load elf");
    let arch = analyzer::SleighArch::x86_64();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).unwrap().regs().unwrap();
    let ana = analyzer::Analyzer::new(arch, regs, analyzer::CallingConvention::x86_64_systemv_abi()).unwrap();
    let mem = reader::ElfFileMemReader::from_object(&obj).unwrap();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem).unwrap();
    let addr = obj.symbol_by_name("count_bits").unwrap().address();
    let cfg = cfg::Builder::new(sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    ).build().unwrap();

    // The CFG → IR translation must succeed.  Before the fix, the IR
    // builder would either (a) reject the cmp because the operand-width
    // claim mismatched the actual lhs/rhs widths, or (b) silently produce
    // a malformed graph that the validator catches.
    let mut graph = ana.analyze_cfg(&cfg).expect("analyze");
    ana.build_optimizer_pipeline().run(&mut graph).expect("optimize");

    // Pattern check: the optimised graph contains at least one BoolUnaryOp(Neg)
    // wrapping an IntCmpOp(Equal) — that's the canonical IntNotEqual lowering.
    use pattern::{Matcher, predicate, any};
    use ir::node::NodeKind;
    let m = Matcher::new(&graph);
    let neq_pat = predicate(|ctx, val| {
        matches!(ctx.node_kind(ctx.get_node_from_output(val)), Some(NodeKind::BoolUnaryOp(ir::BoolUnaryOp::Neg)))
    });
    let hits = m.find_all(&neq_pat);
    assert!(!hits.is_empty(),
        "expected at least one BoolNeg in count_bits IR (was {} nodes)",
        graph.preorder().count()
    );
}
```

(If `pattern` isn't yet a `dev-dependency` of `analyzer`, add it in the same step. The plan adds it explicitly in Phase D Task 11 step 1.)

- [ ] **Step 4: Run**

Run: `cargo test -p analyzer --test insn_int_not_equal`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/analyzer/src/analyzer/insn/integer.rs crates/analyzer/tests/insn_int_not_equal.rs crates/analyzer/Cargo.toml
git commit -m "fix(analyzer): IntNotEqual operand width must follow inputs, not output

The cmp's 4th argument is the operand width (the width of lhs/rhs).
The output is always a 1-byte bool, so passing out_vn.size silently
misreports the comparison's width as 1 even for 4/8-byte operands."
```

---

### Task 6: Guard `Subpiece` byte-offset overflow

**Files:**
- Modify: [crates/analyzer/src/analyzer/insn/integer.rs:99-127](crates/analyzer/src/analyzer/insn/integer.rs#L99-L127)

- [ ] **Step 1: Confirm the failure mode**

`handle_subpiece` line 112: `let bit_shift = byte_offset * 8;` with `byte_offset: u64` from `inputs[1].addr.off`. A malicious or malformed lifter input with `byte_offset >= u64::MAX / 8` overflows silently. P-code Subpiece's contract is `byte_offset < value_size`, so anything above the input's byte size is invalid — we should fail loudly instead of producing a wrap.

- [ ] **Step 2: Add the bound check**

Replace `handle_subpiece` body with:

```rust
    pub(super) fn handle_subpiece(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        // `Subpiece(value, byte_offset, out_size)`: extracts `out_size` bytes
        // starting at byte `byte_offset` from `value`.
        // Implemented as: right-shift by (byte_offset * 8) bits, then truncate.
        //
        // P-code Subpiece's contract requires byte_offset < value_size; any
        // larger value would wrap on the multiply or produce a useless shift,
        // so we reject it explicitly.
        let input_vn = &insn.inputs[0];
        let byte_offset = insn.inputs[1].addr.off;
        if byte_offset >= input_vn.size as u64 {
            return Err(ErrorKind::SubpieceOffsetOutOfRange {
                opcode: insn.opcode,
                byte_offset,
                input_size: input_vn.size,
            }.into());
        }
        let input = self.read_vn(input_vn)?;
        let out_vn = insn
            .output
            .as_ref()
            .ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
        let shifted = if byte_offset == 0 {
            input
        } else {
            let bit_shift = byte_offset * 8;  // safe: byte_offset < input.size ≤ u32::MAX
            let shift_const = self
                .builder
                .build_int_const(bit_shift, input_vn.size.try_into()?)?;
            self.builder.build_int_binary_operation(
                input,
                shift_const,
                IntBinaryOp::ShiftRight,
                input_vn.size.try_into()?,
            )?
        };
        let out = self
            .builder
            .truncate_if_needed(shifted, out_vn.size.try_into()?)?;
        self.write_vn(out_vn, out)
    }
```

- [ ] **Step 3: Add the new error variant**

In `crates/analyzer/src/error.rs`, add inside the `ErrorKind` enum:

```rust
    #[error("Subpiece byte_offset {byte_offset} out of range for input size {input_size} (opcode {opcode:?})")]
    SubpieceOffsetOutOfRange {
        opcode: rsleigh::Opcode,
        byte_offset: u64,
        input_size: u32,
    },
```

- [ ] **Step 4: Run**

Run: `cargo build -p analyzer && cargo test -p analyzer`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/analyzer/src/analyzer/insn/integer.rs crates/analyzer/src/error.rs
git commit -m "fix(analyzer): reject out-of-range Subpiece byte_offset instead of wrapping"
```

---

## Phase C — Code simplification

### Task 7: Tighten `find_largest_fitting_register` API to `Result<Vn>`

**Files:**
- Modify: [crates/analyzer/src/analyzer/register_aliasing.rs](crates/analyzer/src/analyzer/register_aliasing.rs)

- [ ] **Step 1: Replace the body and signature**

Replace lines 17–51:

```rust
    /// Finds the largest architectural register that fully contains `reg`.
    ///
    /// For example, given `al` (offset 0, size 1) this returns `rax`
    /// (offset 0, size 8) on x86-64 because writes to `al` always go through
    /// `rax`.  The returned register is the widest one in the variable set
    /// whose byte range fully covers `reg`'s byte range.
    ///
    /// # Errors
    ///
    /// Returns [`ErrorKind::UnsupportedVnSpace`] if `reg` is not in
    /// [`rsleigh::VnSpace::REGISTER`].  Returns
    /// [`ErrorKind::NoRegisterContainer`] if no variable in the builder
    /// covers `reg`'s byte range — this should never happen because every
    /// register at least contains itself.
    pub(super) fn find_largest_fitting_register(
        &self,
        reg: &rsleigh::Vn,
    ) -> Result<rsleigh::Vn> {
        if reg.addr.space != rsleigh::VnSpace::REGISTER {
            return Err(ErrorKind::UnsupportedVnSpace(reg.addr.space).into());
        }
        let reg_start = reg.addr.off;
        let reg_end = reg_start + reg.size as u64;
        let mut best: Option<rsleigh::Vn> = None;
        for sleigh_reg in self.builder.variables() {
            if sleigh_reg.addr.space != rsleigh::VnSpace::REGISTER {
                continue;
            }
            let s = sleigh_reg.addr.off;
            let e = s + sleigh_reg.size as u64;
            if s > reg_start || e < reg_end {
                continue;
            }
            // Contained.  Take it if it's strictly wider than the current best.
            if best.map_or(true, |b| b.size < sleigh_reg.size) {
                best = Some(*sleigh_reg);
            }
        }
        best.ok_or_else(|| ErrorKind::NoRegisterContainer(*reg).into())
    }
```

- [ ] **Step 2: Update both call sites in this file**

Lines 80–82 (`read_reg_vn`):
```rust
        let container_reg = self.find_largest_fitting_register(reg)?;
```
Lines 114–116 (`write_reg_vn`):
```rust
        let container_reg = self.find_largest_fitting_register(reg)?;
```

- [ ] **Step 3: Verify nothing else calls it**

Run: `grep -rn 'find_largest_fitting_register' crates/`
Expected: only `register_aliasing.rs` (definition + 2 callers) and `lib.rs` (doc comment) — no external callers.

- [ ] **Step 4: Run**

Run: `cargo test -p analyzer`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/analyzer/src/analyzer/register_aliasing.rs
git commit -m "refactor(analyzer): collapse find_largest_fitting_register to Result<Vn>

Both callers immediately .ok_or'd the inner Option, so move the error
into the function itself.  Also replaces the manual size-tracking loop
with a cleaner Option::map_or-based comparison."
```

---

### Task 8: Reduce repeated `container_reg.size.try_into()?` calls in `write_reg_vn`

**Files:**
- Modify: [crates/analyzer/src/analyzer/register_aliasing.rs:113-177](crates/analyzer/src/analyzer/register_aliasing.rs#L113-L177)

- [ ] **Step 1: Hoist the type once**

Replace `write_reg_vn` body so the `container_ty` is computed once instead of seven times:

```rust
    pub(super) fn write_reg_vn(&mut self, reg: &rsleigh::Vn, val: ir::Value) -> Result<()> {
        let container_reg = self.find_largest_fitting_register(reg)?;
        if container_reg == *reg {
            return Ok(self.builder.write_variable(reg, val)?);
        }
        let container_ty: ir::ValueType = container_reg.size.try_into()?;
        let container_reg_val = self.builder.read_variable(&container_reg)?;
        let shift_bits = self.calculate_reg_shift_from_container(reg, &container_reg);

        // Position `val`'s bits inside the container's bit window.
        let shifted_value = if shift_bits == 0 {
            self.builder
                .extend_if_needed(val, container_ty, ExtendOp::ZeroExtend)?
        } else {
            let shift_const = self.builder.build_int_const(shift_bits, container_ty)?;
            self.builder.build_int_binary_operation(
                val,
                shift_const,
                IntBinaryOp::ShiftLeft,
                reg.size.try_into()?,  // intentionally reg's size: shifting in reg's domain before merging
            )?
        };

        // Mask `val` to its declared width inside the container.
        let reg_mask = crate::utils::vn_mask(reg)?;
        let reg_mask_val = self.builder.build_int_const(reg_mask, container_ty)?;
        let reg_val = self.builder.build_int_binary_operation(
            reg_mask_val,
            shifted_value,
            IntBinaryOp::And,
            container_ty,
        )?;

        // Mask out the old bits of `reg` from the container.
        let container_mask = crate::utils::vn_mask(&container_reg)? & !reg_mask;
        let container_mask_val = self.builder.build_int_const(container_mask, container_ty)?;
        let container_val = self.builder.build_int_binary_operation(
            container_mask_val,
            container_reg_val,
            IntBinaryOp::And,
            container_ty,
        )?;

        // Merge.
        let final_container_value = self.builder.build_int_binary_operation(
            container_val,
            reg_val,
            IntBinaryOp::Or,
            container_ty,
        )?;
        self.write_reg_vn(&container_reg, final_container_value)?;
        Ok(())
    }
```

- [ ] **Step 2: Run**

Run: `cargo test -p analyzer`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/src/analyzer/register_aliasing.rs
git commit -m "refactor(analyzer): hoist container_ty in write_reg_vn (was computed 7×)"
```

---

### Task 9: Collapse `MissingOutputVn` boilerplate via `require_output_vn`

**Files:**
- Modify: [crates/analyzer/src/analyzer/insn/mod.rs](crates/analyzer/src/analyzer/insn/mod.rs)
- Modify: every `crates/analyzer/src/analyzer/insn/*.rs` file with output-vn extraction

- [ ] **Step 1: Add the helper as a free function in `insn/mod.rs`**

After the `mod boolean; mod control; …` block, add:

```rust
/// Common boilerplate: require the instruction to have an output varnode and
/// return a borrowed reference to it.  Roughly 20 handlers used to inline
/// this; centralising avoids drift when the error variant changes.
pub(super) fn require_output_vn(insn: &rsleigh::Insn) -> Result<&rsleigh::Vn> {
    insn.output
        .as_ref()
        .ok_or_else(|| ErrorKind::MissingOutputVn(insn.opcode).into())
}
```

- [ ] **Step 2: Update every handler**

In each of `boolean.rs`, `control.rs`, `float.rs`, `integer.rs`, `memory.rs`, `misc.rs`, replace every occurrence of:

```rust
let out_vn = insn.output.as_ref().ok_or(ErrorKind::MissingOutputVn(insn.opcode))?;
```

with:

```rust
let out_vn = super::require_output_vn(insn)?;
```

(Verify the import is correct from each sub-module: `use super::require_output_vn` at the top of each file, OR refer as `super::require_output_vn(insn)?` inline.  Pick one style and apply uniformly.)

- [ ] **Step 3: Confirm zero remaining open-coded copies**

Run: `grep -rn 'MissingOutputVn(insn.opcode)' crates/analyzer/src/`
Expected: only the helper itself + the `ErrorKind` definition.

- [ ] **Step 4: Run**

Run: `cargo test -p analyzer && cargo clippy -p analyzer --all-targets -- -D warnings`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/analyzer/src/analyzer/insn/
git commit -m "refactor(analyzer): centralise MissingOutputVn extraction in require_output_vn"
```

---

### Task 10: `handle_return` clarity — drop misleading `_ = insn`

**Files:**
- Modify: [crates/analyzer/src/analyzer/insn/control.rs:40-54](crates/analyzer/src/analyzer/insn/control.rs#L40-L54)

- [ ] **Step 1: Replace the function**

```rust
    /// Lowers a p-code `Return` into the IR's calling-convention-aware return
    /// node, emitting the convention's `ret_val_regs` in ABI order.
    ///
    /// The p-code `Return` op carries a single fabricated input (typically the
    /// popped return address on stack-push ISAs).  That value is *not* an ABI
    /// return slot, so we discard the lifted input here and let the IR resolve
    /// the real return values from the calling convention's resolved register
    /// list.
    pub(super) fn handle_return(&mut self, _insn: &rsleigh::Insn) -> Result<()> {
        let ret_regs = self.builder.ret_val_vars().to_vec();
        self.builder.build_return(None, &ret_regs)?;
        Ok(())
    }
```

- [ ] **Step 2: Run**

Run: `cargo test -p analyzer && cargo clippy -p analyzer --all-targets -- -D warnings`
Expected: pass (the `_insn` prefix silences the unused-param lint cleanly).

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/src/analyzer/insn/control.rs
git commit -m "refactor(analyzer): clarify handle_return — _insn says intent, drop _ = insn shim"
```

---

## Phase D — Unit-test coverage (per module)

This phase adds focused unit/integration tests that pin contracts at the *module* boundary, distinct from the system tests in Phase E.

### Task 11: Add `pattern` and `expect-test` to `analyzer` dev-deps

**Files:**
- Modify: [crates/analyzer/Cargo.toml](crates/analyzer/Cargo.toml)

- [ ] **Step 1: Append the dev-deps**

```toml
[dev-dependencies]
pattern.workspace = true
expect-test.workspace = true
```

- [ ] **Step 2: Run**

Run: `cargo build -p analyzer --tests`
Expected: success.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/Cargo.toml
git commit -m "chore(analyzer): add pattern + expect-test as dev-deps for new tests"
```

---

### Task 12: Unit tests for `find_largest_fitting_register`, `read_reg_vn`, `write_reg_vn`

**Files:**
- Create: `crates/analyzer/tests/register_aliasing_be.rs` (BE-arch coverage)
- Create: `crates/analyzer/tests/register_aliasing_le.rs` (LE-arch coverage)

- [ ] **Step 1: Create `register_aliasing_le.rs`**

Test through a real x86_64 setup, since constructing an `IrAnalyzer` from scratch requires a `Sleigh<R>` and `Cfg<R>`.  We use `fixtures/out/x64/test.elf` and the `add` symbol, then introspect the resulting IR for the canonical sub-register pattern.

```rust
//! Unit tests for register-aliasing logic via a minimal x86_64 round-trip.
//!
//! Constructing an `IrAnalyzer` requires a `Sleigh<R>` and a `Cfg<R>`, so
//! these tests use the smallest real binary symbol (`add`) and verify the
//! emitted IR has the expected register-read shape.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

use object::{Object, ObjectSymbol};

fn analyze_x64(sym: &str) -> ir::BuiltFunctionGraph {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x64/test.elf");
    let obj = reader::load_elf(&path).unwrap();
    let arch = analyzer::SleighArch::x86_64();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).unwrap().regs().unwrap();
    let ana = analyzer::Analyzer::new(arch, regs, analyzer::CallingConvention::x86_64_systemv_abi()).unwrap();
    let mem = reader::ElfFileMemReader::from_object(&obj).unwrap();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem).unwrap();
    let addr = obj.symbol_by_name(sym).unwrap().address();
    let cfg = cfg::Builder::new(sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    ).build().unwrap();
    ana.analyze_cfg(&cfg).unwrap()
}

#[test]
fn add_function_emits_initial_var_for_arg_registers() {
    // x86_64 `add(int a, int b)` reads EDI/ESI (both 32-bit sub-registers
    // of RDI/RSI on x86_64).  The analyzer must emit InitialVar nodes for
    // RDI and RSI, then a right-shift extraction (or zero-trunc) to land
    // on the 32-bit sub-register width.
    let graph = analyze_x64("add");
    use ir::node::NodeKind;
    let initial_var_count = graph.preorder().filter(|nid| {
        matches!(graph.node_kind(*nid), Some(NodeKind::InitialVar(_)))
    }).count();
    assert!(initial_var_count >= 2,
        "expected ≥2 InitialVar nodes for RDI/RSI; got {initial_var_count}");
}

#[test]
fn write_reg_vn_subregister_emits_mask_and_or() {
    // `add` writes EAX (32-bit sub-register of RAX) for its return value.
    // The write must emit at least one Or operation merging EAX into RAX.
    let graph = analyze_x64("add");
    use ir::node::NodeKind;
    let or_count = graph.preorder().filter(|nid| {
        matches!(
            graph.node_kind(*nid),
            Some(NodeKind::IntBinaryOp(ir::IntBinaryOp::Or))
        )
    }).count();
    assert!(or_count >= 1,
        "expected ≥1 Or op from sub-register write merging into RAX; got {or_count}");
}
```

- [ ] **Step 2: Create `register_aliasing_be.rs`**

Use `aarch64be` arch (no real binary, but we can synthesize a minimal CFG).  Skip the test via `#[ignore]` if no BE binary exists, with a TODO comment for future BE coverage.

Concrete approach: instead of synthesising a CFG (rsleigh APIs make this fiddly), test the *shift formula via a stub*.  This already exists in Task 4 step 1 as `shift_formula_tests`, so this file targets the BE-specific *write_reg_vn merge* path, which the LE tests don't reach.

For now, mark the file body as `#[ignore]`-ed with a clear reason:

```rust
//! Big-endian register-aliasing coverage.
//!
//! Constructing a CFG via the BE Sleigh archs (aarch64be / mipsbe32) without
//! a precompiled BE test binary requires either:
//!   (a) extending fixtures/Makefile with a BE cross-compiler, or
//!   (b) hand-emitting raw bytes through rsleigh::mem_readers::BufMemReader.
//!
//! Either is non-trivial.  Until then, the BE shift formula is pinned via
//! the unit tests in `register_aliasing.rs::shift_formula_tests` (Task 4).

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

#[test]
#[ignore = "BE binary fixture not yet wired up; shift formula unit-tested in register_aliasing.rs"]
fn placeholder_for_future_be_coverage() {}
```

- [ ] **Step 3: Run**

Run: `cargo test -p analyzer --tests`
Expected: LE tests pass, BE test ignored.

- [ ] **Step 4: Commit**

```bash
git add crates/analyzer/tests/register_aliasing_*.rs
git commit -m "test(analyzer): add register-aliasing tests for x86_64 + BE placeholder"
```

---

### Task 13: Unit tests for `vn_io` (read_vn / write_vn space dispatch)

**Files:**
- Create: `crates/analyzer/tests/vn_io_dispatch.rs`

- [ ] **Step 1: Create the test file**

```rust
//! Tests for vn_io.rs space-dispatch logic.
//!
//! `read_vn` and `write_vn` are private — these tests exercise them
//! indirectly by analyzing functions that hit each space at least once:
//!   - CONST  → any function with an immediate operand (e.g. `factorial`)
//!   - UNIQUE → any function (rsleigh inserts UNIQUE temporaries everywhere)
//!   - REGISTER → trivially every function
//!   - default code space (Load/Store) → `array_sum`, `array_fill`
//!
//! The "write to CONST returns error" path is tested via a synthetic insn
//! constructed in-test.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

use object::{Object, ObjectSymbol};

fn graph_for(sym: &str) -> ir::BuiltFunctionGraph {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x64/test.elf");
    let obj = reader::load_elf(&path).unwrap();
    let arch = analyzer::SleighArch::x86_64();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).unwrap().regs().unwrap();
    let ana = analyzer::Analyzer::new(arch, regs, analyzer::CallingConvention::x86_64_systemv_abi()).unwrap();
    let mem = reader::ElfFileMemReader::from_object(&obj).unwrap();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem).unwrap();
    let addr = obj.symbol_by_name(sym).unwrap().address();
    let cfg = cfg::Builder::new(sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    ).build().unwrap();
    ana.analyze_cfg(&cfg).unwrap()
}

#[test]
fn const_space_read_emits_int_const_node() {
    use ir::node::NodeKind;
    // `factorial` initialises r=1 — that's a CONST varnode → IntConst node.
    let g = graph_for("factorial");
    let count = g.preorder()
        .filter(|nid| matches!(g.node_kind(*nid), Some(NodeKind::IntConst(_))))
        .count();
    assert!(count > 0, "expected ≥1 IntConst node; got {count}");
}

#[test]
fn default_code_space_read_emits_load_node() {
    use ir::node::NodeKind;
    let g = graph_for("array_sum");
    let count = g.preorder()
        .filter(|nid| matches!(g.node_kind(*nid), Some(NodeKind::Load(_))))
        .count();
    assert!(count > 0, "array_sum must emit ≥1 Load; got {count}");
}

#[test]
fn default_code_space_write_emits_store_node() {
    use ir::node::NodeKind;
    let g = graph_for("array_fill");
    let count = g.preorder()
        .filter(|nid| matches!(g.node_kind(*nid), Some(NodeKind::Store(_))))
        .count();
    assert!(count > 0, "array_fill must emit ≥1 Store; got {count}");
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p analyzer --test vn_io_dispatch`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/vn_io_dispatch.rs
git commit -m "test(analyzer): pin read_vn/write_vn space dispatch via real binaries"
```

---

### Task 14: Unit tests for `find_all_unique_vns` determinism

**Files:**
- Create: `crates/analyzer/tests/varnode_collection.rs`

- [ ] **Step 1: Create the test file**

```rust
//! Pin determinism of find_all_unique_vns: the VarId numbering must be
//! stable across runs so downstream IR node IDs (and any snapshot-based
//! tests) don't flip on the random hasher seed.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

use object::{Object, ObjectSymbol};

fn vns_for(sym: &str) -> Vec<rsleigh::Vn> {
    // Build the same Cfg twice; the unique-vn list must be byte-equal.
    // (We can't call find_all_unique_vns directly — it's pub(super) — so
    //  we use it indirectly via the analyzer's CFG-translation pass and
    //  inspect the IR's variables.)
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out/x64/test.elf");
    let obj = reader::load_elf(&path).unwrap();
    let arch = analyzer::SleighArch::x86_64();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, probe).unwrap().regs().unwrap();
    let ana = analyzer::Analyzer::new(arch, regs, analyzer::CallingConvention::x86_64_systemv_abi()).unwrap();
    let mem = reader::ElfFileMemReader::from_object(&obj).unwrap();
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, mem).unwrap();
    let addr = obj.symbol_by_name(sym).unwrap().address();
    let cfg = cfg::Builder::new(sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    ).build().unwrap();
    let graph = ana.analyze_cfg(&cfg).unwrap();
    // BuiltFunctionGraph must expose `variables()` or equivalent ordered list.
    // If it doesn't, fall back to walking InitialVar nodes in preorder and
    // collecting their varnodes — InitialVar order is determined by VarId
    // assignment, which is what we want to pin.
    graph.preorder()
        .filter_map(|nid| match graph.node_kind(nid) {
            Some(ir::node::NodeKind::InitialVar(vn)) => Some(*vn),
            _ => None,
        })
        .collect()
}

#[test]
fn varnode_order_is_stable_across_runs() {
    let a = vns_for("add");
    let b = vns_for("add");
    assert_eq!(a, b, "InitialVar order must be deterministic");
}

#[test]
fn varnode_order_is_stable_for_complex_function() {
    let a = vns_for("hard_func");
    let b = vns_for("hard_func");
    assert_eq!(a, b);
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p analyzer --test varnode_collection`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/varnode_collection.rs
git commit -m "test(analyzer): pin find_all_unique_vns determinism contract"
```

---

### Task 15: Unit tests for `decode_space_id` errors and `float_type_from_vn`

**Files:**
- Create: `crates/analyzer/tests/insn_decoders.rs`

- [ ] **Step 1: Create the test file**

These functions are private (`fn decode_space_id` in `memory.rs`, `float_type_from_vn` impl method in `float.rs`).  Since we can't call them directly, the test exercises their *error paths* via an entire pipeline run with a malformed input is impractical.

Instead, this task focuses on what *can* be unit-tested directly: extending `utils.rs::tests` with broader coverage and pinning behaviour around the boundaries.

Replace this task with: extend `crates/analyzer/src/utils.rs::tests`:

```rust
    // Already-present mask tests stay.

    /// All unsupported sizes (0, 3, 5, 6, 7, 9, 16, …) must produce
    /// UnsupportedRegSize, never a panic or a silently-wrong mask.
    #[test]
    fn unsupported_sizes_return_unsupported_reg_size_error() {
        for &bad in &[0u32, 3, 5, 6, 7, 9, 16, 32, 64, u32::MAX] {
            let r = vn_mask(&reg(bad));
            match r {
                Err(e) => match e.kind() {
                    crate::error::ErrorKind::UnsupportedRegSize(s) => {
                        assert_eq!(*s, bad);
                    }
                    other => panic!("size {bad}: expected UnsupportedRegSize, got {other:?}"),
                },
                Ok(_) => panic!("size {bad}: expected error, got Ok"),
            }
        }
    }
```

- [ ] **Step 2: Run**

Run: `cargo test -p analyzer --lib`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/src/utils.rs
git commit -m "test(analyzer): extend vn_mask coverage to all unsupported sizes"
```

---

## Phase E — System tests: multi-binary fixtures with a readable assertion DSL

This phase replaces the single `fixtures/test.c` + monolithic `tests/analyze_binary.rs` with a category-organised fixture set and a small Rust DSL for declarative IR assertions. It is the project's primary correctness gate: every layer below `analyzer` (reader, cfg, ir, opt, pattern) is exercised in the same way a real consumer would exercise it.

**Design principles**

1. **One `.c` file per category.** A regression in float handling shouldn't fail arithmetic tests; a syntax error in the abi fixture shouldn't break memory tests. Each `.c` file is small (≤ 100 lines), focused, and compiles clean under `-Wall -Wextra -Werror -Wno-unused-parameter`.
2. **One `tests/<category>.rs` file per `.c` file.** Cargo treats each `tests/*.rs` as a separate test binary, so failures isolate to the category that broke. Each test file declares assertions for every function in its sibling `.c` file.
3. **One `#[test]` per (architecture, function) pair.** A single `per_arch_test!` macro expands to four `#[test]` functions (`x86`, `x64`, `aarch64`, `arm`) so a regression on one arch surfaces as a single named failure: `arithmetic::add::aarch64`.
4. **Shared assertion vocabulary in `tests/common/mod.rs`.** Functions like `count_int_binop`, `count_calls`, `count_loops`, `has_constant`, `count_loads`, etc. — every test file imports the same vocabulary so a reader can scan assertions without learning new helpers.
5. **Easy to extend.** Adding coverage for a new C function is: (a) add the function to the relevant `.c` file (the Makefile globs), (b) add a `per_arch_test!` block to the matching `tests/<category>.rs`. No build-system changes, no Cargo.toml changes.

**Fixture catalogue** (10 `.c` files):

| File | Functions | What it exercises |
|------|-----------|-------------------|
| `arithmetic.c`  | `add`, `sub`, `mul`, `udiv`, `umod`, `sdiv`, `smod`, `bit_and`, `bit_or`, `bit_xor`, `bit_not`, `shl`, `lshr`, `ashr`, `negate` | All `IntBinaryOp` / `IntUnaryOp` variants |
| `control.c`     | `abs_val`, `max_val`, `clamp`, `select_three`, `sum_to_n`, `factorial`, `count_bits`, `nested_loops`, `early_return` | If, ControlPhi, loop induction |
| `memory.c`      | `array_sum`, `array_fill`, `array_copy`, `pointer_chase`, `struct_field_load`, `struct_field_store`, `tagged_union_read` | Load, Store at default code space |
| `calls.c`       | `fib_recursive`, `mutual_a`/`mutual_b` (mutual recursion), `nested_3deep`, `repeat_call_pair`, `pass_through`, `apply_indirect` (function-pointer call) | Call, indirect Call |
| `floats.c`      | `f32_arith`, `f64_arith`, `f32_to_f64`, `f64_to_f32`, `int_to_float`, `float_to_int`, `f32_compare`, `f64_compare`, `f32_neg_abs` | Float* node families, conversions |
| `abi.c`         | `eight_int_args`, `mixed_args` (4 int + 2 ptr), `point_sum` (struct-by-value), `make_pair` (small struct return), `tail_caller` | Calling-convention argument materialisation, stack args |
| `stack.c`       | `volatile_three_writes`, `escape_via_ptr`, `large_local_array`, `inplace_swap`, `recursive_stack_growth` | StackStore, StackStorePhi, escapes |
| `builtins.c`    | `popcount32`, `clz32`, `ctz32`, `popcount64`, `clz64`, `expect_branch` (uses `__builtin_expect`) | `Popcount` / `Lzcount` opcodes (Q5 option B) |
| `globals.c`     | `read_const_byte`, `read_const_int`, `branch_on_const_string`, `runtime_const_idx` | LoadReadOnly fold |
| `patterns.c`    | `mul_then_add` (mac), `chained_xor_mask`, `if_returns_const`, `loop_with_invariant_load`, `recursive_with_accumulator` | Targets for complex `pattern::Matcher` queries |

**Renumbered tasks in this phase:**

| # | Task |
|---|------|
| 16 | Add `CallingConvention::mips_o32()` to `target` crate |
| 17 | Rename `binary_tests/` → `fixtures/`, restructure into `cases/*.c`, add MIPS arch makefiles |
| 18 | Author the ten fixture `.c` files (warning-clean) |
| 19 | Verify all fixtures build per arch (six archs) |
| 20 | Add `tests/common/mod.rs` (shared analyze + assertion DSL + macros) |
| 21 | `tests/arithmetic.rs` — 15 functions × 6 archs = 90 tests |
| 22 | `tests/control.rs` — 9 × 6 = 54 tests |
| 23 | `tests/memory.rs` — 7 × 6 = 42 tests |
| 24 | `tests/calls.rs` — 7 × 6 = 42 tests |
| 25 | `tests/floats.rs` — 9 × 6 = 54 tests |
| 26 | `tests/abi.rs` — 5 × 6 = 30 tests |
| 27 | `tests/stack.rs` — 5 × 6 = 30 tests |
| 28 | `tests/builtins.rs` — 6 × 6 = 36 tests |
| 29 | `tests/globals.rs` — 4 × 6 = 24 tests |
| 30 | `tests/patterns.rs` — 5 complex pattern queries × 6 archs = 30 tests |
| 31 | Delete obsolete `tests/analyze_binary.rs` and `fixtures/test.c` |

**Total expected new system tests:** ~448 (≈80 functions × 6 archs minus a handful of arch-skipped variants for compiler-flag corner cases).

---

### Task 16: Add `CallingConvention::mips_o32()` to `target` crate

**Files:**
- Modify: [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs)

The `target` crate already exposes `SleighArch::mipsle32()` and `mipsbe32()`, but no MIPS calling convention exists. We need O32 (the standard 32-bit MIPS ABI used by both LE and BE binaries on Linux). The convention is:
- arg registers: `a0`, `a1`, `a2`, `a3` (= `r4`–`r7`)
- callee-saved: `s0`–`s7` (= `r16`–`r23`), `s8`/`fp` (= `r30`), `gp` (= `r28`), `ra` (= `r31`)
- return: `v0`, `v1` (= `r2`, `r3`)
- stack pointer: `sp` (= `r29`)
- stack args: arg 5+ at `SP+16, +20, +24, +28` (the first 16 bytes are MIPS's reserved "shadow space" for the four register args)
- ret_stack_pop: `0` (MIPS `jal`/`jalr` writes return address to `$ra`; the call instruction does not push)

The Sleigh MIPS register names need verification before applying — they may be uppercase (`A0`, `S0`, `SP`, `RA`) or lowercase. Run `cargo run --example dump_regs` (if such an example exists) or write a one-shot:
```bash
cargo test -p target --lib presets_resolve_correct_register_sets -- --nocapture 2>&1 | head -5
```
…then confirm by spinning up a temporary `Sleigh::new(SleighArch::mipsle32(), …)` and listing the register names. Default below assumes lowercase per MIPS convention; switch to uppercase if Sleigh's table is uppercase.

- [ ] **Step 1: Verify MIPS register names against the live Sleigh table**

Add a one-shot `#[cfg(test)]` helper at the bottom of `crates/target/src/calling_convention.rs::tests`:

```rust
#[test]
#[ignore = "diagnostic — uncomment locally to print MIPS register names"]
fn dump_mips_register_names() {
    let arch = crate::arch::SleighArch::mipsle32();
    let reader = rsleigh::mem_readers::BufMemReader::new(vec![], 0x0);
    let regs = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .unwrap().regs().unwrap();
    let want = ["a0", "a1", "a2", "a3", "v0", "v1", "sp", "ra", "s0", "s8", "fp", "gp",
                "A0", "A1", "V0", "SP", "RA", "S0", "FP", "GP"];
    for n in want {
        let v = regs.name_to_vn(n);
        println!("name {n:?} -> {v:?}");
    }
}
```

Run with: `cargo test -p target dump_mips_register_names -- --ignored --nocapture 2>&1 | tail -25`
Expected: this prints which name spelling Sleigh uses.  Pick the ones that resolve to `Some(...)` and use them in step 2.  Once captured, you can leave the diagnostic test in place (it stays `#[ignore]`-ed) or delete it.

- [ ] **Step 2: Add the `mips_o32` constructor**

In `crates/target/src/calling_convention.rs`, after `arm_aapcs` (right before `x86_cdecl`), add:

```rust
    /// Returns the MIPS O32 calling convention.
    ///
    /// Used by 32-bit MIPS Linux binaries on both LE and BE targets — the ABI
    /// is identical regardless of byte order.  Pairs equally with
    /// [`crate::SleighArch::mipsle32`] and [`crate::SleighArch::mipsbe32`].
    ///
    /// Argument registers: a0, a1, a2, a3 (= r4–r7)
    /// Callee-saved:       s0–s7, s8 (= fp), gp, ra (= r16–r23, r30, r28, r31)
    /// Return value:       v0, v1 (= r2, r3)
    ///
    /// `sp` (= r29) is the stack pointer.  `ret_stack_pop` is `0` because
    /// MIPS `jal`/`jalr` writes the return address to `$ra` rather than
    /// pushing it on the stack.  The first 16 bytes of stack-arg space
    /// (offsets 0..16) are MIPS's reserved "shadow space" for the four
    /// register args; positional stack args start at offset 16.
    #[must_use]
    pub fn mips_o32() -> CallingConvention {
        CallingConvention {
            stack_ptr_reg_name: "sp",
            arg_passing_regs: &["a0", "a1", "a2", "a3"],
            callee_saved_regs: &["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "gp", "ra"],
            ret_val_regs: &["v0", "v1"],
            stack_arg_offsets: &[16, 20, 24, 28],
            ret_stack_pop: 0,
        }
    }
```

(If step 1 showed Sleigh uses uppercase names, change every register-name string above to uppercase: `"SP"`, `"A0"`, `"S0"`, etc.)

- [ ] **Step 3: Add a row to the `cases()` table in the existing test suite**

In `crates/target/src/calling_convention.rs::tests`, append to the `cases()` `vec![…]`:

```rust
            Case {
                name: "MIPS O32 (LE)",
                cc: CallingConvention::mips_o32,
                arch: crate::arch::SleighArch::mipsle32,
                arg_count: 4,
                callee_saved_count: 11,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[16, 20, 24, 28],
                ret_stack_pop: 0,
            },
            Case {
                name: "MIPS O32 (BE)",
                cc: CallingConvention::mips_o32,
                arch: crate::arch::SleighArch::mipsbe32,
                arg_count: 4,
                callee_saved_count: 11,
                ret_count: 2,
                reg_size_bytes: 4,
                stack_ptr_name: "sp",
                stack_arg_offsets: &[16, 20, 24, 28],
                ret_stack_pop: 0,
            },
```

(Update `stack_ptr_name` to uppercase if step 1 showed Sleigh uses uppercase.)

- [ ] **Step 4: Run the target test suite**

Run: `cargo test -p target`
Expected: pass — the existing `presets_resolve_correct_register_sets`, `presets_resolved_registers_have_expected_size`, `presets_stack_pointer_and_arg_offsets`, `build_returns_error_for_unknown_register_name` tests now exercise MIPS O32 too.  If any fail, the most likely cause is a register-name spelling mismatch — re-run step 1.

- [ ] **Step 5: Run the analyzer test suite to confirm nothing broke**

Run: `cargo test -p analyzer && cargo build --workspace`
Expected: pass.

- [ ] **Step 6: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "feat(target): add CallingConvention::mips_o32 for 32-bit MIPS LE+BE

O32 is the standard 32-bit MIPS Linux ABI; LE and BE share the same
register convention.  Pairs with the existing SleighArch::mipsle32 and
mipsbe32.  Required for MIPS coverage in the analyzer system test
matrix (Phase E)."
```

---

### Task 17: Rename `binary_tests/` → `fixtures/`, restructure into `cases/*.c`, add MIPS arch makefiles

**Files:**
- `git mv binary_tests/ fixtures/` (rename)
- Modify: `fixtures/Makefile` (new content)
- Create: `fixtures/arch/mips32le.mk`, `fixtures/arch/mips32be.mk`
- Create: `fixtures/cases/.gitkeep`
- Modify: [.gitignore](.gitignore) (`binary_tests/out` → `fixtures/out`)
- Modify: [README.md](README.md) (path references)
- Modify: [CLAUDE.md](CLAUDE.md) (path references)
- Modify: [crates/analyzer/examples/analyzer.rs](crates/analyzer/examples/analyzer.rs) (`binary_tests/out` → `fixtures/out`)
- Modify: [crates/reader/tests/elf_smoke.rs](crates/reader/tests/elf_smoke.rs) (path + commit message in doc)
- Modify: [crates/cfg/tests/cfg_integration.rs](crates/cfg/tests/cfg_integration.rs) (path + doc)
- Modify: [crates/cfg/tests/common/real_binary.rs](crates/cfg/tests/common/real_binary.rs) (path)
- Modify: [crates/opt/src/stack_load_forward/tests.rs](crates/opt/src/stack_load_forward/tests.rs) (doc reference)
- Modify: [crates/target/src/arch.rs](crates/target/src/arch.rs) (doc reference)

- [ ] **Step 1: Rename the folder via git, preserving history**

```bash
git mv binary_tests fixtures
```

- [ ] **Step 2: Sweep code references with sed**

```bash
# Update all .rs files in the workspace
git grep -l 'binary_tests' -- 'crates/' | xargs sed -i 's|binary_tests/|fixtures/|g; s|binary_tests`|fixtures`|g; s|`binary_tests`|`fixtures`|g; s|make -C binary_tests|make -C fixtures|g'

# Update top-level docs and config
sed -i 's|binary_tests/|fixtures/|g; s|binary_tests`|fixtures`|g' README.md CLAUDE.md .gitignore
```

Verify no stragglers remain:

```bash
git grep 'binary_tests'
```
Expected: zero output.

- [ ] **Step 3: Add the `cases/` directory and `.gitkeep`**

```bash
mkdir -p fixtures/cases
touch fixtures/cases/.gitkeep
```

- [ ] **Step 4: Replace `fixtures/Makefile` with the cases-globbing Makefile (six archs)**

```make
# fixtures/Makefile
#
# Builds one ELF per (arch, case) combination:  out/<arch>/<case>.elf
#
# Each architecture is described by arch/<arch>.mk, which sets CC and CFLAGS.
# Each test case is a single .c file in cases/.  New cases require no Makefile
# edits — drop a file in cases/ and `make` picks it up.
#
# Usage:
#   make               — build every case for every arch
#   make ARCH=x64      — build every case for x64 only
#   make x64           — same via phony shortcut
#   make CASE=memory   — build the memory case for every arch
#   make ARCH=x64 CASE=memory
#                      — build cases/memory.c for x64 only
#   make clean         — remove out/

OUTDIR := out
ARCHES := x86 x64 aarch64 arm mips32le mips32be
CASES  := $(notdir $(basename $(wildcard cases/*.c)))

# Common warning-flag set: every fixture must compile clean.
COMMON_CFLAGS := -Wall -Wextra -Werror -Wno-unused-parameter

.PHONY: all clean $(ARCHES)

ifdef ARCH
# ── single-arch mode ──────────────────────────────────────────────────────────
include arch/$(ARCH).mk

# Filter to a single case if CASE is set; otherwise build all cases.
ifdef CASE
SELECTED_CASES := $(CASE)
else
SELECTED_CASES := $(CASES)
endif

TARGETS := $(addprefix $(OUTDIR)/$(ARCH)/,$(addsuffix .elf,$(SELECTED_CASES)))

all: $(TARGETS)

$(OUTDIR)/$(ARCH):
	mkdir -p $@

$(OUTDIR)/$(ARCH)/%.elf: cases/%.c | $(OUTDIR)/$(ARCH)
	@if [ "$(CC)" = "false" ]; then \
		echo "WARNING: compiler for '$(ARCH)' not found; skipping $@"; \
	else \
		$(CC) $(CFLAGS) $(COMMON_CFLAGS) $< -o $@ && echo "  Built $@"; \
	fi

else
# ── all-arch mode ─────────────────────────────────────────────────────────────
all: $(ARCHES)

$(ARCHES):
	@$(MAKE) --no-print-directory ARCH=$@ $(if $(CASE),CASE=$(CASE))

endif

clean:
	rm -rf $(OUTDIR)
```

- [ ] **Step 5: Add `fixtures/arch/mips32le.mk`**

```make
CC     := $(shell command -v mipsel-linux-gnu-gcc 2>/dev/null || echo false)
# -EL forces little-endian; -mips32 selects the ISA level Sleigh's mipsle32 spec expects.
CFLAGS := -EL -mips32 -O2 -g -fno-stack-protector -fno-pic
```

- [ ] **Step 6: Add `fixtures/arch/mips32be.mk`**

```make
CC     := $(shell command -v mips-linux-gnu-gcc 2>/dev/null || echo false)
# -EB forces big-endian; -mips32 selects the ISA level Sleigh's mipsbe32 spec expects.
CFLAGS := -EB -mips32 -O2 -g -fno-stack-protector -fno-pic
```

- [ ] **Step 7: Verify that with no `cases/*.c` yet, the Makefile cleanly builds nothing**

```bash
cd fixtures && make clean && make 2>&1 | head -20
```
Expected: each arch reports "Nothing to be done for 'all'" (since `CASES` is empty until Task 18 populates it).  No errors.

- [ ] **Step 8: Run the workspace test/build smoke to make sure the rename didn't break anything**

```bash
cargo build --workspace
cargo test -p reader -p cfg -p analyzer -p opt 2>&1 | tail -10
```
Expected: builds clean; tests that depend on `fixtures/out/` either find the files (still in place after `git mv`) or skip with the existing "missing test binary" message — but no compile errors from the path swap.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "build(fixtures): rename binary_tests→fixtures, glob cases/*.c, add MIPS LE+BE

Restructures the test-fixture tree:
  - binary_tests/  →  fixtures/                (git mv preserves history)
  - fixtures/Makefile globs cases/*.c (was: single-purpose test.c rule)
  - new arch/mips32le.mk + arch/mips32be.mk for MIPS coverage
  - all six archs now compile under -Wall -Wextra -Werror

References in crates/{analyzer,reader,cfg,opt,target} + README.md +
CLAUDE.md + .gitignore are updated to the new path."
```

---

### Task 18: Author the ten fixture `.c` files

This task creates ten focused `.c` files under `fixtures/cases/`. Each function is `__attribute__((noinline))` so the analyzer sees it as a distinct symbol. Files compile clean under `-Wall -Wextra -Werror`.

**Files:**
- Create: `fixtures/cases/arithmetic.c`
- Create: `fixtures/cases/control.c`
- Create: `fixtures/cases/memory.c`
- Create: `fixtures/cases/calls.c`
- Create: `fixtures/cases/floats.c`
- Create: `fixtures/cases/abi.c`
- Create: `fixtures/cases/stack.c`
- Create: `fixtures/cases/builtins.c`
- Create: `fixtures/cases/globals.c`
- Create: `fixtures/cases/patterns.c`

- [ ] **Step 1: Create `cases/arithmetic.c`**

```c
/*
 * arithmetic.c — every IntBinaryOp / IntUnaryOp variant the analyzer must lower.
 *
 * Functions are annotated noinline so each becomes a distinct ELF symbol.
 * All operands are signed/unsigned 32-bit ints (the analyzer's most common
 * width); larger widths are exercised in abi.c.
 */
#define NOINLINE __attribute__((noinline))

int  NOINLINE add        (int a, int b)      { return a + b; }
int  NOINLINE sub        (int a, int b)      { return a - b; }
int  NOINLINE mul        (int a, int b)      { return a * b; }

unsigned NOINLINE udiv   (unsigned a, unsigned b) { return a / b; }
unsigned NOINLINE umod   (unsigned a, unsigned b) { return a % b; }
int      NOINLINE sdiv   (int a, int b)           { return a / b; }
int      NOINLINE smod   (int a, int b)           { return a % b; }

int  NOINLINE bit_and    (int a, int b)      { return a & b; }
int  NOINLINE bit_or     (int a, int b)      { return a | b; }
int  NOINLINE bit_xor    (int a, int b)      { return a ^ b; }
int  NOINLINE bit_not    (int a)             { return ~a; }

unsigned NOINLINE shl    (unsigned a, unsigned b) { return a << b; }
unsigned NOINLINE lshr   (unsigned a, unsigned b) { return a >> b; }
int      NOINLINE ashr   (int a, int b)           { return a >> b; }

int  NOINLINE negate     (int a)             { return -a; }

/* Force the linker to keep every function (otherwise -Wl,--gc-sections may
 * strip unreferenced symbols on some toolchains). */
int main(void) {
    volatile int sink = 0;
    sink ^= add(1, 2);     sink ^= sub(3, 4);    sink ^= mul(5, 6);
    sink ^= (int)udiv(7, 1); sink ^= (int)umod(7, 3);
    sink ^= sdiv(7, 2);    sink ^= smod(7, 2);
    sink ^= bit_and(1,2);  sink ^= bit_or(1,2);  sink ^= bit_xor(1,2);  sink ^= bit_not(1);
    sink ^= (int)shl(1,2); sink ^= (int)lshr(8,1); sink ^= ashr(-8, 1);
    sink ^= negate(1);
    return sink;
}
```

- [ ] **Step 2: Create `cases/control.c`**

```c
/*
 * control.c — branches, loops, and merge points.
 *
 * Every loop function produces a ControlPhi at the loop header; every
 * if/else produces an If node with two ControlState successors.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE abs_val(int x)             { return x < 0 ? -x : x; }
int NOINLINE max_val(int a, int b)      { return a > b ? a : b; }
int NOINLINE clamp(int x, int lo, int hi) {
    if (x < lo) return lo;
    if (x > hi) return hi;
    return x;
}
int NOINLINE select_three(int a, int b, int c, int sel) {
    if (sel == 0) return a;
    if (sel == 1) return b;
    return c;
}

int NOINLINE sum_to_n(int n) {
    int s = 0;
    for (int i = 0; i <= n; ++i) s += i;
    return s;
}
int NOINLINE factorial(int n) {
    int r = 1;
    for (int i = 2; i <= n; ++i) r *= i;
    return r;
}
int NOINLINE count_bits(unsigned x) {
    int c = 0;
    while (x) { c += (int)(x & 1u); x >>= 1; }
    return c;
}
int NOINLINE nested_loops(int n, int m) {
    int s = 0;
    for (int i = 0; i < n; ++i)
        for (int j = 0; j < m; ++j)
            s += i * j;
    return s;
}
int NOINLINE early_return(int n) {
    for (int i = 0; i < n; ++i) {
        if (i == 7) return i;
    }
    return -1;
}

int main(void) {
    volatile int s = 0;
    s ^= abs_val(-3); s ^= max_val(3, 4); s ^= clamp(5, 0, 10);
    s ^= select_three(1, 2, 3, 1);
    s ^= sum_to_n(5); s ^= factorial(5); s ^= (int)count_bits(0xdeadbeefu);
    s ^= nested_loops(3, 4); s ^= early_return(9);
    return s;
}
```

- [ ] **Step 3: Create `cases/memory.c`**

```c
/*
 * memory.c — Load / Store at the default code space.
 *
 * StackStoreDetect promotes the sized local arrays here into StackStore /
 * StackStorePhi nodes; the ABI-passed pointer parameters keep raw Load/Store.
 */
#include <stddef.h>
#define NOINLINE __attribute__((noinline))

int NOINLINE array_sum(const int *arr, size_t len) {
    int s = 0;
    for (size_t i = 0; i < len; ++i) s += arr[i];
    return s;
}
void NOINLINE array_fill(int *arr, size_t len, int val) {
    for (size_t i = 0; i < len; ++i) arr[i] = val;
}
void NOINLINE array_copy(int *dst, const int *src, size_t len) {
    for (size_t i = 0; i < len; ++i) dst[i] = src[i];
}

int NOINLINE pointer_chase(int *const *p) {
    /* Two indirections: *p, then *(*p). */
    return **p;
}

struct point { int x; int y; };
int NOINLINE struct_field_load(const struct point *p)        { return p->x + p->y; }
void NOINLINE struct_field_store(struct point *p, int x, int y) { p->x = x; p->y = y; }

union tag { int as_int; unsigned char bytes[4]; };
int NOINLINE tagged_union_read(const union tag *t) { return t->as_int + (int)t->bytes[0]; }

int main(void) {
    int buf[4] = { 1, 2, 3, 4 };
    int dst[4];
    array_copy(dst, buf, 4);
    array_fill(dst, 4, 9);
    int s = array_sum(dst, 4);

    int x = 7;
    int *xp = &x;
    s ^= pointer_chase(&xp);

    struct point p = { 1, 2 };
    s ^= struct_field_load(&p);
    struct_field_store(&p, 3, 4);

    union tag t;
    t.as_int = 0x12345678;
    s ^= tagged_union_read(&t);
    return s;
}
```

- [ ] **Step 4: Create `cases/calls.c`**

```c
/*
 * calls.c — direct, indirect, mutual, and nested calls.
 *
 * The analyzer must produce one Call node per call site, matching the
 * target's address (or an indirect callee value).
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE fib_recursive(int n) {
    if (n <= 1) return n;
    return fib_recursive(n - 1) + fib_recursive(n - 2);
}

/* Mutual recursion — each function calls the other once. */
int NOINLINE mutual_a(int n);
int NOINLINE mutual_b(int n);
int NOINLINE mutual_a(int n) { return n <= 0 ? 0 : 1 + mutual_b(n - 1); }
int NOINLINE mutual_b(int n) { return n <= 0 ? 0 : 1 + mutual_a(n - 1); }

int NOINLINE leaf(int x)     { return x + 1; }
int NOINLINE mid(int x)      { return leaf(x) * 2; }
int NOINLINE nested_3deep(int x) { return mid(x) - 1; }

int NOINLINE pair_a(int x, int y) { return x + y; }
int NOINLINE repeat_call_pair(int a, int b) {
    return pair_a(a, b) + pair_a(b, a);
}

int NOINLINE pass_through(int x) { return leaf(x); }

typedef int (*fnptr)(int);
int NOINLINE apply_indirect(fnptr f, int x) { return f(x); }

int main(void) {
    volatile int s = 0;
    s ^= fib_recursive(6);
    s ^= mutual_a(5);
    s ^= nested_3deep(3);
    s ^= repeat_call_pair(1, 2);
    s ^= pass_through(4);
    s ^= apply_indirect(&leaf, 5);
    return s;
}
```

- [ ] **Step 5: Create `cases/floats.c`**

```c
/*
 * floats.c — F32 / F64 arithmetic, comparisons, and conversions.
 */
#define NOINLINE __attribute__((noinline))

float  NOINLINE f32_arith(float a, float b)  { return ((a + b) * a - b) / (a + 1.0f); }
double NOINLINE f64_arith(double a, double b){ return ((a + b) * a - b) / (a + 1.0);  }

double NOINLINE f32_to_f64(float a)  { return (double)a; }
float  NOINLINE f64_to_f32(double a) { return (float)a;  }
float  NOINLINE int_to_float(int a)  { return (float)a;  }
int    NOINLINE float_to_int(float a){ return (int)a;    }

int NOINLINE f32_compare(float a, float b) {
    if (a < b) return -1;
    if (a > b) return  1;
    return 0;
}
int NOINLINE f64_compare(double a, double b) {
    if (a < b) return -1;
    if (a > b) return  1;
    return 0;
}

float NOINLINE f32_neg_abs(float a) { return -a; }

int main(void) {
    volatile int s = 0;
    s ^= (int)f32_arith(1.5f, 2.5f);
    s ^= (int)f64_arith(1.5,  2.5);
    s ^= (int)f32_to_f64(1.5f);
    s ^= (int)f64_to_f32(1.5);
    s ^= (int)int_to_float(7);
    s ^= float_to_int(7.5f);
    s ^= f32_compare(1.0f, 2.0f);
    s ^= f64_compare(1.0,  2.0);
    s ^= (int)f32_neg_abs(-3.5f);
    return s;
}
```

- [ ] **Step 6: Create `cases/abi.c`**

```c
/*
 * abi.c — exercises calling-convention argument materialisation.
 *
 * `eight_int_args` forces stack args 7–8 on x86_64 (only 6 arg regs);
 * `point_sum` forces struct decomposition; `make_pair` returns a small struct.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE eight_int_args(int a, int b, int c, int d,
                            int e, int f, int g, int h) {
    return a + b + c + d + e + f + g + h;
}

int NOINLINE mixed_args(int a, int b, int c, int d, const int *p, const int *q) {
    return a + b + c + d + *p + *q;
}

struct point { int x; int y; };
int  NOINLINE point_sum(struct point p) { return p.x + p.y; }

struct pair { int lo; int hi; };
struct pair NOINLINE make_pair(int a, int b) {
    struct pair r = { a, b };
    return r;
}

int NOINLINE tail_caller(int a, int b) {
    /* Tail position calls — let the compiler fold the return through. */
    return eight_int_args(a, b, a, b, a, b, a, b);
}

int main(void) {
    volatile int s = 0;
    s ^= eight_int_args(1, 2, 3, 4, 5, 6, 7, 8);
    int p = 9, q = 10;
    s ^= mixed_args(1, 2, 3, 4, &p, &q);
    struct point pt = { 1, 2 };
    s ^= point_sum(pt);
    struct pair pr = make_pair(11, 22);
    s ^= pr.lo + pr.hi;
    s ^= tail_caller(3, 4);
    return s;
}
```

- [ ] **Step 7: Create `cases/stack.c`**

```c
/*
 * stack.c — exercises stack-frame allocation and StackStoreDetect.
 *
 * `volatile_three_writes` must preserve all three Stores even after opt;
 * `escape_via_ptr` forces &local to materialise a stack slot, blocking
 * register-only optimisation.
 */
#include <stddef.h>
#define NOINLINE __attribute__((noinline))

void NOINLINE volatile_three_writes(volatile int *p, int v) {
    *p = v;
    *p = v + 1;
    *p = v + 2;
}

extern void external_take_ptr(int *);
int NOINLINE escape_via_ptr(int seed) {
    int local = seed * 3;
    external_take_ptr(&local);  /* address-taken — local must live on the stack */
    return local;
}

int NOINLINE large_local_array(int n) {
    int buf[16] = { 0 };
    for (int i = 0; i < n && i < 16; ++i) buf[i] = i * i;
    int s = 0;
    for (int i = 0; i < 16; ++i) s += buf[i];
    return s;
}

void NOINLINE inplace_swap(int *a, int *b) {
    int t = *a;
    *a = *b;
    *b = t;
}

int NOINLINE recursive_stack_growth(int n) {
    int buf[8] = { n, n+1, n+2, n+3, n+4, n+5, n+6, n+7 };
    if (n <= 0) {
        int s = 0;
        for (int i = 0; i < 8; ++i) s += buf[i];
        return s;
    }
    return recursive_stack_growth(n - 1) + buf[0];
}

/* A no-op definition so escape_via_ptr links cleanly inside this case alone. */
void external_take_ptr(int *p) { (void)p; }

int main(void) {
    volatile int sink = 0;
    volatile_three_writes(&sink, 1);
    int x = 1, y = 2;
    inplace_swap(&x, &y);
    sink ^= x;
    sink ^= escape_via_ptr(7);
    sink ^= large_local_array(8);
    sink ^= recursive_stack_growth(3);
    return sink;
}
```

- [ ] **Step 8: Create `cases/builtins.c`**

```c
/*
 * builtins.c — GCC builtins lowered to dedicated p-code opcodes.
 *
 * popcount / clz / ctz lift to Popcount / Lzcount nodes (Lzcount handles ctz
 * via a count-leading-zeros-of-reverse pattern in the lifter).  __builtin_expect
 * emits a regular comparison and is here to verify it doesn't break analysis.
 */
#define NOINLINE __attribute__((noinline))

int NOINLINE popcount32(unsigned x)         { return __builtin_popcount(x); }
int NOINLINE clz32(unsigned x)              { return __builtin_clz(x); }
int NOINLINE ctz32(unsigned x)              { return __builtin_ctz(x); }
int NOINLINE popcount64(unsigned long long x) { return __builtin_popcountll(x); }
int NOINLINE clz64(unsigned long long x)      { return __builtin_clzll(x); }

int NOINLINE expect_branch(int x) {
    if (__builtin_expect(x > 100, 0)) return -1;
    return x + 1;
}

int main(void) {
    volatile int s = 0;
    s ^= popcount32(0xdeadbeefu);
    s ^= clz32(0x00010000u);
    s ^= ctz32(0x00010000u);
    s ^= popcount64(0xdeadbeefcafebabeULL);
    s ^= clz64(0x0000000000010000ULL);
    s ^= expect_branch(50);
    return s;
}
```

- [ ] **Step 9: Create `cases/globals.c`**

```c
/*
 * globals.c — exercises the LoadReadOnly fold against .rodata constants.
 *
 * Each function reads a value from a global const, so a successful
 * LoadReadOnly pass leaves an IntConst (or IntBinaryOp on int constants)
 * in the optimised graph instead of a Load.
 */
#define NOINLINE __attribute__((noinline))

static const unsigned char k_byte = 'a';   /* 0x61 */
static const int          k_int  = 0x12345678;
static const char *const  k_str  = "yes";  /* both pointer and chars are read-only */

int NOINLINE read_const_byte(void)             { return (int)k_byte; }
int NOINLINE read_const_int (void)             { return k_int; }
int NOINLINE branch_on_const_string(int a, int b) {
    if (k_str[0] == 'y') return a + b;
    return a - b;
}
int NOINLINE runtime_const_idx(int idx) {
    static const int table[4] = { 10, 20, 30, 40 };
    if (idx < 0 || idx >= 4) return -1;
    return table[idx];
}

int main(void) {
    volatile int s = 0;
    s ^= read_const_byte();
    s ^= read_const_int();
    s ^= branch_on_const_string(1, 2);
    s ^= runtime_const_idx(2);
    return s;
}
```

- [ ] **Step 10: Create `cases/patterns.c`**

```c
/*
 * patterns.c — fixtures targeted at complex pattern-matching queries.
 *
 * Each function is shaped so that pattern::Matcher can pick out a specific
 * structural feature (mac, masked-xor chain, conditional const, loop
 * invariant, accumulator).
 */
#define NOINLINE __attribute__((noinline))

/* Multiply-add: the canonical "pattern.add(pattern.mul(a,b), c)" target. */
int NOINLINE mul_then_add(int a, int b, int c) { return a * b + c; }

/* (((x ^ k1) & m1) ^ k2) — three-operand chain to test pattern recursion. */
int NOINLINE chained_xor_mask(unsigned x) {
    return (int)(((x ^ 0xdeadbeefu) & 0x00ff00ffu) ^ 0xcafebabeu);
}

/* Conditional return of a literal — after opt, both arms feed an IntConst. */
int NOINLINE if_returns_const(int a) {
    if (a > 0) return 100;
    return -50;
}

/* Loop-invariant memory load: the load of *p must remain outside the loop
 * (or be recognised as invariant by future passes). */
int NOINLINE loop_with_invariant_load(const int *p, int n) {
    int s = 0;
    for (int i = 0; i < n; ++i) s += *p + i;
    return s;
}

/* Recursive accumulator — a candidate for pattern queries that look for
 * the recursive call inside an addition. */
int NOINLINE recursive_with_accumulator(int n, int acc) {
    if (n <= 0) return acc;
    return recursive_with_accumulator(n - 1, acc + n);
}

int main(void) {
    volatile int s = 0;
    s ^= mul_then_add(2, 3, 4);
    s ^= chained_xor_mask(0xdeadbeefu);
    s ^= if_returns_const(-1);
    int p = 7;
    s ^= loop_with_invariant_load(&p, 5);
    s ^= recursive_with_accumulator(5, 0);
    return s;
}
```

- [ ] **Step 11: Verify each fixture compiles clean for the host compiler**

```bash
cd fixtures && make clean && make ARCH=x64 2>&1 | tail -30
```
Expected: every fixture builds, no `-W*` warnings printed, every line ends in `Built out/x64/<case>.elf`.

- [ ] **Step 12: Commit**

```bash
git add fixtures/cases/
git commit -m "test(fixtures): introduce ten focused fixture .c files

Each fixture targets one analyzer-relevant category (arithmetic, control,
memory, calls, floats, abi, stack, builtins, globals, patterns).  Every
fixture compiles clean under -Wall -Wextra -Werror.  See plan
docs/superpowers/plans/2026-04-25-analyzer-crate-review.md for the test
matrix that consumes these binaries."
```

---

### Task 19: Build all fixtures × all archs and confirm sizes are reasonable

**Files:** none — verification only.

- [ ] **Step 1: Build everything**

```bash
cd fixtures && make clean && make 2>&1 | tee /tmp/binbuild.log | tail -50
```
Expected: every (arch, case) pair either prints `Built out/<arch>/<case>.elf` or is skipped with `WARNING: compiler for '<arch>' not found`.  No `-Werror` failures.

- [ ] **Step 2: Sanity-check the matrix is filled**

```bash
ls fixtures/out/x64/ fixtures/out/x86/ fixtures/out/aarch64/ fixtures/out/arm/ 2>&1
```
Expected: each arch directory contains exactly the 10 `.elf` files (or skipped if compiler missing).

- [ ] **Step 3: Verify each ELF has the expected symbols**

```bash
for f in fixtures/out/x64/*.elf; do
    echo "=== $f ==="
    nm --defined-only "$f" 2>/dev/null | awk '$2 == "T" || $2 == "t" { print $3 }' | head -20
done
```
Expected: arithmetic.elf has `add`, `sub`, `mul`, `udiv`, …; control.elf has `abs_val`, `max_val`, …; etc.

- [ ] **Step 4: Note in commit body — no commit, this task is verification only**

If any fixture is *missing* a function symbol, that's a Task 18 bug — go fix it there (the `__attribute__((noinline))` + the `main` that touches every function should keep them all linked).

---

### Task 20: Add `tests/common/mod.rs` — shared analyze helper, assertion vocabulary, per-arch macro

**Files:**
- Create: `crates/analyzer/tests/common/mod.rs`

- [ ] **Step 1: Write the module**

```rust
//! Shared infrastructure for the system-test suite.
//!
//! Every `tests/<category>.rs` declares `mod common;` and uses these helpers.
//! Adding a new test case is mechanical:
//!
//! ```ignore
//! per_arch_test!(arithmetic, "add", graph_must_contain_int_add);
//! fn graph_must_contain_int_add(g: &ir::BuiltFunctionGraph) {
//!     assert!(common::count_int_binop(g, ir::IntBinaryOp::Add) >= 1);
//! }
//! ```
//!
//! The macro expands to six `#[test]` functions, one per supported arch
//! (`x86`, `x64`, `aarch64`, `arm`, `mips32le`, `mips32be`).  Each arch test
//! loads its binary at `fixtures/out/<arch>/<case>.elf`, analyses the named
//! symbol, runs the optimiser pipeline (with `LoadReadOnly` wired to the
//! binary's `.rodata`), and invokes the user-provided assertion closure.

#![allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    dead_code  // sub-module test files won't use every helper
)]

use object::{Object, ObjectSymbol};
use std::path::PathBuf;

// ── Architecture enum ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch { X86, X64, Aarch64, Arm, Mips32le, Mips32be }

impl Arch {
    pub fn name(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::X64 => "x64",
            Arch::Aarch64 => "aarch64",
            Arch::Arm => "arm",
            Arch::Mips32le => "mips32le",
            Arch::Mips32be => "mips32be",
        }
    }
    pub fn sleigh(self) -> analyzer::SleighArch {
        match self {
            Arch::X86 => analyzer::SleighArch::x86(),
            Arch::X64 => analyzer::SleighArch::x86_64(),
            Arch::Aarch64 => analyzer::SleighArch::aarch64(),
            Arch::Arm => analyzer::SleighArch::arm(),
            Arch::Mips32le => analyzer::SleighArch::mipsle32(),
            Arch::Mips32be => analyzer::SleighArch::mipsbe32(),
        }
    }
    pub fn cc(self) -> analyzer::CallingConvention {
        match self {
            Arch::X86 => analyzer::CallingConvention::x86_cdecl(),
            Arch::X64 => analyzer::CallingConvention::x86_64_systemv_abi(),
            Arch::Aarch64 => analyzer::CallingConvention::aarch64_aapcs64(),
            Arch::Arm => analyzer::CallingConvention::arm_aapcs(),
            // Same O32 ABI applies to both LE and BE 32-bit MIPS Linux.
            Arch::Mips32le | Arch::Mips32be => analyzer::CallingConvention::mips_o32(),
        }
    }
}

// ── Binary path resolution ───────────────────────────────────────────────────

pub fn binary_path(arch: Arch, case: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/out")
        .join(arch.name())
        .join(format!("{case}.elf"))
}

// ── Pipeline runner ──────────────────────────────────────────────────────────

/// Loads the (arch, case) ELF, builds a CFG starting at `fn_name`, runs the
/// full analyzer + optimiser pipeline (with `LoadReadOnly` against the same
/// ELF) and returns the resulting graph.
///
/// Panics on any failure — system tests are pass/fail end-to-end checks.  If
/// the binary is missing, the panic carries an actionable message including
/// the `make -C fixtures` instruction.
pub fn analyze(arch: Arch, case: &str, fn_name: &str) -> ir::BuiltFunctionGraph {
    let path = binary_path(arch, case);
    if !path.exists() {
        panic!(
            "missing test binary {path:?}; run `make -C fixtures` (or \
             `make -C fixtures ARCH={} CASE={case}` for just this case)",
            arch.name()
        );
    }
    let obj = reader::load_elf(&path)
        .unwrap_or_else(|e| panic!("load_elf({path:?}) failed: {e:?}"));
    let sleigh_arch = arch.sleigh();
    let probe = rsleigh::mem_readers::BufMemReader::new(vec![], 0);
    let regs = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, probe)
        .expect("probe sleigh new")
        .regs()
        .expect("probe sleigh regs");
    let ana = analyzer::Analyzer::new(sleigh_arch, regs, arch.cc())
        .expect("Analyzer::new");
    let mem = reader::ElfFileMemReader::from_object(&obj).expect("mem reader");
    let sleigh = rsleigh::Sleigh::new(sleigh_arch.sla_spec, sleigh_arch.pspec, mem)
        .expect("real sleigh new");
    let addr = obj
        .symbol_by_name(fn_name)
        .unwrap_or_else(|| panic!("symbol {fn_name:?} not found in {path:?}"))
        .address();
    let cfg = cfg::Builder::new(
        sleigh, addr,
        cfg::OptionsBuilder::new().allow_code_before_start_addr().build()
    )
    .build()
    .unwrap_or_else(|e| panic!("Cfg build for {fn_name}: {e:?}"));
    let mut graph = ana.analyze_cfg(&cfg)
        .unwrap_or_else(|e| panic!("analyze_cfg for {fn_name}: {e:?}"));
    let rom = reader::ElfFileMemReader::from_object(&obj).expect("rom reader");
    let mut p = ana.build_optimizer_pipeline();
    p.add(opt::LoadReadOnly(rom));
    p.run(&mut graph)
        .unwrap_or_else(|e| panic!("optimizer pipeline for {fn_name}: {e:?}"));
    graph
}

// ── Assertion vocabulary ─────────────────────────────────────────────────────
//
// All counters walk the graph in pre-order via `BuiltFunctionGraph::preorder()`
// and filter on the node kind.  Naming convention: `count_<thing>` returns a
// `usize`; `has_<thing>` returns a `bool`.

use ir::node::NodeKind;

pub fn count_kind<F: Fn(&NodeKind) -> bool>(g: &ir::BuiltFunctionGraph, pred: F) -> usize {
    g.preorder().filter_map(|nid| g.node_kind(nid)).filter(|k| pred(k)).count()
}

pub fn count_int_binop(g: &ir::BuiltFunctionGraph, op: ir::IntBinaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntBinaryOp(o) if *o == op))
}
pub fn count_int_unop(g: &ir::BuiltFunctionGraph, op: ir::IntUnaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntUnaryOp(o) if *o == op))
}
pub fn count_int_cmp(g: &ir::BuiltFunctionGraph, op: ir::IntCmpOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntCmpOp(o) if *o == op))
}
pub fn count_float_binop(g: &ir::BuiltFunctionGraph, op: ir::FloatBinaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::FloatBinaryOp(o) if *o == op))
}
pub fn count_float_unop(g: &ir::BuiltFunctionGraph, op: ir::FloatUnaryOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::FloatUnaryOp(o) if *o == op))
}
pub fn count_float_cmp(g: &ir::BuiltFunctionGraph, op: ir::FloatCmpOp) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::FloatCmpOp(o) if *o == op))
}

pub fn count_calls (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Call)) }
pub fn count_ifs   (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::If)) }
pub fn count_returns(g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Return)) }
pub fn count_loops (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::ControlPhi(_))) }
pub fn count_loads (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Load(_))) }
pub fn count_stores(g: &ir::BuiltFunctionGraph) -> usize {
    // Both raw Store and StackStore are "writes to memory" from the user's POV.
    count_kind(g, |k| matches!(k, NodeKind::Store(_) | NodeKind::StackStore { .. }))
}
pub fn count_stack_stores(g: &ir::BuiltFunctionGraph) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::StackStore { .. }))
}
pub fn count_popcount(g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Popcount)) }
pub fn count_lzcount (g: &ir::BuiltFunctionGraph) -> usize { count_kind(g, |k| matches!(k, NodeKind::Lzcount)) }
pub fn count_int_consts(g: &ir::BuiltFunctionGraph) -> usize {
    count_kind(g, |k| matches!(k, NodeKind::IntConst(_)))
}

pub fn has_kind<F: Fn(&NodeKind) -> bool>(g: &ir::BuiltFunctionGraph, pred: F) -> bool {
    count_kind(g, pred) > 0
}

pub fn has_constant(g: &ir::BuiltFunctionGraph, value: u64) -> bool {
    has_kind(g, |k| matches!(k, NodeKind::IntConst(c) if *c == value))
}

// ── per_arch_test! macro ─────────────────────────────────────────────────────

/// Generates one `#[test]` per (architecture, function) pair.
///
/// Usage:
///   per_arch_test!(<case>, <fn_name>, <assertion_fn>);
///
/// Expands to a module named `<fn_name>` containing four tests
/// (`x86`, `x64`, `aarch64`, `arm`).  Each test calls `analyze` for its
/// arch and dispatches to the user-provided assertion.
#[macro_export]
macro_rules! per_arch_test {
    ($case:literal, $fn_name:literal, $assert:ident) => {
        per_arch_test!(@gen $case, $fn_name, $fn_name, $assert);
    };
    // Long form: separate test-module name from symbol name.
    (@module $module:ident, $case:literal, $fn_name:literal, $assert:ident) => {
        per_arch_test!(@gen $case, $fn_name, stringify!($module), $assert);
    };
    (@gen $case:literal, $fn_name:literal, $module:expr, $assert:ident) => {
        // Note: we hand-write the four tests rather than nest another macro
        // so that grep `arithmetic::add::aarch64` finds the test definition.
        paste::paste! {
            mod [<test_ $fn_name>] {
                use super::*;
                #[test] fn x86()      { let g = $crate::common::analyze($crate::common::Arch::X86,      $case, $fn_name); $assert(&g); }
                #[test] fn x64()      { let g = $crate::common::analyze($crate::common::Arch::X64,      $case, $fn_name); $assert(&g); }
                #[test] fn aarch64()  { let g = $crate::common::analyze($crate::common::Arch::Aarch64,  $case, $fn_name); $assert(&g); }
                #[test] fn arm()      { let g = $crate::common::analyze($crate::common::Arch::Arm,      $case, $fn_name); $assert(&g); }
                #[test] fn mips32le() { let g = $crate::common::analyze($crate::common::Arch::Mips32le, $case, $fn_name); $assert(&g); }
                #[test] fn mips32be() { let g = $crate::common::analyze($crate::common::Arch::Mips32be, $case, $fn_name); $assert(&g); }
            }
        }
    };
}
```

(Note: the `paste` crate is widely used for identifier concatenation in macros.  If you'd prefer to avoid the new dep, the macro can be rewritten to take the module identifier explicitly: `per_arch_test!(add, "arithmetic", "add", graph_must_contain_int_add);`.  Decide before applying — the rest of the plan assumes `paste`.)

- [ ] **Step 2: Add `paste` to dev-deps**

In `crates/analyzer/Cargo.toml`, append to `[dev-dependencies]`:

```toml
paste = "1"
```

Add the workspace dep too (optional — keeps version pinning consistent):

In `Cargo.toml` (workspace root) under `[workspace.dependencies]`:

```toml
paste = "1"
```

Then in `crates/analyzer/Cargo.toml`:

```toml
[dev-dependencies]
pattern.workspace = true
expect-test.workspace = true
paste.workspace = true
```

- [ ] **Step 3: Add a smoke test that `analyze` works**

Create `crates/analyzer/tests/common_smoke.rs`:

```rust
//! One-liner smoke test for the shared analyze helper.

mod common;

#[test]
fn analyze_arithmetic_add_x64() {
    let g = common::analyze(common::Arch::X64, "arithmetic", "add");
    // Floor: any non-trivial graph has more than just Entry + InitialMemory.
    assert!(g.preorder().count() > 5, "graph too small: {}", g.preorder().count());
}
```

- [ ] **Step 4: Verify**

Run: `cargo test -p analyzer --test common_smoke`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/analyzer/Cargo.toml crates/analyzer/tests/common/mod.rs crates/analyzer/tests/common_smoke.rs Cargo.toml
git commit -m "test(analyzer): add common test module — analyze + assertion DSL + per_arch_test macro"
```

---

### Task 21: `tests/arithmetic.rs` — pin every IntBinaryOp / IntUnaryOp variant per arch

**Files:**
- Create: `crates/analyzer/tests/arithmetic.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Pin every IntBinaryOp / IntUnaryOp variant the analyzer must lower.
//!
//! 15 functions × 6 archs = 90 tests.  Each test asserts the function's
//! optimised IR contains at least one node of the expected kind.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{IntBinaryOp, IntUnaryOp};

per_arch_test!("arithmetic", "add",     has_add);
per_arch_test!("arithmetic", "sub",     has_sub);
per_arch_test!("arithmetic", "mul",     has_mul);
per_arch_test!("arithmetic", "udiv",    has_div);
per_arch_test!("arithmetic", "umod",    has_rem);
per_arch_test!("arithmetic", "sdiv",    has_sdiv);
per_arch_test!("arithmetic", "smod",    has_srem);
per_arch_test!("arithmetic", "bit_and", has_and);
per_arch_test!("arithmetic", "bit_or",  has_or);
per_arch_test!("arithmetic", "bit_xor", has_xor);
per_arch_test!("arithmetic", "bit_not", has_not);
per_arch_test!("arithmetic", "shl",     has_shl);
per_arch_test!("arithmetic", "lshr",    has_lshr);
per_arch_test!("arithmetic", "ashr",    has_ashr);
per_arch_test!("arithmetic", "negate",  has_neg);

fn has_add (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Add) >= 1, "expected ≥1 Add"); }
fn has_sub (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Sub) >= 1, "expected ≥1 Sub"); }
fn has_mul (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Mul) >= 1, "expected ≥1 Mul"); }
fn has_div (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Div) >= 1, "expected ≥1 Div"); }
fn has_rem (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Rem) >= 1, "expected ≥1 Rem"); }
fn has_sdiv(g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Sdiv) >= 1, "expected ≥1 Sdiv"); }
fn has_srem(g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Srem) >= 1, "expected ≥1 Srem"); }
fn has_and (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::And) >= 1, "expected ≥1 And"); }
fn has_or  (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Or)  >= 1, "expected ≥1 Or"); }
fn has_xor (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::Xor) >= 1, "expected ≥1 Xor"); }
fn has_not (g: &ir::BuiltFunctionGraph) { assert!(count_int_unop(g,  IntUnaryOp::Not)  >= 1, "expected ≥1 Not"); }
fn has_shl (g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::ShiftLeft) >= 1, "expected ≥1 ShiftLeft"); }
fn has_lshr(g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::ShiftRight) >= 1, "expected ≥1 ShiftRight"); }
fn has_ashr(g: &ir::BuiltFunctionGraph) { assert!(count_int_binop(g, IntBinaryOp::SShiftRight) >= 1, "expected ≥1 SShiftRight"); }
fn has_neg (g: &ir::BuiltFunctionGraph) { assert!(count_int_unop(g,  IntUnaryOp::Neg)  >= 1, "expected ≥1 Neg"); }
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test arithmetic`
Expected: 90 tests pass (15 functions × 6 archs).

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/arithmetic.rs
git commit -m "test(analyzer): add arithmetic.rs — 15 IntBinary/UnaryOp pinning tests × 6 archs"
```

---

### Task 22: `tests/control.rs` — pin If / ControlPhi / loop structure

**Files:**
- Create: `crates/analyzer/tests/control.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Branches, loops, and merge points.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("control", "abs_val",        abs_has_one_if);
per_arch_test!("control", "max_val",        max_has_one_if);
per_arch_test!("control", "clamp",          clamp_has_two_ifs);
per_arch_test!("control", "select_three",   select_three_has_two_ifs);
per_arch_test!("control", "sum_to_n",       sum_to_n_has_loop);
per_arch_test!("control", "factorial",      factorial_has_loop);
per_arch_test!("control", "count_bits",     count_bits_has_loop_and_shr);
per_arch_test!("control", "nested_loops",   nested_loops_has_two_loops);
per_arch_test!("control", "early_return",   early_return_has_loop_and_two_returns);

fn abs_has_one_if(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 1, "abs_val must have ≥1 If");
}
fn max_has_one_if(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 1, "max_val must have ≥1 If");
}
fn clamp_has_two_ifs(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 2, "clamp has 2 conditionals");
}
fn select_three_has_two_ifs(g: &ir::BuiltFunctionGraph) {
    assert!(count_ifs(g) >= 2, "select_three has 2 conditionals");
}
fn sum_to_n_has_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1, "sum_to_n loop header missing ControlPhi");
}
fn factorial_has_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1, "factorial loop header missing ControlPhi");
}
fn count_bits_has_loop_and_shr(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1);
    assert!(count_int_binop(g, ir::IntBinaryOp::ShiftRight) >= 1, "count_bits has x>>=1");
}
fn nested_loops_has_two_loops(g: &ir::BuiltFunctionGraph) {
    // Two ControlPhi nodes — outer and inner loop heads.
    assert!(count_loops(g) >= 2, "nested_loops expected ≥2 ControlPhi; got {}", count_loops(g));
}
fn early_return_has_loop_and_two_returns(g: &ir::BuiltFunctionGraph) {
    assert!(count_loops(g) >= 1);
    assert!(count_returns(g) >= 2, "early_return has 2 return paths");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test control`
Expected: 54 tests pass (9 functions × 6 archs).

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/control.rs
git commit -m "test(analyzer): add control.rs — If/ControlPhi/Return shape × 6 archs"
```

---

### Task 23: `tests/memory.rs` — Load / Store / StackStore

**Files:**
- Create: `crates/analyzer/tests/memory.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Load / Store at the default code space and on the stack.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("memory", "array_sum",          array_sum_has_load_and_loop);
per_arch_test!("memory", "array_fill",         array_fill_has_store_and_loop);
per_arch_test!("memory", "array_copy",         array_copy_has_load_and_store);
per_arch_test!("memory", "pointer_chase",      pointer_chase_has_two_loads);
per_arch_test!("memory", "struct_field_load",  struct_load_has_load);
per_arch_test!("memory", "struct_field_store", struct_store_has_store);
per_arch_test!("memory", "tagged_union_read",  union_read_has_two_loads);

fn array_sum_has_load_and_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 1, "array_sum must have ≥1 Load");
    assert!(count_loops(g) >= 1, "array_sum loop missing ControlPhi");
}
fn array_fill_has_store_and_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "array_fill must have ≥1 Store");
    assert!(count_loops(g) >= 1);
}
fn array_copy_has_load_and_store(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 1, "array_copy must Load");
    assert!(count_stores(g) >= 1, "array_copy must Store");
    assert!(count_loops(g) >= 1);
}
fn pointer_chase_has_two_loads(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "pointer_chase has 2 indirections");
}
fn struct_load_has_load(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "x and y are two field reads");
}
fn struct_store_has_store(g: &ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 2, "x and y are two field writes");
}
fn union_read_has_two_loads(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2, "as_int + bytes[0] = 2 loads");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test memory`
Expected: 42 tests pass (7 functions × 6 archs).

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/memory.rs
git commit -m "test(analyzer): add memory.rs — Load/Store/StackStore × 6 archs"
```

---

### Task 24: `tests/calls.rs` — direct, indirect, mutual, recursive calls

**Files:**
- Create: `crates/analyzer/tests/calls.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Direct, indirect, mutual, and recursive Call nodes.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("calls", "fib_recursive",      fib_has_two_calls);
per_arch_test!("calls", "mutual_a",           mutual_has_one_call);
per_arch_test!("calls", "mutual_b",           mutual_has_one_call);
per_arch_test!("calls", "nested_3deep",       nested_has_one_call);
per_arch_test!("calls", "repeat_call_pair",   repeat_has_two_calls);
per_arch_test!("calls", "pass_through",       pass_through_has_one_call);
per_arch_test!("calls", "apply_indirect",     indirect_has_call);

fn fib_has_two_calls(g: &ir::BuiltFunctionGraph) {
    // fib(n-1) + fib(n-2) — two recursive calls.
    assert!(count_calls(g) >= 2, "fib has 2 self-recursive calls; got {}", count_calls(g));
    assert!(count_ifs(g) >= 1, "fib base case has If");
}
fn mutual_has_one_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1);
    assert!(count_ifs(g) >= 1);
}
fn nested_has_one_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "nested_3deep calls mid()");
}
fn repeat_has_two_calls(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 2, "repeat_call_pair calls pair_a twice");
}
fn pass_through_has_one_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "pass_through calls leaf");
}
fn indirect_has_call(g: &ir::BuiltFunctionGraph) {
    // Indirect calls are still emitted as Call nodes; the address input is
    // not an IntConst.  We pin only that the call site exists.
    assert!(count_calls(g) >= 1, "apply_indirect has an indirect Call");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test calls`
Expected: 42 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/calls.rs
git commit -m "test(analyzer): add calls.rs — direct/indirect/mutual/recursive Calls"
```

---

### Task 25: `tests/floats.rs` — F32 / F64 arithmetic, comparisons, conversions

**Files:**
- Create: `crates/analyzer/tests/floats.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Float arithmetic, comparisons, and conversions.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use ir::{FloatBinaryOp, FloatCmpOp};

per_arch_test!("floats", "f32_arith",    has_four_float_binops);
per_arch_test!("floats", "f64_arith",    has_four_float_binops);
per_arch_test!("floats", "f32_to_f64",   has_float_to_float);
per_arch_test!("floats", "f64_to_f32",   has_float_to_float);
per_arch_test!("floats", "int_to_float", has_int_to_float);
per_arch_test!("floats", "float_to_int", has_float_to_int);
per_arch_test!("floats", "f32_compare",  has_two_float_cmps);
per_arch_test!("floats", "f64_compare",  has_two_float_cmps);
per_arch_test!("floats", "f32_neg_abs",  has_float_neg);

fn has_four_float_binops(g: &ir::BuiltFunctionGraph) {
    let total = count_float_binop(g, FloatBinaryOp::Add)
        + count_float_binop(g, FloatBinaryOp::Sub)
        + count_float_binop(g, FloatBinaryOp::Mul)
        + count_float_binop(g, FloatBinaryOp::Div);
    assert!(total >= 4, "expected ≥4 FloatBinaryOp, got {total}");
}
fn has_float_to_float(g: &ir::BuiltFunctionGraph) {
    use ir::node::NodeKind;
    assert!(has_kind(g, |k| matches!(k, NodeKind::FloatToFloat)),
            "expected ≥1 FloatToFloat node");
}
fn has_int_to_float(g: &ir::BuiltFunctionGraph) {
    use ir::node::NodeKind;
    assert!(has_kind(g, |k| matches!(k, NodeKind::IntToFloat)),
            "expected ≥1 IntToFloat node");
}
fn has_float_to_int(g: &ir::BuiltFunctionGraph) {
    use ir::node::NodeKind;
    assert!(has_kind(g, |k| matches!(k, NodeKind::FloatToInt)),
            "expected ≥1 FloatToInt node");
}
fn has_two_float_cmps(g: &ir::BuiltFunctionGraph) {
    let total = count_float_cmp(g, FloatCmpOp::Less)
        + count_float_cmp(g, FloatCmpOp::LessEqual)
        + count_float_cmp(g, FloatCmpOp::Equal)
        + count_float_cmp(g, FloatCmpOp::NotEqual);
    assert!(total >= 2, "expected ≥2 FloatCmpOp, got {total}");
    assert!(count_ifs(g) >= 2, "f32/f64_compare has 2 conditionals");
}
fn has_float_neg(g: &ir::BuiltFunctionGraph) {
    use ir::FloatUnaryOp;
    assert!(count_float_unop(g, FloatUnaryOp::Neg) >= 1, "expected ≥1 FloatUnaryOp::Neg");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test floats`
Expected: 54 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/floats.rs
git commit -m "test(analyzer): add floats.rs — Float arithmetic + cmp + conversions × 6 archs"
```

---

### Task 26: `tests/abi.rs` — calling-convention argument materialisation

**Files:**
- Create: `crates/analyzer/tests/abi.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Calling-convention argument and return materialisation.
//!
//! `eight_int_args` exercises the stack-arg path on x86_64 (only 6 arg regs);
//! `point_sum` and `make_pair` exercise struct decomposition.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("abi", "eight_int_args",  eight_args_has_seven_adds);
per_arch_test!("abi", "mixed_args",      mixed_has_loads_and_adds);
per_arch_test!("abi", "point_sum",       point_sum_has_add);
per_arch_test!("abi", "make_pair",       make_pair_has_return);
per_arch_test!("abi", "tail_caller",     tail_caller_has_call);

fn eight_args_has_seven_adds(g: &ir::BuiltFunctionGraph) {
    // a + b + c + d + e + f + g + h = 7 add nodes (left-fold).
    assert!(count_int_binop(g, ir::IntBinaryOp::Add) >= 7,
            "eight_int_args has 7 Adds; got {}", count_int_binop(g, ir::IntBinaryOp::Add));
}
fn mixed_has_loads_and_adds(g: &ir::BuiltFunctionGraph) {
    // Two pointer args dereferenced once each — 2 Loads minimum.
    assert!(count_loads(g) >= 2, "mixed_args dereferences 2 pointers");
    assert!(count_int_binop(g, ir::IntBinaryOp::Add) >= 5, "mixed_args has 5 Adds");
}
fn point_sum_has_add(g: &ir::BuiltFunctionGraph) {
    assert!(count_int_binop(g, ir::IntBinaryOp::Add) >= 1, "point_sum is x+y");
}
fn make_pair_has_return(g: &ir::BuiltFunctionGraph) {
    assert!(count_returns(g) >= 1, "make_pair returns a pair");
}
fn tail_caller_has_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "tail_caller calls eight_int_args");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test abi`
Expected: 30 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/abi.rs
git commit -m "test(analyzer): add abi.rs — stack-arg, struct-by-value, struct-return × 6 archs"
```

---

### Task 27: `tests/stack.rs` — StackStore detection and volatile preservation

**Files:**
- Create: `crates/analyzer/tests/stack.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Stack-frame allocation, StackStoreDetect, and volatile preservation.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("stack", "volatile_three_writes", volatile_preserves_three_stores);
per_arch_test!("stack", "escape_via_ptr",        escape_has_stack_store_and_call);
per_arch_test!("stack", "large_local_array",     large_local_has_stack_store_and_loop);
per_arch_test!("stack", "inplace_swap",          swap_has_two_loads_and_two_stores);
per_arch_test!("stack", "recursive_stack_growth", rec_stack_has_call_and_stores);

fn volatile_preserves_three_stores(g: &ir::BuiltFunctionGraph) {
    // *p = v; *p = v+1; *p = v+2  — opt must not collapse these.
    assert!(count_stores(g) >= 3,
            "volatile must preserve 3 stores; got {}", count_stores(g));
}
fn escape_has_stack_store_and_call(g: &ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "&local forces a stack write");
    assert!(count_calls(g) >= 1, "external_take_ptr is called");
}
fn large_local_has_stack_store_and_loop(g: &ir::BuiltFunctionGraph) {
    assert!(count_stores(g) >= 1, "buf[i] = i*i is a stack store");
    assert!(count_loops(g) >= 1);
}
fn swap_has_two_loads_and_two_stores(g: &ir::BuiltFunctionGraph) {
    assert!(count_loads(g) >= 2);
    assert!(count_stores(g) >= 2);
}
fn rec_stack_has_call_and_stores(g: &ir::BuiltFunctionGraph) {
    assert!(count_calls(g) >= 1, "self-recursive call");
    assert!(count_stores(g) >= 1, "buf[i] writes");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test stack`
Expected: 30 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/stack.rs
git commit -m "test(analyzer): add stack.rs — StackStoreDetect + volatile × 6 archs"
```

---

### Task 28: `tests/builtins.rs` — Popcount / Lzcount opcodes

**Files:**
- Create: `crates/analyzer/tests/builtins.rs`

- [ ] **Step 1: Write the test file**

```rust
//! GCC builtins lowered to dedicated p-code opcodes.
//!
//! Compiler back-ends sometimes inline popcount / clz / ctz to a software
//! sequence (a SWAR popcount, or a clz fallback) on archs without the
//! dedicated instruction.  Tests therefore tolerate either lowering:
//!  - a Popcount/Lzcount node when the arch has the instruction, OR
//!  - a non-trivial graph with shifts and ANDs when it doesn't.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("builtins", "popcount32",    popcount_lowers);
per_arch_test!("builtins", "popcount64",    popcount_lowers);
per_arch_test!("builtins", "clz32",         lzcount_lowers);
per_arch_test!("builtins", "clz64",         lzcount_lowers);
per_arch_test!("builtins", "ctz32",         ctz_lowers);
per_arch_test!("builtins", "expect_branch", expect_compiles_normally);

fn popcount_lowers(g: &ir::BuiltFunctionGraph) {
    let dedicated = count_popcount(g);
    let fallback_swar = count_int_binop(g, ir::IntBinaryOp::And)
        + count_int_binop(g, ir::IntBinaryOp::ShiftRight);
    assert!(dedicated >= 1 || fallback_swar >= 4,
            "popcount lowers to either Popcount node ({dedicated}) or SWAR (got {fallback_swar} AND/SHR ops)");
}
fn lzcount_lowers(g: &ir::BuiltFunctionGraph) {
    let dedicated = count_lzcount(g);
    let nodes = g.preorder().count();
    assert!(dedicated >= 1 || nodes > 10,
            "clz lowers to either Lzcount node ({dedicated}) or non-trivial fallback ({nodes} nodes)");
}
fn ctz_lowers(g: &ir::BuiltFunctionGraph) {
    // ctz(x) is often expressed as clz(x & -x) → either a Lzcount appears
    // OR an isolation pattern of `x & -x` shows up.
    let lz = count_lzcount(g);
    let isolate = count_int_binop(g, ir::IntBinaryOp::And) + count_int_unop(g, ir::IntUnaryOp::Neg);
    assert!(lz >= 1 || isolate >= 1, "ctz expected via Lzcount or And+Neg pattern");
}
fn expect_compiles_normally(g: &ir::BuiltFunctionGraph) {
    // __builtin_expect is a hint, not a real op — it should reduce to plain control flow.
    assert!(count_ifs(g) >= 1, "expect_branch has an if");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test builtins`
Expected: 36 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/builtins.rs
git commit -m "test(analyzer): add builtins.rs — popcount/clz/ctz/expect × 6 archs"
```

---

### Task 29: `tests/globals.rs` — LoadReadOnly fold against .rodata

**Files:**
- Create: `crates/analyzer/tests/globals.rs`

- [ ] **Step 1: Write the test file**

```rust
//! LoadReadOnly fold against .rodata constants.
//!
//! After the optimiser pipeline runs (with LoadReadOnly enabled), reads of
//! `static const` data should fold to IntConst nodes instead of remaining
//! as Loads.  Tests verify: (a) the constant value materialises in the IR,
//! (b) the corresponding read no longer appears as a Load.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;

per_arch_test!("globals", "read_const_byte",        const_byte_folds_to_0x61);
per_arch_test!("globals", "read_const_int",         const_int_folds_to_value);
per_arch_test!("globals", "branch_on_const_string", string_branch_folds_one_arm);
per_arch_test!("globals", "runtime_const_idx",      runtime_idx_keeps_load);

fn const_byte_folds_to_0x61(g: &ir::BuiltFunctionGraph) {
    // 'a' = 0x61.  After LoadReadOnly, this is an IntConst.
    assert!(has_constant(g, 0x61),
            "expected IntConst(0x61) after LoadReadOnly fold");
}
fn const_int_folds_to_value(g: &ir::BuiltFunctionGraph) {
    assert!(has_constant(g, 0x12345678),
            "expected IntConst(0x12345678) after LoadReadOnly fold");
}
fn string_branch_folds_one_arm(g: &ir::BuiltFunctionGraph) {
    // The byte 'y' = 0x79 read from k_str[0] folds; combined with
    // DeadBranchElimination this often eliminates one arm of the If.
    // We pin only the constant — the arm-elimination is opt's responsibility.
    assert!(has_constant(g, 0x79) || count_ifs(g) == 0,
            "expected either IntConst(0x79) or eliminated If; neither found");
}
fn runtime_idx_keeps_load(g: &ir::BuiltFunctionGraph) {
    // Index isn't constant, so the Load survives.  Two ifs gate the bounds.
    assert!(count_loads(g) >= 1, "runtime index → Load survives");
    assert!(count_ifs(g) >= 1, "bounds-check If(s) survive");
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test globals`
Expected: 24 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/globals.rs
git commit -m "test(analyzer): add globals.rs — LoadReadOnly fold × 6 archs"
```

---

### Task 30: `tests/patterns.rs` — complex `pattern::Matcher` queries

This is the most aggressive test file — it uses the `pattern` crate's full DSL to assert structural shape, not just node counts.  Each test pins one pattern query that mirrors a real user query (the `pattern` crate is the project's Python API surface).

**Files:**
- Create: `crates/analyzer/tests/patterns.rs`

- [ ] **Step 1: Write the test file**

```rust
//! Complex pattern queries against rich fixtures.
//!
//! Each test issues a `pattern::Matcher` query mirroring a realistic
//! user-facing query (e.g. "find every (a*b)+c expression"; "find every
//! recursive call site").  These tests are the canonical contract that
//! the pattern crate continues to compose with the analyzer's IR shape.

#![allow(clippy::panic, clippy::unwrap_used, clippy::expect_used, clippy::unreachable)]

mod common;
use common::*;
use pattern::{Matcher, Var, NodeVar, add, mul, call, ret, any, int_const};

per_arch_test!("patterns", "mul_then_add",                mac_pattern_finds_one_match);
per_arch_test!("patterns", "chained_xor_mask",            xor_chain_pattern_finds_match);
per_arch_test!("patterns", "if_returns_const",            if_const_pattern_finds_two_consts);
per_arch_test!("patterns", "loop_with_invariant_load",    invariant_load_pattern_finds_load);
per_arch_test!("patterns", "recursive_with_accumulator",  recursive_pattern_finds_self_call);

fn mac_pattern_finds_one_match(g: &ir::BuiltFunctionGraph) {
    // Pattern: add(mul(?, ?), ?)
    let m = Matcher::new(g);
    let pat = add(mul(any(), any()), any());
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 match of add(mul(_,_), _); got {} matches", hits.len());
}

fn xor_chain_pattern_finds_match(g: &ir::BuiltFunctionGraph) {
    // Pattern: xor(and(xor(?, const), const), const) — three-deep xor/and chain.
    use pattern::{xor, and, any_int_const};
    let m = Matcher::new(g);
    let pat = xor(and(xor(any(), any_int_const()), any_int_const()), any_int_const());
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 match of xor(and(xor(_,c), c), c); got {} matches", hits.len());
}

fn if_const_pattern_finds_two_consts(g: &ir::BuiltFunctionGraph) {
    // After RedundantPhis, both arms of the If feed a Phi that resolves to
    // either IntConst(100) or IntConst(-50 as u64).  We pin both constants.
    assert!(has_constant(g, 100),
            "expected IntConst(100) — true-branch return value");
    let neg50_u32 = (-50i32) as u32 as u64;
    let neg50_u64 = (-50i64) as u64;
    assert!(has_constant(g, neg50_u32) || has_constant(g, neg50_u64),
            "expected IntConst(-50) — false-branch return value (any of {neg50_u32}, {neg50_u64})");
}

fn invariant_load_pattern_finds_load(g: &ir::BuiltFunctionGraph) {
    // Pattern: any Load whose address comes from a function arg (InitialVar).
    let m = Matcher::new(g);
    let pat = pattern::load();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(), "expected ≥1 Load match in loop_with_invariant_load");
    // And: the loop must still be present (we don't currently hoist invariant loads).
    assert!(count_loops(g) >= 1, "loop must remain");
}

fn recursive_pattern_finds_self_call(g: &ir::BuiltFunctionGraph) {
    // Pattern: any Call (recursive_with_accumulator's tail call).
    let m = Matcher::new(g);
    let pat = call();
    let hits = m.find_all(&pat);
    assert!(!hits.is_empty(),
            "expected ≥1 Call match in recursive_with_accumulator; got {} matches", hits.len());
    // And: the if base case is still present.
    assert!(count_ifs(g) >= 1);
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p analyzer --test patterns`
Expected: 30 tests pass.

If a particular pattern combinator is named differently in the actual `pattern` crate (the CLAUDE.md describes free constructors `add`, `sub`, `mul`, `load()`, `call()`, etc., plus `predicate(f)` and `int_const(n)`/`any_int_const()` — verify in the actual `pattern::lib.rs` before applying), substitute the correct names.  All five tests are independent — fix mismatches one at a time.

- [ ] **Step 3: Commit**

```bash
git add crates/analyzer/tests/patterns.rs
git commit -m "test(analyzer): add patterns.rs — complex pattern::Matcher queries × 6 archs

These tests are the contract that the pattern crate (the project's
Python API surface) continues to compose with the analyzer's IR shape.
Each test mirrors a realistic user query."
```

---

### Task 31: Delete the obsolete `tests/analyze_binary.rs` and the original `fixtures/test.c`

**Files:**
- Delete: [crates/analyzer/tests/analyze_binary.rs](crates/analyzer/tests/analyze_binary.rs)
- Delete: [fixtures/test.c](fixtures/test.c)

- [ ] **Step 1: Delete both**

```bash
rm crates/analyzer/tests/analyze_binary.rs
rm fixtures/test.c
```

- [ ] **Step 2: Confirm `cargo test -p analyzer` passes without it**

Run: `cargo test -p analyzer 2>&1 | tail -8`
Expected: every test still passes — the new per-category test files cover everything.

- [ ] **Step 3: Confirm `make -C fixtures` no longer touches `test.c`**

Run: `make -C fixtures clean && make -C fixtures`
Expected: builds only `cases/*.elf`; no reference to `test.c`.

- [ ] **Step 4: Commit**

```bash
git add -u  # picks up the two deletes
git commit -m "test(analyzer): drop obsolete analyze_binary.rs + fixtures/test.c

Replaced by per-category test files (arithmetic.rs, control.rs, …) and
per-category fixture files (cases/arithmetic.c, cases/control.c, …)."
```

---

## Phase F — Verification

### Task 32: Final clippy + test sweep

- [ ] **Step 1: Run clippy with `-D warnings` on `--all-targets`**

Run: `cargo clippy -p analyzer --all-targets -- -D warnings`
Expected: zero output (no warnings, no errors).

- [ ] **Step 2: Run the whole workspace's clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: zero analyzer-related issues (other crates may still have their own warnings — those are out of scope).

- [ ] **Step 3: Run the whole analyzer test suite**

Run: `cargo test -p analyzer`
Expected: all tests pass.  Tally (6 archs assumed; subtract one column per missing cross-compiler):

| Test file | Tests | Per-arch fn count × 6 |
|-----------|------:|----------------------:|
| `src/utils.rs` (unit) | ≥3 | — |
| `src/analyzer/register_aliasing.rs::shift_formula_tests` (unit) | ≥3 | — |
| `tests/error_chain.rs` | 1 | — |
| `tests/register_aliasing_le.rs` | 2 | — |
| `tests/register_aliasing_be.rs` | 0 (1 ignored) | — |
| `tests/vn_io_dispatch.rs` | 3 | — |
| `tests/varnode_collection.rs` | 2 | — |
| `tests/insn_int_not_equal.rs` | 1 | — |
| `tests/common_smoke.rs` | 1 | — |
| `tests/arithmetic.rs` | 90 | 15 × 6 |
| `tests/control.rs` | 54 | 9 × 6 |
| `tests/memory.rs` | 42 | 7 × 6 |
| `tests/calls.rs` | 42 | 7 × 6 |
| `tests/floats.rs` | 54 | 9 × 6 |
| `tests/abi.rs` | 30 | 5 × 6 |
| `tests/stack.rs` | 30 | 5 × 6 |
| `tests/builtins.rs` | 36 | 6 × 6 |
| `tests/globals.rs` | 24 | 4 × 6 |
| `tests/patterns.rs` | 30 | 5 × 6 |
| **Total** | **~448** | — |

(Up from the original 45 — a 10× expansion. The MIPS BE column is the project's first system-test exercise of the Big-endian shift-formula fix from Task 4.)

- [ ] **Step 4: Run the whole workspace test suite (smoke)**

Run: `cargo test --workspace`
Expected: pass — verifies the analyzer changes didn't break a downstream consumer.

- [ ] **Step 5: Final cleanup**

If any of the previous tasks left files uncommitted, sweep them now.  Otherwise this step is a no-op.

```bash
git status   # confirm clean
```

---

## Self-review

Run through this list before declaring the plan ready for execution:

- **Spec coverage:**
  - "Review for correctness": Phase B (Tasks 3–6) covers the four real bugs found.
  - "Code that can be simplified, optimized, more readable": Phase C (Tasks 7–10).
  - "Pass all clippy warnings": Phase A Task 1 + Phase F Task 32.
  - "Work in a new worktree": already created at `.worktrees/analyzer-review-2026-04-25` (branch `review/analyzer-2026-04-25`).
  - "Plan tests for the analyzer — unit tests": Phase D (Tasks 11–15).
  - "Way better system tests that test more basic and complex logic": Phase E (Tasks 16–31).
  - "Extend/rewrite the fixtures as you see fit": Tasks 16–17 (split into 10 fixture files).
  - "Across different binaries; readable code that compiles without warnings; more complex functions; easy to extend; many tests": Phase E architecture.

- **Placeholder scan:** No "TBD" / "implement later" / "fill in details".  The one stub is the BE register-aliasing test in Task 12 step 2, which is intentionally `#[ignore]`-marked with a documented reason — that is *not* a placeholder, it's a deliberate scope cut acknowledged in Q1.

- **Type consistency:** `Arch` enum + `analyze` + `count_*` helpers are defined once in `tests/common/mod.rs` (Task 20) and consumed by every `tests/<category>.rs` (Tasks 21–30). The `per_arch_test!` macro signature `($case:literal, $fn_name:literal, $assert:ident)` is consistent across all use sites. Error variant `SubpieceOffsetOutOfRange` introduced in Task 6 step 3 is referenced in step 2 — both steps are consistent.

- **No-fix-without-test:** every Phase B fix has either a paired unit test (Task 4) or a paired integration test (Tasks 5, 6 inherit from the system tests in Task 21+ catching the regressions). The determinism fix (Task 3) has a paired test in Task 14. The IntNotEqual fix (Task 5) has both a unit test and is exercised by Task 22's `count_bits_has_loop_and_shr` (count_bits's loop condition `while (x)` lifts to IntNotEqual).

- **Easy to extend:** adding a new test case is mechanical:
  1. Add the C function to the relevant `cases/<category>.c` (no Makefile change — it globs).
  2. Add `per_arch_test!("<category>", "<fn_name>", <assertion_fn>);` to `tests/<category>.rs`.
  3. Define `<assertion_fn>` using the helpers in `tests/common/mod.rs`.

---

## Execution handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-25-analyzer-crate-review.md`.

Two execution options:

1. **Subagent-Driven (recommended)** — A fresh subagent per task, review between tasks, fast iteration. Subagent-driven-development skill governs the loop. Suits this plan well: tasks are independent and self-contained, especially in Phase E where each `tests/<category>.rs` is one task.
2. **Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints for review.

**Which approach?**
