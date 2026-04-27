# Indirect Branch Resolution — Implementation Plan

> **Spec:** [2026-04-27-indirect-branch-resolution-design.md](../specs/2026-04-27-indirect-branch-resolution-design.md)
>
> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task.  Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the blanket `BranchIndirect → Return` misclassification with a lazy, per-occurrence mini-IR resolver.  Resolved-constant indirect branches become intra-fn `Branch` (target inside fn range) or `Call+Return` tail calls (target outside).  `bx lr` (BranchIndirect with target = link register) becomes `Return`.  Unresolvable `BranchIndirect` cases hard-error with `cfg::ErrorKind::UnresolvedIndirectBranch`.  `CallIndirect` (incl. `blx lr`) is **out of scope** — it keeps its existing `Call(unknown_value)` lift.  Future jump tables drop in via the reserved `RegionTerminator::Switch` variant + `ResolvedTargets::Multiple`.

**Architecture:** Phased so each commit leaves the workspace green (`cargo test --workspace` passes, `cargo clippy --workspace --all-targets` clean).  Phases 1–3 are pure refactor; phases 4–7 add the new behaviour.

**Tech Stack:** Rust, `petgraph::StableDiGraph`, `rsleigh` (path crate), existing `ir`/`opt`/`pattern` crates, `strider-error`, `thiserror`.

## Test discipline (applies to every phase)

> **Hard rule:** every new piece of logic in `cfg` and `pcode-lift` ships with a unit test in the same phase as the logic.  No "added in Phase N, tested in Phase M".  Coverage targets:
>
> - **`pcode-lift`:** at least one positive test per `ValueLifter::lift` opcode-family branch; one negative test per error path; one `Ok(false)` test per control-flow opcode the lifter rejects.
> - **`cfg`:** at least one positive test per `RegionTerminator` variant; at least one positive test per `ResolvedTargets` variant; one negative test per resolver error path; one test per `Options` knob added; one test per new public/private helper.
> - **`target`:** the `link_register_vn` invariant test must enumerate every calling-convention preset.
>
> A phase is "done" only when (a) the new code has unit tests, (b) `cargo test -p <crate>` passes for each touched crate, and (c) full-workspace `cargo test --workspace` matches the running baseline plus the newly-added tests.

---

## Pre-conditions (current state, 2026-04-27)

