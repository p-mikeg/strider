# CallOther Classification Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace strider's two-stage CallOther handling (emit-then-elide) with construction-time classification keyed on Sleigh user-op names; fix the BUG_ON / trap-instruction lift crashes (commit_creds, do_exit, …) on x86_64 and aarch64 Linux kernels.

**Architecture:** Single classification table in `target::user_ops::classify(name)`, consumed by two callers — `cfg::region_builder` (terminates trap regions on iteration 0 as `RegionTerminator::NoReturn`) and `ir::FunctionBuilder::build_call_other` (dispatches IR shape NoOp / NoReturn-terminal / Opaque, errors on unknown). `opt::CallOtherElide` and `NO_OP_USER_OPS` are deleted; tests migrate to real user-op names.

**Tech Stack:** Rust 1.x, anyhow, thiserror, petgraph (cfg), cranelift-entity (ir), rsleigh (Sleigh wrapper), pytest + maturin (strider-py).

**Spec:** `docs/superpowers/specs/2026-05-05-callother-classification-design.md`

---

## File Map

**Create:**
- `crates/target/src/user_ops.rs` — `UserOpClass` enum + `classify` table.
- `crates/ir/tests/call_other_classification.rs` — outcome unit tests.
- `crates/strider/tests/bug_on_lifts_cleanly.rs` — integration tests for x86 ud2 + aarch64 brk.

**Modify:**
- `crates/target/src/lib.rs` — `pub mod user_ops;`
- `crates/ir/src/error.rs` — add `UnknownUserOpError`.
- `crates/ir/src/builder/call.rs` — `CallOtherOutcome` enum, new `build_call_other`, helpers.
- `crates/ir/src/lib.rs` — export `CallOtherOutcome`.
- `crates/cfg/Cargo.toml` — add `target = { path = "../target" }`.
- `crates/cfg/src/cfg/types.rs` — add `RegionTerminator::NoReturn`.
- `crates/cfg/src/cfg/builder/region_builder.rs` — handle `Opcode::CallOther`.
- `crates/strider/src/strider/insn/mod.rs` — simplify `handle_call_other`.
- `crates/strider/src/strider/pipeline.rs` — drop `CallOtherElide` from destructive pipeline.
- `crates/strider/src/errors.rs` — re-export `UnknownUserOpError`.
- `crates/pattern/src/pat/builders/call.rs` — add `.name(s)` to `CallOtherPat`.
- `crates/pattern/tests/get_vn_with_callother_clobber.rs` — migrate to real names.
- `crates/pattern/tests/matching/support/graph.rs` — helper takes name.
- `crates/ir/src/builder/tests.rs` — migrate tests.
- `crates/ir/tests/call_other_conservative_clobber.rs` — migrate tests.
- `crates/opt/src/dead_branch/tests.rs` — migrate test.
- `crates/strider-py/src/errors.rs` — `UnknownUserOpError` exception.
- `crates/strider-py/src/pattern.rs` — wrap `.name(s)`.
- `crates/strider-py/strider/errors.pyi` + `pattern.pyi` — type stubs.
- `CLAUDE.md` — update `opt::CallOtherElide` references.

**Delete:**
- `crates/opt/src/call_other_elide/` — entire directory.

---

## Task 1: `target::user_ops` module + initial classification table

**Files:**
- Create: `crates/target/src/user_ops.rs`
- Modify: `crates/target/src/lib.rs`

- [ ] **Step 1: Add the module declaration**

Modify `crates/target/src/lib.rs` to add (in the right section among existing `pub mod` declarations):

```rust
pub mod user_ops;
```

- [ ] **Step 2: Write the failing test**

Create `crates/target/src/user_ops.rs` with this initial content (test only, no implementation yet):

```rust
//! Sleigh user-op (CallOther) classification table.  See
//! `docs/superpowers/specs/2026-05-05-callother-classification-design.md`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_noop_classifies_as_noop() {
        assert_eq!(classify("setISAMode"), Some(UserOpClass::NoOp));
        assert_eq!(classify("setEndianState"), Some(UserOpClass::NoOp));
    }

    #[test]
    fn known_trap_classifies_as_noreturn() {
        assert_eq!(
            classify("invalidInstructionException"),
            Some(UserOpClass::NoReturn),
        );
        assert_eq!(
            classify("SoftwareBreakpoint"),
            Some(UserOpClass::NoReturn),
        );
    }

    #[test]
    fn known_opaque_classifies_as_opaque() {
        assert_eq!(classify("cpuid"), Some(UserOpClass::Opaque));
        assert_eq!(classify("syscall"), Some(UserOpClass::Opaque));
        assert_eq!(classify("rdtsc"), Some(UserOpClass::Opaque));
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(classify("nonexistent_op_xyzzy_abc"), None);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p target user_ops --lib 2>&1 | tail -20`

Expected: compilation errors for `classify`, `UserOpClass`, `UserOpClass::NoOp`, etc. (all undefined).

- [ ] **Step 4: Implement the enum + table**

Add the production code at the top of `crates/target/src/user_ops.rs`, before the `#[cfg(test)] mod tests`:

```rust
/// What `ir::FunctionBuilder::build_call_other` does for a given
/// user-op name.  Also consulted by `cfg::region_builder` to know
/// whether to terminate a region on a `NoReturn` CallOther.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOpClass {
    /// True no-op in the IR's data-flow / control-flow / memory model.
    /// `build_call_other` skips the node entirely; control and memory
    /// pass through unchanged; the pcode insn's output varnode (if
    /// any) is ignored.  Cfg behaviour: same as today (insn stays in
    /// the region; loop continues).
    NoOp,

    /// Trap instruction whose semantic effect is "execution does not
    /// continue past this point" (Linux `BUG()` / `BUG_ON()` /
    /// `WARN()`-class).  `build_call_other` emits a CallOther node
    /// (with control + memory inputs only — no clobber outputs, no
    /// value output, dangling control + memory outputs).  Cfg
    /// behaviour: terminates the region as `RegionTerminator::NoReturn`
    /// before processing the trailing `BranchIndirect`.
    NoReturn,

    /// Known opaque user-op (cpuid, syscall, lock-prefix, …).
    /// `build_call_other` emits today's CallOther shape with the
    /// conservative "every tracked variable except SP" clobber set.
    /// Cfg behaviour: same as today.
    Opaque,
}

/// Look up a user-op name in the classification table.  Returns
/// `None` for unknown names.
///
/// Strict-on-emission policy: the IR builder converts `None` into
/// `UnknownUserOpError` (hard fail).  The cfg builder treats `None`
/// as "fall through to today's behaviour" (insn stays in the region;
/// loop continues) — the IR layer is the single strict gate.
///
/// Names are expected to be unambiguous across the arches we
/// currently support.  If a future name collides between arches with
/// different semantics, promote the API to
/// `(arch, name) -> UserOpClass`.
#[must_use]
pub fn classify(name: &str) -> Option<UserOpClass> {
    // ASCII-sorted within each group for diffability.
    match name {
        // ── No-ops (decoder context bits; no IR-visible effect) ──
        "setEndianState" => Some(UserOpClass::NoOp),
        "setISAMode"     => Some(UserOpClass::NoOp),

        // ── Noreturn traps (Linux BUG_ON / WARN-class) ──
        "invalidInstructionException" => Some(UserOpClass::NoReturn),
        "SoftwareBreakpoint"          => Some(UserOpClass::NoReturn),

        // ── Opaque (test-required + initial real-world set) ──
        "cpuid"   => Some(UserOpClass::Opaque),
        "syscall" => Some(UserOpClass::Opaque),
        "rdtsc"   => Some(UserOpClass::Opaque),

        _ => None,
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p target user_ops --lib 2>&1 | tail -10`

