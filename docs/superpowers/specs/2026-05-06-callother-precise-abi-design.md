# Precise per-user-op CallOther ABIs (v2)

Replace v1's `CallOtherClass::Opaque` catch-all with explicit
`CallOtherClass::Call(CallOtherAbi)` entries that give each Sleigh user-op
its precise ISA-level register footprint and memory-edge effect.
Hard cutover: no `Opaque` exists after this lands.  The IR builder
clobbers only the registers an op actually touches (typically zero
or two, never "every tracked variable except SP" as v1's
conservative default did).

This is a follow-up to
`2026-05-05-callother-classification-design.md` (v1).  v1 introduced
the classification table; v2 replaces the imprecise Opaque variant
with structured per-op ABIs.

## Motivation

Every entry currently classified `Opaque` in
`crates/target/src/call_other_abi.rs` is being modelled imprecisely.  The
v1 IR builder's Opaque arm conservatively clobbers every tracked
variable except the stack pointer — destroying SSA provenance for
every register on every CallOther site, even when the actual ISA op
only writes one register.

Concrete pain: an aarch64 function that reads a system register via
`mrs x0, <unmodelled_sysreg>` lifts (under v1) to a CallOther whose
output slots clobber x1/x2/.../x30 in addition to x0.  Subsequent
reads of x1 (e.g. function arg 1) are bound to the CallOther's
clobber slot, not to `FunctionArg(x1)` — and pattern queries like
`call().arg(1, function_arg(x1))` stop matching.  The MRS only
writes x0; killing x1's provenance is a soundness-preserving but
precision-destroying lie.

Every Opaque entry has a known ISA-level register footprint.  The
register names of `cpuid`'s implicit writes (EAX/EBX/ECX/EDX) live
in Intel's manual.  ARM's SMCCC fixes the I/O register set for
HVC / SMC.  NEON / SVE ops have their I/O registers in Sleigh's
pcode args directly.  Encoding these explicitly gives patterns the
precision they need.

## Goals

1. **No `Opaque` variant.**  `CallOtherClass = NoOp | NoReturn |
   Call(CallOtherAbi)`.  Every previously-Opaque entry gets reclassified
   into exactly one of the three.

2. **`CallOtherAbi` describes the *delta* on top of Sleigh's pcode.**
   Sleigh's pcode insn already carries per-instruction-encoded
   operands as `inputs[1..]` and `output`.  The ABI specifies only
   the *implicit* (ISA-fixed, not in pcode) reads, writes, and
   memory effect.

3. **The IR builder clobbers only what the ABI specifies.**  No more
   "every tracked variable except SP" default.  An op with empty
   `implicit_writes` gets exactly the pcode-explicit output as its
   one value slot; no other tracked variable is rebound.

4. **Memory edge is precise.**  An op with `memory_edge: false`
   does not advance the IR's memory token — `LoadForward` and
   similar opt passes can still forward across it.  An op with
   `memory_edge: true` advances the token, breaking forwarding (as
   would happen across any real memory write).

5. **Hard cutover, no transitional shim.**  v1's
   `CallOtherClass::Opaque` and the IR's `build_call_other(name, …)`
   /`build_call_other_opaque` /`CallOtherOutcome` surface are
   removed in the same change-set.  No "v1-compat fallback" lingers.

6. **Memory barriers are `NoOp`.**  DMB / DSB / ISB / DC_CVAC and the
   x86 LOCK / UNLOCK prefix markers all become `NoOp` — they have no
   IR-visible register or memory-value effect, only ordering effects
   strider does not currently model.

## Non-goals

* **Per-arch keying.**  The classify table remains keyed by name
  string only.  None of the names involved collide across the
  arches we currently support.

* **Lane-level vector modelling.**  NEON / SVE ops touch full
  vector registers (V0…V31, Z0…Z31); their ABIs operate at
  register-level, not lane-level.  Sleigh's pcode already passes
  the actual chosen vector reg via `inputs[1..]`; the empty ABI is
  correct for our analyses.

* **Per-sysreg modelling for `UnkSytemRegRead`.**  We add the
  empty-ABI entry per c1 so lifts succeed; per-sysreg constant
  values (e.g. "reading `CurrentEL` returns 1 in user mode") are a
  future spec on top of this one.