- `feature/ai` HEAD: `7e3327e` (spec clarified).
- Workspace baseline: **2561 passed / 0 failed / 18 ignored**, clippy clean.
- `Region::ends_with_tail_call: bool` is set by the cfg builder but **never read** in `crates/strider/`.
- `BranchIndirect` is collapsed with `Return` in [crates/strider/src/strider/insn/mod.rs:108](../../crates/strider/src/strider/insn/mod.rs#L108) and terminates regions in [crates/cfg/src/cfg/builder/region_builder.rs:317-326](../../crates/cfg/src/cfg/builder/region_builder.rs#L317-L326) without target follow-up.
- `BuiltCallingConvention` has no `link_register_vn` field.
- Per-opcode pcode→IR translation lives entirely in `crates/strider/src/strider/insn/`.

---

## Phase 1 — `BuiltCallingConvention::link_register_vn`

Smallest scope, no behaviour change, sets up a precondition for the resolver.

**Files:**
- Modify: `crates/target/src/calling_convention.rs`
- Modify: `crates/target/tests/arch_smoke.rs` (or wherever the convention preset assertions live)

- [ ] **Step 1.1: Add `link_register_reg_name: Option<&'static str>`** to `pub struct CallingConvention`.  Default for new presets is `None`; explicitly fill in:
  - `arm_aapcs` → `Some("lr")`
  - `aarch64_aapcs64` → `Some("x30")` (AArch64's `lr` aliases to `x30`)
  - `mips_o32` / `mips_n64` → `Some("ra")`
  - `powerpc_sysv32` / `powerpc64_elf_v1` / `powerpc64_elf_v2` → `Some("LR")`
  - `x86_64_systemv_abi` / `x86_cdecl` → `None`

- [ ] **Step 1.2: Add `link_register_vn: Option<rsleigh::Vn>`** to `pub struct BuiltCallingConvention`.

- [ ] **Step 1.3: Resolve in `build()`** using the existing `vn_for_name` helper.  When `link_register_reg_name` is `None`, store `None`; otherwise propagate any `vn_for_name` error.

- [ ] **Step 1.4: Unit tests** in `crates/target/tests/calling_convention.rs` (extend existing or add new):
  - **`link_register_vn_set_for_link_register_presets`** — enumerate `arm_aapcs`, `aarch64_aapcs64`, `mips_o32`, `mips_n64`, `powerpc_sysv32`, `powerpc64_elf_v1`, `powerpc64_elf_v2`; assert `link_register_vn = Some(...)`.
  - **`link_register_vn_none_for_stack_push_presets`** — enumerate `x86_64_systemv_abi`, `x86_cdecl`; assert `link_register_vn = None`.
  - **`link_register_vn_resolves_to_callee_saved_lr`** — for ARM AAPCS, assert the resolved varnode is identical to the one in `callee_saved_regs` named `lr`.

- [ ] **Step 1.5: Verify** — `cargo test -p target` clean; full workspace test count unchanged.

- [ ] **Step 1.6: Commit** — `target: expose link_register_vn on BuiltCallingConvention`.

---

## Phase 2 — Extract `pcode-lift` crate (refactor only, zero behaviour change)

This is the heaviest phase.  Move the strider value-op handlers into a new low-layer crate so cfg can reuse them.

**Files:**
- Add: `crates/pcode-lift/Cargo.toml`
- Add: `crates/pcode-lift/src/lib.rs`
- Add: `crates/pcode-lift/src/value/mod.rs`
- Add: `crates/pcode-lift/src/value/{arithmetic,integer,float,boolean,cast,mem_load,misc_value}.rs`
- Add: `crates/pcode-lift/src/vn_io.rs`
- Modify: `Cargo.toml` (workspace member list)
- Modify: `crates/strider/Cargo.toml` (add `pcode-lift` dep)
- Modify: `crates/strider/src/strider/insn/mod.rs` (delegate to `ValueLifter`)
- Modify: `crates/strider/src/strider/mod.rs` if needed
- Move (`git mv`):
  - `crates/strider/src/strider/insn/arithmetic.rs` → `crates/pcode-lift/src/value/arithmetic.rs`
  - `crates/strider/src/strider/insn/integer.rs` → `crates/pcode-lift/src/value/integer.rs`
  - `crates/strider/src/strider/insn/float.rs` → `crates/pcode-lift/src/value/float.rs`
  - `crates/strider/src/strider/insn/boolean.rs` → `crates/pcode-lift/src/value/boolean.rs`
  - The cast / integer-extend / piece / extract / insert / popcount / lzcount handlers (currently scattered) → `crates/pcode-lift/src/value/cast.rs`
  - `Load` handler → `crates/pcode-lift/src/value/mem_load.rs` (Store stays in strider)
  - `vn_io` reader/writer helpers → `crates/pcode-lift/src/vn_io.rs`

- [ ] **Step 2.1: Bootstrap the crate.**  Empty `Cargo.toml` + `lib.rs` defining only the `ValueLifter` skeleton.  Add to workspace members.  Verify `cargo build -p pcode-lift`.

- [ ] **Step 2.2: Move `vn_io` first.**  Smallest, most-foundational module.  Strider re-imports from `pcode-lift`.  Verify full workspace tests still pass.

- [ ] **Step 2.3: Move per-opcode handler families one at a time** in this order — `boolean`, `integer`, `arithmetic`, `cast`, `float`, `mem_load`, `misc_value`.  After each move:
  - Strider's `process_insn` arm for those opcodes delegates to `ValueLifter::lift`.
  - Run `cargo test --workspace`.  Test count must stay at 2561.

- [ ] **Step 2.4: Define the public `ValueLifter` struct + API:**

  ```rust
  pub struct ValueLifter<'a, 'b, R: rsleigh::MemReader> {
      pub builder: &'a mut FunctionBuilder<'b>,
      pub vn_to_value: &'a mut HashMap<rsleigh::Vn, NodeOutputId>,
      pub sleigh: &'a rsleigh::Sleigh<R>,
  }

  impl<'a, 'b, R: rsleigh::MemReader> ValueLifter<'a, 'b, R> {
      /// Lifts one value-producing pcode insn.  Returns `Ok(true)`
      /// when the insn was lifted, `Ok(false)` when the opcode is
      /// a control-flow / call / store op that the caller is
      /// responsible for handling.
      pub fn lift(&mut self, insn: &rsleigh::Insn) -> Result<bool>;

      pub fn read_vn(&mut self, vn: &rsleigh::Vn) -> Result<NodeOutputId>;
      pub fn write_vn(&mut self, vn: &rsleigh::Vn, value: NodeOutputId) -> Result<()>;
  }
  ```

- [ ] **Step 2.5: Strider's `process_insn` becomes:**

  ```rust
  pub(crate) fn process_insn(&mut self, ...) -> Result<()> {
      let mut lifter = ValueLifter {
          builder: &mut self.builder,
          vn_to_value: &mut self.vn_to_value,
          sleigh: self.cfg.sleigh,
      };
      if lifter.lift(insn)? { return Ok(()); }
      // control-flow / call / store opcodes handled here
      match insn.opcode { ... }
  }
  ```

  *(Field name `vn_to_value` may need to be lifted from a local to an `IrStrider` field if it isn't already — check during implementation.)*

- [ ] **Step 2.6: Add unit tests** at `crates/pcode-lift/tests/value_lifter.rs`.  At least one positive test per opcode family + the `Ok(false)` paths:
  - **arithmetic family:** `lift_int_add_of_consts`, `lift_int_sub_of_consts`, `lift_int_mul_of_consts` — build the IR, run `ConstantFold`, assert the producer is `IntConst(expected)`.
  - **integer family:** `lift_int_copy_from_const`, `lift_int_zext_extends_const`, `lift_int_sext_extends_const`.
  - **boolean family:** `lift_bool_and_of_consts`, `lift_bool_or_of_consts`, `lift_bool_neg_of_const`.
  - **float family:** `lift_float_const`, `lift_float_add_of_consts`.
  - **cast family:** `lift_truncate_extracts_low_bits`, `lift_piece_concatenates`, `lift_extract_returns_slice`.
  - **mem_load family:** `lift_load_emits_load_node` (no opt run; just structural).
  - **misc_value family:** `lift_popcount`, `lift_lzcount` (whichever live there).
  - **rejects control flow:** `lift_returns_false_for_branch_indirect`, `lift_returns_false_for_return`, `lift_returns_false_for_call`, `lift_returns_false_for_call_indirect`, `lift_returns_false_for_branch`, `lift_returns_false_for_cond_branch`, `lift_returns_false_for_store`, `lift_returns_false_for_call_other`.
  - **vn_io:** `read_vn_unknown_returns_initial_var` (asserts a not-yet-written varnode produces an `InitialVar` node), `write_vn_then_read_vn_round_trip`.
  - **error paths:** at least one negative test per error variant emerging from `lift` (size mismatch, unsupported opcode-with-bad-output, ...).
  - **regression:** `lift_round_trip_matches_strider` — lift the same pcode sequence via `ValueLifter` and via the strider-side path, compare resulting `Graph` structure node-for-node.

- [ ] **Step 2.7: Verify** — workspace test count unchanged from the pre-Phase-2 baseline (2561) plus the new pcode-lift unit tests, clippy clean.

- [ ] **Step 2.8: Commit** — `extract pcode-lift crate from strider's value-op handlers`.

---

## Phase 3 — `RegionTerminator` enum (refactor, zero behaviour change)

Replace `Region::ends_with_tail_call: bool` with a richer enum.  This unlocks the next phase but doesn't change observable behaviour yet.

**Files:**
- Modify: `crates/cfg/src/cfg/types.rs`
- Modify: `crates/cfg/src/cfg/builder/region_builder.rs`
- Modify: `crates/cfg/src/cfg/builder/split.rs`

- [ ] **Step 3.1: Define `RegionTerminator`** in `types.rs`:

  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum RegionTerminator {
      Fallthrough,
      Branch,
      CondBranch,
      Return,
      TailCall { target: u64 },
      /// Reserved for future jump-table resolver.
      Switch { targets: Vec<u64> },
  }
  ```

  Make `Region::ends_with_tail_call` go away; add `pub terminator: RegionTerminator`.

- [ ] **Step 3.2: Migrate `finish_current_region`** signature: `(ends_with_tail_call: bool)` → `(terminator: RegionTerminator)`.  Existing call sites translate as:
  - tail-call branch: `RegionTerminator::TailCall { target }` (target = `branch_target_addr.machine_addr.addr`)
  - direct branch (non-tail): `RegionTerminator::Branch`
  - `CondBranch`: `RegionTerminator::CondBranch`
  - `Return`: `RegionTerminator::Return`
  - `BranchIndirect` (current behaviour, will be replaced in Phase 4): `RegionTerminator::Return`
  - implicit fall-through (region absorbed by adjacent insn): `RegionTerminator::Fallthrough`

- [ ] **Step 3.3: Update `split_region`** in `split.rs`: first half always becomes `RegionTerminator::Fallthrough`, second half inherits the original region's terminator.

- [ ] **Step 3.4: Unit tests** at `crates/cfg/tests/region_terminator.rs` (new file).  At least one positive test per `RegionTerminator` variant currently produced by the cfg builder:
  - **`finish_with_branch_terminator`** — direct intra-fn `Branch` produces `RegionTerminator::Branch`.
  - **`finish_with_cond_branch_terminator`** — `CondBranch` produces `RegionTerminator::CondBranch`.
  - **`finish_with_return_terminator`** — `Return` opcode produces `RegionTerminator::Return`.
  - **`finish_with_tail_call_terminator`** — direct branch outside fn range produces `RegionTerminator::TailCall { target }` with the resolved target.
  - **`finish_with_fallthrough_terminator`** — region absorbed by adjacent insn produces `RegionTerminator::Fallthrough`.
  - **`split_first_half_becomes_fallthrough`** — splitting a region puts the first half on `Fallthrough` and the second half inherits the original terminator.
  - **`branch_indirect_currently_terminates_as_return`** — Phase 3 keeps the legacy mapping (`BranchIndirect → Return`).  This test will be **superseded** by Phase 5's negative test, but pinning it now catches accidental behaviour shifts during the refactor.
  - **`switch_variant_is_constructible_but_unused`** — sanity check that `RegionTerminator::Switch { targets: vec![] }` compiles and round-trips through `Region` without producing edges.  Locks the API shape for the future jump-table work.

- [ ] **Step 3.5: Verify** — `cargo test --workspace` clean, test count = previous baseline + Phase-3 new tests, clippy clean.

- [ ] **Step 3.6: Commit** — `cfg: replace ends_with_tail_call with RegionTerminator enum`.

---

## Phase 4 — cfg crate dependencies + lazy mini-graph resolver

Add the new dependencies and the resolver.  Still no `BranchIndirect` behaviour change yet (resolver invocation comes in Phase 5).

**Files:**
- Modify: `crates/cfg/Cargo.toml` (add `ir`, `opt`, `pcode-lift`, `target` deps)
- Add: `crates/cfg/src/cfg/builder/indirect_resolve.rs`
- Modify: `crates/cfg/src/cfg/builder/mod.rs` (declare submodule, expose helper)
- Add: `crates/cfg/src/error.rs::ErrorKind::UnresolvedIndirectBranch(PcodeInsnAddr)`

- [ ] **Step 4.1: Add cfg deps.**  Workspace already has `ir`, `opt`, `pcode-lift`, `target` — add path entries to `crates/cfg/Cargo.toml`.  Verify `cargo build -p cfg`.

- [ ] **Step 4.2: New module `indirect_resolve.rs`** with:

  ```rust
  pub enum ResolvedTargets {
      LinkRegister,
      Single(u64),
      /// Future jump-table variant; not constructed this round.
      Multiple(Vec<u64>),
  }

  pub(super) fn resolve_indirect_target<R: MemReader>(
      region_insns: &[RegionInstruction],
      target_vn: rsleigh::Vn,
      sleigh: &rsleigh::Sleigh<R>,
      cc_link_register_vn: Option<rsleigh::Vn>,
      rom: Option<&dyn ReadOnlyMemory>,
      insn_addr: PcodeInsnAddr,
  ) -> Result<ResolvedTargets, Error>;
  ```

  Internals (lazy build, called on demand only):
  - `Graph::new()`, fresh `FunctionBuilder` (light-weight, no calling-convention plumbing).
  - Iterate `region_insns`; for each insn ask `ValueLifter::lift`; stop at the first opcode `lift` returns `false` for (which is the `BranchIndirect`/`CallIndirect`/etc. itself).
  - Read `target_vn`'s current `NodeOutputId` from the `vn_to_value` map.
  - Emit `build_return(Some(target_value), &[])` so the value is reachable.
  - `OptimizerPipeline::new() + ConstantFold + KnownBits + RedundantPhis + (optional) LoadReadOnly`; run.
  - Inspect the producer node of `target_value` after fold:
    - `NodeKind::IntConst(k)` → `ResolvedTargets::Single(k as u64)`
    - `NodeKind::InitialVar(vn) if Some(*vn) == cc_link_register_vn` → `ResolvedTargets::LinkRegister`
    - anything else → `Err(UnresolvedIndirectBranch(insn_addr))`

- [ ] **Step 4.3: Add `UnresolvedIndirectBranch` error variant** with thiserror message including the offending `PcodeInsnAddr`.

- [ ] **Step 4.4: Verify** — workspace builds, tests still pass (resolver unused yet), clippy clean.

- [ ] **Step 4.5: Add resolver unit tests** at `crates/cfg/tests/indirect_resolve.rs`.  The full set, one per branch of `resolve_indirect_target`:
  - **`resolves_direct_const_to_single`** — `mov reg, K; <branch_indirect_placeholder>` → `Single(K)`.
  - **`resolves_arithmetic_chain_to_single`** — `mov reg, K1; add reg, K2` → `Single(K1+K2)` after `ConstantFold`.
  - **`resolves_sub_register_aliasing_to_single`** — `mov eax, K; <jmp *rax>` → `Single(K)` after `KnownBits` simplifies `Piece`/`Insert`.
  - **`resolves_link_register_to_link_register`** — `target_vn` is the calling-convention LR, no prior write → `LinkRegister`.
  - **`resolves_rodata_load_to_single`** — `mov rax, [const_addr]; <jmp *rax>` with a `ReadOnlyMemory` covering `const_addr` → `Single(K)`.  Skipped when no `ReadOnlyMemory` is provided.
  - **`unknown_memory_errors_unresolved`** — same shape as above without `ReadOnlyMemory` → `Err(UnresolvedIndirectBranch(addr))`.
  - **`runtime_input_errors_unresolved`** — `<jmp *arg_reg>` with no constant write to `arg_reg` → `Err(...)`.
  - **`empty_region_errors_unresolved`** — resolver invoked on a region with no value-producing insns prior to the BranchIndirect → `Err(...)`.
  - **`malformed_branch_indirect_errors`** — BranchIndirect with no inputs → `Err(MissingBranchTarget)` (unrelated to UnresolvedIndirectBranch but pin the error path).
  - **`error_carries_pcode_addr`** — `Err(UnresolvedIndirectBranch(addr))`'s `addr` matches the offending insn's `PcodeInsnAddr`.

- [ ] **Step 4.6: Commit** — `cfg: add lazy mini-graph indirect-branch resolver (unused yet)`.

---

## Phase 5 — Wire resolver into CFG dispatch

Now the behaviour change.  Only `BranchIndirect` is touched;
`CallIndirect` is unchanged.

**Files:**
- Modify: `crates/cfg/src/cfg/builder/region_builder.rs` (`process_new_insn` for `BranchIndirect`)
- Modify: `crates/cfg/src/cfg/builder/mod.rs::Builder` if a `cc_link_register_vn` field is needed — see Step 5.1
- Modify: `crates/cfg/src/cfg/options.rs` if we need to thread the LR varnode through `Options` instead

- [ ] **Step 5.1: Decide LR plumbing.**  The resolver needs `cc_link_register_vn`.  Options:
  - (a) Add a new `OptionsBuilder::set_link_register(Vn)` knob.  Caller (strider) computes from `BuiltCallingConvention::link_register_vn` and passes through.  **Default choice — minimal coupling.**
  - (b) Have cfg take the full `BuiltCallingConvention` as a builder arg.  Bigger API change.

  Going with (a) for this round.

- [ ] **Step 5.2: BranchIndirect arm:**

  ```rust
  rsleigh::Opcode::BranchIndirect => {
      let target_vn = *insn.inputs.first()
          .ok_or(ErrorKind::MissingBranchTarget(addr))?;
      let resolved = indirect_resolve::resolve_indirect_target(
          &self.insns,
          target_vn,
          &self.builder.sleigh,
          self.builder.options.link_register_vn,
          self.builder.options.read_only_memory.as_deref(),
          addr,
      )?;
      let terminator = match resolved {
          ResolvedTargets::LinkRegister => RegionTerminator::Return,
          ResolvedTargets::Single(target) => {
              let target_addr = MachineInsnAddr { addr: target }.into();
              if self.is_branch_tail_call(target_addr)? {
                  RegionTerminator::TailCall { target }
              } else {
                  // intra-fn — enqueue successor exploration
                  let region = self.finish_current_region(RegionTerminator::Branch)?;
                  self.builder.work_queue.push(
                      (Some((region, RegionEdgeKind::Branch)), target_addr),
                  );
                  return Ok(ProcessInsnRes::FinishedProcessing);
              }
          }
          ResolvedTargets::Multiple(_) => unreachable!(
              "resolver does not produce Multiple yet"
          ),
      };
      self.finish_current_region(terminator)?;
      Ok(ProcessInsnRes::FinishedProcessing)
  }
  ```

- [ ] **Step 5.3: ReadOnlyMemory plumbing.**  `Options` doesn't currently carry a `ReadOnlyMemory`.  Add an `Options::read_only_memory: Option<Arc<dyn ReadOnlyMemory>>` knob.  Strider already constructs one for its own `LoadReadOnly` pass — wire it into the cfg `Options` at strider's CFG-build call site.

- [ ] **Step 5.4: Unit tests** for the dispatch wiring at `crates/cfg/tests/indirect_dispatch.rs`:
  - **`branch_indirect_to_in_range_const_produces_branch_terminator`** — synthetic CFG fixture; resolver returns `Single(K)` with `K` inside fn range; assert `region.terminator == Branch` AND a `RegionEdgeKind::Branch` successor edge to a fresh region at K.
  - **`branch_indirect_to_out_of_range_const_produces_tail_call_terminator`** — `Single(K)` with `K >= start_addr + fn_max_size`; assert `region.terminator == TailCall { target: K }` AND no successor edge.
  - **`branch_indirect_to_link_register_produces_return_terminator`** — `LinkRegister`; assert `region.terminator == Return`.
  - **`unresolved_branch_indirect_errors`** — synthetic CFG with an unresolvable BranchIndirect; assert `Builder::build()` returns `Err(UnresolvedIndirectBranch)`.
  - **`call_indirect_unchanged_when_target_is_lr`** — `blx lr` (CallIndirect with target VN = LR) does NOT terminate the region with `Return`.  Asserts the resolver does not bleed into CallIndirect.
  - **`options_set_link_register_round_trips`** — `OptionsBuilder::set_link_register(vn).build()` exposes the same `Vn` to the resolver.
  - **`options_read_only_memory_round_trips`** — same for `Options::read_only_memory`.
  - **`branch_indirect_inside_split_region_resolves_correctly`** — a region that gets split by an incoming edge before reaching the BranchIndirect still resolves correctly (regression guard for the split-then-finish ordering).

- [ ] **Step 5.5: Verify** — build, run all tests.  If any pre-existing fixture trips `UnresolvedIndirectBranch`, investigate per Phase 7.

- [ ] **Step 5.6: Commit** — `cfg: resolve known-target BranchIndirect (constants + bx lr)`.

---

## Phase 6 — Strider IR-layer terminator dispatch

Honor the new `RegionTerminator::TailCall { target }` and clean up the `Return | BranchIndirect` arm.

**Files:**
- Modify: `crates/strider/src/strider/insn/mod.rs`
- Modify: `crates/strider/src/strider/insn/control.rs`
- Modify: `crates/strider/src/strider/pipeline.rs` (or wherever the per-region post-loop lives) — hook the terminator dispatch

- [ ] **Step 6.1: Split the `Return | BranchIndirect` arm:**

  ```rust
  Opcode::Return => self.handle_return(insn)?,
  // BranchIndirect: CFG terminator drives the IR; per-insn handler is a no-op.
  Opcode::BranchIndirect => {}
  ```

- [ ] **Step 6.2: New `handle_tail_call(target: u64)`:**

  ```rust
  pub(super) fn handle_tail_call(&mut self, target: u64) -> Result<()> {
      let pointer_size = ...; // arch's pointer width
      let target_const = self.builder.build_int_const(target, pointer_size);
      self.builder.build_call(target_const)?;
      let ret_regs = self.builder.ret_val_vars().to_vec();
      self.builder.build_return(None, &ret_regs)?;
      Ok(())
  }
  ```

- [ ] **Step 6.3: Per-region post-loop dispatch.**  After processing a region's instructions, consult `region.terminator`:
  - `Return` → already handled by the `Opcode::Return` arm in the inner loop.
  - `TailCall { target }` → call `handle_tail_call(target)`.
  - `Branch`, `CondBranch`, `Fallthrough` → already handled.
  - `Switch` → `unreachable!` for this round.

- [ ] **Step 6.4: Update the long-form comment** at [crates/strider/src/strider/insn/mod.rs:84-107](../../crates/strider/src/strider/insn/mod.rs#L84-L107) to reflect the new dispatch.

- [ ] **Step 6.5: Unit tests** for the new IR dispatch at `crates/strider/tests/tail_call.rs` (or extend `control.rs`):
  - **`tail_call_emits_call_then_return`** — synthetic CFG region with `terminator = TailCall { target: 0xdead }`; build IR; assert the IR's last two control-chain nodes are `Call` (with input = `IntConst(0xdead)`) followed by `Return`.
  - **`tail_call_target_pointer_size_matches_arch`** — repeat for an arch with non-8-byte pointers (32-bit ARM); assert `IntConst` width matches the arch's pointer size.
  - **`return_terminator_emits_only_return`** — control test; `terminator = Return` produces a single `Return` node, no `Call`.
  - **`branch_indirect_with_return_terminator_does_not_emit_call`** — pin the contract that a `BranchIndirect`-derived `Return` terminator stays a `Return` (regression guard for the misclassification we're fixing).

- [ ] **Step 6.6: Verify** — `cargo test --workspace`, count must include the existing 2561 + new tests added in Phases 1–6.  Clippy clean.

- [ ] **Step 6.7: Commit** — `strider: route TailCall terminator to handle_tail_call`.

---

## Phase 7 — Fixture + per-arch tests + regression cleanup

**Files:**
- Add: `fixtures/cases/indirect_branch.c`
- Modify: `fixtures/Makefile` (consider adding `-fno-jump-tables` to `COMMON_CFLAGS` — see Step 7.2)
- Possibly modify: `fixtures/arch/<arch>.mk` for arches whose toolchain rejects `-fno-jump-tables`
- Add: `crates/strider/tests/indirect_branch.rs`
- Modify: `docs/superpowers/plans/2026-04-25-analyzer-known-issues.md` (close BUG-5)

- [ ] **Step 7.1: Add the C fixture.**  Computed-goto, two labels, returning distinct values:

  ```c
  // fixtures/cases/indirect_branch.c
  int indirect_branch_resolved(int x) {
      void *targets[] = {&&L0, &&L1};
      goto *targets[(unsigned)x & 1];
  L0: return 0;
  L1: return 1;
  }
  ```

- [ ] **Step 7.2: Decide `-fno-jump-tables`.**  Build the fixture per-arch and inspect the lifted shape.  If any arch turns the computed-goto into a jump table (clang -O0 may), add `-fno-jump-tables` to `COMMON_CFLAGS`.  If a per-arch toolchain rejects the flag, guard it in the per-arch `.mk`.

- [ ] **Step 7.3: Add the per-arch test** at `crates/strider/tests/indirect_branch.rs`.  Model after `crates/strider/tests/control.rs`.  For each arch, assert the IR contains either:
  - a `Branch` edge to the resolved-target region (intra-fn), OR
  - a `Call(IntConst(K)) + Return` shape if the arch happened to land the targets outside the function range (unlikely with our test).

  Use the `__one_arch_test!` machinery for per-arch ignores when an arch's lifter doesn't produce `BranchIndirect` for computed-goto.

- [ ] **Step 7.4: Investigate any regression.**  If any pre-existing fixture/test now trips `UnresolvedIndirectBranch`, root-cause it:
  - If the resolver is missing a pattern that `ConstantFold + KnownBits` *should* catch — file a tracker entry, do not loosen the strict policy.
  - If the fixture genuinely contains an unresolvable indirect branch (rare) — surface as a real bug, add explicit ignore with tracker entry.

- [ ] **Step 7.5: Close BUG-5** in `docs/superpowers/plans/2026-04-25-analyzer-known-issues.md`, or downgrade it to "future: cross-region resolution + jump tables" pointing at this spec.

- [ ] **Step 7.6: Verify** — full workspace tests pass, clippy clean, `cargo run --example strider` (smoke-runs the example).

- [ ] **Step 7.7: Commit** — `strider: indirect_branch fixture + per-arch tests`.

---

## Acceptance criteria

- [ ] All 7 phases committed on `feature/ai`.
- [ ] `cargo test --workspace` ≥ 2561 passed / 0 failed / 18 ignored, plus the new pcode-lift / cfg-resolver / strider-integration tests.
- [ ] `cargo clippy --workspace --all-targets` clean.
- [ ] `Region::ends_with_tail_call: bool` is gone; replaced by `RegionTerminator`.
- [ ] `BuiltCallingConvention::link_register_vn: Option<Vn>` is set for every link-register preset.
- [ ] `ResolvedTargets::Multiple` exists in the API but is not constructed yet.
- [ ] `RegionTerminator::Switch` exists in the API but is not constructed yet.
- [ ] BUG-5 closed (or moved to future-work tracker entry).

## Open questions for the implementer (decide before starting)

**Q1 — pcode-lift `Cargo.toml` package name.**  The natural name `pcode-lift` is hyphenated.  Rust crate names in `Cargo.toml` `[package]` `name` accept hyphens but the consumed name in `use` statements becomes `pcode_lift`.  Confirm both `name = "pcode-lift"` (TOML) and `use pcode_lift::ValueLifter` (source) is what we want.

**Q2 — `ReadOnlyMemory` ownership in `Options`.**  Strider's existing `LoadReadOnly` takes `&dyn ReadOnlyMemory` per-pass-instance.  Threading through cfg `Options` likely needs `Arc<dyn ReadOnlyMemory>` for shared ownership.  Confirm `ReadOnlyMemory: Send + Sync` (likely true) and that `Arc<dyn ReadOnlyMemory>` is acceptable.

**Q3 — `vn_to_value` field on `IrStrider`.**  The map currently lives as a builder-internal HashMap.  Phase 2.5 may need it lifted to a struct field if it isn't already.  Verify during implementation.

**Q4 — `OptimizerPipeline` reusability.**  The mini-graph resolver builds a fresh pipeline per invocation.  If construction cost shows up in a profile, we cache one per `Builder`.  Defer optimization until measured.
