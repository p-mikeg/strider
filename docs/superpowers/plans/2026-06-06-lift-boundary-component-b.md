# Component B — generic `Strider` + OptCtx/options SSoT cleanups — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task.

**Goal:** Replace `RunConfig`/`RunOptions`/`run` with a generic `Strider<R>` handle (per-binary invariants + one `analyze` method); fold the cfg builder options into `LiftOptions`; make `OptCtx` hold an `OptOptions`; delete `PositionalArgLayout` (the CC is the SSoT).

**Architecture:** Four sub-tasks, mostly independent. B1 (delete PositionalArgLayout) and B2 (OptCtx holds OptOptions) are behavior-preserving SSoT cleanups in strider-opt/strider-target — do them first. B3 collapses cfg-build + IR-lift into one `strider_lift::lift::lift_function(... LiftOptions ...)` that subsumes the cfg `OptionsBuilder`/`Options`. B4 introduces `Strider<R>`, deletes `run`/`RunConfig`/`RunOptions`, and rewires the orchestrator loop + strider-py onto it.

**Tech Stack:** Rust workspace; `cargo build/test --workspace`, `clippy --workspace --all-targets`, `uv run pytest`.

**Gate (every task):** `cargo build --workspace` clean; at each task's end `cargo test --workspace` 0 NEW failures + `clippy --workspace --all-targets` clean; for tasks touching strider-py also `uv run maturin develop && uv run pytest`. Commit per task (trailer `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`); push at the end.

Reference spec: `docs/superpowers/specs/2026-06-06-orchestrator-lift-boundary-design.md`.

---

## Task B1: Delete `PositionalArgLayout` — derive arg layout from the CC

**Files:**
- `crates/strider-target/src/calling_convention/mod.rs` (delete `PositionalArgLayout` struct + `from_convention`; add a `BuiltCallingConvention::positional_arg_layout()` method exposing the same ordered slot list / iterator)
- `crates/strider-target/src/lib.rs` (drop the `PositionalArgLayout` re-export)
- `crates/strider-opt/src/pipeline.rs` (delete `OptCtx.arg_layout` field, its `None` init, and the two `octx.arg_layout = Some(PositionalArgLayout::from_convention(...))` population sites at ~301 and ~466)
- `crates/strider-opt/src/call_stack_args/mod.rs` (~375) and `crates/strider-opt/src/function_args/mod.rs` (~87): replace `ctx.arg_layout.as_ref().expect(...)` with `function.default_cc().positional_arg_layout()` (the pass already has the function)
- `crates/strider-py/src/opt.rs` (~286): update the comment / any from_convention reference