* **Memory ordering analysis.**  `memory_edge: bool` is just the
  IR's value-flow memory token; it does not encode happens-before /
  acquire-release ordering.  A future ordering analysis would need
  a separate signal.

* **New `Pure` enum variant.**  Empty-ABI `Call(CallOtherAbi { all-empty })`
  is the canonical way to express "Sleigh's pcode is fully sufficient";
  no separate `Pure` variant is added (decided in design discussion —
  uniform dispatch through `Call` is preferred over a fourth variant).

## Architectural facts

* v1 (`2026-05-05-callother-classification-design.md`) put the
  classification table in `crates/target/src/call_other_abi.rs`,
  consumed by `cfg::region_builder` (NoReturn termination only) and
  by `ir::FunctionBuilder::build_call_other(name, …)` (full IR
  shape dispatch via the `CallOtherOutcome` return type).

* v1's `CallOtherClass::Opaque` arm in `build_call_other` calls
  `build_call_other_opaque`, which calls
  `build_call_other_with_clobbers` with the conservative
  `every-tracked-variable-except-SP` clobber set.  The output
  CallOther node has `[ctrl, mem, args…]` inputs and
  `[ctrl, mem, value?, clobber_for_each_tracked_var…]` outputs,
  and each clobber output rebinds the corresponding tracked var.

* `rsleigh::Sleigh::regs() -> SleighRegs` exists and provides
  `name_to_vn(&str) -> Option<rsleigh::Vn>` for resolving a
  register name to a Sleigh varnode.  The `Strider` layer holds a
  `Sleigh<R>` handle (via `Cfg<R>::sleigh`) and can resolve names
  cheaply at lift time.

* `ir::FunctionBuilder` does not store a `Sleigh` handle.  It
  receives pre-resolved Vn lists at construction (CC's args /
  callee-saved / clobbered).  The v2 design keeps this layering:
  the strider layer resolves ABI register names → Vns and passes
  the resolved Vn slices to the IR builder.

* The CallOther node's expected_signature in
  [`crates/ir/src/node_signature.rs`] tolerates a variadic value
  output tail (`out_tail: ANY_VAL`), so emitting a CallOther with
  zero clobber slots, one clobber slot, or N clobber slots is
  signature-compatible.

* `cfg::region_builder::process_new_insn`'s v1 `Opcode::CallOther`
  arm (added in commit 26898da7) consults `classify` and finishes
  the region as `RegionTerminator::NoReturn` only when the result
  is `CallOtherClass::NoReturn`.  All other classifications fall
  through to today's catch-all (insn appended to `self.insns`,
  loop continues).  v2 leaves this arm unchanged: cfg never reads
  the `CallOtherAbi` payload.

## Design

### Module changes — `target::call_other_abi`

```rust
// crates/target/src/call_other_abi.rs (v2)

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallOtherClass {
    /// True no-op in the IR's data-flow / control-flow / memory model.
    /// `build_call_other_*` is never called (strider's
    /// `handle_call_other` returns Ok(()) without IR construction).
    NoOp,

    /// Trap instruction — control flow ends here.  cfg terminates
    /// the region as `RegionTerminator::NoReturn`; IR's
    /// `build_call_other_terminal` emits a CallOther with
    /// `[ctrl, mem]` inputs and `[ctrl, mem]` outputs; outputs dangle.
    NoReturn,

    /// Op with a precise ABI describing its register footprint and
    /// memory effect *beyond* what Sleigh's pcode already encodes.
    Call(CallOtherAbi),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallOtherAbi {
    /// Register names this op reads beyond Sleigh's pcode-explicit
    /// `inputs[1..]`.  Resolved to `rsleigh::Vn` by the strider
    /// layer at lift time and appended to the CallOther's value
    /// inputs.  Use the exact Sleigh register name (case-sensitive;
    /// e.g. "EAX" / "RAX" on x86_64, "x0" / "w0" on aarch64).
    pub implicit_reads: &'static [&'static str],

    /// Register names this op writes (or scratch-clobbers) beyond
    /// Sleigh's pcode-explicit `output`.  Resolved by the strider
    /// layer; each becomes one extra clobber output slot on the
    /// CallOther node.  The strider layer rebinds the matching
    /// tracked variable to that slot.
    pub implicit_writes: &'static [&'static str],

    /// Whether this op advances the IR's memory edge.  True for ops
    /// whose effect on memory is observable to subsequent loads /
    /// stores (syscall, port I/O, cache writeback).  False for pure
    /// register-level computation (cpuid, rdtsc, NEON math).
    ///
    /// Implementation note: when false, the strider layer does NOT
    /// call `advance_cur_region_memory` after `build_call_other_modeled`.
    /// The CallOther's mem_in is the current region memory token;
    /// its mem_out dangles and subsequent ops continue from mem_in.
    pub memory_edge: bool,
}

#[must_use]
pub fn classify(name: &str) -> Option<CallOtherClass> { … }
```

