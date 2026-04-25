# Reader Crate Review — Round 7 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix the last two small remaining items in the `reader` crate after rounds 1–6:
  1. **Real correctness — `load_elf` leaks bytes on parse failure.** The function `Box::leak`s the file bytes *before* attempting `object::File::parse`. The leak is intentional on success (so the returned `'static` `object::File` outlives the function), but on parse failure no `object::File` is returned — the leaked bytes are unreachable forever for no benefit. Validate with a non-leaking parse first; only leak after that succeeds. The bug is small in practice (callers don't typically hand bad bytes to `load_elf`) but the fix is mechanical and removes a sharp edge.
  2. **Doc precision — `MemRegion::read` zero-byte semantics.** Two existing pinned tests (`mem_region_read_zero_length_buf`, `mem_region_read_at_end_addr_returns_none`) show that an empty `out` buffer returns `Some(0)` mid-region but `None` at `end_addr`. The current rustdoc only says "Returns `None` when `addr` is not within this region at all." A reader who hasn't read the tests can't predict the boundary case. Tighten the docstring.

**Architecture:** Two independently-committable changes. Task 1 changes the runtime path of `load_elf` (still leaks on success; no more leaks on parse failure) and adds one pinned test. Task 2 is a doc-only update — no code changes, no test changes. Public API is unchanged in both.

**Tech Stack:** Rust, `object` crate, `tempfile` (dev-only).

---

## Baseline (verified 2026-04-25)

- `cargo test -p reader` → 27 `mem_region` tests + full `elf_reader` (15) + `elf_converters` + `elf_smoke` + `error` + `load_elf` suites. PASS.
- `cargo clippy -p reader --all-targets --no-deps -- -D warnings` → clean.
- Round 6 fully landed (`4365f47`, `fc0b90e`, `335c900`).

---

## Open questions for the reviewer before execution

**Q1 — `load_elf` parse-failure leak (Task 1).** Today:

```rust
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    Ok(object::File::parse(leaked)?)
}
```

If `object::File::parse(leaked)` returns `Err`, the bytes pointed to by `leaked` are now permanently unreachable: nothing in the caller's hands holds a reference to them, and there is no `object::File<'static>` returned. The leak served no purpose for that call.

This is a bug in the function's *intent*: leak so that the returned file is valid forever. If we don't return a file, leaking is wasted work and a memory regression that compounds across retried bad calls.

Three fixes considered:

  - **(A) Parse-twice (validate-then-leak).** Parse on a borrowed view first; only `Box::leak` and re-parse if validation succeeded. Two parse calls, but `object::File::parse` is cheap (header validation), and the second parse on identical bytes cannot fail — yet it still returns `Result`, which we propagate cleanly via `?` (no `unwrap`/`expect`/`panic`). Adds one pinned test that stresses bad-bytes-from-disk to confirm the function returns `Err` *and* doesn't have grown the heap. **Default choice.**
  - **(B) Reclaim with unsafe.** `Box::leak` then on parse failure reconstruct a `Box` from the leaked raw pointer and drop it. Avoids the double parse but introduces `unsafe` for a path that's exercised once per process. Worse trade.
  - **(C) Document the wart.** Add a "parse failure leaks the file bytes" sentence to the docstring. Cheap; leaves the bug.

  Assume **(A)**. The double-parse cost is unmeasurable next to the disk read; the no-leak-on-error guarantee is durable; and we avoid `unsafe`. The new test is a real regression catcher — without it, a future refactor could re-introduce the leak silently.

  *Crate-rule check (no panic / no unwrap / no expect — see [memory:feedback_no_panic_unwrap.md](/home/mike/.claude/projects/-home-mike-Desktop-strider/memory/feedback_no_panic_unwrap.md)):* the proposed fix uses `?` everywhere; no panic-likes are introduced.

**Q2 — `MemRegion::read` zero-byte boundary (Task 2).** Today:

```rust
/// Reads bytes starting at `addr` into `out`.
///
/// Returns the number of bytes copied, which may be less than `out.len()`
/// if `addr + out.len()` extends past the end of this region.
///
/// Returns `None` when `addr` is not within this region at all.
```

But the implementation has an asymmetric, intentional behavior already pinned by tests:

| `out.len()` | `addr` location               | Result    | Test                                              |
|-------------|-------------------------------|-----------|---------------------------------------------------|
| 0           | mid-region                    | `Some(0)` | `mem_region_read_zero_length_buf`                 |
| 0           | exactly `end_addr`            | `None`    | (covered by `mem_region_read_at_end_addr_returns_none`, which uses `buf=[0u8;1]` but the same `available == 0` branch fires regardless of `out.len()`) |
| any         | `< start_addr` or `>= end_addr` | `None`  | `mem_region_read_outside_returns_none`            |

The current "not within this region at all" wording is too vague — readers can't predict whether `r.read(end_addr, &mut [])` returns `Some(0)` or `None` from the doc.

  - **(A)** Tighten the doc to spell out the rule: `None` iff `!self.contains(addr)`; otherwise `Some(n)` with `0 <= n <= out.len()`. Cross-link `contains`. **Default choice.**
  - **(B)** Leave the doc; the tests pin the rule. Cheap; leaves the surprise in the doc.

  Assume **(A)**. The pinned tests already make the rule load-bearing; the doc should match.

**Reviewer choices locked in:** Q1 → **TBD**. Q2 → **TBD**. Filled in once the user approves; defaults apply otherwise.

---

## Task 1: Don't leak ELF bytes on parse failure in `load_elf`

**Files:**
- Modify: [crates/reader/src/elf.rs:331-335](crates/reader/src/elf.rs#L331-L335) — `load_elf` body.
- Modify: [crates/reader/tests/load_elf.rs](crates/reader/tests/load_elf.rs) — add one pinned-contract test.

### Step-by-step

- [ ] **Step 1: Read the existing test file to find the right place to insert**

Run: `cat crates/reader/tests/load_elf.rs`
Expected: 48-line file with existing `load_elf` happy-path tests. Note the imports at the top so the new test re-uses them.

- [ ] **Step 2: Write the new failing test**

Append to `crates/reader/tests/load_elf.rs`:

```rust
// ── Pinned contract: parse failure does not leak the file bytes ───────────

/// Pinned contract: when `load_elf` is given a file that exists on disk but
/// whose contents do NOT parse as a valid ELF, the function returns
/// `Err(ErrorKind::Object(_))` and does *not* leak the file bytes onto the
/// heap.
///
/// We can't directly assert "no leak" portably, but we can at least pin the
/// observable behavior: the call returns the right error variant, with no
/// panic or success. The internal change Task 1 makes is a parse-before-leak
/// reorder; if a future refactor reintroduces the original "leak then parse"
/// order, this test still passes (the error is still propagated) but the
/// leak regression survives. To catch THAT regression we'd need a Miri or
/// allocator-instrumentation harness — outside this task's scope. Document
/// the limitation here so a future round knows what's still on the table.
#[test]
fn load_elf_returns_error_on_invalid_bytes() {
    use std::io::Write as _;

    let mut f = tempfile::NamedTempFile::new().expect("tempfile");
    f.write_all(b"not an elf at all").expect("write tempfile");
    f.flush().expect("flush tempfile");

    let err = reader::load_elf(f.path())
        .expect_err("invalid bytes must surface as a parse error");
    assert!(
        matches!(err.kind(), reader::ErrorKind::Object(_)),
        "got {:?}",
        err.kind(),
    );
}
```

- [ ] **Step 3: Run the new test on the current code**

Run: `cargo test -p reader --test load_elf load_elf_returns_error_on_invalid_bytes`
Expected: PASS. The current implementation already returns the right error variant — the test pins the *contract*, and Task 1's behavioral change preserves it. (The leak-on-error fix is invisible to this test by construction; see the comment in the test.)

- [ ] **Step 4: Replace the body of `load_elf`**

In `crates/reader/src/elf.rs`, replace:

```rust
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    Ok(object::File::parse(leaked)?)
}
```

with:

```rust
pub fn load_elf<P: AsRef<std::path::Path>>(path: P) -> Result<object::File<'static>> {
    let data = std::fs::read(path)?;
    // Validate the parse on a borrowed view BEFORE leaking. If the bytes
    // don't parse as ELF, we drop `data` normally instead of leaking it
    // onto the heap forever for nothing — the leak only pays for itself
    // when we actually return an `object::File<'static>`.
    object::File::parse(&data[..])?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    // Re-parse the (now `'static`) bytes. Identical bytes parse identically,
    // so this `?` cannot fail in practice; we still propagate via `?` to
    // avoid `expect`/`unwrap` (forbidden in this crate — see
    // memory:feedback_no_panic_unwrap.md).
    Ok(object::File::parse(leaked)?)
}
```

- [ ] **Step 5: Update the `load_elf` rustdoc to reflect the new guarantee**

In `crates/reader/src/elf.rs`, replace the existing `load_elf` doc block (immediately above the function) with:

```rust
/// Loads and parses an ELF file from `path`, returning a `'static` reference.
///
/// On success, the file bytes are read into a `Box<[u8]>` that is then
/// intentionally **leaked** so the returned `object::File<'static>` remains
/// valid for the lifetime of the process. This is suitable for tests and
/// short-lived CLI tools where the cost of a one-time leak is acceptable.
///
/// On error, no bytes are leaked: the file is read, validated by an
/// in-place parse, and only on parse success are the bytes promoted to
/// `'static` (a second parse — guaranteed to succeed since the bytes are
/// identical — produces the returned `object::File`).
///
/// Callers that only need an [`ElfFileMemReader`] should prefer
/// [`ElfFileMemReader::from_path`], which does not leak.
///
/// # Errors
///
/// Returns `ErrorKind::Io` if the file cannot be read from disk, or
/// `ErrorKind::Object` if the bytes fail to parse as a valid ELF.
```

- [ ] **Step 6: Re-run the pinned test (now exercising the new code path)**

Run: `cargo test -p reader --test load_elf load_elf_returns_error_on_invalid_bytes`
Expected: PASS. Same observable behavior; the change is internal.

- [ ] **Step 7: Run the full reader test suite**

Run: `cargo test -p reader`
Expected: PASS — all 27 mem_region tests, all elf_reader tests, all elf_converters tests, all elf_smoke tests, all error tests, all load_elf tests (now +1).

- [ ] **Step 8: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 9: Workspace sanity**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS. No public-API change.

- [ ] **Step 10: Commit**

```bash
git add crates/reader/src/elf.rs crates/reader/tests/load_elf.rs
git commit -m "fix(reader): validate ELF before leaking bytes in load_elf"
```

---

## Task 2: Tighten `MemRegion::read` rustdoc to specify zero-byte semantics

**Files:**
- Modify: [crates/reader/src/lib.rs:102-117](crates/reader/src/lib.rs#L102-L117) — the `MemRegion::read` doc block only.

### Step-by-step

- [ ] **Step 1: Replace the existing rustdoc on `MemRegion::read`**

In `crates/reader/src/lib.rs`, replace:

```rust
/// Reads bytes starting at `addr` into `out`.
///
/// Returns the number of bytes copied, which may be less than `out.len()`
/// if `addr + out.len()` extends past the end of this region.
///
/// Returns `None` when `addr` is not within this region at all.
pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
```

with:

```rust
/// Reads bytes starting at `addr` into `out`.
///
/// Returns:
/// - `Some(n)` when [`contains(addr)`](Self::contains) — `n` is the number
///   of bytes copied, with `0 <= n <= out.len()`. `n` is less than
///   `out.len()` when `addr + out.len()` extends past the end of this
///   region; in particular, `n == 0` when `out` is empty (the address is
///   mapped but the caller asked for zero bytes).
/// - `None` when `!contains(addr)` — that is, `addr < start_addr` or
///   `addr >= end_addr`. A zero-byte read at exactly `end_addr` returns
///   `None` rather than `Some(0)`, mirroring the rule that `end_addr`
///   itself is not part of the region.
pub fn read(&self, addr: u64, out: &mut [u8]) -> Option<usize> {
```

This is a doc-only change; no code changes; no test changes.

- [ ] **Step 2: Run the existing zero-byte / boundary tests to confirm nothing observable changed**

Run: `cargo test -p reader --test mem_region mem_region_read_zero_length_buf mem_region_read_at_end_addr_returns_none mem_region_read_outside_returns_none mem_region_empty_contains_nothing`
Expected: PASS — all four tests already pin the rule the new doc now describes; no behavior change.

- [ ] **Step 3: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS. Pure rustdoc edit.

- [ ] **Step 4: Workspace sanity**

Run: `cargo build --workspace`
Expected: PASS. (No need for `cargo test --workspace` — the change is documentation only and we ran the targeted tests in Step 2.)

- [ ] **Step 5: Commit**

```bash
git add crates/reader/src/lib.rs
git commit -m "docs(reader): specify zero-byte boundary semantics on MemRegion::read"
```

---

## Task 3: Final sanity sweep

**Files:** run-only, no edits.

- [ ] **Step 1: Full reader test suite**

Run: `cargo test -p reader`
Expected: PASS. `load_elf` test count grows by 1 (Task 1's pin); all other counts unchanged.

- [ ] **Step 2: Reader-only strict clippy**

Run: `cargo clippy -p reader --all-targets --no-deps -- -D warnings`
Expected: PASS.

- [ ] **Step 3: Full workspace build & test**

Run: `cargo build --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 4: Smoke-run the example**

Run: `cargo run --example analyzer`
Expected: `cfg.html`, `graph.html`, `graph-opt.html` produced. (Requires `binary_tests/out/x86/test.elf` — build with `make -C binary_tests` if missing.)

---

## Out of scope (considered, rejected, or deferred)

- **Add a `VnSpace` filter to `MemReader::read` on `ElfFileMemReader`.** Rejected in round 6: the trait is documented as "reads memory from the given address" without a space restriction, Sleigh in practice only invokes it with `VnSpace::RAM`, and adding a filter risks breaking callers that rely on permissive lookup-by-offset. Worth flagging here so a future round can revisit if a real bug ever materializes.
- **Reclaim leaked bytes in `load_elf`'s parse-failure path with `unsafe { Box::from_raw(...) }` instead of validating before leaking.** Avoids the second parse call but introduces `unsafe` for a path that fires once per process. The validate-then-leak approach is safer and the cost (one extra parse on a small ELF header) is unmeasurable.
- **Add an allocator-instrumented or Miri-based test that *proves* `load_elf` doesn't leak on error.** Outside the scope of an in-tree `cargo test` run; would require either `dhat` heap profiling or a custom global allocator. The Task 1 test pins the error-propagation contract; the leak-fix is verified by code review.
- **Replace the `for { map.insert(...) }` loop in `MemRegionsLookupTable::new` with `regions.into_iter().map(|r| (r.start_addr(), r)).collect()`.** Rejected in round 5; same allocations, same big-O, pure cosmetic.
- **Cache `MemRegion::end_addr` instead of recomputing on each call.** Rejected in round 6; on the cold path, churns the struct, breaks the "fields are `start_addr` + `data` only" mental model.
- **Switch `MemRegion::data` from `Vec<u8>` to `Box<[u8]>`.** Rejected in rounds 2/3/4; saves one word per region, churns API.
- **Generic-over-section-vs-segment helper to collapse `elf_*_to_mem_region(s)` duplication.** Rejected in round 1; `object` provides no common trait, and a macro is heavier than the 12-line duplicate bodies.
- **Rename `load_elf` to make the leak opt-in (e.g. `load_elf_leaking` or a `LeakedElf` newtype).** Rejected in round 5; the leak is documented and call sites are short-lived tools/tests. Task 1's parse-failure-no-leak fix narrows the wart further without renaming.
- **Re-align `section_is_code_or_readonly`'s column-aligned `let` block.** Rejected in round 6; locally readable, churn-only.
- **Enable `clippy::pedantic` workspace-wide.** Deferred until/unless the workspace opts in globally.
