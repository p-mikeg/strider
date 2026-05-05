# CallOther Precise ABI v2 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace v1's `UserOpClass::Opaque` catch-all with explicit `Call(UserOpAbi)` entries giving each Sleigh user-op its precise ISA register footprint and memory-edge effect.  Hard cutover — no `Opaque` variant survives.  IR builder clobbers only what the ABI specifies (no more "every tracked variable except SP" default).

**Architecture:** Single `target::user_ops::classify` table consumed by cfg (NoReturn termination only, unchanged from v1) and strider (full dispatch + ABI resolution).  IR layer gains `build_call_other_modeled(user_op_id, name, args, output_ty, implicit_reads_vns, implicit_writes_vns)`; v1's `build_call_other(name, …)` / `_opaque` / `_with_clobbers` / `CallOtherOutcome` are removed.

**Tech Stack:** Rust 1.x, anyhow, thiserror, petgraph (cfg), cranelift-entity (ir), rsleigh (Sleigh wrapper), pytest + maturin (strider-py).

**Spec:** `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`

---

## File Map

**Modify:**
- `crates/target/src/user_ops.rs` — `UserOpAbi` struct; `UserOpClass` loses `Opaque`, gains `Call(UserOpAbi)`; `classify` body reclassifies all 28 previously-Opaque entries.
- `crates/ir/src/builder/call.rs` — add `build_call_other_modeled`; remove `build_call_other(name, …)` (v1) + `build_call_other_opaque` + `build_call_other_with_clobbers`.
- `crates/ir/src/lib.rs` — remove `pub use builder::CallOtherOutcome`.
- `crates/strider/src/strider/insn/mod.rs` — replace `handle_call_other`'s body with the v2 dispatch (resolve ABI Vns, call `build_call_other_modeled`, handle `memory_edge` + clobber rebinding).
- `crates/ir/src/builder/tests.rs` — delete v1-Opaque-shape tests; rewrite the value-shape test against `build_call_other_modeled`.
- `crates/ir/tests/call_other_classification.rs` — rewrite NoOp / NoReturn / Built tests against the new helpers.
- `crates/pattern/tests/get_vn_with_callother_clobber.rs` — use `build_call_other_modeled` with explicit `[EAX, EBX, ECX, EDX]`.
- `crates/pattern/tests/matching/support/graph.rs` — `callother_node` helper grows `implicit_reads` / `implicit_writes` parameters.
- `crates/opt/src/dead_branch/tests.rs` — switch to `build_call_other_modeled` with empty implicit slices.
- `CLAUDE.md` — update the "callother classification" section to reflect the v2 shape (UserOpAbi, no Opaque).

**Create:**
- `crates/strider/tests/call_other_precise_abi.rs` — two integration tests proving (a) `cpuid` rebinds exactly the four named registers and (b) `mrs x0, S3_3_C15_C0_7` rebinds only x0.

**Delete:**
- `crates/ir/tests/call_other_conservative_clobber.rs` — entire file tests v1 conservative-clobber default; behaviour gone.

**No change to:**
- `crates/cfg/src/cfg/builder/region_builder.rs` — v1's `Opcode::CallOther` arm continues to read `classify()` only for NoReturn detection; doesn't touch `UserOpAbi`.
- `crates/cfg/src/cfg/types.rs` — `RegionTerminator::NoReturn` already exists from v1.
- `crates/pattern/src/pat/builders/call.rs` — pattern builder API unchanged.
- `crates/strider-py/` — Python surface unchanged (UserOpAbi is internal; `UnknownUserOpError` still exists from v1).

---

## Task 1: `target::user_ops` — add `UserOpAbi`, reclassify table

**Files:**
- Modify: `crates/target/src/user_ops.rs`

- [ ] **Step 1: Read the current file**

Run: `cat /home/mike/Desktop/strider/crates/target/src/user_ops.rs`

Confirm the v1 shape: `UserOpClass = NoOp | NoReturn | Opaque`, single classify match, 33 entries.

- [ ] **Step 2: Write the failing test for UserOpAbi presence + reclassification**

