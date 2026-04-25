# Target Crate Review — Round 4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the residual doc-drift and test-coverage gaps left after rounds 1–3 of the `target` crate review. The crate's *production code* is unchanged from round 3 (clippy clean with `-D warnings`, 6 unit + 7 smoke tests pass); this round is two docstring fixes and two test-coverage extensions.

**Architecture:** Four small, independently-committable changes. Two tighten docstrings against invariants the round-3 tests already pin; two extend the existing test suites to pin currently-implicit invariants (cross-list disjointness; per-preset endianness).

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## What the review found (findings → tasks)

| # | Finding | Evidence | Task |
|---|---------|----------|------|
| F1 | The `stack_ptr_vn` field doc on `BuiltCallingConvention` says SP is "Deliberately not listed in `Self::callee_saved_regs`" — but round-3's `presets_stack_pointer_and_arg_offsets` test now pins the **stronger** invariant that SP is absent from all three resolved register lists (`arg_passing_regs`, `callee_saved_regs`, `ret_val_regs`). The doc drifts from the test. | [calling_convention.rs:62-64](crates/target/src/calling_convention.rs#L62-L64) (doc) vs. [calling_convention.rs:372-382](crates/target/src/calling_convention.rs#L372-L382) (test) | Task 1 |
| F2 | `aarch64_aapcs64()`'s docstring describes the ABI generically but never mentions that AAPCS64 is endianness-agnostic — i.e. it pairs equally with [`SleighArch::aarch64()`](crates/target/src/arch.rs#L86) and [`SleighArch::aarch64be()`](crates/target/src/arch.rs#L96). A user with a BE binary has to read source to find this out. | [calling_convention.rs:102-123](crates/target/src/calling_convention.rs#L102-L123); BE preset at [arch.rs:94-102](crates/target/src/arch.rs#L94-L102) | Task 2 |
| F3 | `presets_resolve_correct_register_sets` checks `arg_passing_regs ∩ callee_saved_regs == ∅`, but not `ret_val_regs ∩ callee_saved_regs == ∅` or `arg_passing_regs ∩ ret_val_regs`. The latter pair holds trivially for every current preset (return regs are caller-saved on every supported ABI), but if a future preset broke it, nothing fails. | [calling_convention.rs:325-331](crates/target/src/calling_convention.rs#L325-L331) | Task 3 |
| F4 | `Endianness` is load-bearing — [`crates/analyzer/src/analyzer/register_aliasing.rs:65-67`](crates/analyzer/src/analyzer/register_aliasing.rs#L65-L67) branches on it for sub-register extraction. Currently `arch_smoke.rs` only asserts each `SleighArch` preset feeds `rsleigh::Sleigh::new`. If a preset's `endianness` field is mistyped (e.g. `aarch64be` set to `Little`), BE register aliasing silently breaks at the analyzer layer with no signal from the target crate. | [arch.rs:32-102](crates/target/src/arch.rs#L32-L102), [tests/arch_smoke.rs:14-21](crates/target/tests/arch_smoke.rs#L14-L21) | Task 4 |

### Considered and rejected

- **Cross-checking `SleighArch::endianness` against the `.sla_spec`.** Same as round 3 — `rsleigh::Sleigh` exposes `sla_spec()` / `pspec()` but no `endianness()` accessor. Without an upstream change we'd have to parse the SLA blob ourselves.
- **Sealing `BuiltCallingConvention` fields behind a constructor.** Pattern tests at [crates/pattern/tests/matching/support/graph.rs:66-92](crates/pattern/tests/matching/support/graph.rs#L66-L92) construct it field-by-field. Sealing would force a builder/factory; not worth the churn for one test crate.
- **Inlining `regs_to_vns` into `build()`.** Called 3 times in `build()`; the named helper documents intent and dedupes the `ok_or_else` boilerplate. Inlining would lengthen `build()` from 16 lines to ~30 with no readability win.
- **Runtime validation of `stack_arg_offsets`** (duplicates, monotonicity). Same as round 2 — fields are crate-private and presets are the source of truth. Belongs in tests, not `build()`.
- **Adding an `arm_be`, `mipsbe32_o32`, or other untested arch/CC combinations.** No test binaries; same rationale as prior rounds.
- **Switching `Vec<Vn>` to `Box<[Vn]>` in `BuiltCallingConvention`.** Same as round 2/3 — touches 4+ consumer sites in `opt`; no measurable win.
- **Renaming `regs_to_vns` → `resolve_reg_names`.** Same as round 3 — pure naming churn.
- **Asserting `arg_passing_regs ∩ ret_val_regs == ∅`.** This invariant **does not hold** — on x86-64 SysV, `RDX` is both an argument register (`arg_passing_regs[2]`) and a return register (`ret_val_regs[1]`). On x86 cdecl, `EAX`/`EDX` are returns; arg list is empty. So `args ∩ rets` can be non-empty by ABI design (RDX is the standard "second 64-bit return" slot). Task 3 only adds the `rets ∩ callee_saved == ∅` check, which **is** universally true.
- **Adding a doc cross-reference from `aarch64()`/`aarch64be()` back to `aarch64_aapcs64()`.** Asymmetric — the CC is the user-facing primitive that consults the arch. One-way reference (CC → arch) is enough.

---

## Task 1: Tighten `stack_ptr_vn` field doc to match the round-3 invariant

Closes F1. The docstring was correct when written but is now strictly weaker than what the test pins. Update it to mention all three lists.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:62-64](crates/target/src/calling_convention.rs#L62-L64)

- [ ] **Step 1: Replace the `stack_ptr_vn` doc block**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
    /// The hardware stack-pointer varnode (e.g. `RSP` on x86-64, `sp` on
    /// AArch64).  Deliberately not listed in [`Self::callee_saved_regs`].
    pub stack_ptr_vn: rsleigh::Vn,
```

with:

```rust
    /// The hardware stack-pointer varnode (e.g. `RSP` on x86-64, `sp` on
    /// AArch64).  Deliberately absent from all three resolved register lists
    /// ([`Self::arg_passing_regs`], [`Self::callee_saved_regs`],
    /// [`Self::ret_val_regs`]) — SP's cross-call behaviour is expressed
    /// through [`Self::ret_stack_pop`] instead.  This invariant is pinned by
    /// the `presets_stack_pointer_and_arg_offsets` unit test.
    pub stack_ptr_vn: rsleigh::Vn,
```

- [ ] **Step 2: Build the docs cleanly**

Run: `cargo doc -p target --no-deps 2>&1 | tail -20`
Expected: no warnings about broken intra-doc links — every `Self::*` reference resolves.

- [ ] **Step 3: Run the target tests**

Run: `cargo test -p target`
Expected: 6 unit + 7 smoke = 13 PASS (no behaviour change; doc-only edit).

- [ ] **Step 4: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): widen stack_ptr_vn invariant note to all three reg lists"
```

---

## Task 2: Document AAPCS64 endianness-agnosticism on `aarch64_aapcs64()`

Closes F2. AAPCS64's register conventions don't depend on byte order, so the same preset works with both `aarch64()` and `aarch64be()`. Note this in the docstring so a BE-binary user doesn't have to dig.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:102-123](crates/target/src/calling_convention.rs#L102-L123)

- [ ] **Step 1: Replace the `aarch64_aapcs64` doc block**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
    /// Returns the AArch64 AAPCS64 calling convention.
    ///
    /// Argument registers: x0–x7
    /// Callee-saved: x19–x28, x29 (frame pointer), x30 (link register)
    /// Return value: x0, x1
    ///
    /// `sp` is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret_stack_pop` is `0` on AAPCS64 because `bl` writes
    /// the return address to `lr` rather than pushing it.
    #[must_use]
    pub fn aarch64_aapcs64() -> CallingConvention {
```

with:

```rust
    /// Returns the AArch64 AAPCS64 calling convention.
    ///
    /// Argument registers: x0–x7
    /// Callee-saved: x19–x28, x29 (frame pointer), x30 (link register)
    /// Return value: x0, x1
    ///
    /// `sp` is the stack pointer (see `stack_ptr_reg_name`) and is not listed
    /// as callee-saved — `ret_stack_pop` is `0` on AAPCS64 because `bl` writes
    /// the return address to `lr` rather than pushing it.
    ///
    /// AAPCS64 register conventions are independent of byte order, so this
    /// preset pairs equally with [`crate::SleighArch::aarch64`] (LE) and
    /// [`crate::SleighArch::aarch64be`] (BE).
    #[must_use]
    pub fn aarch64_aapcs64() -> CallingConvention {
```

- [ ] **Step 2: Build the docs cleanly**

Run: `cargo doc -p target --no-deps 2>&1 | tail -20`
Expected: no warnings about broken intra-doc links — `crate::SleighArch::aarch64` and `crate::SleighArch::aarch64be` both resolve via the `pub use` in `lib.rs`.

- [ ] **Step 3: Run the target tests**

Run: `cargo test -p target`
Expected: 6 unit + 7 smoke = 13 PASS (no behaviour change).

- [ ] **Step 4: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): note aarch64_aapcs64 pairs with both LE and BE arch presets"
```

---

## Task 3: Extend `presets_resolve_correct_register_sets` to pin `ret_val_regs ∩ callee_saved_regs == ∅`

Closes F3. The existing `arg_passing_regs ∩ callee_saved_regs == ∅` check is generalised into a small `assert_disjoint` helper covering both pairs.

Note: We do **not** assert `arg_passing_regs ∩ ret_val_regs == ∅`. That invariant fails on x86-64 SysV, where `RDX` legitimately appears in both lists (3rd argument register and 2nd return-value register).

**Files:**
- Modify: [crates/target/src/calling_convention.rs:290-333](crates/target/src/calling_convention.rs#L290-L333)

- [ ] **Step 1: Add an `assert_disjoint` helper next to `assert_all_distinct`**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), immediately after the `assert_all_distinct` function (line 300), insert:

```rust
    #[track_caller]
    fn assert_disjoint(
        a: &[rsleigh::Vn],
        b: &[rsleigh::Vn],
        a_label: &str,
        b_label: &str,
        case_name: &str,
    ) {
        for vn in a {
            assert!(
                !b.contains(vn),
                "{case_name}: {a_label} reg {vn:?} also appears in {b_label}",
            );
        }
    }
```

- [ ] **Step 2: Replace the open-coded disjointness loop in `presets_resolve_correct_register_sets`**

Replace:

```rust
            for vn in &built.arg_passing_regs {
                assert!(
                    !built.callee_saved_regs.contains(vn),
                    "{}: arg reg {vn:?} is also callee-saved",
                    c.name,
                );
            }
```

with:

```rust
            assert_disjoint(
                &built.arg_passing_regs,
                &built.callee_saved_regs,
                "arg_passing_regs",
                "callee_saved_regs",
                c.name,
            );
            assert_disjoint(
                &built.ret_val_regs,
                &built.callee_saved_regs,
                "ret_val_regs",
                "callee_saved_regs",
                c.name,
            );
```

- [ ] **Step 3: Run the extended invariant test**

Run: `cargo test -p target presets_resolve_correct_register_sets -- --nocapture`
Expected: PASS on all 4 cases. If any fails, a real ABI invariant is broken — investigate the preset, do **not** weaken the test.

- [ ] **Step 4: Run the full target suite**

Run: `cargo test -p target`
Expected: 6 unit + 7 smoke = 13 PASS.

- [ ] **Step 5: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "test(target): pin ret_val_regs disjoint from callee_saved_regs"
```

---

## Task 4: Pin `Endianness` value per `SleighArch` preset

Closes F4. `Endianness` flows from `target` through `analyzer::Analyzer.arch.endianness` into [`register_aliasing.rs:65-67`](crates/analyzer/src/analyzer/register_aliasing.rs#L65-L67) where it gates sub-register byte-shift direction. A BE-mistyped `Endianness::Little` on `aarch64be()` would silently produce wrong shifts. Add a single parameterised test in `arch_smoke.rs` that pins each preset's expected endianness.

**Files:**
- Modify: [crates/target/tests/arch_smoke.rs](crates/target/tests/arch_smoke.rs)

- [ ] **Step 1: Add the endianness assertion test at the end of `arch_smoke.rs`**

In [crates/target/tests/arch_smoke.rs](crates/target/tests/arch_smoke.rs), after `aarch64be_preset_resolves` (the last existing test, around line 56), append:

```rust

/// Pins the [`target::Endianness`] field of every `SleighArch` preset.
///
/// `Endianness` is consumed by `analyzer::register_aliasing` to decide the
/// shift direction when extracting a sub-register from its container; a
/// mistyped value on a BE preset (or vice-versa) silently produces wrong
/// shifts at the analyzer layer, with no signal from this crate.  Pin it
/// here so a typo in `arch.rs` is caught at unit-test time.
#[test]
fn presets_endianness_matches_arch() {
    use target::Endianness;
    let cases: &[(&str, SleighArch, Endianness)] = &[
        ("x86_64", SleighArch::x86_64(), Endianness::Little),
        ("x86", SleighArch::x86(), Endianness::Little),
        ("mipsbe32", SleighArch::mipsbe32(), Endianness::Big),
        ("mipsle32", SleighArch::mipsle32(), Endianness::Little),
        ("arm", SleighArch::arm(), Endianness::Little),
        ("aarch64", SleighArch::aarch64(), Endianness::Little),
        ("aarch64be", SleighArch::aarch64be(), Endianness::Big),
    ];
    for (label, arch, expected) in cases {
        assert_eq!(
            arch.endianness, *expected,
            "{label}: expected {expected:?}, got {:?}",
            arch.endianness,
        );
    }
}
```

- [ ] **Step 2: Run the new test in isolation**

Run: `cargo test -p target --test arch_smoke presets_endianness_matches_arch -- --nocapture`
Expected: PASS — single test asserts 7 (label, arch, endianness) tuples.

- [ ] **Step 3: Run the full target suite**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS (one new smoke test).

- [ ] **Step 4: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/target/tests/arch_smoke.rs
git commit -m "test(target): pin endianness value of every SleighArch preset"
```

---

## Task 5: Final workspace sanity sweep

**Files:**
- Run-only, no edits.

- [ ] **Step 1: Strict lint on the target crate**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 2: Full workspace build**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 3: Full workspace tests**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced without error.

- [ ] **Step 5: Workspace lint (informational)**

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: PASS if no other crate has a latent lint; otherwise the remaining warnings are outside this review's scope — flag to the reviewer, do not fix here.

---

## Out of scope (considered, rejected, or deferred)

All items from rounds 1–3's out-of-scope lists remain out of scope. Additionally:

- **Endianness self-consistency with `sla_spec`.** Still no `rsleigh::Sleigh::endianness()` accessor; deferred. Task 4 only pins what `arch.rs` declares, not what the SLA blob actually requires — but since the analyzer layer trusts `arch.endianness`, this is the load-bearing invariant.
- **Asserting `arg_passing_regs ∩ ret_val_regs == ∅`.** Fails by ABI design on x86-64 SysV (`RDX` is both arg₂ and ret₁). Not a real invariant.
- **Per-field constructor for `BuiltCallingConvention`.** Pattern tests construct it field-by-field; sealing fields would force a builder shim with no real win.
- **Adding `arm_be` or MIPS calling-convention presets.** Same as prior rounds — no test binary to exercise.
- **`Vec<…>` → `Box<[…]>` in `BuiltCallingConvention`.** Same as round 2/3 — multi-crate change, no measurable win.
- **Doc cross-reference from `SleighArch::aarch64()` back to `aarch64_aapcs64()`.** Asymmetric; the CC is the user-facing primitive. One-way reference is enough.
