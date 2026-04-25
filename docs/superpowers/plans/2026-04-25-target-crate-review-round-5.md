# Target Crate Review — Round 5 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Three small simplification / readability improvements left after rounds 1–4 of the `target` crate review. The crate is clippy-clean (`-D warnings`), all 6 unit + 8 smoke tests pass, and ABI/register-set invariants are pinned. This round dedupes one open-coded lookup, fills three field doc gaps, and rewords one awkward error string. No behaviour change.

**Architecture:** Three independently-committable changes followed by a workspace sanity sweep. Each has its own task with a TDD-style or doc-build verification step.

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## What the review found (findings → tasks)

| # | Finding | Evidence | Task |
|---|---------|----------|------|
| F1 | `build()` open-codes the same `name_to_vn → ok_or_else → UnknownRegName(name.to_string())` pattern that `regs_to_vns` already encapsulates for slice elements — just for the single `stack_ptr_reg_name` case. Extracting a `vn_for_name(regs, name) -> Result<Vn>` helper that both call sites reuse centralises the error-construction boilerplate and lets `regs_to_vns` shrink to a single `.map()` over it. | [calling_convention.rs:9-18](crates/target/src/calling_convention.rs#L9-L18) (slice path), [calling_convention.rs:195-197](crates/target/src/calling_convention.rs#L195-L197) (SP path) | Task 1 |
| F2 | `BuiltCallingConvention` documents every field; `CallingConvention` documents only `stack_ptr_reg_name`, `stack_arg_offsets`, `ret_stack_pop` — `arg_passing_regs`, `callee_saved_regs`, `ret_val_regs` carry no doc comment at all. Because `CallingConvention` is the surface a future contributor reads when adding a preset (e.g. a new ABI), these gaps matter most where they're missing. Field semantics are identical to the resolved counterparts; cross-reference is enough. | [calling_convention.rs:32-34](crates/target/src/calling_convention.rs#L32-L34) | Task 2 |
| F3 | The `UnknownRegName` error message reads `"unknown register name by sleigh {0:?}"`. "by sleigh" is grammatically off — Sleigh isn't doing the unknowing, it's the lookup table the name was missing from. `"unknown sleigh register name {0:?}"` is the same words rearranged for grammar. Verified safe: no test or downstream crate matches against the formatted string (variant pattern matches use the payload only). | [error.rs:7](crates/target/src/error.rs#L7) | Task 3 |

### Considered and rejected

- **Reordering `BuiltCallingConvention` fields to match `CallingConvention` field order.** The two structs have slightly different orderings; aligning them would help diff input/output one-to-one but is pure cosmetic churn that touches every consumer that constructs `BuiltCallingConvention` field-by-field (e.g. [crates/pattern/tests/matching/support/graph.rs:66-92](crates/pattern/tests/matching/support/graph.rs#L66-L92)). Not worth the multi-crate edit.
- **Adding `#[non_exhaustive]` to `ErrorKind`.** Forward-looking but not motivated by current need — the enum has been single-variant across 4 review rounds and no downstream code does an exhaustive match against it that would benefit from the marker. Add when a second variant appears.
- **Dropping `Hash` derives on `SleighArch` / `CallingConvention` / `BuiltCallingConvention`.** Possibly unused outside the crate, but the cost of removing them (downstream breakage if anyone hashes them) outweighs the win of a tighter trait surface. Not the target crate's call to make.
- **Const-ifying the test `cases()` fixture into a `static CASES: &[Case]`.** `fn` pointers are valid in const since 1.61 so this is mechanically possible, but `cases()` is called 3 times in test code; the `Vec` allocation it produces is irrelevant and the `vec![...]` form reads more naturally inline.
- **Moving the `calling_convention` unit tests to `tests/`.** They access the private `regs_for` helper and the test-internal `Case` struct. Splitting unit (private-visibility) from integration (public-only) tests is intentional; `arch_smoke.rs` is the public-API integration suite, the unit tests stay where they are.
- **Adding doctest examples to `CallingConvention::build`.** Useful but requires constructing a real `rsleigh::SleighRegs` in a `# fn main() {}` block — non-trivial wiring for a doctest, and the public test in `presets_resolve_correct_register_sets` already serves as a runnable example. Skip.
- **Inlining `regs_to_vns` into `build()`.** Same as round 4 — three call sites, named helper documents intent. Task 1 goes the *opposite* direction (extracts a deeper helper) which is consistent with this rejection.
- **Sealing `BuiltCallingConvention` fields behind a constructor.** Same as round 4 — pattern tests construct it field-by-field; sealing forces a builder shim with no real win.
- All round 1–4 rejected items remain rejected (`Box<[Vn]>` instead of `Vec<Vn>`, runtime validation of `stack_arg_offsets`, adding `arm_be`/MIPS calling-convention presets, etc.).

---

## Task 1: Extract `vn_for_name` helper to dedupe name-to-vn lookups

Closes F1. After this task, every `name_to_vn → UnknownRegName` translation in the crate goes through one function. The slice-mapping helper `regs_to_vns` becomes a one-line wrapper; the SP lookup in `build()` becomes a single call.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:1-18](crates/target/src/calling_convention.rs#L1-L18) (helpers section)
- Modify: [crates/target/src/calling_convention.rs:191-206](crates/target/src/calling_convention.rs#L191-L206) (`build()` body)

- [ ] **Step 1: Replace the `regs_to_vns` definition with a `vn_for_name` helper plus a thin wrapper**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace lines 1–18:

```rust
use crate::error::{ErrorKind, Result};

/// Converts a slice of register name strings into their corresponding varnode
/// representations using the provided Sleigh register map.
///
/// Iterates over each name in `reg_names`, looks it up in `sleigh_regs`, and
/// returns the list of resolved varnodes in the same order.  Returns an error
/// the moment any name is not found.
fn regs_to_vns(reg_names: &[&str], sleigh_regs: &rsleigh::SleighRegs) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&reg_name| {
            sleigh_regs
                .name_to_vn(reg_name)
                .ok_or_else(|| ErrorKind::UnknownRegName(reg_name.to_string()).into())
        })
        .collect()
}
```

with:

```rust
use crate::error::{ErrorKind, Result};

/// Resolves a single Sleigh register name to its [`rsleigh::Vn`], or returns
/// [`ErrorKind::UnknownRegName`] if the name is not known.  Single source of
/// truth for the name-to-varnode error path.
fn vn_for_name(sleigh_regs: &rsleigh::SleighRegs, name: &str) -> Result<rsleigh::Vn> {
    sleigh_regs
        .name_to_vn(name)
        .ok_or_else(|| ErrorKind::UnknownRegName(name.to_string()).into())
}

/// Resolves a slice of Sleigh register names to varnodes in the same order.
/// Short-circuits on the first unknown name.
fn regs_to_vns(reg_names: &[&str], sleigh_regs: &rsleigh::SleighRegs) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&name| vn_for_name(sleigh_regs, name))
        .collect()
}
```

- [ ] **Step 2: Replace the open-coded SP lookup in `build()`**

In the `build()` method, replace:

```rust
        let stack_ptr_vn = sleigh_regs
            .name_to_vn(self.stack_ptr_reg_name)
            .ok_or_else(|| ErrorKind::UnknownRegName(self.stack_ptr_reg_name.to_string()))?;
```

with:

```rust
        let stack_ptr_vn = vn_for_name(sleigh_regs, self.stack_ptr_reg_name)?;
```

- [ ] **Step 3: Run the existing error-path tests to verify behaviour is unchanged**

Run: `cargo test -p target build_returns_error build_returns_error_for_unknown_stack_pointer_name -- --nocapture`
Expected: 3 PASS — `build_returns_error_for_unknown_register_name`, `build_returns_error_even_when_some_names_are_valid`, `build_returns_error_for_unknown_stack_pointer_name`. The third is critical: it pins that the SP path still surfaces `UnknownRegName` after the helper extraction.

- [ ] **Step 4: Run the full target suite**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS.

- [ ] **Step 5: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "refactor(target): extract vn_for_name helper, dedupe name-to-vn lookup"
```

---

## Task 2: Add field docs to `CallingConvention`'s three reg-list fields

Closes F2. Make `CallingConvention` field-doc coverage match `BuiltCallingConvention` so a contributor adding a new preset can read every field's semantic in place. Cross-reference the resolved counterpart instead of duplicating the multi-line semantics.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:32-34](crates/target/src/calling_convention.rs#L32-L34)

- [ ] **Step 1: Add doc comments to the three reg-list fields**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
    arg_passing_regs: &'static [&'static str],
    callee_saved_regs: &'static [&'static str],
    ret_val_regs: &'static [&'static str],
```

with:

```rust
    /// Sleigh register names for the ABI's argument-passing registers, in
    /// positional order.  Resolved into
    /// [`BuiltCallingConvention::arg_passing_regs`] by [`Self::build`].
    arg_passing_regs: &'static [&'static str],
    /// Sleigh register names for registers the callee must preserve across
    /// the call.  Resolved into [`BuiltCallingConvention::callee_saved_regs`]
    /// by [`Self::build`].  Excludes the stack pointer; SP's cross-call
    /// behaviour is expressed through [`Self::ret_stack_pop`].
    callee_saved_regs: &'static [&'static str],
    /// Sleigh register names for return-value registers, in positional order.
    /// Resolved into [`BuiltCallingConvention::ret_val_regs`] by
    /// [`Self::build`].
    ret_val_regs: &'static [&'static str],
```

- [ ] **Step 2: Build the docs cleanly**

Run: `cargo doc -p target --no-deps 2>&1 | tail -20`
Expected: no warnings about broken intra-doc links — every `BuiltCallingConvention::*` and `Self::*` reference resolves.

- [ ] **Step 3: Run the target suite (no behavioural change expected)**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS.

- [ ] **Step 4: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "docs(target): document CallingConvention's reg-list fields"
```

---

## Task 3: Reword `UnknownRegName` error message for grammar

Closes F3. `"unknown register name by sleigh {0:?}"` → `"unknown sleigh register name {0:?}"`. Same vocabulary, fixed grammar. The empty-string case remains visually distinct because of `:?` (renders as `""`).

**Files:**
- Modify: [crates/target/src/error.rs:7](crates/target/src/error.rs#L7)

- [ ] **Step 1: Update the `#[error]` format string**

In [crates/target/src/error.rs](crates/target/src/error.rs), replace:

```rust
    #[error("unknown register name by sleigh {0:?}")]
    UnknownRegName(String),
```

with:

```rust
    #[error("unknown sleigh register name {0:?}")]
    UnknownRegName(String),
```

- [ ] **Step 2: Run the error-path tests — they match on payload, not message, so they must still pass**

Run: `cargo test -p target build_returns_error -- --nocapture`
Expected: PASS for both `build_returns_error_for_unknown_register_name` and `build_returns_error_even_when_some_names_are_valid`. (The test bodies use `matches!(... ErrorKind::UnknownRegName(n) if n == bad_name)` — they care about the variant payload, not the rendered string.)

- [ ] **Step 3: Run the full target suite**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS.

- [ ] **Step 4: Run the full workspace tests — no other crate constructs or matches the message string, but verify**

Run: `cargo test --workspace 2>&1 | tail -20`
Expected: PASS workspace-wide.

- [ ] **Step 5: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/target/src/error.rs
git commit -m "docs(target): reword UnknownRegName message for grammar"
```

---

## Task 4: Final workspace sanity sweep

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
Expected: PASS if no other crate has a latent lint; otherwise any remaining warnings are outside this review's scope — flag to the reviewer, do not fix here.

---

## Out of scope (considered, rejected, or deferred)

All items from rounds 1–4's out-of-scope lists remain out of scope. Additionally:

- **`BuiltCallingConvention` field reorder to match `CallingConvention`.** Cosmetic, multi-crate consumer churn (pattern tests and others construct it field-by-field). Reject.
- **`#[non_exhaustive]` on `ErrorKind`.** No second variant in sight; add when needed.
- **Removing `Hash` derives.** Possibly unused outside the crate, but removal is breaking and not motivated.
- **Const-ifying the `cases()` test fixture.** Test-only allocation, no measurable cost.
- **Adding `CallingConvention::build` doctest.** Wiring a real `SleighRegs` for a doctest is heavyweight; existing public unit tests serve the same documentation role.
- **Splitting / moving the `calling_convention` unit tests.** Test split (private vs. public-API) is intentional and load-bearing.