Replace the bottom `#[cfg(test)] mod tests` block with this expanded version:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn empty_abi() -> UserOpAbi {
        UserOpAbi {
            implicit_reads:  &[],
            implicit_writes: &[],
            memory_edge:     false,
        }
    }

    #[test]
    fn known_noop_classifies_as_noop() {
        for n in ["setEndianState", "setISAMode", "DataMemoryBarrier",
                  "DataSynchronizationBarrier", "DC_CVAC", "Hint_Prefetch",
                  "InstructionSynchronizationBarrier", "LOCK", "UNLOCK",
                  "Yield"] {
            assert_eq!(classify(n), Some(UserOpClass::NoOp), "{n}");
        }
    }

    #[test]
    fn known_trap_classifies_as_noreturn() {
        for n in ["invalidInstructionException", "SoftwareBreakpoint",
                  "UndefinedInstructionException", "sysret", "trap"] {
            assert_eq!(classify(n), Some(UserOpClass::NoReturn), "{n}");
        }
    }

    #[test]
    fn syscall_has_linux_x86_64_abi() {
        let class = classify("syscall").expect("syscall classified");
        let UserOpClass::Call(abi) = class else { panic!("expected Call, got {class:?}") };
        assert_eq!(abi.implicit_reads,  &["RAX","RDI","RSI","RDX","R10","R8","R9"]);
        assert_eq!(abi.implicit_writes, &["RAX","RCX","R11"]);
        assert!(abi.memory_edge);
    }

    #[test]
    fn cpuid_has_implicit_writes_to_four_regs() {
        let class = classify("cpuid").expect("cpuid classified");
        let UserOpClass::Call(abi) = class else { panic!("expected Call, got {class:?}") };
        assert_eq!(abi.implicit_reads,  &["ECX"]);
        assert_eq!(abi.implicit_writes, &["EAX","EBX","ECX","EDX"]);
        assert!(!abi.memory_edge);
    }

    #[test]
    fn rdtsc_writes_edx_eax_no_memory_edge() {
        let class = classify("rdtsc").expect("rdtsc classified");
        let UserOpClass::Call(abi) = class else { panic!("expected Call, got {class:?}") };
        assert_eq!(abi.implicit_reads,  &[]);
        assert_eq!(abi.implicit_writes, &["EAX","EDX"]);
        assert!(!abi.memory_edge);
    }

    #[test]
    fn empty_abi_ops_use_call_with_empty_abi() {
        for n in ["NEON_rev64", "NEON_sqshl", "NEON_uaddlv", "SVE_fnmla",
                  "MP_INT_ABS", "UnkSytemRegRead", "swapgs",
                  "ExclusiveMonitorPass", "ExclusiveMonitorsStatus"] {
            let class = classify(n).unwrap_or_else(|| panic!("{n} classified"));
            let UserOpClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert_eq!(abi, empty_abi(), "{n}");
        }
    }

    #[test]
    fn smccc_ops_share_x0_x7_in_x0_x3_out() {
        for n in ["CallHyperVisor", "CallSecureMonitor"] {
            let class = classify(n).expect(n);
            let UserOpClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert_eq!(abi.implicit_reads,  &["x0","x1","x2","x3","x4","x5","x6","x7"], "{n}");
            assert_eq!(abi.implicit_writes, &["x0","x1","x2","x3"], "{n}");
            assert!(abi.memory_edge, "{n}");
        }
    }

    #[test]
    fn port_io_has_memory_edge_no_implicit_regs() {
        for n in ["in", "out"] {
            let class = classify(n).expect(n);
            let UserOpClass::Call(abi) = class else { panic!("{n}: expected Call") };
            assert_eq!(abi.implicit_reads,  &[], "{n}");
            assert_eq!(abi.implicit_writes, &[], "{n}");
            assert!(abi.memory_edge, "{n}");
        }
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(classify("nonexistent_op_xyzzy_abc"), None);
    }

    #[test]
    fn opaque_variant_does_not_exist() {
        // Compile-time guard: every variant of UserOpClass is matched
        // exhaustively here, so adding/removing a variant fails compile.
        for n in ["setISAMode", "invalidInstructionException", "cpuid"] {
            let class = classify(n).unwrap();
            match class {
                UserOpClass::NoOp | UserOpClass::NoReturn | UserOpClass::Call(_) => {}
            }
        }
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p target user_ops --lib 2>&1 | tail -30`

Expected: compile errors for `UserOpAbi`, `UserOpClass::Call`, etc. (v1 doesn't have these).

- [ ] **Step 4: Implement UserOpAbi + reclassified table**

Replace the file content above the `#[cfg(test)]` block with:

```rust
//! Sleigh user-op (CallOther) classification table.  See
//! `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`
//! (and the v1 spec `2026-05-05-callother-classification-design.md`
//! for the original cfg/ir consumer split).

/// Per-user-op ABI describing register and memory effects beyond
/// what Sleigh's pcode insn already encodes.  Sleigh emits
/// `CALLOTHER(user_op_id, args…)` with a possible `output` field;
/// the ABI fills in the *implicit* (ISA-fixed, not in pcode)
/// channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserOpAbi {
    /// Register names this op reads beyond Sleigh's pcode-explicit
    /// `inputs[1..]`.  Resolved to `rsleigh::Vn` by the strider
    /// layer at lift time and appended to the CallOther's value
    /// inputs.  Use the exact Sleigh register name (case-sensitive).
    pub implicit_reads: &'static [&'static str],

    /// Register names this op writes (or scratch-clobbers) beyond
    /// Sleigh's pcode-explicit `output`.  Each becomes one extra
    /// clobber output slot on the CallOther node; the strider layer
    /// rebinds the matching tracked variable to that slot.
    pub implicit_writes: &'static [&'static str],

    /// Whether this op advances the IR's memory edge (token).  True
    /// for ops whose effect on memory is observable to subsequent
    /// loads / stores (syscall, port I/O, cache writeback).  False
    /// for pure register-level computation (cpuid, rdtsc, NEON math).
    pub memory_edge: bool,
}

/// What `strider::handle_call_other` does for a given user-op name.
/// Single source of truth for all CallOther dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserOpClass {
    /// True no-op.  No IR node emitted; control / memory unchanged;
    /// pcode-explicit output (if any) is ignored.
    NoOp,

    /// Trap — control flow ends here.  cfg terminates the region as
    /// `RegionTerminator::NoReturn`; ir's `build_call_other_terminal`
    /// emits a `[ctrl, mem]` → `[ctrl, mem]` CallOther whose outputs
    /// dangle.
    NoReturn,

    /// Op with a precise ABI describing its register footprint and
    /// memory effect beyond what Sleigh's pcode already encodes.
    Call(UserOpAbi),
}

/// Look up a user-op name in the classification table.
///
/// Strict-on-emission policy: the ir layer (`build_call_other_modeled`'s
/// caller) converts `None` into `UnknownUserOpError`.  The cfg builder
/// treats `None` as "fall through to today's behaviour" (insn stays in
/// the region) — the ir layer is the single strict gate.
//
// `match_same_arms`: each name is a separate diffable entry — combining
// arms via `|` would defeat the table's per-line diff property.
#[allow(clippy::match_same_arms)]
#[must_use]
pub fn classify(name: &str) -> Option<UserOpClass> {
    // ASCII-sorted within each group for diffability.
    match name {
        // ─── NoOp ─────────────────────────────────────────────────
        "DC_CVAC"                           => Some(UserOpClass::NoOp),
        "DataMemoryBarrier"                 => Some(UserOpClass::NoOp),
        "DataSynchronizationBarrier"        => Some(UserOpClass::NoOp),
        "Hint_Prefetch"                     => Some(UserOpClass::NoOp),
        "InstructionSynchronizationBarrier" => Some(UserOpClass::NoOp),
        "LOCK"                              => Some(UserOpClass::NoOp),
        "UNLOCK"                            => Some(UserOpClass::NoOp),
        "Yield"                             => Some(UserOpClass::NoOp),
        "setEndianState"                    => Some(UserOpClass::NoOp),
        "setISAMode"                        => Some(UserOpClass::NoOp),

        // ─── NoReturn ─────────────────────────────────────────────
        "SoftwareBreakpoint"          => Some(UserOpClass::NoReturn),
        "UndefinedInstructionException" => Some(UserOpClass::NoReturn),
        "invalidInstructionException" => Some(UserOpClass::NoReturn),
        "sysret"                      => Some(UserOpClass::NoReturn),
        "trap"                        => Some(UserOpClass::NoReturn),

        // ─── Call (precise ABI) ───────────────────────────────────

        // Linux x86_64 syscall ABI.
        "syscall" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &["RAX","RDI","RSI","RDX","R10","R8","R9"],
            implicit_writes: &["RAX","RCX","R11"],
            memory_edge:     true,
        })),

        // Linux ARM SWI ABI.
        "swi" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &["r7","r0","r1","r2","r3","r4","r5","r6"],
            implicit_writes: &["r0"],
            memory_edge:     true,
        })),

        // ARM SMCCC (X0..X7 in, X0..X3 out).
        "CallHyperVisor" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &["x0","x1","x2","x3","x4","x5","x6","x7"],
            implicit_writes: &["x0","x1","x2","x3"],
            memory_edge:     true,
        })),
        "CallSecureMonitor" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &["x0","x1","x2","x3","x4","x5","x6","x7"],
            implicit_writes: &["x0","x1","x2","x3"],
            memory_edge:     true,
        })),

        // x86 CPUID — Sleigh emits CALLOTHER(cpuid, EAX) with no output.
        "cpuid" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &["ECX"],
            implicit_writes: &["EAX","EBX","ECX","EDX"],
            memory_edge:     false,
        })),

        // x86 RDTSC — no inputs, writes EDX:EAX.
        "rdtsc" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &[],
            implicit_writes: &["EAX","EDX"],
            memory_edge:     false,
        })),

        // x86 RDPKRU — ECX must be 0; writes EAX, clears EDX.
        "rdpkru_u32" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads:  &["ECX"],
            implicit_writes: &["EAX","EDX"],
            memory_edge:     false,
        })),

        // x86 port I/O — port + value are pcode-explicit; memory edge captures
        // the external port-state effect.
        "in"  => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: true,
        })),
        "out" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: true,
        })),

        // x86 SWAPGS — touches the synthetic GS_base MSR; no general-reg
        // effect, no memory edge (kernel-mode register swap).
        "swapgs" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),

        // ARM exclusive-monitor primitives — synthetic monitor flag,
        // pcode-handled.  LDREX/STREX themselves emit pcode loads/stores.
        "ExclusiveMonitorPass" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),
        "ExclusiveMonitorsStatus" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),

        // ARM unmodelled sysreg read — pcode-explicit encoding constant
        // and destination.  Empty ABI per c1 (lift succeeds; opaque value).
        "UnkSytemRegRead" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),

        // NEON / SVE / multi-precision — Sleigh's pcode is fully sufficient.
        "MP_INT_ABS"  => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),
        "NEON_rev64"  => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),
        "NEON_sqshl"  => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),
        "NEON_uaddlv" => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),
        "SVE_fnmla"   => Some(UserOpClass::Call(UserOpAbi {
            implicit_reads: &[], implicit_writes: &[], memory_edge: false,
        })),

        _ => None,
    }
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p target user_ops --lib 2>&1 | tail -15`

Expected: all 9 tests pass.

- [ ] **Step 6: Verify the workspace still builds (with v1 callers still using the old API)**

Run: `cargo build --workspace 2>&1 | tail -10`

Expected: compile errors in ir/strider/etc. that still call `build_call_other(name, …)` expecting the old `Opaque` variant.  This is correct — Tasks 2 and 3 fix those callers.  If you see ONLY `error[E0599]: no variant or associated item named Opaque found for enum UserOpClass` — good, that's the planned breakage.  Any other unexpected error is a problem.

- [ ] **Step 7: Commit**

```bash
git add crates/target/src/user_ops.rs
git commit -m "$(cat <<'EOF'
target: replace UserOpClass::Opaque with Call(UserOpAbi)

Hard cutover to precise per-op ABIs.  UserOpAbi describes the
implicit (ISA-fixed, not in Sleigh's pcode) register reads,
writes, and memory edge.  All 28 previously-Opaque entries
reclassified:

  * 8 → NoOp (memory barriers, lock prefixes, hints)
  * 2 → NoReturn (sysret, trap)
  * 18 → Call with precise ABI

The workspace will not compile until ir + strider migrate to
the new shape (Tasks 2-3 of the plan).

Spec: docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: `ir::FunctionBuilder::build_call_other_modeled`

**Files:**
- Modify: `crates/ir/src/builder/call.rs`

- [ ] **Step 1: Read the current build_call_other and helpers**

Run: `sed -n '170,310p' /home/mike/Desktop/strider/crates/ir/src/builder/call.rs`

Note `build_call_other`, `build_call_other_opaque`, `build_call_other_with_clobbers`, and `build_call_other_terminal`.  All of these will be touched in this and Task 6.

- [ ] **Step 2: Write the failing test**

Create `crates/ir/tests/call_other_modeled.rs`:

```rust
//! `build_call_other_modeled` emits a CallOther whose clobber slots
//! correspond exactly to the ABI's implicit_writes (no conservative
//! all-vars default).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};

fn make_builder() -> FunctionBuilder {
    FunctionBuilder::new_raw_for_test().expect("builder")
}

#[test]
fn modeled_with_no_implicit_writes_emits_no_clobber_slots() {
    let mut b = make_builder();
    let (node, value, clobber_outs) = b
        .build_call_other_modeled(7, "NEON_rev64", &[], None, &[], &[])
        .expect("modeled ok");
    let kind = b.body().graph.node_kind(node);
    assert!(matches!(kind, NodeKind::CallOther { user_op_id: 7 }), "{kind:?}");
    assert!(value.is_none(), "no output_ty -> no value slot");
    assert!(clobber_outs.is_empty(), "no implicit_writes -> no clobber slots");
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 2, "ctrl + mem only");
}