The `classify` function's body becomes the table below.

### The reclassified table

Counting from v1's existing 33 entries (2 NoOp + 3 NoReturn + 28
Opaque), v2's allocation:

**NoOp (10 total = 2 existing + 8 promoted from Opaque)**

| Name | Why NoOp |
|---|---|
| `setEndianState` | (existing) ARM SETEND — decoder context bit only. |
| `setISAMode` | (existing) ARM ISA-mode bit only. |
| `DataMemoryBarrier` | DMB — orders observability; no register or value effect. |
| `DataSynchronizationBarrier` | DSB — same; just stronger. |
| `InstructionSynchronizationBarrier` | ISB — pipeline flush; no IR effect. |
| `DC_CVAC` | Cache writeback to PoC; no register effect, no value-flow effect. |
| `LOCK` | x86 lock prefix marker — the next pcode insn has the actual atomic load/store. |
| `UNLOCK` | Sleigh-internal lock-prefix-end marker; no real instruction. |
| `Hint_Prefetch` | ARM PRFM hint; no architectural state change. |
| `Yield` | ARM YIELD / x86 PAUSE; scheduler hint, no IR effect. |

**NoReturn (5 total = 3 existing + 2 promoted from Opaque)**

| Name | Why NoReturn |
|---|---|
| `invalidInstructionException` | (existing) x86 ud2 — BUG_ON trap. |
| `SoftwareBreakpoint` | (existing) aarch64 brk — BUG_ON trap. |
| `UndefinedInstructionException` | (existing) ARM-32 UDF — BUG_ON trap. |
| `sysret` | x86 SYSRET — control transfers to user space; kernel function does not continue. |
| `trap` | ARM TRAP — fatal trap; control does not return. |

(Implementation note: for `sysret`, verify Sleigh's actual emitted
pcode shape during Task 5 of the plan.  If Sleigh already emits a
plain `Return` opcode after `CALLOTHER(sysret)`, the cfg builder
already terminates the region and the NoReturn classification is
redundant-but-harmless.)

**Call (18 total = formerly Opaque)**

Each entry below specifies `implicit_reads` / `implicit_writes` /
`memory_edge`.  Register names are the exact Sleigh names — verify
against `Sleigh::regs()` during implementation.

Linux x86_64 syscall-class (SYSCALL ABI):
```rust
"syscall" => Call(CallOtherAbi {
    implicit_reads:  &["RAX", "RDI", "RSI", "RDX", "R10", "R8", "R9"],
    implicit_writes: &["RAX", "RCX", "R11"],
    memory_edge:     true,   // syscall affects arbitrary kernel state
}),
```

Linux ARM syscall-class (SWI):
```rust
"swi" => Call(CallOtherAbi {
    implicit_reads:  &["r7", "r0", "r1", "r2", "r3", "r4", "r5", "r6"],
    implicit_writes: &["r0"],
    memory_edge:     true,
}),
```

ARM SMCCC (HVC and SMC use the same ABI: X0..X7 in, X0..X3 out):
```rust
"CallHyperVisor" => Call(CallOtherAbi {
    implicit_reads:  &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
    implicit_writes: &["x0", "x1", "x2", "x3"],
    memory_edge:     true,
}),
"CallSecureMonitor" => Call(CallOtherAbi {
    implicit_reads:  &["x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7"],
    implicit_writes: &["x0", "x1", "x2", "x3"],
    memory_edge:     true,
}),
```