Expected: `running 4 tests ... test result: ok. 4 passed; 0 failed`.

- [ ] **Step 6: Commit**

```bash
git add crates/target/src/user_ops.rs crates/target/src/lib.rs
git commit -m "target: add user_ops classification module with initial table

Single-source-of-truth name → UserOpClass {NoOp, NoReturn, Opaque}
table for Sleigh CallOther dispatch.  Two callers in subsequent
commits: cfg::region_builder (region termination on NoReturn) and
ir::FunctionBuilder::build_call_other (IR-shape dispatch).

Spec: docs/superpowers/specs/2026-05-05-callother-classification-design.md"
```

---

## Task 2: `UnknownUserOpError` in `ir::error`

**Files:**
- Modify: `crates/ir/src/error.rs`

- [ ] **Step 1: Read the current error file**

Run: `cat /home/mike/Desktop/strider/crates/ir/src/error.rs`

Note the existing error pattern.

- [ ] **Step 2: Write the failing test**

Append to `crates/ir/src/error.rs`:

```rust
#[cfg(test)]
mod unknown_user_op_tests {
    use super::*;

    #[test]
    fn display_contains_name() {
        let e = UnknownUserOpError {
            name: "mystery".to_string(),
        };
        let s = format!("{e}");
        assert!(s.contains("mystery"), "got: {s}");
        assert!(s.contains("user-op"), "got: {s}");
    }

    #[test]
    fn anyhow_downcast_recovers_type() {
        let e: anyhow::Error = UnknownUserOpError {
            name: "mystery".to_string(),
        }
        .into();
        assert!(e.downcast_ref::<UnknownUserOpError>().is_some());
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p ir error::unknown_user_op_tests 2>&1 | tail -10`

Expected: `UnknownUserOpError` not defined.

- [ ] **Step 4: Add the error type**

Add to `crates/ir/src/error.rs`:

```rust
/// Returned by [`crate::FunctionBuilder::build_call_other`] when the
/// supplied user-op `name` has no entry in
/// [`target::user_ops::classify`].  Strict-on-emission policy: any
/// new user-op surfaced by a real lift must be classified before the
/// lift can succeed.
///
/// Recover via `anyhow::Error::downcast_ref::<UnknownUserOpError>()`.
#[derive(Debug, thiserror::Error)]
#[error("unknown CallOther user-op name {name:?}; \
         add an entry to target::user_ops::classify")]
pub struct UnknownUserOpError {
    pub name: String,
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ir error::unknown_user_op_tests 2>&1 | tail -10`

Expected: 2 passed.

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/error.rs
git commit -m "ir: add UnknownUserOpError type

Surfaced by FunctionBuilder::build_call_other (next commit) when
a user-op name is not in target::user_ops::classify.  Downcast-
friendly via anyhow."
```

---

## Task 3: `CallOtherOutcome` enum

**Files:**
- Modify: `crates/ir/src/builder/call.rs`
- Modify: `crates/ir/src/lib.rs`

- [ ] **Step 1: Add the enum**

At the top of `crates/ir/src/builder/call.rs` (after the existing `use` statements), add:

```rust
/// Outcome of [`FunctionBuilder::build_call_other`].
#[derive(Debug)]
#[must_use]
pub enum CallOtherOutcome {
    /// Classification was [`target::user_ops::UserOpClass::NoOp`].
    /// No IR node emitted; control / memory unchanged.
    NoOp,

    /// Classification was [`target::user_ops::UserOpClass::NoReturn`].
    /// A `NodeKind::CallOther` node was emitted with control + memory
    /// inputs only (no clobber outputs, no value output); its outputs
    /// dangle.  The cfg has already terminated the region on this
    /// CallOther (see `RegionTerminator::NoReturn`), so the per-region
    /// IR walk has nothing left to process.
    NoReturn,

    /// Classification was [`target::user_ops::UserOpClass::Opaque`].
    /// Today's behaviour: full CallOther with conservative clobber
    /// outputs and the optional value output.
    Built {
        node: crate::node::NodeId,
        value: Option<crate::node::NodeOutputId>,
    },
}
```

- [ ] **Step 2: Re-export from `ir::lib`**

Modify `crates/ir/src/lib.rs` to expose `CallOtherOutcome`. Following whatever convention the file uses for builder re-exports, add:

```rust
pub use builder::CallOtherOutcome;
```

- [ ] **Step 3: Compile check**

Run: `cargo build -p ir 2>&1 | tail -5`

Expected: clean compile (no behavioural change yet).

- [ ] **Step 4: Commit**

```bash
git add crates/ir/src/builder/call.rs crates/ir/src/lib.rs
git commit -m "ir: introduce CallOtherOutcome enum

Pending consumer in next commit.  Three variants mirror the three
UserOpClass values.  NoReturn carries no payload — the cfg has
already terminated the region by the time IR construction reaches
the trap CallOther."
```

---

## Task 4: New classified `build_call_other` (parallel API)

**Files:**
- Modify: `crates/ir/src/builder/call.rs`
- Modify: `crates/ir/Cargo.toml` (verify `target` dep)

- [ ] **Step 1: Verify `ir → target` dep exists**

Run: `grep -n target /home/mike/Desktop/strider/crates/ir/Cargo.toml | head -3`

Expected: at least one line referencing `target = ...`. If absent, add `target = { path = "../target" }` to `[dependencies]`.

- [ ] **Step 2: Read existing `build_call_other`**

Run: `sed -n '170,260p' /home/mike/Desktop/strider/crates/ir/src/builder/call.rs`

Note the body of `build_call_other_with_clobbers` — it becomes `build_call_other_opaque`'s implementation.

- [ ] **Step 3: Add the helper `build_call_other_opaque`**

Inside the existing `impl<...> FunctionBuilder<...>` block in `crates/ir/src/builder/call.rs`, immediately after `build_call_other_with_clobbers`, add:

```rust
    /// Internal: emit a CallOther node with the conservative clobber
    /// set (every tracked variable except SP).  Used by the new
    /// classified `build_call_other` for the `Opaque` arm.
    pub(crate) fn build_call_other_opaque(
        &mut self,
        user_op_id: u64,
        args: &[crate::node::NodeOutputId],
        output_ty: Option<crate::node::NodeOutputType>,
    ) -> crate::error::Result<(crate::node::NodeId, Option<crate::node::NodeOutputId>)> {
        // Body identical to today's build_call_other.
        let stack_ptr_vn = self.stack_ptr_vn;
        let clobber_vars: smallvec::SmallVec<[rsleigh::Vn; 8]> = self
            .variables
            .values()
            .copied()
            .filter(|v| Some(*v) != stack_ptr_vn)
            .collect();
        self.build_call_other_with_clobbers(user_op_id, args, output_ty, &clobber_vars)
    }
