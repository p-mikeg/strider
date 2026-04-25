# Target Crate Review — Round 6 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Two small remaining items after rounds 1–5 of the `target` crate review. The crate is clippy-clean (`-D warnings`), all 6 unit + 8 smoke tests pass, ABI/register-set invariants are pinned, and field-doc coverage is complete. This round aligns one parameter-order inconsistency introduced in Round 5 and closes a real test-coverage gap on SP register sizing. No behaviour change at runtime.

**Architecture:** Two independently-committable changes followed by a workspace sanity sweep. Each change has its own task with focused verification.

**Tech Stack:** Rust, `rsleigh`, `strider-error`, `thiserror`.

---

## What the review found (findings → tasks)

| # | Finding | Evidence | Task |
|---|---------|----------|------|
| F1 | The two name-to-varnode helpers introduced in Round 5 disagree on parameter order: `vn_for_name(sleigh_regs, name)` puts the lookup-table first (idiomatic — `map.get(key)` rather than `key.get(map)`), but its caller `regs_to_vns(reg_names, sleigh_regs)` puts the lookup-table second. They are sibling functions and `regs_to_vns` exists only as a thin `.map()` over `vn_for_name`. Aligning them removes a small papercut for anyone reading both signatures together. | [calling_convention.rs:6](crates/target/src/calling_convention.rs#L6) (helper sig), [calling_convention.rs:14](crates/target/src/calling_convention.rs#L14) (wrapper sig) | Task 1 |
| F2 | `presets_resolved_registers_have_expected_size` iterates the three resolved register lists (`arg_passing_regs`, `callee_saved_regs`, `ret_val_regs`) and asserts each varnode's `.size` equals the architecture's natural word size, but it does NOT include `stack_ptr_vn`. A future preset that declared `stack_ptr_reg_name = "AX"` (16-bit) on an x86-64 case would not be caught by this test — only by downstream analyzer breakage. SP is a load-bearing field for `StackStoreDetect` and the analyzer's calling-convention wiring; its size must be the natural word size. Closing the gap is a one-line `.chain(...)` addition. | [calling_convention.rs:378-394](crates/target/src/calling_convention.rs#L378-L394) | Task 2 |

### Considered and rejected

- **Adding explanatory comments to `presets_resolve_correct_register_sets` about why arg/ret_val disjointness is NOT asserted.** RDX appears in both `arg_passing_regs` and `ret_val_regs` for x86_64 SysV (RDX is arg #3 *and* the high half of a 128-bit return), so the omission is intentional. A short comment would prevent a future contributor "fixing" it by adding a bogus disjoint check. But the next reader who tries it would have a failing test pointing them at x86-64, so the cost of the trap is bounded. Not worth a task this round; if it bites, add the comment then.
- **Adding a doc note on `assert_disjoint` clarifying why one-directional iteration is sufficient.** Set intersection is symmetric — `a ∩ b = ∅` iff `b ∩ a = ∅`. Mathematically obvious; not worth a comment.
- **Reordering `build()` to look up SP before the three reg lists (fail-fast on SP typos).** `build()` is called once at analyzer startup; saving up to ~30 register lookups on the error path is irrelevant. Cosmetic.
- **Sharing the `Sleigh::new(...).regs().unwrap()` boilerplate between the unit-test `regs_for` and the integration-test `assert_preset_resolves`.** Unit tests in `src/` and integration tests in `tests/` cannot easily share helper code without either a public API addition or a separate `tests/common/mod.rs` referenced from both — the cleanest path requires a `pub fn` that bloats the crate's public surface. Three lines of duplication are cheaper.
- All round 1–5 rejected items remain rejected (`Box<[Vn]>` vs `Vec<Vn>`, runtime validation of `stack_arg_offsets`, `#[non_exhaustive]` on `ErrorKind`, dropping `Hash` derives, const-ifying `cases()`, moving the unit tests to `tests/`, doctests on `build`, inlining `regs_to_vns`, sealing `BuiltCallingConvention` fields, reordering `BuiltCallingConvention` fields to match `CallingConvention`, additional ABI presets).

---

## Task 1: Align parameter order — `regs_to_vns` takes `sleigh_regs` first

Closes F1. Both name-to-varnode helpers now share the same `(sleigh_regs, names_or_name)` shape. The change is mechanical — flip the two parameters in the `regs_to_vns` definition and update the three call sites in `build()`.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:14-19](crates/target/src/calling_convention.rs#L14-L19) (helper signature + body)
- Modify: [crates/target/src/calling_convention.rs:202-205](crates/target/src/calling_convention.rs#L202-L205) (`build()` call sites)

- [ ] **Step 1: Flip `regs_to_vns` parameter order**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
/// Resolves a slice of Sleigh register names to varnodes in the same order.
/// Short-circuits on the first unknown name.
fn regs_to_vns(reg_names: &[&str], sleigh_regs: &rsleigh::SleighRegs) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&name| vn_for_name(sleigh_regs, name))
        .collect()
}
```

with:

```rust
/// Resolves a slice of Sleigh register names to varnodes in the same order.
/// Short-circuits on the first unknown name.
fn regs_to_vns(sleigh_regs: &rsleigh::SleighRegs, reg_names: &[&str]) -> Result<Vec<rsleigh::Vn>> {
    reg_names
        .iter()
        .map(|&name| vn_for_name(sleigh_regs, name))
        .collect()
}
```

- [ ] **Step 2: Update the three call sites in `build()`**

In the `build()` method, replace:

```rust
        let arg_passing_regs = regs_to_vns(self.arg_passing_regs, sleigh_regs)?;
        let callee_saved_regs = regs_to_vns(self.callee_saved_regs, sleigh_regs)?;
        let ret_val_regs = regs_to_vns(self.ret_val_regs, sleigh_regs)?;
```

with:

```rust
        let arg_passing_regs = regs_to_vns(sleigh_regs, self.arg_passing_regs)?;
        let callee_saved_regs = regs_to_vns(sleigh_regs, self.callee_saved_regs)?;
        let ret_val_regs = regs_to_vns(sleigh_regs, self.ret_val_regs)?;
```

- [ ] **Step 3: Run the full target suite — all behaviour preserved**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS.

- [ ] **Step 4: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 5: Workspace build (verify no other crate calls `regs_to_vns` — it's `fn`-private, so nothing should, but be sure)**

Run: `cargo build --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "refactor(target): align regs_to_vns parameter order with vn_for_name"
```

---

## Task 2: Cover `stack_ptr_vn` in the register-size assertion

Closes F2. Add `stack_ptr_vn` to the `.chain(...)` of the size-check loop so a future preset declaring an undersized SP name is caught at unit-test time.

**Files:**
- Modify: [crates/target/src/calling_convention.rs:378-394](crates/target/src/calling_convention.rs#L378-L394)

- [ ] **Step 1: Write the failing test by tightening the existing one**

In [crates/target/src/calling_convention.rs](crates/target/src/calling_convention.rs), replace:

```rust
    /// Every register resolved by a preset must have the architecture's
    /// natural word size.
    #[test]
    fn presets_resolved_registers_have_expected_size() {
        for c in cases() {
            let (built, _) = build_case(&c);
            for vn in built
                .arg_passing_regs
                .iter()
                .chain(&built.callee_saved_regs)
                .chain(&built.ret_val_regs)
            {
                assert_eq!(
                    vn.size, c.reg_size_bytes,
                    "{}: expected {}-byte register, got {vn:?}",
                    c.name, c.reg_size_bytes,
                );
            }
        }
    }
```

with:

```rust
    /// Every register resolved by a preset (including the stack pointer) must
    /// have the architecture's natural word size.  SP is included because
    /// `StackStoreDetect` and the analyzer's stack-arg machinery assume an
    /// SP-sized address — an undersized SP would silently miscompute offsets
    /// downstream and produce no diagnostic from this crate.
    #[test]
    fn presets_resolved_registers_have_expected_size() {
        for c in cases() {
            let (built, _) = build_case(&c);
            for vn in built
                .arg_passing_regs
                .iter()
                .chain(&built.callee_saved_regs)
                .chain(&built.ret_val_regs)
                .chain(std::iter::once(&built.stack_ptr_vn))
            {
                assert_eq!(
                    vn.size, c.reg_size_bytes,
                    "{}: expected {}-byte register, got {vn:?}",
                    c.name, c.reg_size_bytes,
                );
            }
        }
    }
```

- [ ] **Step 2: Run the targeted test to verify it still passes for all current presets**

Run: `cargo test -p target presets_resolved_registers_have_expected_size -- --nocapture`
Expected: PASS. All four presets (x86-64 SysV, x86 cdecl, ARM AAPCS, AArch64 AAPCS64) declare correctly-sized SP names (`RSP`, `ESP`, `sp`, `sp` respectively), so the new chain element does not perturb them. The new coverage is for *future* presets — the diff is the proof that SP is now included; the running test is the standing guard.

- [ ] **Step 3: Run the full target suite**

Run: `cargo test -p target`
Expected: 6 unit + 8 smoke = 14 PASS.

- [ ] **Step 4: Strict clippy sweep**

Run: `cargo clippy -p target --all-targets --no-deps -- -D warnings`
Expected: PASS with zero warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/target/src/calling_convention.rs
git commit -m "test(target): cover stack_ptr_vn in register-size assertion"
```

---

## Task 3: Final workspace sanity sweep

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

All items from rounds 1–5's out-of-scope lists remain out of scope. Additionally:

- **Adding explanatory comments to `presets_resolve_correct_register_sets` re. arg/ret_val overlap (RDX in both for x86-64 SysV).** The current test correctly omits this check; a future contributor who adds it would see a clear x86-64 failure. Not worth pre-empting.
- **Adding a doc note on `assert_disjoint` re. one-directional iteration.** Set intersection is symmetric; the asymmetric iteration is mathematically sufficient.
- **Reordering `build()` to look up SP first.** Cosmetic, no real benefit — `build()` runs once at startup.
- **Sharing Sleigh-construction boilerplate between unit and integration tests.** Cleanest path requires a `pub fn` that bloats the crate's public surface; three lines of duplication are cheaper.
- **Const-ifying or `static`-ing the `cases()` test fixture.** Already rejected in round 5; same reasoning applies.