#[test]
fn modeled_with_value_emits_value_then_clobbers_in_order() {
    let mut b = make_builder();
    // Resolve a couple of test register Vns.  Use the test_utils helper
    // (reg_vn) to avoid a Sleigh dependency in ir-only tests.
    use ir::test_utils::reg_vn;
    let r0 = reg_vn(0, 4);    // 4-byte reg at addr 0
    let r1 = reg_vn(4, 4);    // 4-byte reg at addr 4

    let (node, value, clobber_outs) = b
        .build_call_other_modeled(
            8, "cpuid", &[],
            Some(NodeOutputType::U32),     // pcode-explicit value output
            &[],                            // no implicit_reads
            &[r0, r1],                      // 2 implicit_writes
        )
        .expect("modeled ok");
    assert!(value.is_some(), "output_ty -> value slot");
    assert_eq!(clobber_outs.len(), 2, "two implicit_writes -> two clobber slots");
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 5, "ctrl + mem + value + 2 clobbers");
    assert_eq!(b.body().graph.call_other_name(node), Some("cpuid"));
}

#[test]
fn modeled_does_not_advance_memory_token() {
    // Caller (strider) is responsible for advancing memory based on
    // memory_edge.  build_call_other_modeled should NOT advance it.
    let mut b = make_builder();
    let mem_before = b.cur_region_memory().expect("mem in");
    let (_n, _v, _c) = b
        .build_call_other_modeled(9, "NEON_rev64", &[], None, &[], &[])
        .expect("ok");
    let mem_after = b.cur_region_memory().expect("mem after");
    assert_eq!(mem_before, mem_after,
        "build_call_other_modeled must not advance the memory token");
}
```

If `FunctionBuilder::new_raw_for_test()` or `cur_region_memory()` accessor doesn't exist in the form above, search:

```bash
grep -n "new_raw_for_test\|cur_region_memory\|reg_vn" \
    /home/mike/Desktop/strider/crates/ir/src/builder/mod.rs \
    /home/mike/Desktop/strider/crates/ir/src/test_utils.rs
```

and adapt the test calls to whatever the real surface offers.  If `cur_region_memory` is `pub(crate)`, expose a `pub fn` wrapper for tests, or use a less-direct assertion (e.g., add a Store before and after and verify both reach the same memory state).

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p ir --test call_other_modeled 2>&1 | tail -15`

Expected: `build_call_other_modeled` undefined.

- [ ] **Step 4: Implement `build_call_other_modeled`**

In `crates/ir/src/builder/call.rs`, after the existing `build_call_other_terminal` method, add:

```rust
    /// Emit a CallOther with the precise per-op ABI shape.
    ///
    /// Inputs of the resulting node:
    ///   `[ctrl_in, mem_in, *args, *implicit_read_values]`
    ///
    /// Outputs of the resulting node:
    ///   `[ctrl_out, mem_out, value?, *clobber_per_implicit_write]`
    ///
    /// This method advances the region's control token to the new
    /// `ctrl_out` but **does not** advance the memory token — the
    /// strider layer is responsible for calling
    /// `advance_cur_region_memory(mem_out)` IFF the ABI's
    /// `memory_edge` is true.  Similarly the strider layer rebinds
    /// each `implicit_writes_vns` Vn to its corresponding
    /// `clobber_outputs` slot via `write_variable`.
    ///
    /// Returns `(node, value_output, clobber_outputs)`.
    /// `value_output.is_some() == output_ty.is_some()`.
    /// `clobber_outputs.len() == implicit_writes_vns.len()`.
    pub fn build_call_other_modeled(
        &mut self,
        user_op_id: u64,
        name: &str,
        args: &[crate::node::NodeOutputId],
        output_ty: Option<crate::node::NodeOutputType>,
        implicit_reads_vns: &[rsleigh::Vn],
        implicit_writes_vns: &[rsleigh::Vn],
    ) -> crate::error::Result<(
        crate::node::NodeId,
        Option<crate::node::NodeOutputId>,
        Vec<crate::node::NodeOutputId>,
    )> {
        use crate::node::{NodeKind, NodeOutputKind};
        let ctrl = self.cur_region_control()?;
        let memory = self.cur_region_memory()?;

        // Validate args are value edges (existing helper).
        self.validate_value_inputs(args)?;

        // Read each implicit-read register through the variable
        // machinery — this gives us the current SSA value for that
        // register and includes any aliasing fixups.  Width must be a
        // value edge.
        let mut implicit_read_values: smallvec::SmallVec<[crate::node::NodeOutputId; 8]> =
            smallvec::SmallVec::new();
        for vn in implicit_reads_vns {
            let out = self.read_variable(vn)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow::anyhow!(
                    "implicit_read for user-op {name:?}: output {out:?} \
                     is not a value edge (got {k:?})"
                ));
            }
            implicit_read_values.push(out);
        }

        // Read each implicit-write register's *kind* so we can
        // declare the correct output slot type.  The value itself
        // is irrelevant here — we just need the kind.
        let mut implicit_write_kinds: smallvec::SmallVec<[NodeOutputKind; 8]> =
            smallvec::SmallVec::new();
        for vn in implicit_writes_vns {
            let out = self.read_variable(vn)?;
            let k = self.graph().output_kind(out);
            if !k.is_value() {
                return Err(anyhow::anyhow!(
                    "implicit_write for user-op {name:?}: output {out:?} \
                     is not a value edge (got {k:?})"
                ));
            }
            implicit_write_kinds.push(k);
        }

        let mut output_kinds: smallvec::SmallVec<[NodeOutputKind; 8]> =
            smallvec::SmallVec::new();
        output_kinds.push(NodeOutputKind::Control);
        output_kinds.push(NodeOutputKind::Memory);
        if let Some(ty) = output_ty {
            output_kinds.push(NodeOutputKind::OutputType(ty));
        }
        output_kinds.extend(implicit_write_kinds);

        let inputs = [ctrl, memory]
            .into_iter()
            .chain(args.iter().copied())
            .chain(implicit_read_values.iter().copied());

        let node = self.create_node(
            NodeKind::CallOther { user_op_id },
            inputs,
            output_kinds,
        );
        let outputs: smallvec::SmallVec<[crate::node::NodeOutputId; 8]> =
            self.graph().node_outputs(node).into_iter().collect();

        // Advance ctrl only.  Memory is the strider layer's call.
        self.advance_cur_region_ctrl(outputs[0])?;

        let (value_output, clobber_start_slot) = if output_ty.is_some() {
            (Some(outputs[2]), 3usize)
        } else {
            (None, 2usize)
        };

        let clobber_outputs: Vec<crate::node::NodeOutputId> =
            outputs[clobber_start_slot..].to_vec();

        // Stamp the user-op name on the side table for patterns.
        self.body_mut()
            .graph
            .set_call_other_name(node, name.to_string());

        Ok((node, value_output, clobber_outputs))
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p ir --test call_other_modeled 2>&1 | tail -15`

Expected: 3 passed.

If a test fails because `cur_region_memory` isn't `pub`, expose a `pub` wrapper or rewrite the test using a Store-before / Store-after pattern.

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/builder/call.rs crates/ir/tests/call_other_modeled.rs
git commit -m "$(cat <<'EOF'
ir: add build_call_other_modeled with precise ABI shape

New entry point for per-op ABI-driven CallOther construction.
Inputs: [ctrl, mem, *args, *implicit_reads].
Outputs: [ctrl, mem, value?, *clobber per implicit_write].

Memory-edge handling is deferred to the strider layer: the new
method advances ctrl but NOT memory.  The strider caller checks
the ABI's memory_edge and calls advance_cur_region_memory iff true.
Similarly the strider caller rebinds each implicit_writes Vn to
its corresponding clobber output slot.

v1's build_call_other(name, ...) / _opaque / _with_clobbers /
CallOtherOutcome are NOT removed yet — Task 6 cleans them up after
the strider migration in Task 3 lands.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Strider — `handle_call_other` rewrites to use new dispatch

**Files:**
- Modify: `crates/strider/src/strider/insn/mod.rs`

- [ ] **Step 1: Read the current handle_call_other**

Run: `sed -n '109,165p' /home/mike/Desktop/strider/crates/strider/src/strider/insn/mod.rs`

- [ ] **Step 2: Replace handle_call_other body**

Replace the existing body (the v1 version that dispatches via `CallOtherOutcome`) with:

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
        let user_op_id_u32 = u32::try_from(user_op_id).map_err(|_| {
            anyhow::anyhow!("CallOther user-op id {user_op_id:#x} exceeds u32")
        })?;
        let name = self.cfg.sleigh.user_op_name(user_op_id_u32).ok_or_else(|| {
            anyhow::anyhow!(
                "CallOther user-op id {user_op_id_u32} not in Sleigh's user_op table"
            )
        })?;

        let class = target::user_ops::classify(name).ok_or_else(|| {
            ir::error::UnknownUserOpError { name: name.to_string() }
        })?;

        match class {
            target::user_ops::UserOpClass::NoOp => Ok(()),

            target::user_ops::UserOpClass::NoReturn => {
                let _ = self.builder.build_call_other_terminal(user_op_id, name)?;
                Ok(())
            }

            target::user_ops::UserOpClass::Call(abi) => {
                // 1. Resolve pcode-explicit inputs (args).
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

                // 2. Resolve ABI register names → Vns via Sleigh.
                let regs = self.cfg.sleigh.regs().map_err(|e| {
                    anyhow::anyhow!("strider: Sleigh::regs() failed for user-op {name:?}: {e:?}")
                })?;
                let resolve = |reg_names: &[&str]| -> Result<Vec<rsleigh::Vn>> {
                    reg_names.iter().map(|n| {
                        regs.name_to_vn(n).ok_or_else(|| anyhow::anyhow!(
                            "user-op {name:?} ABI references unknown register {n:?}"
                        ))
                    }).collect()
                };
                let implicit_reads_vns = resolve(abi.implicit_reads)?;
                let implicit_writes_vns = resolve(abi.implicit_writes)?;

                // 3. Build the precise CallOther node.
                let (node, value, clobber_outs) = self.builder.build_call_other_modeled(
                    user_op_id, name, &args, output_ty,
                    &implicit_reads_vns, &implicit_writes_vns,
                )?;

                // 4. Memory edge: strider decides whether to advance.
                if abi.memory_edge {
                    let mem_out = self.builder.body().graph.node_outputs(node)[1];
                    self.builder.advance_cur_region_memory(mem_out)?;
                }

                // 5. Rebind tracked variables.
                if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), value) {
                    self.write_vn(out_vn, val)?;
                }
                for (vn, slot) in implicit_writes_vns.iter().zip(clobber_outs) {
                    self.write_variable(vn, slot)?;
                }

                Ok(())
            }
        }
    }