```

- [ ] **Step 4: Add the helper `build_call_other_terminal`**

Immediately after `build_call_other_opaque`, add:

```rust
    /// Internal: emit a CallOther node intended as a region
    /// terminator (Linux `BUG_ON`-class trap).  Has only ctrl +
    /// memory inputs and ctrl + memory outputs — no clobbers, no
    /// value, no args.  The outputs are expected to dangle: the
    /// cfg has already terminated the region with
    /// `RegionTerminator::NoReturn`, so no successor will read them.
    pub(crate) fn build_call_other_terminal(
        &mut self,
        user_op_id: u64,
    ) -> crate::error::Result<crate::node::NodeId> {
        use crate::node::{NodeKind, NodeOutputKind};
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;
        let mut output_kinds: smallvec::SmallVec<[NodeOutputKind; 4]> =
            smallvec::SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        let inputs = [ctrl, memory];
        let node = self.create_node(
            NodeKind::CallOther { user_op_id },
            inputs.into_iter(),
            output_kinds,
        );
        // Intentionally DO NOT call advance_cur_region_ctrl /
        // advance_cur_region_memory — outputs dangle.
        Ok(node)
    }
```

- [ ] **Step 5: Add the new classified entry point**

Inside the same `impl` block, add after `build_call_other_terminal`:

```rust
    /// Classified CallOther construction.  Looks up `name` in
    /// [`target::user_ops::classify`] and dispatches to the matching
    /// builder shape.
    ///
    /// # Errors
    /// Returns [`crate::error::UnknownUserOpError`] (via `anyhow`) if
    /// `name` has no entry in the classification table.
    pub fn build_call_other(
        &mut self,
        name: &str,
        user_op_id: u64,
        args: &[crate::node::NodeOutputId],
        output_ty: Option<crate::node::NodeOutputType>,
    ) -> crate::error::Result<CallOtherOutcome> {
        use target::user_ops::{classify, UserOpClass};
        let class = classify(name).ok_or_else(|| crate::error::UnknownUserOpError {
            name: name.to_string(),
        })?;
        match class {
            UserOpClass::NoOp => Ok(CallOtherOutcome::NoOp),
            UserOpClass::NoReturn => {
                let node = self.build_call_other_terminal(user_op_id)?;
                self.body_mut()
                    .graph
                    .set_call_other_name(node, name.to_string());
                Ok(CallOtherOutcome::NoReturn)
            }
            UserOpClass::Opaque => {
                let (node, value) =
                    self.build_call_other_opaque(user_op_id, args, output_ty)?;
                self.body_mut()
                    .graph
                    .set_call_other_name(node, name.to_string());
                Ok(CallOtherOutcome::Built { node, value })
            }
        }
    }
```

**This shadows the old `build_call_other(user_op_id, ...)`.** Rename the OLD method (the one taking `user_op_id` first, no `name`) to `build_call_other_legacy` and mark it `pub(crate)`. Body unchanged.

- [ ] **Step 6: Compile check**

Run: `cargo build -p ir --lib 2>&1 | tail -10`

Expected: clean compile. Other crates' callers will fail in subsequent tasks — that's OK.

- [ ] **Step 7: Write the unit tests**

Create `crates/ir/tests/call_other_classification.rs`:

```rust
//! Outcome-level tests for [`ir::FunctionBuilder::build_call_other`]'s
//! classification dispatch.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::{CallOtherOutcome, FunctionBuilder};
use ir::error::UnknownUserOpError;
use ir::node::{NodeKind, NodeOutputType};

fn make_builder() -> FunctionBuilder {
    FunctionBuilder::new_raw_for_test().expect("test builder")
}

#[test]
fn build_call_other_noop_skips_node() {
    let mut b = make_builder();
    let before = b.body().graph.node_count();
    let outcome = b
        .build_call_other("setISAMode", 7, &[], None)
        .expect("classify ok");
    assert!(matches!(outcome, CallOtherOutcome::NoOp), "got {outcome:?}");
    let after = b.body().graph.node_count();
    assert_eq!(before, after, "no node added for NoOp");
}

#[test]
fn build_call_other_noreturn_emits_terminal_node() {
    let mut b = make_builder();
    let outcome = b
        .build_call_other("invalidInstructionException", 8, &[], None)
        .expect("classify ok");
    let CallOtherOutcome::NoReturn = outcome else {
        panic!("expected NoReturn, got {outcome:?}")
    };
    let last_node = (b.body().graph.node_count() - 1) as u32;
    let nid = ir::node::NodeId::from_u32(last_node);
    let kind = b.body().graph.node_kind(nid);
    assert!(
        matches!(kind, NodeKind::CallOther { user_op_id: 8 }),
        "got {kind:?}",
    );
    let n_outputs = b.body().graph.node_outputs(nid).len();
    assert_eq!(n_outputs, 2, "terminal CallOther has only ctrl + mem outputs");
    assert_eq!(
        b.body().graph.call_other_name(nid),
        Some("invalidInstructionException"),
    );
}

#[test]
fn build_call_other_opaque_builds_with_clobbers_and_name() {
    let mut b = make_builder();
    let outcome = b
        .build_call_other("cpuid", 9, &[], Some(NodeOutputType::U32))
        .expect("classify ok");
    let CallOtherOutcome::Built { node, value } = outcome else {
        panic!("expected Built, got {outcome:?}")
    };
    assert!(value.is_some(), "value output present");
    let kind = b.body().graph.node_kind(node);
    assert!(matches!(kind, NodeKind::CallOther { user_op_id: 9 }), "got {kind:?}");
    let n_outputs = b.body().graph.node_outputs(node).len();
    assert!(n_outputs > 2, "opaque has clobber outputs after [ctrl, mem, value]");
    assert_eq!(b.body().graph.call_other_name(node), Some("cpuid"));
}

#[test]
fn build_call_other_unknown_name_errors() {
    let mut b = make_builder();
    let err = b
        .build_call_other("nonexistent_op_xyz_qqq", 0, &[], None)
        .unwrap_err();
    let downcast = err.downcast_ref::<UnknownUserOpError>();
    assert!(downcast.is_some(), "expected UnknownUserOpError, got: {err}");
    assert_eq!(downcast.unwrap().name, "nonexistent_op_xyz_qqq");
}
```

If `FunctionBuilder::new_raw_for_test()` doesn't exist, find the test-utility constructor used by other ir tests (`grep -n "new_raw\|new_for_test" crates/ir/src/builder/mod.rs`) and use that. As a fallback, copy the setup from `crates/ir/src/builder/tests.rs` into a private helper.

- [ ] **Step 8: Run the tests**

Run: `cargo test -p ir --test call_other_classification 2>&1 | tail -15`

Expected: 4 passed.

- [ ] **Step 9: Commit**

```bash
git add crates/ir/src/builder/call.rs crates/ir/src/lib.rs crates/ir/tests/call_other_classification.rs crates/ir/Cargo.toml
git commit -m "ir: add classified build_call_other(name, ...) entry point

Consults target::user_ops::classify and dispatches to one of three
shapes:
  * NoOp     → no node, ctrl/mem unchanged
  * NoReturn → terminal CallOther (ctrl + mem only, outputs dangle)
  * Opaque   → today's CallOther + conservative clobbers
Unknown user-op names error out with UnknownUserOpError.

