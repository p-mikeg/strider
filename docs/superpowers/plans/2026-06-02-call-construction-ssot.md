# Call/CallOther Construction SSoT Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `strider-ir`'s `FunctionBuilder` the single source of truth for emitting `Call` and `CallOther` nodes — both constructed in-crate from a vn-resolved descriptor — by giving the builder register-aliasing (`read_reg_vn`/`write_reg_vn`) and storing the per-call footprint as a natural-shape `CallDescriptor` enum.

**Architecture:** `Function` gains an `endianness` scalar, which lets the register-aliasing core move out of `strider-lift`'s `ValueLifter` into `FunctionBuilder` (the lifter's `read_vn`/`write_vn` keep only the Sleigh-coupled space dispatch). Call gains an SP input before its args and a distinct ret-val output group; both call kinds construct in the builder from their own footprint (`BuiltCallingConvention` for Call, a new vn-resolved `BuiltCallOtherAbi` for CallOther) and record it in a `CallDescriptor` side-table.

**Tech Stack:** Rust workspace (strider-ir, strider-lift, strider-analyze, strider-target, strider-py/PyO3), `cargo test`/`clippy`, `uv run pytest`.

---

## Background context (read before starting)

The pipeline is Binary → CFG → IR (`strider-lift`) → opt/pattern (`strider-analyze`). Calls today are built two different ways:

- **Call** is resolved *inside* `strider-ir`'s `FunctionBuilder::build_call_with_cc` (`crates/strider-ir/src/builder/call.rs`) using `read_variable`/`write_variable` (exact tracked reads — works because Call operands are full registers).
- **CallOther** is resolved *in the lifter* (`crates/strider-analyze/src/strider/insn/mod.rs::handle_call_other_modeled`) using `read_vn`/`write_vn` (register aliasing, needed because CallOther operands are pcode-granular: UNIQUE temps, sub-registers like `EAX`), then passed as values to the value-only `build_call_other`.

`read_vn`/`write_vn` live in `crates/strider-lift/src/pcode_lift/vn_io.rs` on `ValueLifter`. The register-aliasing core there (`read_reg_vn`, `write_reg_vn`, `find_largest_fitting_register`, `enter_sub_register`, `calculate_reg_shift_from_container`, `vn_mask`, `build_masked_insert`) depends on exactly ONE thing outside the `FunctionBuilder`: `self.endianness` (one big-endian branch in `calculate_reg_shift_from_container`). Everything else is already `self.builder.*`. The top-of-`read_vn` *space dispatch* (CONST-fold, RAM Load/Store) needs `self.sleigh` and stays in the lifter.

`PerRegionDriver` (`crates/strider-analyze/src/strider/mod.rs`) owns `builder: strider_ir::FunctionBuilder` and exposes `read_vn`/`write_vn` (`crates/strider-analyze/src/strider/vn_io.rs`) by spinning up a `ValueLifter` sharing the builder.

**Invariants from CLAUDE.md / project memory that constrain this work:**
- `Function` must stay `Sync` (strider-py `#[pyclass]`). `endianness` is a `Copy` enum — fine. No `Arc`/`RefCell`.
- Every reachable non-exempt node MUST carry ≥1 asm-fingerprint. Nodes built via `read_reg_vn` pick up the builder's `lift_addr`; this is preserved by the move.
- Panics/`expect`/indexing are acceptable for validator-guaranteed structural invariants; `Result` only for genuinely-fallible ops.
- Never mention Phase/Step/WS identifiers in code, doc comments, or commit messages.
- `strider-ir` already depends on `strider-target` (so `Endianness`, `BuiltCallingConvention`, and a new `BuiltCallOtherAbi` are all reachable) and on `rsleigh`.

**Branch:** continue on `refactor/function-side-tables`. Push to `origin refactor/function-side-tables` after each commit.

**Test gate after every workstream:** `cargo test --workspace` (baseline: 3049 pass, 0 fail — 4 pre-existing fixture failures are NOT from this work; "no NEW failures" is the criterion), `cargo clippy --workspace --all-targets` clean, and `uv run pytest -q` in `crates/strider-py` (841 pass) whenever the lifter or Python surface changed.

---

## Workstream order & rationale