x86 SWAPGS (touches the synthetic GS_base MSR; no general-reg effect):
```rust
"swapgs" => Call(CallOtherAbi {
    implicit_reads: &[], implicit_writes: &[],
    memory_edge:    false,
}),
```

x86 CPUID (Sleigh emits `CALLOTHER(cpuid, EAX)` — EAX is in pcode args;
ECX is read for subleaves but Sleigh's spec doesn't model that;
EAX/EBX/ECX/EDX are written but Sleigh's spec doesn't model that):
```rust
"cpuid" => Call(CallOtherAbi {
    implicit_reads:  &["ECX"],
    implicit_writes: &["EAX", "EBX", "ECX", "EDX"],
    memory_edge:     false,
}),
```

x86 RDTSC / RDPKRU:
```rust
"rdtsc" => Call(CallOtherAbi {
    implicit_reads:  &[],
    implicit_writes: &["EAX", "EDX"],
    memory_edge:     false,
}),
"rdpkru_u32" => Call(CallOtherAbi {
    implicit_reads:  &["ECX"],            // must be 0 per ISA
    implicit_writes: &["EAX", "EDX"],     // EDX cleared per ISA
    memory_edge:     false,
}),
```

x86 IN / OUT (port I/O — pcode args carry the port + value; we just
note the memory edge):
```rust
"in"  => Call(CallOtherAbi {
    implicit_reads: &[], implicit_writes: &[],
    memory_edge:    true,
}),
"out" => Call(CallOtherAbi {
    implicit_reads: &[], implicit_writes: &[],
    memory_edge:    true,
}),
```

ARM exclusive-monitor primitives (synthetic monitor-flag effect; no
general-reg effect; LDREX/STREX themselves emit pcode loads/stores):
```rust
"ExclusiveMonitorPass" => Call(CallOtherAbi {
    implicit_reads: &[], implicit_writes: &[],
    memory_edge:    false,
}),
"ExclusiveMonitorsStatus" => Call(CallOtherAbi {
    implicit_reads: &[], implicit_writes: &[],
    memory_edge:    false,
}),
```

ARM unmodelled sysreg read (per c1 — see spec discussion at
`docs/superpowers/specs/2026-05-06-callother-precise-abi-design.md`,
Q&A on UnkSytemRegRead):
```rust
"UnkSytemRegRead" => Call(CallOtherAbi {
    implicit_reads: &[], implicit_writes: &[],
    memory_edge:    false,
}),
```

NEON / SVE / multi-precision ops (Sleigh's pcode is fully sufficient;
no implicit channel):
```rust
"NEON_rev64"  => Call(CallOtherAbi { implicit_reads: &[], implicit_writes: &[], memory_edge: false }),
"NEON_sqshl"  => Call(CallOtherAbi { implicit_reads: &[], implicit_writes: &[], memory_edge: false }),
"NEON_uaddlv" => Call(CallOtherAbi { implicit_reads: &[], implicit_writes: &[], memory_edge: false }),
"SVE_fnmla"   => Call(CallOtherAbi { implicit_reads: &[], implicit_writes: &[], memory_edge: false }),
"MP_INT_ABS"  => Call(CallOtherAbi { implicit_reads: &[], implicit_writes: &[], memory_edge: false }),
```

### IR builder changes

**Removed (entire APIs):**

* `ir::FunctionBuilder::build_call_other(name, user_op_id, args, output_ty) -> Result<CallOtherOutcome>` — v1's high-level dispatch entry.
* `ir::FunctionBuilder::build_call_other_opaque(...)` — v1's helper for the conservative-clobber Opaque path.
* `ir::FunctionBuilder::build_call_other_with_clobbers(...)` — v1's lower-level builder; no caller after v2.
* `ir::CallOtherOutcome` — v1's three-variant dispatch return type.

**Kept from v1, unchanged:**

* `ir::FunctionBuilder::build_call_other_terminal(user_op_id, name) -> Result<NodeId>` — emits the `[ctrl, mem]` → `[ctrl, mem]` CallOther for `NoReturn`.

**Added:**