- [ ] **Step 1:** Read `PositionalArgLayout::from_convention` (strider-target). Add `BuiltCallingConvention::positional_arg_layout(&self) -> PositionalArgs` (or an iterator yielding the same `(index → slot)` order: register slots in `arg_passing_regs` order, then stack slots at `stack_arg_offsets`). Move the exact logic from `from_convention` into this method so behavior is identical. Keep whatever small return type the consumers need (an owned `Vec` of slots is fine — it's a handful per call).
- [ ] **Step 2:** Update the two consumers to call `function.default_cc().positional_arg_layout()` instead of `ctx.arg_layout...expect(...)`. Both passes already receive the function (via `rctx`/`opt_ctx`); read the CC off it.
- [ ] **Step 3:** Delete `OptCtx.arg_layout`, its init, and the two pipeline population sites. Delete the `PositionalArgLayout` struct + `from_convention` + the lib.rs re-export. Fix the strider-py comment.
- [ ] **Step 4:** `cargo build --workspace` clean; `cargo test --workspace` 0 new failures; clippy clean. The arg-detection tests (`function_args`, `call_stack_args`) must still pass — behavior is identical (same layout, derived on-demand).
- [ ] **Step 5:** Commit: `refactor: delete PositionalArgLayout; derive positional arg layout from the CC`.

---

## Task B2: `OptCtx` holds an `OptOptions`

**Files:**
- `crates/strider-opt/src/pipeline.rs` (`OptCtx`: replace the loose `alias_mode` + `call_clobbers_args` fields with `options: OptOptions`; define `OptOptions { alias_mode: AliasMode, call_clobbers_args: bool, compact: bool }`; update `empty()`/`with_rom()`)
- consumers: `call_stack_args/mod.rs` (~370 `opt_ctx.alias_mode` → `opt_ctx.options.alias_mode`), `function_args/mod.rs` (~79-80), `load_forward/mod.rs` (~70), and `test_support.rs` (~73,83 set `ctx.alias_mode` → `ctx.options.alias_mode`)
- `crates/strider-opt/src/function_args/tests.rs` (~1113,1183 `octx.call_clobbers_args = true` → `octx.options.call_clobbers_args = true`)
- orchestrator: `crates/strider-orchestrator/src/orchestrator/mod.rs` `opt_ctx_for_run` (seeds `alias_mode` → seeds `ctx.options`)

- [ ] **Step 1:** Define `pub struct OptOptions { pub alias_mode: AliasMode, pub call_clobbers_args: bool, pub compact: bool }` with a `Default` (alias_mode default, call_clobbers_args false, compact true). Replace `OptCtx`'s `alias_mode`/`call_clobbers_args` fields with `pub options: OptOptions`.
- [ ] **Step 2:** Update every consumer read (`ctx.alias_mode` → `ctx.options.alias_mode`, `ctx.call_clobbers_args` → `ctx.options.call_clobbers_args`) and every writer (tests/test_support/orchestrator). `compact` is NOT consumed by any pass — it rides for the orchestrator to read at finalize (B4 wires that).
- [ ] **Step 3:** `cargo build --workspace` clean; `cargo test --workspace` 0 new failures; clippy clean.
- [ ] **Step 4:** Commit: `refactor(strider-opt): OptCtx holds an OptOptions (SSoT for opt config)`.

---

## Task B3: `LiftOptions` absorbs the cfg builder options; single `lift_function`

**Files:**
- `crates/strider-lift/src/lift/mod.rs` (extend `LiftOptions` with `fn_max_size: Option<u64>`, `allow_code_before_start_addr: bool`, `known_targets: FxHashMap<PcodeInsnAddr, ResolvedTargets>`; add `pub fn lift_function<R>(sleigh, arch, entry, cc, options) -> Result<LiftOutcome>` that builds the CFG via `cfg::Builder` from `options` then lifts via a `Lifter`)
- `crates/strider-lift/src/cfg/options.rs` and `crates/strider-lift/src/cfg/builder/mod.rs` (the cfg `Builder` consumes the cfg-relevant fields from `LiftOptions` directly; remove the now-redundant `OptionsBuilder`/`Options` OR have `Builder::for_arch` accept the slice it needs from `LiftOptions`. Keep `Builder` usable standalone for `strider.build_cfg`.)
- consumers of `cfg::OptionsBuilder` migrate to building a `LiftOptions`.

- [ ] **Step 1:** Extend `LiftOptions` with the three cfg knobs. Decide the cfg `Builder` seam: simplest is `Builder::for_arch(arch, sleigh, entry, &LiftOptions)` reading `fn_max_size`/`allow_code_before_start_addr`/`known_targets` from it, and delete `OptionsBuilder`/`Options` (the user's "LiftOptions replaces cfg builder options"). If `strider.build_cfg` (strider-py) needs a standalone cfg build, it constructs a `LiftOptions` with empty `known_targets`/`all_vns`.
- [ ] **Step 2:** Add `lift_function(sleigh, arch, entry, cc, options)`: build the `Cfg` (Builder from `options`), then `Lifter::from_built_cc(arch, sleigh.regs()?, cc).analyze_cfg_with(&cfg, sleigh, LiftOptions{ all_vns, per_address_ccs, .. })`. (Reuse the existing `Lifter`.) Return `LiftOutcome`.
- [ ] **Step 3:** Migrate cfg-Builder/`OptionsBuilder` call sites (orchestrator `build_cfg`, strider-py `build_cfg`, benches, tests) to the new shape. The orchestrator's loop keeps building the CFG itself for now (B4 may switch it to `lift_function`), or adopt `lift_function` here if clean.
- [ ] **Step 4:** Full gate (incl. pytest, since strider-py `build_cfg` changes). Commit: `refactor(strider-lift): LiftOptions subsumes the cfg builder options; add lift_function`.

---

## Task B4: generic `Strider<R>`; delete `run`/`RunConfig`/`RunOptions`

**Files:**
- `crates/strider-orchestrator/src/orchestrator/mod.rs` (add `pub struct Strider<R> { arch, sleigh, rom }` + `Strider::new` + `Strider::analyze(entry, cc, lift_opts, opt_opts) -> Result<Function>`; move the `LoopState` fixed-point loop to be driven by `Strider::analyze`; delete `RunConfig`, `RunOptions`, `run`)
- `crates/strider-orchestrator/src/lib.rs` (drop `run`/`RunConfig`/`RunOptions` re-exports; add `Strider`)
- `crates/strider-py/src/run.rs` (rewire `run_via_orchestrator` onto `Strider::new(...).analyze(...)`); `crates/strider-py/src/strider_cls.rs` (`PyStrider` may wrap `Strider` instead of `LiftDriver`, or stay as the lift-only handle — keep the Python `Strider` class behavior)
- orchestrator integration tests using `run(RunConfig)` migrate to `Strider::new(...).analyze(...)`.

- [ ] **Step 1:** Define `Strider<R: MemReader> { arch: SleighArch, sleigh: Sleigh<R>, rom: Option<Box<dyn ReadOnlyMemory>> }`, `new`, and `analyze(&mut self, entry: u64, cc: &BuiltCallingConvention, lift_opts: &LiftOptions, opt_opts: &OptOptions) -> Result<Function>`. `analyze` body = today's `run` loop (the `LoopState` machinery): build/lift via `strider_lift::lift_function` (or the existing build_cfg+analyze), run the pipeline + `IndirectBranchClassify`, step/record/rebuild to fixed point, finalize (compact from `opt_opts.compact`). The pipeline is built internally (`LiftDriver::build_optimizer_pipeline` or a freestanding builder configured from `opt_opts`).
- [ ] **Step 2:** Delete `run`, `RunConfig`, `RunOptions` and their re-exports. Move any logic they held (per_address_ccs handling, etc.) into `LiftOptions`/`OptOptions`/`Strider`.
- [ ] **Step 3:** Migrate the orchestrator integration tests (`tests/orchestrator_indirect_resolution.rs`, `tests/indirect_branch.rs`, etc.) from `run(RunConfig::new(...))` to `Strider::new(...).analyze(...)`.
- [ ] **Step 4:** Rewire strider-py `run.rs` `run_via_orchestrator` onto `Strider`. Keep the Python `strider.run(...)` signature stable (it maps its args onto `LiftOptions`/`OptOptions`/`Strider::analyze`). `PyStrider` (the Python `Strider` class) keeps its `analyze_cfg` lift surface (it wraps `LiftDriver` for the single-lift path).
- [ ] **Step 5:** Full gate (cargo test --workspace + clippy --all-targets + maturin develop + pytest). Commit: `refactor(strider-orchestrator): generic Strider handle; delete run/RunConfig/RunOptions`.
- [ ] **Step 6:** Push `feature/orchestrator-lift-boundary`.

---

## Self-review notes
- **Spec coverage:** PositionalArgLayout deletion → B1; OptCtx holds OptOptions → B2; LiftOptions replaces cfg options + lift_function → B3; generic Strider + delete run/RunConfig/RunOptions → B4. All covered.
- **Ordering:** B1, B2 are behavior-preserving and independent (do first, lowest risk). B3 changes the lift entry shape. B4 depends on B2 (OptOptions) and B3 (lift_function/LiftOptions). Each leaves the workspace green.
- **Risk:** B4 is the cross-crate behavioral cutover (deletes the public `run`/`RunConfig`); the orchestrator integration tests + pytest are the safety net. B3's cfg-Builder seam change ripples to `strider.build_cfg` — pytest covers it.
- **Scope discipline:** Component C (Python `ElfStrider`/`load_elf`) is NOT in this plan.