- **WS0 — Return termination revert.** Tiny, isolated correctness fix; unblocks a clean mental model before the call rework. Behavior-restoring.
- **WS1 — `endianness` on `Function` + move register-aliasing core into the builder.** Behavior-neutral foundation; full suite stays green. De-risks the layering change before any call-shape change rides on it.
- **WS2 — `CallDescriptor` enum + `BuiltCallOtherAbi`.** New descriptor types + generalize the `call_cc` side-table. Recording only; construction still goes through existing paths.
- **WS3 — Call ret-val output group.** Split the variadic Call out-tail into ret-vals + clobbers (order-identical relabel; low churn).
- **WS4 — SP-as-Call-input.** The one structural input change; updates signature/validate/consumers/tests. Plant-only (CallStackArgCollect rewiring deferred per decision).
- **WS5 — Construct both kinds in the builder from descriptors.** Delete the lifter's ad-hoc CallOther resolution and the builder's old CC-resolution helpers; both kinds flow through one generalized emit.

Each workstream ends green and is committed + pushed independently.

---

## WS0 — Termination model cleanup (Return self-terminates; CallOther terminates via a flag)

**Goal:** Each builder emit owns its own termination. `build_return` terminates unconditionally (it is always the region exit). The no-return **CallOther** terminates via a `terminate: bool` threaded through `build_call_other`/`build_call_kind` — NOT via an exposed `mark_cur_region_terminated()`. That method becomes private to the builder (callers never invoke termination directly).

**Files:**
- Modify: `crates/strider-ir/src/builder/nodes.rs:476` (`build_return`)
- Modify: `crates/strider-ir/src/builder/call.rs` (`build_call_kind`/`build_call_other` gain `terminate: bool`)
- Modify: `crates/strider-ir/src/region.rs:142` (`mark_cur_region_terminated` → private)
- Modify: `crates/strider-analyze/src/strider/insn/mod.rs:118-127` (no-return CallOther passes `terminate: true`)
- Modify: `crates/strider-analyze/src/strider/insn/control.rs:219,278`
- Modify: `crates/strider-analyze/src/indirect_resolver.rs:289`
- Modify: `crates/strider-ir/src/builder/tests.rs:808-822`

- [ ] **Step 1: Update the builder test to expect self-termination + flag-driven CallOther termination.**

In `crates/strider-ir/src/builder/tests.rs` around line 808-822: change the Return test so it does NOT call `mark_cur_region_terminated` and asserts `cur_region_control()` fails *immediately after* `build_return`/`build_function_return` returns. Add (or adjust) a CallOther test that calls `build_call_other(..., terminate = true)` and asserts the region is terminated afterward, and a sibling with `terminate = false` that asserts control continues (the region's control advanced to the CallOther's control output).

- [ ] **Step 2: Run it to verify it fails.**

Run: `cargo test -p strider-ir --lib builder:: 2>&1 | tail -20`
Expected: FAIL — `build_return` doesn't self-terminate; `build_call_other` has no `terminate` param.

- [ ] **Step 3: Restore termination inside `build_return`.**

In `crates/strider-ir/src/builder/nodes.rs`, `build_return` (line 476): after creating the Return node, call `self.terminate_cur_region()?;` before returning (mirror the pre-refactor body — same helper used at lines 550/572/592). Update the doc comments on `build_return` (460-462) and `build_function_return` (512-513) to say it **terminates the current region**.

- [ ] **Step 4: Thread `terminate` through the call emit.**

In `crates/strider-ir/src/builder/call.rs`: add `terminate: bool` to `build_call_kind`. After creating the node, branch: `if terminate { self.mark_cur_region_terminated()?; } else { self.advance_cur_region_ctrl(outputs[0])?; }` (today it always advances control). Memory advance (`advance_memory`) is unchanged. Add `terminate: bool` to `build_call_other` and forward it. `build_call`/`build_call_with_cc` pass `terminate = false` (a normal Call is inline).

- [ ] **Step 5: Make `mark_cur_region_terminated` private + route the no-return CallOther through the flag.**

In `crates/strider-ir/src/region.rs:142`, drop the `pub` on `mark_cur_region_terminated` (make it `pub(crate)` or private — only `build_call_kind` calls it now). In `crates/strider-analyze/src/strider/insn/mod.rs:118-127`, the no-return CallOther passes `terminate: true` to `build_call_other` and **removes** the standalone `self.builder.mark_cur_region_terminated()?;` at line 127.