```rust
// crates/ir/src/builder/call.rs
impl FunctionBuilder {
    /// Emit a CallOther node with the precise per-op ABI shape:
    /// pcode-explicit `args` plus `implicit_reads_vns` as inputs;
    /// pcode-explicit value (when `output_ty.is_some()`) plus one
    /// clobber output slot per `implicit_writes_vns` Vn as outputs.
    ///
    /// The strider layer is responsible for:
    ///   * Resolving the ABI's register *name* lists to Vn slices
    ///     (via `Sleigh::regs().name_to_vn`).
    ///   * Calling `advance_cur_region_memory` on the result IFF the
    ///     ABI's `memory_edge` is true.
    ///   * Calling `write_vn(vn, clobber_slot)` for each
    ///     `implicit_writes_vns` Vn, in order, with the corresponding
    ///     clobber output slot.
    ///   * Calling `write_vn(insn.output, value)` if the pcode insn
    ///     had an output and the returned `value` is `Some`.
    ///
    /// Returns `(node_id, value_output, clobber_output_slots)`.
    /// `value_output` is `Some` iff `output_ty.is_some()`.
    /// `clobber_output_slots.len() == implicit_writes_vns.len()`.
    pub fn build_call_other_modeled(
        &mut self,
        user_op_id: u64,
        name: &str,                                  // for set_call_other_name
        args: &[NodeOutputId],                       // pcode-explicit inputs
        output_ty: Option<NodeOutputType>,           // pcode-explicit output type (if any)
        implicit_reads_vns: &[rsleigh::Vn],          // resolved ABI implicit reads
        implicit_writes_vns: &[rsleigh::Vn],         // resolved ABI implicit writes
    ) -> Result<(NodeId, Option<NodeOutputId>, Vec<NodeOutputId>)>;
}
```

The implementation:
1. Resolves `ctrl` and `memory` from the current region.
2. Reads each `implicit_reads_vns` Vn via the existing `read_variable` machinery, producing additional value inputs.
3. Builds the CallOther node with inputs `[ctrl, memory, *args, *implicit_read_values]` and outputs `[Control, Memory, optional Value, *Clobber per implicit_write]`.
4. Calls `advance_cur_region_ctrl(ctrl_out)`. **Does not** advance memory — the strider layer decides based on `memory_edge`.
5. Stamps `Graph::call_other_names[node] = name`.
6. Returns `(node_id, value_output, clobber_outputs)`.

### Strider layer — `handle_call_other` simplified to dispatch + ABI resolution

```rust
// crates/strider/src/strider/insn/mod.rs
fn handle_call_other(&mut self, insn: &rsleigh::Insn) -> Result<()> {
    let id_vn = pcode_lift::first_input_or_err(insn)?;
    if id_vn.addr_space != rsleigh::VnSpace::CONST {
        anyhow::bail!("opcode {:?} expects CONST input at position 0", insn.opcode);
    }
    let user_op_id = id_vn.addr_off;
    let user_op_id_u32 = u32::try_from(user_op_id)
        .map_err(|_| anyhow::anyhow!("user-op id {user_op_id:#x} exceeds u32"))?;
    let name = self.cfg.sleigh.user_op_name(user_op_id_u32).ok_or_else(|| {
        anyhow::anyhow!("user-op id {user_op_id_u32} not in Sleigh's user_op table")
    })?;

    let class = target::call_other_abi::classify(name)
        .ok_or_else(|| ir::error::UnknownCallOtherError { name: name.to_string() })?;

    match class {
        target::call_other_abi::CallOtherClass::NoOp => Ok(()),

        target::call_other_abi::CallOtherClass::NoReturn => {
            let _ = self.builder.build_call_other_terminal(user_op_id, name)?;
            Ok(())
        }

        target::call_other_abi::CallOtherClass::Call(abi) => {
            // Resolve pcode-explicit inputs.
            let args: Vec<ir::Value> = if insn.inputs.len() > 1 {
                insn.inputs[1..].iter().map(|vn| self.read_vn(vn)).collect::<Result<_>>()?
            } else {
                Vec::new()
            };
            let output_ty: Option<ir::node::NodeOutputType> = match insn.output.as_ref() {
                Some(out_vn) => Some(out_vn.size.try_into()?),
                None => None,
            };
            // Resolve ABI register names → Vns via Sleigh.
            let regs = self.cfg.sleigh.regs()?;
            let implicit_reads_vns: Vec<rsleigh::Vn> = abi.implicit_reads
                .iter()
                .map(|n| regs.name_to_vn(n).ok_or_else(|| anyhow::anyhow!(
                    "user-op {name:?} ABI references unknown register name {n:?}"
                )))
                .collect::<Result<_>>()?;
            let implicit_writes_vns: Vec<rsleigh::Vn> = abi.implicit_writes
                .iter()
                .map(|n| regs.name_to_vn(n).ok_or_else(|| anyhow::anyhow!(
                    "user-op {name:?} ABI references unknown register name {n:?}"
                )))
                .collect::<Result<_>>()?;

            let (_node, value, clobber_outs) = self.builder.build_call_other_modeled(
                user_op_id, name, &args, output_ty,
                &implicit_reads_vns, &implicit_writes_vns,
            )?;

            // Memory edge — strider decides whether to advance the memory token.
            if abi.memory_edge {
                // Re-fetch the CallOther's mem_out (slot 1 of node outputs).
                let mem_out = self.builder.body().graph.node_outputs(_node)[1];
                self.builder.advance_cur_region_memory(mem_out)?;
            }

            // Bind pcode-explicit output to its varnode.
            if let (Some(out_vn), Some(val)) = (insn.output.as_ref(), value) {
                self.write_vn(out_vn, val)?;
            }

            // Bind each implicit-write to its register's tracked variable.
            for (vn, slot) in implicit_writes_vns.iter().zip(clobber_outs) {
                self.write_variable(vn, slot)?;
            }

            Ok(())
        }
    }
}
```

