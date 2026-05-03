# rsleigh 4.0.0 Migration Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Migrate the strider workspace from rsleigh ~3.0 to rsleigh 4.0.0 (origin/master), and replace three hand-rolled formatters with rsleigh-native `Display` / `*CtxFmt` APIs that 4.0.0 now ships.

**Architecture:** rsleigh 4.0.0 is consumed via the path dep `../rsleigh` which resolves through the `.worktrees/rsleigh` worktree (branch `strider-deps`, one cherry-picked commit on top of `origin/master` adding the `Sleigh::user_op_name` ffi that strider's `CallOtherElide` pass depends on). Strider's API breakage from the rsleigh bump is mostly transparent: `Insn.inputs` becoming a `SmallVec` is iter-compatible; `Vn` field renames (`addr.off` → `addr_off`, `addr.space` → `addr_space`) are caught at compile time; `MemReader::Err: std::error::Error` is automatic via `MemReaderErr` blanket impl. The three reimplementation replacements (`PyVn::__repr__`, `Cfg` dot dumper, `ir::dot::label::vn_to_display_name`) all delegate to rsleigh's new `Display` / `VnCtxFmt` / `InsnCtxFmt` impls.

**Tech Stack:** Rust 2024 edition workspace, rsleigh 4.0.0 (path dep), pyo3 + maturin (strider-py), pytest + uv (Python tests), expect-test for snapshots.

---

## File Structure

Files modified during migration (relative to `/home/mike/Desktop/strider/.worktrees/rsleigh-4/`):

| Crate         | Path                                          | Reason                                                               |
|---------------|-----------------------------------------------|----------------------------------------------------------------------|
| strider       | `crates/strider/src/strider/insn/mod.rs`      | Pre-existing; calls `sleigh.user_op_name(...)` (the cherry-picked api) |
| cfg           | `crates/cfg/src/cfg/dot.rs`                   | Replace `{:?}` opcode + hand-rolled vn list with `Insn::ctx_fmt`     |
| cfg (test)    | `crates/cfg/tests/dot_dumper.rs`              | New: pin `InsnCtxFmt` spelling in dot output                          |
| cfg (test)    | `crates/cfg/tests/vn_to_name.rs`              | Update: formerly-erroring paths now return fallback strings           |
| ir            | `crates/ir/src/dot/label.rs`                  | Replace bespoke per-space match with `Vn::ctx_fmt(sleigh, regs)`      |
| strider-py    | `crates/strider-py/src/sleigh.rs`             | Replace hand-rolled `PyVn::__repr__` with `format!("{}", inner)`      |
| strider-py    | `crates/strider-py/tests/python/test_sleigh.py` | New: pin Vn repr produced by rsleigh's `Display` impl                |

The rsleigh worktree (`/home/mike/Desktop/strider/.worktrees/rsleigh`) carries one commit on `strider-deps` ahead of `origin/master`:

| sha       | summary                                       |
|-----------|-----------------------------------------------|
| 8262e10   | add user-op name lookup to sleigh ffi (cherry-picked + adapted from canonical 4cfa593) |

The second canonical commit (`be44309 require Debug on MemReader::Err`) is intentionally **dropped**: rsleigh 4.0.0's `pub trait MemReaderErr: std::error::Error + 'static {}` already implies `Debug` (via `Error: Debug`), so the constraint is now upstream.

---

## Phase 1 — rsleigh prep (worktree at `.worktrees/rsleigh`)

### Task 1: Verify cherry-pick and rsleigh tests

**Files:**
- Inspect: `/home/mike/Desktop/strider/.worktrees/rsleigh/` (already at `8262e10`)

- [x] **Step 1: Confirm strider-deps is one commit ahead of origin/master**

```bash
git -C /home/mike/Desktop/strider/.worktrees/rsleigh log --oneline origin/master..HEAD
```

Expected: exactly one line, `8262e10 add user-op name lookup to sleigh ffi`.

- [x] **Step 2: Confirm `MemReader::Err: std::error::Error` is upstream (so we can skip be44309)**

```bash
grep -n 'pub trait MemReaderErr' /home/mike/Desktop/strider/.worktrees/rsleigh/src/mem_reader.rs
```

Expected: `pub trait MemReaderErr: std::error::Error + 'static {}` — `Error: Debug` so `Debug` is implied; `be44309` is now subsumed and we drop it.

- [x] **Step 3: Run rsleigh's own integration test suite from a tmp copy**

rsleigh's `Cargo.toml` has no `[workspace]` table; running `cargo test` from inside `.worktrees/rsleigh` confuses cargo because that dir is *physically* under strider's workspace root. Workaround for the test run only:

```bash
cp -r /home/mike/Desktop/strider/.worktrees/rsleigh /tmp/rsleigh_test_link
printf '\n[workspace]\n' >> /tmp/rsleigh_test_link/Cargo.toml
cd /tmp/rsleigh_test_link && cargo test --quiet
rm -rf /tmp/rsleigh_test_link
```

Expected: `test result: ok. 23 passed`.  The 23 includes the cherry-picked tests asserting `LOCK` (x86_64) and `setISAMode` (ARM Thumb) live in the user-op table.

- [x] **Step 4: No new commits on strider-deps; cherry-pick is final**

We do not push.

---

## Phase 2 — strider migration (worktree at `.worktrees/rsleigh-4`)

### Task 2: Capture API breakage and fix in dependency order

**Files:**
- Modify (already on disk in `feature/rsleigh-4` commit `a64eb5a`): every crate that touched `Vn`, `Insn.inputs`, `MemReader::Err`, `all_vns()` ordering. See `git show a64eb5a --stat` for the full inventory.

- [x] **Step 1: Run the breakage capture once**

```bash
cd /home/mike/Desktop/strider/.worktrees/rsleigh-4
cargo check --workspace 2>&1 | tee /tmp/rsleigh4_breakage.txt
```

Expected (post-`a64eb5a`): clean compile.

- [x] **Step 2: Per-crate test confirmation that the API repairs hold up**

```bash
cargo test --workspace
```

The fixes were authored together as commit `a64eb5a`; running the full suite is the green-bar that the migration is sound. Pre-existing failures (14 in pytest from bit_not / removed SubToAdd / snapshot drift) are tracked separately and out of scope for this migration.

### Task 3: Replace `PyVn::__repr__` with `format!("{}", inner)` (TDD)

**Files:**
- Test: `crates/strider-py/tests/python/test_sleigh.py`
- Modify: `crates/strider-py/src/sleigh.rs:197-205`

- [x] **Step 1: Write the failing snapshot test (Python)**

```python
def test_vn_repr_for_register_uses_rsleigh_display():
    arch = strider.SleighArch.x86_64()
    mem = strider.MemoryMap()
    sleigh = strider.Sleigh(arch, mem)
    rsp = sleigh.reg("RSP")
    assert rsp is not None
    # rsleigh Display: <space-shortcut>[0x<off>]:<size> — REGISTER shortcut is `%`.
    assert repr(rsp) == "%[0x20]:8"


def test_vn_repr_for_const_drops_space_prefix():
    const_space = strider.VnSpace.const_()
    vn = strider.Vn(const_space, 0x42, 4)
    # rsleigh Display: CONST renders without space prefix or brackets.
    assert repr(vn) == "0x42:4"
```

- [x] **Step 2: Run pytest — old impl fails on the new format**

Old impl produced `"Vn(space=VnSpace.register, off=0x20, size=8)"`; both new asserts fail.

- [x] **Step 3: Replace impl with rsleigh's Display**

```rust
fn __repr__(&self) -> String {
    // Delegate to rsleigh's `impl Display for Vn` (core_types.rs:139)
    // so the spelling tracks rsleigh upstream.  For a register varnode
    // this yields `<space-shortcut>[0x<off>]:<size>` (e.g. `%[0x20]:8`
    // for x86_64 RSP); for CONST-space, `0x<off>:<size>`.
    format!("{}", self.inner)
}
```

- [x] **Step 4: Re-run pytest — both new assertions pass**

### Task 4: Replace `cfg::dot` opcode formatting with `Insn::ctx_fmt` (TDD)

**Files:**
- Test: `crates/cfg/tests/dot_dumper.rs`
- Modify: `crates/cfg/src/cfg/dot.rs:67-86`

- [x] **Step 1: Write the failing pin test**

```rust
#[test]
fn dot_output_uses_rsleigh_insn_ctx_fmt() {
    let cfg = cfg_for("add");
    let s = dot_source(&cfg);
    assert!(
        s.contains("IntAdd R"),
        "expected `IntAdd R<...>` (rsleigh InsnCtxFmt spelling) in \
         the dot source for `add`; got:\n{s}",
    );
    assert!(
        !s.contains("IntAdd, R"),
        "the hand-rolled `<Opcode>, <Reg>` spelling must not appear; \
         the cfg dot dumper should delegate to InsnCtxFmt.\n\nfull dot:\n{s}",
    );
}
```

- [x] **Step 2: Run `cargo test -p cfg` — fails on `IntAdd R`**

- [x] **Step 3: Replace the bespoke loop with `insn.ctx_fmt(&self.0.sleigh, &regs)`**

```rust
let regs = self.0.sleigh.regs()?;
for insn in &node.insns {
    let insn_addr = insn.addr.machine_addr.addr;
    let pretty = insn.insn.ctx_fmt(&self.0.sleigh, &regs);
    write!(&mut label, "\\l{insn_addr:#x}: {pretty}")
        .map_err(anyhow::Error::from)?;
}
```

- [x] **Step 4: Re-run `cargo test -p cfg` — green**

### Task 5: Replace `ir::dot::label::vn_to_display_name` with `Vn::ctx_fmt` (TDD)

**Files:**
- Test: `crates/cfg/tests/vn_to_name.rs` (the test file lives in `cfg`'s tests because it constructs a real `Cfg` to obtain a `Sleigh`)
- Modify: `crates/ir/src/dot/label.rs:6-32`

- [x] **Step 1: Update the formerly-erroring tests to the new fallback strings**

```rust
#[test]
fn register_unknown_offset_falls_back_to_space_addr_size() {
    let cfg = real_cfg();
    let bogus = Vn { addr_off: 0xffff_ffff_ffff_ffff, addr_space: VnSpace::REGISTER, size: 1 };
    let resolved = vn_to_name(&cfg, &bogus).unwrap();
    assert_eq!(resolved, "register[0xffffffffffffffff]:1");
}

#[test]
fn unknown_space_byte_falls_back_to_shortcut_char() {
    let cfg = real_cfg();
    let exotic = Vn { addr_off: 0, addr_space: VnSpace::new(b'?'), size: 1 };
    let resolved = vn_to_name(&cfg, &exotic).unwrap();
    assert_eq!(resolved, "?[0x0]:1");
}
```

- [x] **Step 2: Run `cargo test -p cfg --test vn_to_name` — fails on the two new exact strings**

- [x] **Step 3: Replace the bespoke `match vn.addr_space` with `vn.ctx_fmt`**

```rust
pub fn vn_to_display_name<R: MemReader>(
    sleigh: &rsleigh::Sleigh<R>,
    vn: &rsleigh::Vn,
) -> anyhow::Result<String> {
    let regs = sleigh.regs()?;
    Ok(vn.ctx_fmt(sleigh, &regs).to_string())
}
```

- [x] **Step 4: Re-run `cargo test -p cfg --test vn_to_name` — green**

### Task 6: Commit the migration in two slices

**Files:** the two existing migration commits + the new reimplementation-replacement commit.

- [x] **Step 1: Migration commit** — `a64eb5a strider: migrate to rsleigh 4.0.0` (already in history; covers the API breakage repairs).

- [ ] **Step 2: Reimplementation-replacement commit** — covers Tasks 3-5 inclusive (one commit because the changes share a single rationale: "delegate to rsleigh-native formatters").

```bash
git add crates/cfg/src/cfg/dot.rs \
        crates/cfg/tests/dot_dumper.rs \
        crates/cfg/tests/vn_to_name.rs \
        crates/ir/src/dot/label.rs \
        crates/strider-py/src/sleigh.rs \
        crates/strider-py/tests/python/test_sleigh.py \
        docs/superpowers/specs/2026-05-03-rsleigh-4-migration-plan.md
git commit -m "strider: delegate three formatters to rsleigh 4.0.0 native APIs"
```

### Task 7: Verification

- [ ] **Step 1: Build**

```bash
cd /home/mike/Desktop/strider/.worktrees/rsleigh-4
cargo build --workspace
```

Expected: clean.

- [ ] **Step 2: Clippy**

```bash
cargo clippy --workspace -- -D warnings
```

Expected: clean.

- [ ] **Step 3: Test**

```bash
cargo test --workspace
```

Expected: all green.

- [ ] **Step 4: Pytest**

```bash
cd crates/strider-py && uv run pytest
```

Expected: same 14 pre-existing failures (bit_not variant / removed SubToAdd / snapshot drift), no new ones — minus the new Vn-repr tests, which pass.

---

## Open concerns

- **rsleigh `cargo test` requires a workspace-escape hack.** rsleigh's `Cargo.toml` lacks a `[workspace]` table and the worktree dir is physically nested under strider's workspace root, so cargo refuses to test in place. Documented workaround: copy to `/tmp/`, append `[workspace]`, run, delete. This is a worktree-layout artefact, not an upstream issue.

- **The pre-existing 14 pytest failures.** They stem from earlier strider work that has not yet been propagated through to the test fixtures (bit_not enum rename, removal of SubToAdd, several `.dot` snapshot drifts that were updated in the strider source but not re-snapshotted). Migration-orthogonal — flagged here for completeness so the reviewer doesn't blame this PR.