- [ ] **Step 6: Drop the redundant Return-site terminations.**

Remove `self.builder.mark_cur_region_terminated()?;` at `control.rs:219`, `control.rs:278`, and `indirect_resolver.rs:289` (Return terminates itself now).

- [ ] **Step 7: Verify green.**

Run: `cargo test -p strider-ir -p strider-analyze 2>&1 | tail -15`
Expected: PASS (no new failures vs baseline).

- [ ] **Step 8: Commit + push.**

```bash
git add -A && git commit -m "refactor(strider-ir): builder emits own termination; Return self-terminates, CallOther terminates via flag"
git push origin refactor/function-side-tables
```

---

## WS1 — `endianness` on `Function`; move register-aliasing into the builder

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (add `endianness` field + accessor)
- Modify: `crates/strider-ir/src/builder/mod.rs` (`new_raw` stores endianness; thread through `FunctionBuilder::new`)
- Create: `crates/strider-ir/src/builder/vn_io.rs` (the moved register-aliasing core)
- Modify: `crates/strider-lift/src/pcode_lift/vn_io.rs` (shrink `read_vn`/`write_vn` to space dispatch + delegate)
- Modify: `crates/strider-analyze/src/strider/vn_io.rs` (no signature change; still delegates)

**Design:** `read_reg_vn`/`write_reg_vn` and their helpers move verbatim onto `FunctionBuilder`, with `self.endianness` reading the new `Function` field instead of `ValueLifter.endianness`, and `self.builder.X` becoming `self.X`. The lifter's `read_vn`/`write_vn` keep the `match vn.addr_space` dispatch (CONST → `build_int_const`, default-code-space → `build_load`/`build_store` using `self.sleigh.space_info`) and call `self.builder.read_reg_vn(vn)` / `self.builder.write_reg_vn(vn, val)` for the `REGISTER | UNIQUE` arm. `vn_mask` (free fn) and `build_masked_insert` move with the core.

- [ ] **Step 1: Write a failing builder test for `read_reg_vn` aliasing.**

In `crates/strider-ir/src/builder/tests.rs`, add a test that builds a function tracking `RAX` (8 bytes) on a little-endian arch, writes a known value, then `builder.read_reg_vn(&al_vn)` (the 1-byte sub-register at the same offset) and asserts the result is a `Truncate` of the container read (sub-register slice). Use the existing `RegisterSet`/`make_*` helpers from `strider-ir-test-utils`. Mirror the existing `read_reg_vn` unit tests in `crates/strider-lift/src/pcode_lift/vn_io.rs` (lines ~470-786) for exact expected shapes.