### Cfg layer — unchanged

`cfg::region_builder::process_new_insn`'s v1 `Opcode::CallOther` arm
continues to consult `classify()` only to detect `NoReturn` and
finish the region.  It never reads the `CallOtherAbi` payload.

### Pattern surface — unchanged

`pattern::call_other()`, `.user_op_id(n)`, `.name(s)`, and `.arg(idx, p)`
all continue to work the same.  The shape of the matched CallOther
node changes (precise clobbers instead of conservative all-vars),
which means some existing pattern queries that anchored on
"CallOther's clobber slot N is reg X" will need to know the precise
clobber order — but the API surface is identical.

## Test migration

The breaking change at the test layer is: any test that built a
CallOther via v1's `build_call_other(name, …)` or
`build_call_other_with_clobbers` now needs the new
`build_call_other_modeled` signature with explicit Vn slices.

Affected files (re-survey during Task 7 of the plan):

* `crates/ir/src/builder/tests.rs` — `build_call_other_no_value_emits_clobber_per_tracked_var` and `build_call_other_rebinds_tracked_variables` test the v1 conservative-clobber default behaviour, which is gone.  **Delete** these two tests; the behaviour they test no longer exists.  The `build_call_other_with_value_keeps_value_in_slot_2_clobber_starts_at_3` test similarly anchors on the v1 Opaque shape — rewrite using `build_call_other_modeled` with explicit Vn slices.
* `crates/ir/tests/call_other_classification.rs` — v1's outcome-level test.  Rewrite NoOp / NoReturn / Built tests to call `build_call_other_terminal` / `build_call_other_modeled` directly; assert on the precise IR shape; the "Unknown name → UnknownCallOtherError" case moves to `handle_call_other` (strider-side test).
* `crates/ir/tests/call_other_conservative_clobber.rs` — entire file tests v1's conservative-clobber default.  **Delete** the file.
* `crates/pattern/tests/get_vn_with_callother_clobber.rs` — v1 used `build_call_other("cpuid", 7, &[], None)` and asserted on the all-vars clobber set's recovery.  Rewrite using `build_call_other_modeled` with explicit `&[EAX, EBX, ECX, EDX]` writes; assert recovery of those four named clobbers.
* `crates/pattern/tests/matching/support/graph.rs` — test helper `callother_node` takes a name; update it to take `(name, user_op_id, args, ret_ty, implicit_reads, implicit_writes)` and forward to `build_call_other_modeled`.
* `crates/opt/src/dead_branch/tests.rs:286` — uses `build_call_other("cpuid", 0, &[], None)` to insert an opaque side-effect.  Update to `build_call_other_modeled` with explicit empty slices (since the test only cares that the node exists, not its clobber set).