Old build_call_other renamed to build_call_other_legacy (pub(crate));
Task 7 migrates the strider caller and Task 12 deletes the legacy
helper."
```

---

## Task 5: `cfg::RegionTerminator::NoReturn` variant + cfg → target dep

**Files:**
- Modify: `crates/cfg/Cargo.toml`
- Modify: `crates/cfg/src/cfg/types.rs`
- Modify: any cfg-side code that exhaustively matches on `RegionTerminator`
- Modify: `crates/cfg/src/dot.rs` if there's a renderer

- [ ] **Step 1: Add the target dep**

Modify `crates/cfg/Cargo.toml` — under `[dependencies]`, add:

```toml
target = { path = "../target" }
```

(Place it ASCII-sorted with the other path deps if that's the file's convention.)

- [ ] **Step 2: Find every exhaustive match on RegionTerminator**

Run:
```bash
grep -rn "match.*terminator" /home/mike/Desktop/strider/crates/cfg/src/ /home/mike/Desktop/strider/crates/strider/src/ /home/mike/Desktop/strider/crates/dot/src/ 2>&1
```

Note each match site — they may need a NoReturn arm.

- [ ] **Step 3: Add the variant**

In `crates/cfg/src/cfg/types.rs`, find `pub enum RegionTerminator` and add `NoReturn` immediately after `Return`:

```rust
pub enum RegionTerminator {
    // ... existing variants ...
    Return,
    /// Region terminates with no successor.  Emitted by
    /// `cfg::region_builder::process_new_insn` when a CallOther's
    /// `target::user_ops::classify` result is `NoReturn` (Linux
    /// `BUG()` / `BUG_ON()`-class traps: x86 `ud2`, aarch64
    /// `brk #imm`).  See
    /// `docs/superpowers/specs/2026-05-05-callother-classification-design.md`.
    NoReturn,
    // ... rest of variants ...
}
```

- [ ] **Step 4: Add NoReturn arms where match is exhaustive**

For each match site found in Step 2 that lacks a wildcard `_ =>` arm, add:

```rust
RegionTerminator::NoReturn => { /* no successor edge; no special handler */ }
```

Match sites with a wildcard need no change.

In `SpecialTerm::from_terminator` ([`crates/strider/src/strider/pipeline.rs`]), `NoReturn` falls through the existing `_ => None` arm. Verify by reading the function.

- [ ] **Step 5: Add a dot label if needed**

If `crates/cfg/src/dot.rs` (or the cfg dot dumper) has per-terminator labels, add a `"NoReturn"` label.

- [ ] **Step 6: Compile**

Run: `cargo build --workspace 2>&1 | tail -10`

Expected: clean compile.

- [ ] **Step 7: Run cfg tests**

Run: `cargo test -p cfg 2>&1 | tail -10`

Expected: existing tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/cfg/Cargo.toml crates/cfg/src/cfg/types.rs crates/cfg/src/dot.rs crates/strider/src/strider/pipeline.rs crates/dot/src/
git commit -m "cfg: add RegionTerminator::NoReturn + target dep

NoReturn is emitted by cfg's region builder (next commit) for trap
CallOthers classified as NoReturn in target::user_ops::classify.
cfg gains a direct dep on target (no cycle: target is leaf, cfg
already transitively depends on it via pcode-lift → ir → target)."
```

---

## Task 6: cfg `process_new_insn` — handle `Opcode::CallOther`

**Files:**
- Modify: `crates/cfg/src/cfg/builder/region_builder.rs`

- [ ] **Step 1: Read the current dispatch site**

Run: `sed -n '252,400p' /home/mike/Desktop/strider/crates/cfg/src/cfg/builder/region_builder.rs`

Locate the `match insn.opcode` block and the existing `_ => Ok(ProcessInsnRes::DidntFinishProcessing)` arm.

- [ ] **Step 2: Write the failing test**

Add to `crates/cfg/tests/region_builder_process.rs` (or a new file `crates/cfg/tests/region_terminates_on_noreturn_callother.rs` if cleaner):

```rust
//! cfg::region_builder terminates a region as RegionTerminator::NoReturn
//! when it sees a CallOther whose target::user_ops::classify is NoReturn.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use cfg::RegionTerminator;

#[test]
fn ud2_region_finishes_as_noreturn() {
    // x86_64 ud2 = 0x0F 0x0B.  Sleigh emits:
    //   pcode[0]: CallOther [user_op = invalidInstructionException]
    //   pcode[1]: BranchIndirect
    let arch = strider::SleighArch::x86_64();
    let bytes = vec![0x0fu8, 0x0b];
    let entry = 0x1000u64;
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");
    let any_noreturn = cfg.graph
        .node_weights()
        .any(|r| matches!(r.terminator, RegionTerminator::NoReturn));
    assert!(any_noreturn,
        "expected at least one NoReturn region; got terminators: {:?}",
        cfg.graph.node_weights().map(|r| &r.terminator).collect::<Vec<_>>(),
    );
    // The trailing BranchIndirect must NOT have been processed —
    // there should be no UnresolvedIndirectBranch terminator.
    let any_unresolved = cfg.graph.node_weights().any(|r| {
        matches!(r.terminator, RegionTerminator::UnresolvedIndirectBranch { .. })
    });
    assert!(!any_unresolved,
        "trap region should terminate as NoReturn before its trailing \
         BranchIndirect routes through process_branch_indirect",
    );
}
```

If `cfg.graph` is private outside the cfg crate, use whatever public iterator the cfg exposes (`cfg.region_ids()` + a `cfg.region(id)` accessor). If those don't exist either, add a small `pub fn region_terminators(&self) -> impl Iterator<Item = &RegionTerminator>` accessor on `Cfg` first.

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p cfg ud2_region_finishes_as_noreturn 2>&1 | tail -15`

Expected: assertion fails (current cfg routes the CallOther through DidntFinishProcessing; trailing BranchIndirect → UnresolvedIndirectBranch).

- [ ] **Step 4: Implement the CallOther arm**

In `crates/cfg/src/cfg/builder/region_builder.rs`, find the `match insn.opcode` block in `process_new_insn` and insert this arm BEFORE the final `_ =>` catch-all:

```rust
            rsleigh::Opcode::CallOther => {
                // Resolve the user-op id from the CONST input at
                // position 0.  Fall through to today's behaviour if
                // the input shape is unexpected — the IR layer's
                // strict-on-emission check will surface any real
                // problem with full context.
                let id_vn = match insn.inputs.first() {
                    Some(v) => v,
                    None => return Ok(ProcessInsnRes::DidntFinishProcessing),
                };
                if id_vn.addr_space != rsleigh::VnSpace::CONST {
                    return Ok(ProcessInsnRes::DidntFinishProcessing);
                }
                let id_u32 = match u32::try_from(id_vn.addr_off) {
                    Ok(v) => v,
                    Err(_) => return Ok(ProcessInsnRes::DidntFinishProcessing),
                };
                let name = self.builder.sleigh.user_op_name(id_u32);
                let class = name.and_then(target::user_ops::classify);
                if matches!(class, Some(target::user_ops::UserOpClass::NoReturn)) {
                    // CallOther is already in self.insns from the
                    // process_new_insn prologue push; finish_current_region
                    // carries it.  Trailing BranchIndirect is never decoded.
                    self.finish_current_region(RegionTerminator::NoReturn)?;
                    return Ok(ProcessInsnRes::FinishedProcessing);
                }
                Ok(ProcessInsnRes::DidntFinishProcessing)
            }
```

If `target::user_ops` import isn't already in scope in this file, add `use target::user_ops;` at the top.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p cfg ud2_region_finishes_as_noreturn 2>&1 | tail -15`

Expected: pass.

- [ ] **Step 6: Run all cfg tests**

Run: `cargo test -p cfg 2>&1 | tail -10`