```

If `self.write_variable(vn, slot)` doesn't exist as a public-to-strider method, locate the equivalent on `FunctionBuilder` (likely `write_variable` or `set_variable_value`) — `grep -n "fn write_variable\|fn set_variable" crates/ir/src/builder/mod.rs`.

If `self.builder.advance_cur_region_memory` is `pub(crate)` to ir, expose it via `pub fn` or via a sibling `advance_memory_to(mem_out)` helper that wraps it.

- [ ] **Step 3: Compile**

Run: `cargo build -p strider 2>&1 | tail -10`

Expected: clean build (the v1 `build_call_other(name, ...)` API still exists; nothing forces its removal yet).

- [ ] **Step 4: Run strider tests**

Run: `cargo test -p strider 2>&1 | tail -10`

Expected: existing tests pass.  Some test that asserted on the v1 conservative-clobber CallOther shape may now fail — note them and fix in Task 7 (test migration).

- [ ] **Step 5: Commit**

```bash
git add crates/strider/src/strider/insn/mod.rs
git commit -m "$(cat <<'EOF'
strider: route handle_call_other through the precise-ABI dispatch

handle_call_other now:
  * Resolves the user-op name from Sleigh.
  * Calls target::user_ops::classify(name) and matches on UserOpClass.
  * For NoOp: returns Ok(()).
  * For NoReturn: calls ir::FunctionBuilder::build_call_other_terminal.
  * For Call(abi):
      - Resolves abi.implicit_reads / implicit_writes register names
        to Vns via Sleigh::regs().name_to_vn.
      - Calls build_call_other_modeled with the resolved Vn slices.
      - Calls advance_cur_region_memory iff abi.memory_edge.
      - Rebinds the pcode-explicit output (if any) and each implicit
        write to its tracked variable.

Unknown user-op names error out with UnknownUserOpError (strict
on emission).

v1's build_call_other(name, ...) / _opaque / CallOtherOutcome
are now caller-less; Task 6 deletes them.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Add precise-ABI integration tests

**Files:**
- Create: `crates/strider/tests/call_other_precise_abi.rs`

- [ ] **Step 1: Verify the cpuid byte sequence**

`cpuid` = `0x0F 0xA2`.

Run: `printf '\x0f\xa2\xc3' | objdump -D -b binary -m i386:x86-64 - | tail -5`

Expected: lines `cpuid` and `ret`.

- [ ] **Step 2: Write the failing test**

Create `crates/strider/tests/call_other_precise_abi.rs`:

```rust
//! Integration tests for v2's precise per-op ABI lifting.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::node::NodeKind;
use pattern::{call_other, Matcher, pat::Pat};

#[test]
fn cpuid_clobbers_only_eax_ebx_ecx_edx() {
    let arch = strider::SleighArch::x86_64();
    let regs = arch.probe_regs().expect("probe regs");
    let strider_h = strider::Strider::new(
        arch, regs, strider::CallingConvention::x86_64_systemv_abi(),
    ).expect("strider");

    // Bytes: cpuid (0x0F 0xA2) ; ret (0xC3)
    let bytes = vec![0x0fu8, 0xa2, 0xc3];
    let entry = 0x1000u64;
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");
    let outcome = strider_h.analyze_cfg(&cfg).expect("analyze_cfg");

    // Find the cpuid CallOther via the pattern surface.
    let pat: Pat = call_other().name("cpuid").into();
    let matches = Matcher::new(&outcome.graph).find_all(&pat);
    assert_eq!(matches.len(), 1, "exactly one cpuid CallOther in this fixture");
    let node = matches[0].root();

    // Inspect the CallOther's outputs: [ctrl, mem, EAX_clob, EBX_clob, ECX_clob, EDX_clob].
    // 6 outputs total: 2 (ctrl/mem) + 0 (no pcode-explicit value output) + 4 (clobbers).
    let n_outs = outcome.graph.graph.node_outputs(node).len();
    assert_eq!(n_outs, 6,
        "cpuid CallOther: ctrl + mem + 4 clobbers (EAX/EBX/ECX/EDX); got {n_outs}");
}

#[test]
fn unmodelled_sysreg_read_clobbers_only_destination() {
    let arch = strider::SleighArch::aarch64();
    let regs = arch.probe_regs().expect("probe regs");
    let strider_h = strider::Strider::new(
        arch, regs, strider::CallingConvention::aarch64_aapcs64(),
    ).expect("strider");

    // mrs x0, S3_3_C15_C0_7 (encoding: 0xD53DF0E0 LE = E0 F0 3D D5)
    // Followed by ret (0xD65F03C0 LE = C0 03 5F D6)
    let bytes = vec![0xe0u8, 0xf0, 0x3d, 0xd5, 0xc0, 0x03, 0x5f, 0xd6];
    let entry = 0x1000u64;
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader)
        .expect("sleigh");
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .expect("cfg");
    let outcome = strider_h.analyze_cfg(&cfg).expect("analyze_cfg");

    let pat: Pat = call_other().name("UnkSytemRegRead").into();
    let matches = Matcher::new(&outcome.graph).find_all(&pat);
    assert_eq!(matches.len(), 1, "exactly one UnkSytemRegRead in this fixture");
    let node = matches[0].root();

    // Outputs: [ctrl, mem, value(x0)].  No implicit clobbers.
    // 3 outputs total.
    let n_outs = outcome.graph.graph.node_outputs(node).len();
    assert_eq!(n_outs, 3,
        "UnkSytemRegRead CallOther: ctrl + mem + value (x0); got {n_outs}");
}
```

If the encodings are wrong, verify with:
```bash
printf '\xe0\xf0\x3d\xd5' | aarch64-linux-gnu-objdump -D -b binary -m aarch64 -
```

If `outcome.graph.graph.node_outputs` is `pub(crate)` outside ir, route through whatever public accessor the integration test surface uses (`grep -n "pub fn node_outputs" crates/ir/src/`).

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test -p strider --test call_other_precise_abi 2>&1 | tail -15`

Expected: 2 passed.

If a test fails because the actual register names in our ABI table don't match Sleigh's exact spelling, fix the table in `crates/target/src/user_ops.rs` and re-test.

- [ ] **Step 4: Commit**

```bash
git add crates/strider/tests/call_other_precise_abi.rs
git commit -m "$(cat <<'EOF'
strider: integration tests for precise CallOther ABI dispatch