- [ ] **Step 2: Run it to verify it fails (method doesn't exist on builder).**

Run: `cargo test -p strider-ir --lib read_reg 2>&1 | tail -20`
Expected: FAIL — `no method named read_reg_vn found for FunctionBuilder`.

- [ ] **Step 3: Add `endianness` to `Function`.**

In `crates/strider-ir/src/function.rs`: add `pub(crate) endianness: strider_target::Endianness,` to the struct, a `pub fn endianness(&self) -> strider_target::Endianness { self.endianness }` accessor near `default_cc()` (line 263), and set it in the constructor that builds a `Function`. It is NOT node/value-keyed, so `Function::compact` needs no change for it (confirm the compact impl around line 839 doesn't enumerate all fields).

- [ ] **Step 4: Thread endianness into the builder constructors.**

In `crates/strider-ir/src/builder/mod.rs`: `FunctionBuilder::new_raw` (and the public `new`) must receive the arch endianness and store it on the `Function`. `BuiltCallingConvention` does NOT carry endianness, so add an `endianness: strider_target::Endianness` parameter to `new_raw`/`new`. Update all call sites (lift's `FunctionBuilder::new`, `strider-ir-test-utils` `RegisterSet`/`make_empty_fn`/`make_fn_with_var`, and any in-crate tests) to pass it — default `Endianness::Little` in test helpers.

- [ ] **Step 5: Move the register-aliasing core into the builder.**

Create `crates/strider-ir/src/builder/vn_io.rs` with an `impl FunctionBuilder` block containing `read_reg_vn`, `write_reg_vn`, `find_largest_fitting_register`, `enter_sub_register`, `calculate_reg_shift_from_container`, the `SubRegOutcome`/`SubRegContext` types, the free `vn_mask`, and `build_masked_insert` — copied from `crates/strider-lift/src/pcode_lift/vn_io.rs` with `self.builder.` → `self.` and `self.endianness` → `self.function.endianness()`. Register the module in `crates/strider-ir/src/builder/mod.rs` (`mod vn_io;`). Move the corresponding unit tests too (the `vn_mask_*`, sub-register read/write tests) — see project memory "Preserve v1 test functionality": do not drop coverage.

- [ ] **Step 6: Shrink the lifter's `read_vn`/`write_vn` to delegate.**

In `crates/strider-lift/src/pcode_lift/vn_io.rs`: keep `read_vn`/`write_vn` with their `match vn.addr_space` dispatch, but replace the `REGISTER | UNIQUE` arms (`self.read_reg_vn(vn)` / `self.write_reg_vn(vn, val)`) with `self.builder.read_reg_vn(vn)` / `self.builder.write_reg_vn(vn, val)`. Delete the now-moved private methods + helpers + free fns from this file. `ValueLifter` no longer needs its `endianness` field if nothing else uses it — check `crates/strider-lift/src/pcode_lift/mod.rs:46` and remove the field + constructor param if dead (update `crates/strider-analyze/src/strider/vn_io.rs::value_lifter` accordingly).

- [ ] **Step 7: Verify the new builder test passes + full suite green.**

Run: `cargo test -p strider-ir --lib read_reg 2>&1 | tail -10` → PASS
Run: `cargo test --workspace 2>&1 | tail -15` → no new failures
Run: `cargo clippy --workspace --all-targets 2>&1 | tail -5` → clean

- [ ] **Step 8: Rebuild Python + pytest (lifter touched).**

Run: `cd crates/strider-py && uv run maturin develop 2>&1 | tail -3 && uv run pytest -q 2>&1 | tail -5`
Expected: 841 pass.

- [ ] **Step 9: Commit + push.**

```bash
git add -A && git commit -m "refactor(strider-ir): own register aliasing in the builder via Function endianness"
git push origin refactor/function-side-tables
```

---

## WS2 — `CallDescriptor` enum + vn-resolved `BuiltCallOtherAbi`

**Files:**
- Create: `crates/strider-target/src/call_other_abi/built.rs` (or alongside the existing `CallOtherAbi`) — `BuiltCallOtherAbi`
- Modify: `crates/strider-target/src/lib.rs` (re-export)
- Modify: `crates/strider-ir/src/function.rs` (replace `call_cc` side-table with `call_descriptor`)
- Modify: `crates/strider-analyze/src/strider/insn/mod.rs` (resolve abi → `BuiltCallOtherAbi`, store descriptor)
- Modify: `crates/strider-analyze/src/strider/insn/control.rs` (store `CallDescriptor::Call` for override CCs)

**Design.** `CallOtherAbi` (`crates/strider-target/src/call_other_abi/...`) has `implicit_reads: &'static [&'static str]`, `implicit_writes: &'static [&'static str]`, `clobbers_memory: bool`. The vn-resolved built form:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltCallOtherAbi {
    pub implicit_reads: Vec<rsleigh::Vn>,
    pub implicit_writes: Vec<rsleigh::Vn>,
    pub clobbers_memory: bool,
}
```

The per-node descriptor (lives in `strider-ir`, which can name both strider-target types):

```rust
#[derive(Clone, Debug)]
pub enum CallDescriptor {
    Call(strider_target::BuiltCallingConvention),
    CallOther(strider_target::BuiltCallOtherAbi),
}
```

- [ ] **Step 1: Add `BuiltCallOtherAbi` (failing build a resolver test).**

In `crates/strider-target`, add the `BuiltCallOtherAbi` struct + re-export from `lib.rs`. In `crates/strider-analyze/src/strider/insn/mod.rs`, write a unit test that resolves a known `CallOtherAbi` (e.g. the x86_64 syscall entry: reads `RAX,RDI,RSI,RDX,R10,R8,R9`, writes `RAX,RCX,R11`) into a `BuiltCallOtherAbi` via the lifter's name-resolution and asserts the vns match what `resolve_abi_regs` produces today.

- [ ] **Step 2: Run it to verify it fails.**

Run: `cargo test -p strider-analyze built_call_other 2>&1 | tail -20`
Expected: FAIL — type/fn not yet wired.

- [ ] **Step 3: Resolve abi → built abi in the lifter.**

In `crates/strider-analyze/src/strider/insn/mod.rs`, add `fn resolve_call_other_abi(&self, name: &str, abi: &CallOtherAbi) -> Result<BuiltCallOtherAbi>` that reuses `resolve_abi_regs` for `implicit_reads`/`implicit_writes` and copies `clobbers_memory`. Make the new test pass.

- [ ] **Step 4: Replace `call_cc` with `call_descriptor` (failing recording test).**

In `crates/strider-ir/src/function.rs`: rename the `call_cc: FxHashMap<NodeId, BuiltCallingConvention>` side-table to `call_descriptor: FxHashMap<NodeId, CallDescriptor>`. Update `set_call_cc`/`call_cc` accessors to `set_call_descriptor(node, CallDescriptor)` / `call_descriptor(node) -> Option<&CallDescriptor>`, plus a convenience `call_cc(node) -> Option<&BuiltCallingConvention>` that matches `CallDescriptor::Call`. Update `Function::compact`'s remap of this side-table. Update the `call_stack_arg_offsets_override` derivation (it reads the override CC's `stack_arg_offsets`) to go through the `Call` arm.

- [ ] **Step 5: Record descriptors at the call sites.**

- `control.rs::handle_call`/`handle_tail_call`: when `override_cc` is `Some`, record `CallDescriptor::Call(cc.clone())` on the Call node (unchanged sparseness — default Calls store nothing).
- `insn/mod.rs::handle_call_other_modeled`: after building, record `CallDescriptor::CallOther(built_abi)` on every modeled CallOther node.

- [ ] **Step 6: Update consumers of the old `call_cc` accessor.**

Grep `call_cc(` / `set_call_cc(` across `crates/strider-analyze` and `crates/strider-py` and migrate each to the new API (`call_descriptor` or the `Call`-arm convenience). The Python pattern surface that exposed per-call CC (if any) routes through the convenience accessor.

- [ ] **Step 7: Verify green + pytest.**

Run: `cargo test --workspace 2>&1 | tail -15` → no new failures
Run: `cargo clippy --workspace --all-targets 2>&1 | tail -5` → clean
Run: `cd crates/strider-py && uv run maturin develop 2>&1 | tail -3 && uv run pytest -q 2>&1 | tail -5` → 841 pass

- [ ] **Step 8: Commit + push.**

```bash
git add -A && git commit -m "feat(strider-ir): per-call CallDescriptor side-table with vn-resolved CallOther footprint"
git push origin refactor/function-side-tables
```

---

## WS3 — Call ret-val output group

**Files:**
- Modify: `crates/strider-ir/src/function.rs` (`call_clobbered_for` → split helpers)
- Modify: `crates/strider-ir/src/builder/call.rs` (`build_call_kind` takes `ret_val_kinds` + `clobber_kinds`)
- Modify: `crates/strider-ir/src/builder/call.rs` (`build_call_with_cc` passes the two groups)

**Design.** Today `call_clobbered_for(cc)` returns `ret_prefix ++ rest` (ret regs first, then other caller-saved). Split into two functions that preserve the exact same membership/order:
- `call_ret_vals_for(cc) -> Vec<Vn>` = the current `ret_prefix` (`(ret_val_regs ++ ret_val_regs_float).filter(tracked && is_clobbered)`).
- `call_clobbered_for(cc) -> Vec<Vn>` = the current `rest` only (`all_vns.filter(is_clobbered && !ret_vars.contains)`).

`build_call_kind` replaces `result_ty: Option<ValueType>` with `ret_val_kinds: &[ValueKind]` (the ret-val output group) and keeps `clobber_kinds: &[ValueKind]`. Output order becomes `[Control, Memory, ...ret_vals, ...clobbers]` — identical to today because ret regs were already prefixed into the single tail. Both groups get `value_vn` tags. CallOther's single `result_ty` becomes a one-element `ret_val_kinds`.

- [ ] **Step 1: Write a failing test asserting the ret-val/clobber split.**

In `crates/strider-ir/src/builder/tests.rs` (or the call tests), build a Call under a CC with `ret_val_regs = [RAX]` and assert: outputs are `[Control, Memory, <RAX ret-val>, <other clobbers...>]`, the ret-val output's `value_vn` is `RAX`, and `function.call_ret_vals_for(cc) == [RAX]` while `call_clobbered_for(cc)` excludes `RAX`.

- [ ] **Step 2: Run it to verify it fails.**

Run: `cargo test -p strider-ir --lib call_ret_val 2>&1 | tail -20`
Expected: FAIL — `call_ret_vals_for` doesn't exist; `call_clobbered_for` still includes RAX.

- [ ] **Step 3: Split the clobber derivation.**

In `crates/strider-ir/src/function.rs`, add `call_ret_vals_for` (= old `ret_prefix`) and change `call_clobbered_for` to return only the old `rest`. Update `call_clobbered_regs()` to keep returning the full set if any non-call consumer needs it, or split it likewise — grep callers first.

- [ ] **Step 4: Generalize `build_call_kind`'s result into a ret-val group.**

In `crates/strider-ir/src/builder/call.rs`, change `build_call_kind` to take `ret_val_kinds: &[ValueKind]` + `ret_val_vns: &[rsleigh::Vn]` alongside the existing `clobber_kinds`/`clobber_vns`, emit `[Control, Memory, ...ret_vals, ...clobbers]`, tag `value_vn` on both groups, and return `(node, ret_val_values: Vec<ValueId>, clobber_values: Vec<ValueId>)`.

- [ ] **Step 5: Update `build_call_with_cc` + `build_call_other` callers.**

`build_call_with_cc`: pass `ret_val_vns = call_ret_vals_for(cc)`, `clobber_vns = call_clobbered_for(cc)`, write both groups back to their variables. `build_call_other`: pass the single output as a one-element ret-val group (`ret_val_kinds = [result kind]` or empty), `clobber_*` as before.

- [ ] **Step 6: Verify green + pytest.**

Run: `cargo test --workspace 2>&1 | tail -15` → no new failures (output order is unchanged, so existing Call assertions stay green; only the new split test is added)
Run: `cd crates/strider-py && uv run maturin develop && uv run pytest -q 2>&1 | tail -5` → 841 pass

- [ ] **Step 7: Commit + push.**

```bash
git add -A && git commit -m "feat(strider-ir): emit Call return-value outputs as a distinct group before clobbers"
git push origin refactor/function-side-tables
```

---

## WS4 — SP as a Call input before args (plant only)

**Files:**
- Modify: `crates/strider-ir/src/node_signature.rs:312` (Call signature)
- Modify: `crates/strider-ir/src/builder/call.rs` (`build_call_kind`/`build_call_with_cc` wire SP input)
- Modify: `crates/strider-ir/src/validate/*` (if Call input arity is asserted)
- Modify: Call-input-indexing consumers + tests (the ~handful that assume target at `[2]`, args at `[3]`)

**Design.** Call inputs become `[Control, Memory, Target, SP, ...args]`. SP is `function.stack_vn()` read via `read_reg_vn`. CallOther is unchanged (no SP input). `build_call_kind` gains `sp_value: Option<ValueId>` inserted between `target` and `arg_values`. The CC-default SP read reuses the value already snapshotted for the post-call SP adjust (`snapshot_pre_call_sp`) so no duplicate read node is created.

- [ ] **Step 1: Write a failing test for the SP input slot.**

In the Call builder tests, assert a built Call's `node_inputs` are `[ctrl, mem, target, sp, ...args]` — specifically that input `[3]` is the SP value and the first arg is at `[4]`.

- [ ] **Step 2: Run it to verify it fails.**

Run: `cargo test -p strider-ir --lib call_sp_input 2>&1 | tail -20`
Expected: FAIL — SP not present; arg at `[3]`.

- [ ] **Step 3: Update the Call node signature.**

`crates/strider-ir/src/node_signature.rs:312`: change the Call arm to `inputs: [CTRL, MEM, TARGET, SP]; in_tail: ARG` (add an `SP` `ExpectedValueKind` matching the stack-pointer integer width — reuse `TARGET`/`ARG`'s AnyInt relaxation; see the existing `SP`-less prefix and the AnyInt note at lines 212/224). CallOther stays `inputs: [CTRL, MEM]; in_tail: ARG`.

- [ ] **Step 4: Wire SP into Call construction.**

`build_call_kind`: add `sp_value: Option<ValueId>` param, insert it into the input iterator after `target`. `build_call_with_cc`: read SP via `read_reg_vn(&self.function.stack_vn())` (reuse `snapshot_pre_call_sp`'s value if present; otherwise read it even on link-register ISAs where `ret_stack_pop == 0`, since the SP input is wanted regardless), pass `Some(sp)`. `build_call_other` passes `None`.

- [ ] **Step 5: Update validate + Call-input-indexing consumers.**

Run `grep -rnE 'node_inputs\(.*[Cc]all|Call.*\[2\]|\[3\]' crates/strider-analyze/src crates/strider-ir/src` and fix each site that assumed target/args offsets. `CallStackArgCollect` reads slot `[1]` (memory) and appends at the tail — unaffected except its `[control, memory, target, ...args]` doc comment, which becomes `[control, memory, target, sp, ...args]`. Update any validator that checks Call min input arity.

- [ ] **Step 6: Update affected Rust + Python tests.**

Fix the tests that assert Call input layout (the ~handful identified in Step 5). Update the ~10 pytest call files only where they index raw Call inputs (most use pattern builders and are unaffected).

- [ ] **Step 7: Verify green + pytest.**

Run: `cargo test --workspace 2>&1 | tail -15` → no new failures
Run: `cargo clippy --workspace --all-targets 2>&1 | tail -5` → clean
Run: `cd crates/strider-py && uv run maturin develop && uv run pytest -q 2>&1 | tail -5` → 841 pass

- [ ] **Step 8: Commit + push.**

```bash
git add -A && git commit -m "feat(strider-ir): wire stack pointer as a Call input ahead of arguments"
git push origin refactor/function-side-tables
```

---

## WS5 — Construct both kinds in the builder from descriptors

**Files:**
- Modify: `crates/strider-ir/src/builder/call.rs` (new `build_call(target, cc)` + `build_call_other(user_op, name, built_abi, explicit_args, output)` that fully resolve via the builder; delete the value-passthrough helpers no longer needed)
- Modify: `crates/strider-analyze/src/strider/insn/mod.rs` (lifter hands the builder a `BuiltCallOtherAbi` + explicit operand values + output vn; deletes `write_implicit_clobbers`/`resolve_abi_reg_values` ad-hoc resolution)
- Modify: `crates/strider-analyze/src/strider/insn/control.rs` (`handle_call` calls the new `build_call`)

**Design.** With register aliasing in the builder (WS1) and the descriptor types (WS2), both kinds resolve their *register* footprint inside `strider-ir`:
- `build_call(&mut self, target: ValueId, cc: &BuiltCallingConvention, is_default: bool)`: reads SP + arg regs via `read_reg_vn`, derives ret-vals (`call_ret_vals_for`) + clobbers (`call_clobbered_for`), emits via `build_call_kind`, writes ret-vals + clobbers back via `write_reg_vn`, applies SP adjust, records `CallDescriptor::Call(cc)` when `!is_default`.
- `build_call_other(&mut self, user_op_id, name, abi: &BuiltCallOtherAbi, explicit_args: &[ValueId], output: Option<rsleigh::Vn>)`: reads `abi.implicit_reads` via `read_reg_vn`, prepends `explicit_args`, ret-val group = `output` (one element, type from `output.size`) or empty, clobbers = `abi.implicit_writes`, emits, writes the result to `output` via `write_reg_vn` + clobbers back via `write_reg_vn`, advances memory iff `abi.clobbers_memory`, records `CallDescriptor::CallOther(abi)`. The lifter still resolves the *explicit* pcode operands (which may be CONST/RAM) to values and the `output` vn, and passes them in.

CallOther's explicit operands stay lifter-resolved because the builder has no Sleigh for the space dispatch; everything register-shaped resolves in the builder.

- [ ] **Step 1: Write failing tests for builder-owned construction.**

Add a `strider-ir` builder test that calls the new `build_call_other(... built_abi ..., explicit_args, output)` directly and asserts: implicit reads appear as inputs after the explicit args, the result output is written back to a sub-register `output` via a masked insert, and `CallDescriptor::CallOther` is recorded. Add a `build_call(target, cc, is_default=false)` test asserting the descriptor records and the ret-val/clobber writeback rebinds variables.

- [ ] **Step 2: Run to verify failure.**

Run: `cargo test -p strider-ir --lib build_call_other_builder 2>&1 | tail -20`
Expected: FAIL — new signatures absent.

- [ ] **Step 3: Implement `build_call` (descriptor-driven).**

Rewrite `build_call`/`build_call_with_cc` in `crates/strider-ir/src/builder/call.rs` to resolve register operands via `read_reg_vn`/`write_reg_vn` (replacing `read_variable`/`write_variable`; identical for full registers, so Call output stays correct) and record `CallDescriptor::Call`. Keep SP input (WS4) + ret-val group (WS3).

- [ ] **Step 4: Implement `build_call_other` (descriptor-driven).**

Replace the value-passthrough `build_call_other` with the `BuiltCallOtherAbi`-driven one above. The builder now does the implicit-read reads + implicit-write/result writebacks via `read_reg_vn`/`write_reg_vn`; the lifter passes the resolved explicit operands + `output` vn only.

- [ ] **Step 5: Slim the lifter.**

In `crates/strider-analyze/src/strider/insn/mod.rs::handle_call_other_modeled`: resolve only the explicit pcode operand values (`read_vn` on `insn` inputs) + the `output` vn, build `BuiltCallOtherAbi` (WS2 helper), call the new `build_call_other`. Delete `write_implicit_clobbers`, `resolve_abi_reg_values`, and the manual implicit-read/clobber-kind plumbing now living in the builder. `handle_call` calls `build_call(call_address, cc, override_cc.is_none())`.

- [ ] **Step 6: Verify green + pytest + clippy.**

Run: `cargo test --workspace 2>&1 | tail -15` → no new failures
Run: `cargo clippy --workspace --all-targets 2>&1 | tail -5` → clean
Run: `cd crates/strider-py && uv run maturin develop && uv run pytest -q 2>&1 | tail -5` → 841 pass

- [ ] **Step 7: Simplification pass.**

Re-read `crates/strider-ir/src/builder/call.rs` and `insn/mod.rs` with the code-simplifier lens: collapse any now-dead `CallValueInputs`/`CallAbiSelection`/`select_call_abi` leftovers, ensure one shared `build_call_kind` emit, no duplicate SP reads. Confirm no `clippy::too_many_arguments` regressions beyond the documented allow.

- [ ] **Step 8: Commit + push.**

```bash
git add -A && git commit -m "refactor: construct Call and CallOther in the builder from their resolved descriptors"
git push origin refactor/function-side-tables
```

---

## Self-Review (run before execution)

**Spec coverage:**
- Value-only builder boundary preserved → WS5 keeps `build_call_kind` the emit; lifter only supplies space-resolved explicit operands. ✓
- Endianness on `Function` unblocks builder aliasing → WS1. ✓
- `CallDescriptor` enum, natural shape per kind, resolved-before-builder → WS2. ✓
- Ret-vals as a distinct output group `[CTRL, MEM, ret_vals.., clobbers..]` → WS3. ✓
- SP as Call input before args → WS4 (plant-only; CallStackArgCollect rewiring deferred). ✓
- Return terminates itself → WS0. ✓

**Type consistency:** `CallDescriptor::{Call(BuiltCallingConvention), CallOther(BuiltCallOtherAbi)}` used identically in WS2/WS5. `build_call_kind` final signature — `kind`, `target: Option<ValueId>`, `sp_value: Option<ValueId>` (WS4), `arg_values`, `ret_val_vns`/`ret_val_kinds` (WS3), `clobber_vns`/`clobber_kinds`, `advance_memory: bool`, `terminate: bool` (WS0) — is assembled across WS0/WS3/WS4 and consumed unchanged in WS5. `call_ret_vals_for`/`call_clobbered_for` split named consistently across WS3/WS5. `mark_cur_region_terminated` is private (WS0) and never appears in a caller after WS0.

**Open follow-ups (explicitly out of scope, do NOT implement here):**
- Rewire `CallStackArgCollect`/`FunctionArgDetect` to anchor off the new Call SP input (the "(b)" option) — deferred per decision.
- Whether `call_other_names` folds into the `CallDescriptor::CallOther` variant — left as a separate cleanup.