Expected: all tests pass (the new arm is a strict superset of today's catch-all behaviour for non-NoReturn user-ops).

- [ ] **Step 7: Commit**

```bash
git add crates/cfg/src/cfg/builder/region_builder.rs crates/cfg/tests/region_terminates_on_noreturn_callother.rs
git commit -m "cfg: terminate region on NoReturn CallOther via target::user_ops

process_new_insn gains a typed Opcode::CallOther arm that resolves
the user-op name from Sleigh, consults target::user_ops::classify,
and finishes the region as RegionTerminator::NoReturn for trap-class
ops (Linux BUG_ON: x86 ud2, aarch64 brk #imm).  The trailing
Sleigh-emitted BranchIndirect is never decoded — no
UnresolvedIndirectBranch placeholder, no orchestrator failure.

NoOp / Opaque / unknown user-ops fall through to today's catch-all
(insn stays in the region; loop continues).  The IR layer's strict
gate handles unknowns with full context."
```

---

## Task 7: Migrate `handle_call_other` to classified builder

**Files:**
- Modify: `crates/strider/src/strider/insn/mod.rs`

- [ ] **Step 1: Read the current handle_call_other**

Run: `sed -n '109,165p' /home/mike/Desktop/strider/crates/strider/src/strider/insn/mod.rs`

- [ ] **Step 2: Replace handle_call_other body**

Replace the existing `handle_call_other` method body with:

```rust
    fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
        let id_vn = pcode_lift::first_input_or_err(insn)?;
        if id_vn.addr_space != rsleigh::VnSpace::CONST {
            anyhow::bail!(
                "opcode {:?} expects a CONST input at position 0",
                insn.opcode
            );
        }
        let user_op_id = id_vn.addr_off;
        let user_op_id_u32 = u32::try_from(user_op_id)
            .map_err(|_| anyhow::anyhow!(
                "CallOther user-op id {user_op_id:#x} exceeds u32"
            ))?;
        let name = self.cfg.sleigh.user_op_name(user_op_id_u32).ok_or_else(|| {
            anyhow::anyhow!(
                "CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table"
            )
        })?;
        let args: Vec<ir::Value> = if insn.inputs.len() > 1 {
            insn.inputs[1..]
                .iter()
                .map(|vn| self.read_vn(vn))
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        let output_ty: Option<ir::node::NodeOutputType> = match insn.output.as_ref() {
            Some(out_vn) => Some(out_vn.size.try_into()?),
            None => None,
        };
        match self.builder.build_call_other(name, user_op_id, &args, output_ty)? {
            ir::CallOtherOutcome::NoOp | ir::CallOtherOutcome::NoReturn => Ok(()),
            ir::CallOtherOutcome::Built { value, .. } => {
                if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), value) {
                    self.write_vn(out_vn, val)?;
                }
                Ok(())
            }
        }
    }
```

`is_known_no_op` lookup gone.  Two-builder dispatch gone.  Trailing `set_call_other_name` gone (the IR builder does it now).  Return type stays `Result<()>` — the cfg already terminated trap regions, so the IR walker has nothing extra to signal.

- [ ] **Step 3: Compile**

Run: `cargo build -p strider 2>&1 | tail -10`

Expected: clean compile.

- [ ] **Step 4: Run strider unit tests**

Run: `cargo test -p strider 2>&1 | tail -10`

Expected: existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider/src/strider/insn/mod.rs
git commit -m "strider: route handle_call_other through classified builder

handle_call_other now resolves the user-op name from Sleigh and
calls ir::FunctionBuilder::build_call_other(name, ...).  NoOp and
NoReturn outcomes write nothing; Built carries the optional value
output to bind to the pcode insn's output varnode.  is_known_no_op
shortcut and build_call_other_with_clobbers selection both gone."
```

---

## Task 8: Integration tests — x86_64 ud2 + aarch64 brk lift cleanly

**Files:**
- Create: `crates/strider/tests/bug_on_lifts_cleanly.rs`

- [ ] **Step 1: Verify the ud2 byte sequence**

Run: `printf '\x0f\x0b' | objdump -D -b binary -m i386:x86-64 - | tail -5`

Expected: a line containing `ud2`.

- [ ] **Step 2: Verify the brk byte sequence**

Run: `printf '\x00\x00\x21\xd4' | aarch64-linux-gnu-objdump -D -b binary -m aarch64 - | tail -5`

Expected: a line containing `brk\t#0x800`. If the disassembly differs, recompute (BRK encoding: bits 31:21 = 11010100001, bits 20:5 = imm16, bits 4:0 = 00000).

- [ ] **Step 3: Write the integration tests**

Create `crates/strider/tests/bug_on_lifts_cleanly.rs`:

```rust
//! Integration tests: trap-instruction (BUG_ON-class) regions lift
//! cleanly without UnresolvedIndirectBranch errors.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use cfg::RegionTerminator;

#[test]
fn x86_64_ud2_terminates_cleanly() {
    let arch = strider::SleighArch::x86_64();
    let regs = arch.probe_regs().expect("probe regs");
    let strider = strider::Strider::new(
        arch,
        regs,
        strider::CallingConvention::x86_64_systemv_abi(),
    )
    .expect("strider");

    let bytes = vec![0x0fu8, 0x0b]; // ud2
    let entry = 0x1000u64;
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");

    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    assert!(
        outcome.unresolved_branches.is_empty(),
        "ud2 produced {} unresolved branch(es); expected 0",
        outcome.unresolved_branches.len(),
    );
    assert!(
        cfg.graph.node_weights().any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "expected at least one NoReturn region in cfg",
    );
}

#[test]
fn aarch64_brk_terminates_cleanly() {
    let arch = strider::SleighArch::aarch64();
    let regs = arch.probe_regs().expect("probe regs");
    let strider = strider::Strider::new(
        arch,
        regs,
        strider::CallingConvention::aarch64_aapcs64(),
    )
    .expect("strider");

    // brk #0x800 = 0xD4210000 (LE: 00 00 21 D4)
    let bytes = vec![0x00u8, 0x00, 0x21, 0xd4];
    let entry = 0x1000u64;
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");

    let outcome = strider.analyze_cfg(&cfg).expect("analyze_cfg");
    assert!(
        outcome.unresolved_branches.is_empty(),
        "brk produced {} unresolved branch(es); expected 0",
        outcome.unresolved_branches.len(),
    );
    assert!(
        cfg.graph.node_weights().any(|r| matches!(r.terminator, RegionTerminator::NoReturn)),
        "expected at least one NoReturn region in cfg",
    );
}
```

If `cfg.graph` is private to the cfg crate, expose a small accessor (`Cfg::region_terminators(&self) -> impl Iterator<Item = &RegionTerminator>`) and call that instead.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p strider --test bug_on_lifts_cleanly 2>&1 | tail -15`

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/strider/tests/bug_on_lifts_cleanly.rs crates/cfg/src/cfg/mod.rs
git commit -m "strider: integration tests for ud2 + brk #0x800 trap lift

Both arches: a single trap insn lifts to a region whose terminator
is RegionTerminator::NoReturn; no unresolved indirect branches
surface.  Verifies the end-to-end fix for commit_creds, do_exit,
do_task_dead, __schedule, etc. on real Linux kernels."
```

---

## Task 9: Pattern surface — `CallOtherPat::name()`

**Files:**
- Modify: `crates/pattern/src/pat/builders/call.rs`

- [ ] **Step 1: Read the current builder**

Run: `sed -n '90,135p' /home/mike/Desktop/strider/crates/pattern/src/pat/builders/call.rs`

- [ ] **Step 2: Verify Pat::when and Match::root signatures**

Run: `grep -n "pub fn when\|pub fn root" /home/mike/Desktop/strider/crates/pattern/src/pat/mod.rs /home/mike/Desktop/strider/crates/pattern/src/matcher/match_result.rs 2>&1 | head -10`

Note the closure signature accepted by `Pat::when` and the return type of `Match::root`. The closure shape used in Step 4 below (`|m, g| ...`) may need adjusting to match.

- [ ] **Step 3: Replace the struct + impl + From-impl**

Replace the `CallOtherPat` struct and its `From` impl in `crates/pattern/src/pat/builders/call.rs`:

```rust
pub struct CallOtherPat {
    user_op_id: Option<u64>,
    name: Option<String>,
    args: Vec<(usize, Pat)>,
}

impl CallOtherPat {
    #[must_use]
    pub fn new() -> Self {
        Self {
            user_op_id: None,
            name: None,
            args: Vec::new(),
        }
    }
    /// Constrain the matched node to a specific user-op id (Sleigh's
    /// numeric handle).  Combinable with `.name(...)`.
    #[must_use]
    pub fn user_op_id(mut self, v: u64) -> Self {
        self.user_op_id = Some(v);
        self
    }
    /// Constrain the matched node's user-op name (read from
    /// [`ir::Graph::call_other_name`]) to equal `n`.  Combinable
    /// with `.user_op_id(...)` (both must match) and `.arg(...)`.
    #[must_use]
    pub fn name(mut self, n: impl Into<String>) -> Self {
        self.name = Some(n.into());
        self
    }
    /// Constrain argument at position `idx` (0-based, after ctrl and
    /// mem inputs).
    pub fn arg(mut self, idx: usize, p: impl Into<Pat>) -> Self {
        self.args.push((idx, p.into()));
        self
    }
}

impl From<CallOtherPat> for Pat {
    fn from(b: CallOtherPat) -> Pat {
        let CallOtherPat { user_op_id, name, args } = b;
        let indexed_inputs: Vec<(usize, Pat)> =
            args.into_iter().map(|(i, p)| (2 + i, p)).collect();
        let exemplar = NodeKind::CallOther { user_op_id: 0 };
        let kind = match user_op_id {
            None => KindSpec::variant(&exemplar),
            Some(expected) => KindSpec::variant_with(&exemplar, move |k| {
                matches!(k, NodeKind::CallOther { user_op_id } if *user_op_id == expected)
            }),
        };
        let pat: Pat =
            NodePat::matcher(kind, InputsSpec::Indexed(indexed_inputs)).into_pat();
        match name {
            None => pat,
            Some(want) => pat.when(move |m, g| {
                g.graph
                    .call_other_name(m.root())
                    .is_some_and(|s| s == want.as_str())
            }),
        }
    }
}
```

(Adjust the closure signature to whatever `Pat::when` actually expects, per Step 2.)

- [ ] **Step 4: Write the unit test**

Add to a new file `crates/pattern/tests/call_other_name_filter.rs`:

```rust
//! CallOtherPat::name(s) matches by user-op name (from the side
//! table), not by the user_op_id field.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use ir::FunctionBuilder;
use pattern::{Matcher, pat};

#[test]
fn name_matches_only_target() {
    let mut b = FunctionBuilder::new_raw_for_test().expect("builder");
    let _ = b.build_call_other("cpuid", 1, &[], None).expect("cpuid");
    let _ = b.build_call_other("rdtsc", 2, &[], None).expect("rdtsc");
    let fg = b.build().expect("build");
    let matches = Matcher::new(&fg)
        .find_all(&pat::call_other().name("cpuid").into());
    assert_eq!(matches.len(), 1, "should match exactly the cpuid CallOther");
}
```

(Adjust the import path of `call_other()` to whatever the pattern crate uses today; `grep -n "pub fn call_other" crates/pattern/src/lib.rs crates/pattern/src/pat/mod.rs`.)

- [ ] **Step 5: Run pattern tests**

Run: `cargo test -p pattern 2>&1 | tail -15`

Expected: existing tests pass + new test passes.

- [ ] **Step 6: Commit**

```bash
git add crates/pattern/src/pat/builders/call.rs crates/pattern/tests/call_other_name_filter.rs
git commit -m "pattern: add CallOtherPat::name(s) for user-op name matching

Wraps a Pat::when guard reading Graph::call_other_name(node).
Combinable with .user_op_id() and .arg().  Common queries like
pattern::call_other().name(\"cpuid\") become natural."
```

---

## Task 10: Migrate IR builder tests to real names

**Files:**
- Modify: `crates/ir/src/builder/tests.rs`
- Modify: `crates/ir/tests/call_other_conservative_clobber.rs`

- [ ] **Step 1: Find every fake-id site**

Run: `grep -n "build_call_other(" /home/mike/Desktop/strider/crates/ir/src/builder/tests.rs /home/mike/Desktop/strider/crates/ir/tests/call_other_conservative_clobber.rs`

Note each `build_call_other(<u64>, ...)` call.

- [ ] **Step 2: Replace each call site**

For each call site, change the form `build_call_other(7, &[], None)` to `build_call_other("cpuid", 7, &[], None)`. Use `"cpuid"` for every site (or pick distinct Opaque names if a test needs to disambiguate two calls).

For sites using `build_call_other_with_clobbers`, leave them alone — the helper stays available as `pub(crate)` and is reachable from same-crate tests.

- [ ] **Step 3: Compile + run**

Run: `cargo test -p ir 2>&1 | tail -10`

Expected: all tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ir/src/builder/tests.rs crates/ir/tests/call_other_conservative_clobber.rs
git commit -m "ir: migrate CallOther test sites to real user-op names

build_call_other now requires a name in target::user_ops's
table; tests previously used synthetic IDs (7, 0xdead) and now
use real Opaque entries (\"cpuid\")."
```

---

## Task 11: Migrate pattern + opt test sites to real names

**Files:**
- Modify: `crates/pattern/tests/get_vn_with_callother_clobber.rs`
- Modify: `crates/pattern/tests/matching/support/graph.rs`
- Modify: `crates/opt/src/dead_branch/tests.rs`

- [ ] **Step 1: Find every fake-id site**

Run: `grep -n "build_call_other(\|build_call_other_with_clobbers" /home/mike/Desktop/strider/crates/pattern/tests/get_vn_with_callother_clobber.rs /home/mike/Desktop/strider/crates/pattern/tests/matching/support/graph.rs /home/mike/Desktop/strider/crates/opt/src/dead_branch/tests.rs`

- [ ] **Step 2: Update the test helper signature**

In `crates/pattern/tests/matching/support/graph.rs` (around line 322), update the helper to take a `name: &str` parameter and forward it:

```rust
pub fn callother_node(
    b: &mut FunctionBuilder,
    name: &str,
    user_op_id: u64,
    args: &[NodeOutputId],
    ret_ty: Option<NodeOutputType>,
) -> ... {
    b.build_call_other(name, user_op_id, args, ret_ty)
}
```

(Match the existing helper's exact return type — `Result<...>` or `(NodeId, Option<NodeOutputId>)` etc.)

- [ ] **Step 3: Update all callers of the helper**

Run the grep again to enumerate every caller of `callother_node`. For each, prepend `"cpuid"` (or another Opaque name).

- [ ] **Step 4: Update the dead_branch test**

In `crates/opt/src/dead_branch/tests.rs:286`, change `b.build_call_other(0, &[], None)` to `b.build_call_other("cpuid", 0, &[], None)`. The assertion shapes should still hold.

- [ ] **Step 5: Compile + run**

Run: `cargo test -p pattern -p opt 2>&1 | tail -15`

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/pattern/tests crates/opt/src/dead_branch/tests.rs
git commit -m "pattern, opt: migrate CallOther test sites to real names

Mirrors the ir-side migration.  Test helper in
pattern/tests/matching/support/graph.rs grows a name parameter."
```

---

## Task 12: Delete `opt::CallOtherElide` + `NO_OP_USER_OPS`

**Files:**
- Delete: `crates/opt/src/call_other_elide/`
- Modify: `crates/opt/src/lib.rs`
- Modify: `crates/strider/src/strider/pipeline.rs`
- Modify: `CLAUDE.md`

- [ ] **Step 1: Verify no remaining users**

Run: `grep -rn "CallOtherElide\|NO_OP_USER_OPS" /home/mike/Desktop/strider/crates/ /home/mike/Desktop/strider/CLAUDE.md`

Expected hits: `crates/opt/` (the module) + `crates/strider/src/strider/pipeline.rs` + `CLAUDE.md`. Nothing else.

- [ ] **Step 2: Delete the module**

```bash
rm -r /home/mike/Desktop/strider/crates/opt/src/call_other_elide
```

- [ ] **Step 3: Remove the module declaration**

In `crates/opt/src/lib.rs`, remove the line `pub mod call_other_elide;` (or `mod call_other_elide;` followed by a `pub use call_other_elide::*;`).

- [ ] **Step 4: Drop CallOtherElide from the destructive pipeline**

In `crates/strider/src/strider/pipeline.rs`, find `build_destructive_optimizer_pipeline` (around line 184) and remove the line that adds `opt::CallOtherElide`.

- [ ] **Step 5: Update CLAUDE.md**

Edit `CLAUDE.md`:
- Remove `CallOtherElide` from the list of optimizer passes ("Passes:" bullet list).
- Remove the `destructive_default_pipeline()` mention of `CallOtherElide`.
- Add a sentence in the relevant section about construction-time CallOther classification, citing `target::user_ops` and the spec.

- [ ] **Step 6: Compile + run all tests**

Run: `cargo build --workspace 2>&1 | tail -10 && cargo test --workspace 2>&1 | tail -15`

Expected: clean build, all tests pass.

- [ ] **Step 7: Commit**

```bash
git add -A crates/opt/ crates/strider/src/strider/pipeline.rs CLAUDE.md
git commit -m "opt: delete CallOtherElide pass + NO_OP_USER_OPS

Subsumed by construction-time classification in
target::user_ops::classify (consulted by cfg::region_builder for
region termination + ir::FunctionBuilder::build_call_other for IR
shape).  Strider's destructive pipeline no longer references
CallOtherElide; CLAUDE.md updated accordingly."
```

---

## Task 13: Delete `build_call_other_legacy` + demote `_with_clobbers`

**Files:**
- Modify: `crates/ir/src/builder/call.rs`

- [ ] **Step 1: Verify no remaining users of the legacy method**

Run: `grep -rn "build_call_other_legacy" /home/mike/Desktop/strider/crates/`

Expected: only the implementation itself in `crates/ir/src/builder/call.rs`.

- [ ] **Step 2: Delete `build_call_other_legacy`**

In `crates/ir/src/builder/call.rs`, delete the renamed legacy method.

- [ ] **Step 3: Demote `build_call_other_with_clobbers`**

Find the method (around line 203) and change `pub fn build_call_other_with_clobbers` → `pub(crate) fn build_call_other_with_clobbers`. Update its rustdoc to mention it's an internal helper for `build_call_other_opaque`.

- [ ] **Step 4: Compile + run all tests**

Run: `cargo build --workspace 2>&1 | tail -10 && cargo test --workspace 2>&1 | tail -15`

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ir/src/builder/call.rs
git commit -m "ir: drop legacy build_call_other entry point

build_call_other(user_op_id, ...) (the unclassified one renamed to
_legacy in Task 4) is gone now that all callers use the classified
form build_call_other(name, ...).  build_call_other_with_clobbers
demoted to pub(crate) — only the new build_call_other_opaque uses it."
```

---

## Task 14: strider-py exposure for `UnknownUserOpError` + `CallOtherPat::name`

**Files:**
- Modify: `crates/strider-py/src/errors.rs`
- Modify: `crates/strider-py/src/pattern.rs`
- Modify: `crates/strider-py/strider/errors.pyi`
- Modify: `crates/strider-py/strider/pattern.pyi`

- [ ] **Step 1: Read the current errors module**

Run: `cat /home/mike/Desktop/strider/crates/strider-py/src/errors.rs | head -80`

Note the registration pattern for existing errors (e.g., `UnresolvedIndirectBranchError`).

- [ ] **Step 2: Add the new exception class**

Following the existing convention in `crates/strider-py/src/errors.rs`, add `UnknownUserOpError` as a `StriderError` subclass and map `ir::error::UnknownUserOpError` to it via the existing downcast-and-raise path.

- [ ] **Step 3: Update the .pyi stub**

Add to `crates/strider-py/strider/errors.pyi`:

```python
class UnknownUserOpError(StriderError):
    name: str
```

- [ ] **Step 4: Add `.name(s)` to the Python CallOther pattern**

In `crates/strider-py/src/pattern.rs`, find the `CallOtherPat` Python wrapper (search for `call_other`). Add a `name(self, n: str) -> Self` method that forwards to the Rust builder's `.name(n)`.

Update `crates/strider-py/strider/pattern.pyi`:

```python
class CallOtherPat:
    def name(self, n: str) -> CallOtherPat: ...
```

- [ ] **Step 5: Build the wheel and run Python tests**

```bash
cd /home/mike/Desktop/strider/crates/strider-py
.venv/bin/maturin develop 2>&1 | tail -10
.venv/bin/python -m pytest tests/python -x 2>&1 | tail -20
```

Expected: clean develop, existing pytest suite passes.

- [ ] **Step 6: Add a Python test for the new builder**

Append to a relevant existing test file in `crates/strider-py/tests/python/`:

```python
def test_call_other_pat_name_smoke():
    from strider.pattern import call_other
    p = call_other().name("cpuid")
    # builder accepts name and converts to a Pat without error
    assert p is not None
```

- [ ] **Step 7: Commit**

```bash
git add crates/strider-py/
git commit -m "strider-py: expose UnknownUserOpError + CallOtherPat.name

Mirrors the new ir + pattern surface for Python consumers
(bsdfinder + downstream).  UnknownUserOpError is a StriderError
subclass; CallOtherPat gains a fluent .name(s) method."
```

---

## Task 15: Bsdfinder corpus check — harvest unknown user-ops

**Files:**
- Modify: `crates/target/src/user_ops.rs` (incrementally, with each new entry)

- [ ] **Step 1: Run a focused harvest script**

Save as `/tmp/harvest_unknown_user_ops.py`:

```python
import os
import glob
import strider, strider.errors

KERNELS = {
    'x86_64':  '/home/mike/Desktop/bsdfinder/kernels/linux/x86_64/4.19.0-amd64/vmlinux',
    'aarch64': '/home/mike/Desktop/bsdfinder/kernels/linux/aarch64/4.19.0-arm64/vmlinux',
}

unknown = set()

for arch_name, kernel_path in KERNELS.items():
    if not os.path.exists(kernel_path):
        continue
    if arch_name == 'x86_64':
        sleigh = strider.SleighArch.x86_64()
        cc = strider.CallingConvention.x86_64_linux_kernel()
    else:
        sleigh = strider.SleighArch.aarch64()
        cc = strider.CallingConvention.aarch64_linux_kernel()
    mem = strider.MemoryMap()
    mem.add_region_from_elf(kernel_path)
    try: mem.apply_elf_relocations(kernel_path)
    except Exception: pass
    syms = mem.symbols()
    per_addr = {}
    if arch_name == 'x86_64':
        ap = strider.CallingConvention.x86_64_all_preserving()
        for stub in ('__fentry__', 'mcount'):
            if stub in syms:
                per_addr[syms[stub]] = ap
    # Sample ~500 symbols for breadth without taking forever.
    sample = list(syms.items())[:500]
    for sym, addr in sample:
        try:
            max_size = mem.function_max_size(sym)[1]
        except Exception:
            max_size = None
        try:
            strider.run(arch=sleigh, cc=cc, mem=mem, rom=mem, entry=addr,
                        allow_code_before_start_addr=True,
                        function_max_size=max_size,
                        per_address_ccs=per_addr)
        except strider.errors.UnknownUserOpError as e:
            unknown.add(e.name)
        except Exception:
            pass

print(f'unknown user-op names ({len(unknown)}):')
for n in sorted(unknown):
    print(f'  {n}')
```

Run: `/home/mike/Desktop/strider/crates/strider-py/.venv/bin/python /tmp/harvest_unknown_user_ops.py 2>&1 | tail -30`

- [ ] **Step 2: Add each discovered name to the table as Opaque**

For each name in the output, append a line to `crates/target/src/user_ops.rs`'s `classify` match (in the `Opaque` group, ASCII-sorted):

```rust
"<name>" => Some(UserOpClass::Opaque),
```

- [ ] **Step 3: Re-run the harvest**

Re-run the script. If new unknown names appear (because earlier failures masked later ones), add them too. Iterate until the harvest returns 0 unknowns.

- [ ] **Step 4: Verify trap fix end-to-end on real kernels**

Save as `/tmp/verify_trap_fix.py`:

```python
import strider, strider.errors

mem = strider.MemoryMap()
mem.add_region_from_elf('/home/mike/Desktop/bsdfinder/kernels/linux/x86_64/4.19.0-amd64/vmlinux')
try: mem.apply_elf_relocations('/home/mike/Desktop/bsdfinder/kernels/linux/x86_64/4.19.0-amd64/vmlinux')
except Exception: pass
syms = mem.symbols()
ap = strider.CallingConvention.x86_64_all_preserving()
per_addr = {syms[s]: ap for s in ('__fentry__', 'mcount') if s in syms}
for sym in ('commit_creds', 'do_exit', 'do_task_dead', '__schedule', '__alloc_pages_nodemask'):
    addr = syms[sym]
    try:
        max_size = mem.function_max_size(sym)[1]
    except Exception:
        max_size = None
    try:
        strider.run(arch=strider.SleighArch.x86_64(),
                    cc=strider.CallingConvention.x86_64_linux_kernel(),
                    mem=mem, rom=mem, entry=addr,
                    allow_code_before_start_addr=True,
                    function_max_size=max_size,
                    per_address_ccs=per_addr)
        print(f'{sym}: OK')
    except Exception as e:
        print(f'{sym}: FAIL {type(e).__name__}: {e}')
```

Run: `/home/mike/Desktop/strider/crates/strider-py/.venv/bin/python /tmp/verify_trap_fix.py 2>&1 | tail -10`

Expected: all 5 functions print `OK`. Repeat with the aarch64 kernel & aarch64 CCs (substitute paths and CC factories).

- [ ] **Step 5: Commit each batch**

```bash
git add crates/target/src/user_ops.rs
git commit -m "target: add <N> Opaque user-op entries from bsdfinder corpus

Names harvested by sweeping kernels/linux/*/*/vmlinux with strict-
on-emission classification.  Each entry corresponds to a Sleigh
user-op that real Linux kernel code emits."
```

(Multiple commits across iterations are fine.)

---

## Task 16: Final workspace test sweep

- [ ] **Step 1: Run the full workspace test suite**

```bash
cd /home/mike/Desktop/strider
cargo test --workspace 2>&1 | tail -30
```

Expected: all tests pass.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace 2>&1 | tail -20
```

Expected: clean, or only pre-existing warnings.

- [ ] **Step 3: Run strider-py pytest**

```bash
cd /home/mike/Desktop/strider/crates/strider-py
.venv/bin/python -m pytest tests/python -v 2>&1 | tail -20
```

Expected: all pass.

- [ ] **Step 4: Run a focused bsdfinder smoke**

```bash
cd /home/mike/Desktop/bsdfinder
.venv/bin/python -c "
from bsdfinder.sweep import sweep
res = sweep('kernels/linux/x86_64/4.19.0-amd64', names=['comm_in_struct_task_struct', 'pid_in_struct_task_struct', 'cred_in_struct_task_struct'])
for k, v in res.items():
    print(k, v)
" 2>&1 | tail -10
```

Expected: existing offset scripts still pass on the 4.19 kernel (no regression).

- [ ] **Step 5: Final commit if anything was tweaked**

```bash
git add -A
git commit -m "callother classification: final cleanup + verification" || echo "nothing to commit"
```

---

## Self-Review Checklist (post-write)

**Spec coverage:**
- ✅ Goal 1 (one classification table; two callers) → Task 1 (table) + Task 6 (cfg consumer) + Task 4 (ir consumer).
- ✅ Goal 2 (one IR node kind) → covered by design (no new variant).
- ✅ Goal 3 (strict on emission) → Task 4 (ir errors on unknown) + Task 15 (table grows from corpus harvest).
- ✅ Goal 4 (delete CallOtherElide) → Task 12.
- ✅ Goal 5 (commit_creds etc. lift) → Task 8 (synthetic ud2/brk) + Task 15 (real kernels).
- ✅ Goal 6 (cfg emits truthful terminator on iteration 0) → Task 5 + Task 6.
- ✅ Goal 7 (no orchestrator iteration / tier-2 changes) → covered by absence in plan.
- ✅ Goal 8 (`pattern::call_other().name`) → Task 9.

**Placeholder scan:** Task 15 has discovery work — the implementer iterates against the corpus, adding entries until convergence. Acceptable (this kind of harvest is intrinsically iterative).

**Type consistency:**
- `CallOtherOutcome::Built { node, value }` — `node: NodeId`, `value: Option<NodeOutputId>`. Used identically in Task 3 (definition), Task 4 (constructor), Task 7 (consumer), Task 11 helper signature.
- `RegionTerminator::NoReturn` — used identically in Task 5 (definition), Task 6 (cfg emits), Task 8 (test asserts).
- `UnknownUserOpError { name: String }` — used identically in Task 2 (definition), Task 4 (raised), Task 4 step 7 + Task 14 (asserted).
- `target::user_ops::classify(name) -> Option<UserOpClass>` — used identically in Task 1 (definition), Task 4 (ir consumer), Task 6 (cfg consumer).