cpuid_clobbers_only_eax_ebx_ecx_edx: lifts a 3-byte fixture
(cpuid; ret) and asserts the cpuid CallOther has exactly 6 outputs
(ctrl, mem, EAX/EBX/ECX/EDX clobbers).

unmodelled_sysreg_read_clobbers_only_destination: lifts an
aarch64 fixture (mrs x0, S3_3_C15_C0_7; ret) and asserts the
UnkSytemRegRead CallOther has exactly 3 outputs (ctrl, mem, value
for x0) -- no spurious clobbers of x1..x30.

These regression-test the precision win that motivates v2.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Verify `sysret` and `trap` Sleigh shape (sanity check)

**Files:**
- (Read-only verification.)

- [ ] **Step 1: Check what Sleigh actually emits for sysret**

If the workspace has `crates/strider/examples/dump_pcode.rs`, use it:
```bash
cargo run --example dump_pcode -- /tmp/sysret.elf x86_64 0x1000 1 2>&1 | tail -10
```

If not, write a one-shot:
```rust
// /tmp/dump.rs
fn main() {
    let arch = strider::SleighArch::x86_64();
    // SYSRET = 0x48 0x0F 0x07
    let bytes = vec![0x48u8, 0x0f, 0x07];
    let reader = rsleigh::mem_readers::BufMemReader::new(bytes, 0x1000);
    let mut sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();
    let res = sleigh.lift_one(0x1000).unwrap();
    for (i, ins) in res.insns.iter().enumerate() {
        println!("pcode[{i}]: {:?}", ins.opcode);
    }
}
```

Run and observe.  Outcomes:
* If pcode contains a `Return` opcode after `CallOther(sysret)`: cfg already terminates the region; `NoReturn` classification is correct AND harmless (the cfg's NoReturn-on-CallOther arm fires before the trailing Return is processed).
* If pcode contains only `CallOther(sysret)` with no terminator: `NoReturn` classification is essential — without it, the lifter falls through to garbage past the SYSRET.

- [ ] **Step 2: Same for trap (ARM TRAP)**

Verify the encoding in objdump first.  `trap` is not a real ARM instruction; it's a Sleigh user-op typically emitted for `udf #N` or similar.  If the precise instruction is unclear, the v1 corpus harvest already classified it as Opaque without issue, so v2 marking it NoReturn is on the safer side (terminating regions early is conservative).

- [ ] **Step 3: If either op needs reclassification, edit and commit**

If sysret's pcode does NOT include a final Return AND our NoReturn classification doesn't terminate the region (somehow), or if trap turns out to be a regular call rather than a trap, edit `crates/target/src/user_ops.rs` to reclassify.  Re-run all tests.  Commit:

```bash
git add crates/target/src/user_ops.rs
git commit -m "target: reclassify sysret / trap based on Sleigh pcode shape verification"
```

If both classifications were correct, no commit; just note in Task 13's report.

---

## Task 6: Remove v1 IR builder APIs

**Files:**
- Modify: `crates/ir/src/builder/call.rs`
- Modify: `crates/ir/src/lib.rs`

- [ ] **Step 1: Verify no remaining users of v1 entries**

Run:
```bash
grep -rn "build_call_other(\"\|build_call_other_opaque\|build_call_other_with_clobbers\|CallOtherOutcome" /home/mike/Desktop/strider/crates/ 2>&1 | grep -v "build_call_other_modeled\|build_call_other_terminal" | head -30
```

Expected hits:
* `crates/ir/src/builder/call.rs` — the implementations themselves.
* Test files (Tasks 7–10 will migrate them; for this task, leave them).
* `crates/ir/src/lib.rs` — `pub use builder::CallOtherOutcome`.

If a non-test, non-ir file references any of these, STOP — there's a caller you missed in Task 3.

- [ ] **Step 2: Delete the v1 entries**

In `crates/ir/src/builder/call.rs`:
* Delete the `build_call_other(&mut self, name: &str, …)` method (the one returning `CallOtherOutcome`).
* Delete the `build_call_other_opaque` method.
* Delete the `build_call_other_with_clobbers` method.
* Delete the `pub enum CallOtherOutcome { … }` definition.

In `crates/ir/src/lib.rs`:
* Delete the `pub use builder::CallOtherOutcome;` line.

- [ ] **Step 3: Compile**

Run: `cargo build --workspace 2>&1 | tail -15`

Expected: compile errors only in test files (Tasks 7–10 fix them).

- [ ] **Step 4: Commit**

```bash
git add crates/ir/src/builder/call.rs crates/ir/src/lib.rs
git commit -m "$(cat <<'EOF'
ir: drop v1 build_call_other(name) / _opaque / _with_clobbers / CallOtherOutcome

The v1 high-level dispatch entry and its supporting helpers are
caller-less after Task 3's strider migration.  build_call_other_terminal
remains (used for NoReturn) and build_call_other_modeled is the new
entry for Call(UserOpAbi) construction.

Tests still call the old API; Tasks 7-10 migrate them.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Migrate `crates/ir/src/builder/tests.rs`

**Files:**
- Modify: `crates/ir/src/builder/tests.rs`

- [ ] **Step 1: List the affected sites**

Run: `grep -n "build_call_other" /home/mike/Desktop/strider/crates/ir/src/builder/tests.rs`

Expected hits at lines around 496, 501, 520, 523, 539, 542 (approx).

- [ ] **Step 2: Delete tests for behaviour that no longer exists**

The two tests `build_call_other_no_value_emits_clobber_per_tracked_var` and `build_call_other_rebinds_tracked_variables` (around lines 496 and similar) test v1's conservative-clobber default.  That behaviour is gone.  Delete both test functions entirely.

- [ ] **Step 3: Rewrite the value-shape test**

Locate `build_call_other_with_value_keeps_value_in_slot_2_clobber_starts_at_3` (around line 520) and rewrite it as:

```rust
#[test]
fn modeled_with_value_then_two_clobbers_lays_out_correctly() {
    let mut b = test_builder();
    use crate::test_utils::reg_vn;
    let r0 = reg_vn(0, 4);
    let r1 = reg_vn(4, 4);
    let (_node, value, clobber_outs) = b
        .build_call_other_modeled(
            7, "cpuid", &[],
            Some(NodeOutputType::U32),
            &[],
            &[r0, r1],
        )
        .expect("modeled ok");
    assert!(value.is_some());
    assert_eq!(clobber_outs.len(), 2);
}
```

(Adjust `test_builder()` to the existing helper name.)

- [ ] **Step 4: Rewrite `build_call_other_rejects_non_value_arg`**

The arg-validation behaviour still exists in `build_call_other_modeled`.  Rewrite the test to call the new method:

```rust
#[test]
fn modeled_rejects_non_value_arg() {
    let mut b = test_builder();
    let mem = b.cur_region_memory().expect("mem");   // a Memory edge, not a value
    let res = b.build_call_other_modeled(0, "NEON_rev64", &[mem], None, &[], &[]);
    assert!(res.is_err(), "non-value arg should be rejected");
}
```

(Use whatever the actual non-value source is in the existing test — `mem` is illustrative.)

- [ ] **Step 5: Compile + run**

Run: `cargo test -p ir --lib 2>&1 | tail -15`

Expected: all tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ir/src/builder/tests.rs
git commit -m "$(cat <<'EOF'
ir/tests: migrate to build_call_other_modeled

Two tests (no-value-clobber-per-tracked-var, rebinds-tracked-variables)
deleted entirely — they tested v1's conservative-clobber default,
which v2 removes.  Two surviving tests rewritten to call
build_call_other_modeled with explicit Vn slices.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Migrate `crates/ir/tests/call_other_classification.rs`

**Files:**
- Modify: `crates/ir/tests/call_other_classification.rs`

- [ ] **Step 1: Read the current file**

Run: `cat /home/mike/Desktop/strider/crates/ir/tests/call_other_classification.rs`

Note the four tests (NoOp / NoReturn / Built / Unknown).  All call v1's `build_call_other(name, …)`.

- [ ] **Step 2: Rewrite to test the new helpers directly**

Replace the file contents with:

```rust
//! Outcome-level tests for the IR's CallOther construction helpers.
//! Spec: `docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ir::FunctionBuilder;
use ir::node::{NodeKind, NodeOutputType};

fn make_builder() -> FunctionBuilder {
    FunctionBuilder::new_raw_for_test().expect("builder")
}

#[test]
fn build_call_other_terminal_emits_ctrl_mem_only() {
    let mut b = make_builder();
    let node = b
        .build_call_other_terminal(7, "invalidInstructionException")
        .expect("terminal ok");
    let kind = b.body().graph.node_kind(node);
    assert!(matches!(kind, NodeKind::CallOther { user_op_id: 7 }), "{kind:?}");
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 2, "terminal: ctrl + mem only");
    assert_eq!(
        b.body().graph.call_other_name(node),
        Some("invalidInstructionException"),
    );
}