A new test added by this spec:

* `crates/strider/tests/call_other_precise_abi.rs` — integration-shaped tests:
  * Lift a tiny x86_64 fixture containing `cpuid; mov rcx, rax`.  Assert: the IR's `cpuid` CallOther has exactly four clobber outputs (EAX/EBX/ECX/EDX) in that order, and `rcx` after the cpuid is bound to a *fresh* SSA value (not RAX-from-cpuid; it should be unrelated to cpuid since cpuid doesn't write RCX as RAX-aliased).  Wait — RCX is in EBX/ECX/EDX clobber set as "ECX" (writes to ECX zero-extend to RCX on x86_64), so rcx after cpuid IS bound to the cpuid clobber.  Adjust assertion accordingly.
  * Lift a tiny aarch64 fixture containing `mrs x0, S3_3_C15_C0_7`.  Assert: only x0 is rebound; x1..x30 retain their prior SSA values (verify by reading e.g. `function_arg(x1)` after the MRS — should still resolve).

## Migration / rollout

This is a breaking change to the IR builder API.  Sequence:

1. Update `target::call_other_abi` (CallOtherAbi struct, reclassified table).
   Tests in target verify all 33 entries are exhaustively classified.
2. Add `build_call_other_modeled` to ir.  v1 helpers still exist
   here; both old and new code compiles.
3. Update strider's `handle_call_other` to consult the new ABI and
   call `build_call_other_modeled`.  v1 caller of
   `build_call_other(name, …)` is replaced.
4. Migrate every test site (Task 7) to the new API.
5. Remove v1's `build_call_other(name, …)`, `build_call_other_opaque`,
   `build_call_other_with_clobbers`, and `CallOtherOutcome`.
6. Run the bsdfinder smoke suite to confirm no regression.
7. Final clippy + workspace test sweep.

Steps 1–4 are net-additive (both APIs exist).  Step 5 is the
breaking-cleanup pass.

## Risks & open questions

* **Sleigh register names case-sensitivity / spelling drift.**  The
  ABI tables hardcode names like "RAX", "EAX", "x0", "r7".  If
  Sleigh's spec uses a different exact case (e.g. "rax"),
  `name_to_vn` returns `None` and the lift errors out cleanly
  (`anyhow!("user-op {name} ABI references unknown register name
  {n}")`).  Implementer verifies each name during Task 1 by running
  `cargo test -p target call_other_abi` after each batch of additions.

* **`sysret` classification.**  Best-effort marked NoReturn.  If
  Sleigh's pcode for SYSRET emits a clean `Return` after the
  CALLOTHER, the cfg builder already terminates the region and
  NoReturn is harmless-but-unnecessary.  If Sleigh emits only the
  CALLOTHER and falls through, NoReturn is essential.  Implementer
  verifies via the existing `dump_pcode` workflow during Task 5.

* **`swapgs` and `ExclusiveMonitor*` synthetic effects.**  These ops
  touch state strider doesn't track (GS_base MSR / monitor flag).
  Modelling them as empty-ABI Call captures "this op happened" for
  pattern queries but doesn't track the synthetic state.  Acceptable.

* **NEON / SVE register naming for tests.**  The new test fixtures
  may need to reference vector register names ("V0", "Z0").  These
  exist in Sleigh's spec; verify spelling against `Sleigh::regs()`
  during Task 7.

* **CPUID and the EAX register's pcode-explicit position.**  Sleigh's
  spec calls `cpuid(EAX)` so EAX is in `inputs[1]`.  The ABI lists
  ECX in `implicit_reads`.  EAX is NOT listed in `implicit_reads` to
  avoid double-reading.  Verify Sleigh actually emits it that way
  during Task 1's table validation.

* **Memory-edge for ports (`in`/`out`).**  Marked true: port I/O
  affects external (non-RAM) state but is in the same memory edge as
  RAM accesses today.  A future spec could split memory edges
  (RAM vs MMIO vs port); v2 keeps a single edge.

* **The breaking-change blast radius is large.**  Every test that
  built a CallOther needs migration.  The migration is mechanical
  but touches three crates' test surfaces.