#[test]
fn build_call_other_modeled_with_empty_abi_no_clobbers() {
    let mut b = make_builder();
    let (node, value, clobber_outs) = b
        .build_call_other_modeled(8, "NEON_rev64", &[], None, &[], &[])
        .expect("modeled ok");
    assert!(value.is_none());
    assert!(clobber_outs.is_empty());
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 2);
    assert_eq!(b.body().graph.call_other_name(node), Some("NEON_rev64"));
}

#[test]
fn build_call_other_modeled_with_value_and_clobbers() {
    let mut b = make_builder();
    use ir::test_utils::reg_vn;
    let r0 = reg_vn(0, 4);
    let (node, value, clobber_outs) = b
        .build_call_other_modeled(
            9, "cpuid", &[],
            Some(NodeOutputType::U32),
            &[],
            &[r0],
        )
        .expect("modeled ok");
    assert!(value.is_some());
    assert_eq!(clobber_outs.len(), 1);
    let n_outs = b.body().graph.node_outputs(node).len();
    assert_eq!(n_outs, 4, "ctrl + mem + value + 1 clobber");
}
```

(Note: the v1 "unknown name → UnknownUserOpError" test moves to a strider-side integration test, since v2's `build_call_other_modeled` doesn't classify — strider does.  If a test of UnknownUserOpError surfacing is needed, add it to `crates/strider/tests/call_other_precise_abi.rs` from Task 4.)

- [ ] **Step 3: Run the tests**

Run: `cargo test -p ir --test call_other_classification 2>&1 | tail -10`

Expected: 3 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/ir/tests/call_other_classification.rs
git commit -m "ir/tests: migrate call_other_classification to v2 helpers

Drops the v1 dispatch test (now strider-side); rewrites the three
shape tests against build_call_other_terminal and build_call_other_modeled.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Delete `crates/ir/tests/call_other_conservative_clobber.rs`

**Files:**
- Delete: `crates/ir/tests/call_other_conservative_clobber.rs`

- [ ] **Step 1: Verify the file tests v1 conservative-clobber behaviour**

Run: `cat /home/mike/Desktop/strider/crates/ir/tests/call_other_conservative_clobber.rs`

Confirm it tests the "every tracked variable except SP" clobber default, which is gone in v2.

- [ ] **Step 2: Delete**

```bash
rm /home/mike/Desktop/strider/crates/ir/tests/call_other_conservative_clobber.rs
```

- [ ] **Step 3: Compile + test**

Run: `cargo test -p ir 2>&1 | tail -10`

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add -A crates/ir/tests/
git commit -m "ir/tests: delete call_other_conservative_clobber (v1 behaviour gone)"
```

---

## Task 10: Migrate pattern + opt test sites

**Files:**
- Modify: `crates/pattern/tests/get_vn_with_callother_clobber.rs`
- Modify: `crates/pattern/tests/matching/support/graph.rs`
- Modify: `crates/opt/src/dead_branch/tests.rs`

- [ ] **Step 1: List the sites**

Run: `grep -n "build_call_other" /home/mike/Desktop/strider/crates/pattern/tests/ /home/mike/Desktop/strider/crates/opt/src/dead_branch/tests.rs -r`

- [ ] **Step 2: Update the test helper signature**

In `crates/pattern/tests/matching/support/graph.rs` (around line 320), update the `callother_node` helper:

```rust
pub fn callother_node(
    b: &mut FunctionBuilder,
    name: &str,
    user_op_id: u64,
    args: &[NodeOutputId],
    ret_ty: Option<NodeOutputType>,
    implicit_reads: &[rsleigh::Vn],
    implicit_writes: &[rsleigh::Vn],
) -> NodeId {
    let (node, _value, _clobber_outs) = b
        .build_call_other_modeled(user_op_id, name, args, ret_ty, implicit_reads, implicit_writes)
        .expect("callother_node helper");
    node
}
```

- [ ] **Step 3: Update `get_vn_with_callother_clobber.rs`**

Open the file.  For each test that asserts "the CallOther's clobber slot for register X recovers Vn(X)", thread the matching reg through the helper's `implicit_writes` parameter.

Example transformation: a test that called `b.build_call_other(7, &args, None)` and then asserted that `pattern::Match::get_vn(c, &graph)` returns `RAX` for some clobber slot — now calls `callother_node(b, "cpuid", 7, &args, None, &[], &[rax_vn, rcx_vn, rdx_vn, rbx_vn])` and asserts the same recovery.  Use `ir::test_utils::reg_vn` (or whatever helper exists) to construct the Vn values matching the assertion.

- [ ] **Step 4: Update all callers of `callother_node`**

Run the grep again to find every call.  Add the two trailing slice arguments — most will be `&[], &[]` (empty ABI for tests that don't care about clobbers).

- [ ] **Step 5: Update `dead_branch/tests.rs`**

In `crates/opt/src/dead_branch/tests.rs` line ~286, change `b.build_call_other("cpuid", 0, &[], None)` (v1 form, may have been migrated in v1's plan to use a name) to:
```rust
b.build_call_other_modeled(0, "cpuid", &[], None, &[], &[]).map(|t| t.0)
```
or use `callother_node(b, "cpuid", 0, &[], None, &[], &[])`.

- [ ] **Step 6: Compile + run**

Run: `cargo test -p pattern -p opt 2>&1 | tail -15`

Expected: all tests pass.  If a `get_vn` recovery test fails with the updated clobber set, the assertion likely needs updating to match the new precise clobber order.

- [ ] **Step 7: Commit**

```bash
git add crates/pattern/tests crates/opt/src/dead_branch/tests.rs
git commit -m "$(cat <<'EOF'
pattern, opt: migrate CallOther test sites to build_call_other_modeled

Test helper callother_node grows two parameters (implicit_reads,
implicit_writes).  Tests that asserted on the v1 conservative-clobber
default updated to thread explicit Vn slices.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Bsdfinder smoke + final verification

- [ ] **Step 1: Run the full workspace test suite**

```bash
cd /home/mike/Desktop/strider
cargo test --workspace 2>&1 | grep -E "test result" | awk -F'[ ;]+' \
    '{passed+=$4; failed+=$6; ignored+=$8} END {print "TOTAL: " passed " passed, " failed " failed, " ignored " ignored"}'
```

Expected: 0 failures.

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --workspace 2>&1 | tail -20
```

Expected: clean, or only pre-existing warnings (not introduced by v2).

- [ ] **Step 3: Verify the trap-fix end-to-end on real kernels (regression check from v1)**

Save as `/tmp/v2_verify.py`:
```python
import strider, strider.errors

KERNELS = {
    'x86_64':  ('/home/mike/Desktop/bsdfinder/kernels/linux/x86_64/4.19.0-amd64/vmlinux',
                strider.SleighArch.x86_64(),
                strider.CallingConvention.x86_64_linux_kernel(),
                ['__fentry__','mcount']),
    'aarch64': ('/home/mike/Desktop/bsdfinder/kernels/linux/aarch64/4.19.0-arm64/vmlinux',
                strider.SleighArch.aarch64(),
                strider.CallingConvention.aarch64_linux_kernel(),
                []),
}

for arch_name, (path, sleigh, cc, fentry_stubs) in KERNELS.items():
    print(f"=== {arch_name} ===")
    mem = strider.MemoryMap()
    mem.add_region_from_elf(path)
    try: mem.apply_elf_relocations(path)
    except Exception: pass
    syms = mem.symbols()
    per_addr = {}
    if arch_name == 'x86_64':
        ap = strider.CallingConvention.x86_64_all_preserving()
        for s in fentry_stubs:
            if s in syms: per_addr[syms[s]] = ap
    for sym in ('commit_creds','do_exit','do_task_dead','__schedule','__alloc_pages_nodemask'):
        if sym not in syms:
            print(f'  {sym}: missing'); continue
        addr = syms[sym]
        try: max_size = mem.function_max_size(sym)[1]
        except Exception: max_size = None
        try:
            strider.run(arch=sleigh, cc=cc, mem=mem, rom=mem, entry=addr,
                        allow_code_before_start_addr=True,
                        function_max_size=max_size, per_address_ccs=per_addr)
            print(f'  {sym}: OK')
        except Exception as e:
            print(f'  {sym}: FAIL {type(e).__name__}: {e}')
```

Run: `/home/mike/Desktop/strider/crates/strider-py/.venv/bin/maturin develop --manifest-path /home/mike/Desktop/strider/crates/strider-py/Cargo.toml 2>&1 | tail -3 && /home/mike/Desktop/strider/crates/strider-py/.venv/bin/python /tmp/v2_verify.py 2>&1 | tail -20`

Expected: x86_64 all 5 OK.  aarch64: 3 OK (commit_creds, do_exit, do_task_dead) + 2 with the pre-existing "split address not found" cfg bug (unrelated to this work — same as v1).

- [ ] **Step 4: Run the bsdfinder offset smoke**

```bash
cd /home/mike/Desktop/bsdfinder
.venv/bin/python -c "
from bsdfinder.sweep import sweep
res = sweep('kernels/linux/x86_64/4.19.0-amd64', names=[
    'comm_in_struct_task_struct',
    'pid_in_struct_task_struct',
    'cred_in_struct_task_struct',
])
for k, v in res.items():
    print(k, v)
" 2>&1 | tail -10
```

Expected: same offsets as v1's smoke (0x678, 0x4d0, 0x670).  Any regression here means the precise-ABI clobbers broke a pattern that was tolerating the v1 over-clobber.

- [ ] **Step 5: Harvest any new unknown user-ops**

If Step 3 or Step 4 surfaced `UnknownUserOpError` for a name not in the table, add it to `crates/target/src/user_ops.rs` with the appropriate classification (likely `Call(UserOpAbi { all-empty })` or `NoOp` depending on the op).  Document each addition in its commit.

- [ ] **Step 6: Update CLAUDE.md**

In `CLAUDE.md`, find the "callother classification" / `target::user_ops` section and update to mention:
* The `UserOpAbi` struct.
* The three-variant enum (NoOp / NoReturn / Call).
* No `Opaque` variant.
* The classify table is consumed by both cfg (NoReturn detection) and strider (full dispatch + ABI resolution).

- [ ] **Step 7: Final commit**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs: update CLAUDE.md for CallOther v2 (precise per-op ABIs)

UserOpClass: NoOp | NoReturn | Call(UserOpAbi).  Opaque gone.
ir::FunctionBuilder gains build_call_other_modeled; v1 entries
(build_call_other(name) / _opaque / _with_clobbers / CallOtherOutcome)
removed.  Bsdfinder corpus regression-checked.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Self-Review Checklist (post-write)

**Spec coverage:**
- ✅ Goal 1 (no Opaque variant) → Task 1 + Task 6.
- ✅ Goal 2 (UserOpAbi describes the delta) → Task 1 (struct) + Task 2 (consumer).
- ✅ Goal 3 (no conservative-clobber default) → Task 6 (removes the helper) + Task 9 (deletes its test).
- ✅ Goal 4 (precise memory edge) → Task 2 (build_call_other_modeled doesn't advance memory) + Task 3 (strider advances IFF abi.memory_edge).
- ✅ Goal 5 (hard cutover, no shim) → Task 6.
- ✅ Goal 6 (memory barriers as NoOp) → Task 1 (table reclassifies).

**Placeholder scan:** Task 5 (sysret/trap verification) is intentionally exploratory — outcome may be no-op (no commit).  Task 11 step 5 (harvest) is iterative.  Both are bounded discovery work, not vague placeholders.

**Type consistency:**
- `UserOpAbi { implicit_reads, implicit_writes, memory_edge }` — used identically in Task 1 (definition), Task 3 (consumer), Task 4 (test asserts).
- `build_call_other_modeled` signature — `(user_op_id, name, args, output_ty, implicit_reads_vns, implicit_writes_vns) -> Result<(NodeId, Option<NodeOutputId>, Vec<NodeOutputId>)>` — used identically in Task 2 (definition), Tasks 3, 7, 8, 10 (callers).
