# strider-cfg / strider-lift Crate Split — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use
> superpowers:subagent-driven-development to implement this plan
> task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract a new `strider-cfg` crate (bytes → CFG) out of
`strider-lift` (which keeps CFG → IR), and reshape options so
`LiftOptions` embeds a now-public `CfgOptions`.

**Architecture:** `cfg/` is already IR-free and `lift/ → cfg/` is
one-way, so this makes an existing logical boundary physical. New DAG
edge: `strider-opt → strider-cfg` (opt drops `strider-lift`).

**Tech Stack:** Rust workspace, `cargo`, PyO3/maturin for strider-py.

**Sequencing note:** The workspace will not fully build until Task 5.
Each task's gate is `cargo build -p <crate>` for the crate it owns,
walking up the DAG. The full gate (`cargo test --workspace` + clippy +
pytest) runs in Task 6.

---

### Task 1: Create the `strider-cfg` crate (move `cfg/`, publicise `CfgOptions`)

**Files:**
- Create: `crates/strider-cfg/Cargo.toml`
- Create: `crates/strider-cfg/src/lib.rs`
- Move: `crates/strider-lift/src/cfg/**` →
  `crates/strider-cfg/src/**` (promote `cfg/mod.rs` content to
  `lib.rs`; `cfg/{types,query,dot,options}.rs` and `cfg/builder/**`
  become top-level modules of the new crate).

- [ ] **Step 1: Cargo.toml.** Mirror strider-lift's deps minus
  `strider-ir`: `strider-target`, `rsleigh`, `petgraph`, `dot`,
  `graphwalk`, `rustc-hash`, `anyhow` (workspace versions). Same
  `[package]` workspace inheritance and the `test` lint allows.

- [ ] **Step 2: Move files.** `git mv crates/strider-lift/src/cfg/mod.rs
  crates/strider-cfg/src/lib.rs`, and `git mv` the rest of `cfg/` into
  `crates/strider-cfg/src/`. Keep the `builder/` subdir.

- [ ] **Step 3: Fix internal paths.** Replace `crate::cfg::` →
  `crate::` throughout the moved files. Replace `crate::LiftOptions`
  references (in `options.rs`, `builder/mod.rs`, `region_builder.rs`,
  `query.rs` tests) per Step 4.

- [ ] **Step 4: Publicise `CfgOptions`; delete `from_lift_options`.**
  In the moved `options.rs`: make `CfgOptions` and its three fields
  `pub` (`fn_max_size`, `allow_code_before_start_addr`,
  `known_targets`). Delete `CfgOptions::from_lift_options` (it
  references `crate::LiftOptions`, which no longer exists here). Keep
  the `Some(0) => None` coercion as a `Default`/constructor nicety if a
  `new`/normalising path exists; otherwise the orchestrator already
  rejects zero at its boundary.

- [ ] **Step 5: `Builder::for_arch` takes `&CfgOptions`.** Change the
  signature from `&crate::LiftOptions` to `&CfgOptions`; drop the
  internal `CfgOptions::from_lift_options(options)` snapshot (use the
  passed `&CfgOptions` directly). Update the in-crate tests
  (`query.rs`, `builder/mod.rs`, `region_builder.rs`) that built a
  `LiftOptions` to build a `CfgOptions`.

- [ ] **Step 6: lib.rs surface.** Re-export the public types at crate
  root exactly as `cfg/mod.rs` did (so `strider_cfg::Cfg`,
  `strider_cfg::Builder`, `strider_cfg::ResolvedTargets`,
  `strider_cfg::{MachineInsnAddr,PcodeInsnAddr}`, `CfgOptions`,
  `is_addr_tail_call`, `Result`, error types). Keep the
  `graphwalk::GraphRef for Cfg` impl.

- [ ] **Step 7: Register crate.** It is auto-included by
  `members = ["crates/*"]`; no workspace edit needed.

- [ ] **Step 8: Build.** Run `cargo build -p strider-cfg` and
  `cargo test -p strider-cfg`.
  Expected: compiles; the moved cfg unit tests pass.

- [ ] **Step 9: Commit.** `git add -A && git commit` —
  `refactor(strider-cfg): extract CFG construction into its own crate`.

---

### Task 2: Reshape strider-lift onto strider-cfg

**Files:**
- Modify: `crates/strider-lift/Cargo.toml`
- Modify: `crates/strider-lift/src/lib.rs`
- Modify: `crates/strider-lift/src/lift_options.rs`
- Modify: `crates/strider-lift/src/lift/**`,
  `crates/strider-lift/src/pcode_lift/**`

- [ ] **Step 1: Cargo.toml.** Add `strider-cfg = { workspace = true }`.
  Remove `graphwalk` and `petgraph` **iff** unused by `lift`/`pcode_lift`
  (verify: `grep -rn "graphwalk\|petgraph" crates/strider-lift/src`).
  Keep `strider-ir`, `strider-target`, `rsleigh`, `dot`, `anyhow`,
  `rustc-hash`.

- [ ] **Step 2: lib.rs.** Delete `pub mod cfg;`. Keep `pub mod
  pcode_lift; pub mod lift; pub mod lift_options; pub use
  lift_options::LiftOptions;`.

- [ ] **Step 3: Reshape `LiftOptions`.** New shape:
  `pub struct LiftOptions { pub cfg: strider_cfg::CfgOptions, pub all_vns:
  Option<Vec<rsleigh::Vn>>, pub per_address_ccs: FxHashMap<u64,
  BuiltCallingConvention> }`. `#[derive(Default)]`. Update the module doc
  (it no longer needs the "crate-root to dodge cfg→lift" rationale; it
  embeds `CfgOptions` directly). Drop the `use crate::cfg::{...}` import;
  `known_targets`/`fn_max_size`/`allow_code_before_start_addr` now live
  on `cfg`.

- [ ] **Step 4: Fix `lift/` cfg paths.** Replace `crate::cfg::` →
  `strider_cfg::` throughout `lift/**`. At the `Builder::for_arch` call
  sites in `lift/mod.rs`, pass `&options.cfg` instead of `&options`.

- [ ] **Step 5: Build.** `cargo build -p strider-lift` and
  `cargo test -p strider-lift`. Expected: compiles; lift tests pass.

- [ ] **Step 6: Commit.** `refactor(strider-lift): depend on strider-cfg;
  LiftOptions embeds CfgOptions`.

---

### Task 3: strider-opt — swap dep to strider-cfg

**Files:**
- Modify: `crates/strider-opt/Cargo.toml`
- Modify: `crates/strider-opt/src/indirect_branch_resolve/{classify,mod,table}.rs`,
  `crates/strider-opt/src/pipeline.rs`

- [ ] **Step 1: Cargo.toml.** Replace `strider-lift = { workspace = true
  }` with `strider-cfg = { workspace = true }`; update the explanatory
  comment.

- [ ] **Step 2: Paths.** `strider_lift::cfg::ResolvedTargets` →
  `strider_cfg::ResolvedTargets`; fix the doc-comment path
  `strider_lift::cfg::builder::indirect_resolver` →
  `strider_cfg::indirect_resolver` (or wherever Task 1 placed it) and
  `strider_lift::LiftOptions::known_targets` →
  `strider_lift::LiftOptions::cfg::known_targets`.

- [ ] **Step 3: Build.** `cargo build -p strider-opt && cargo test -p
  strider-opt`. Expected: compiles; opt tests pass.

- [ ] **Step 4: Commit.** `refactor(strider-opt): consume ResolvedTargets
  from strider-cfg`.

---

### Task 4: strider-orchestrator — split cfg/lift imports

**Files:**
- Modify: `crates/strider-orchestrator/Cargo.toml`
- Modify: `crates/strider-orchestrator/src/orchestrator/mod.rs`,
  `crates/strider-orchestrator/src/strider/pipeline.rs`

- [ ] **Step 1: Cargo.toml.** Add `strider-cfg = { workspace = true }`
  (keep `strider-lift`).

- [ ] **Step 2: Paths (src only).** `strider_lift::cfg::{Cfg,
  MachineInsnAddr, is_addr_tail_call, …}` → `strider_cfg::…`. Keep
  `strider_lift::{lift::Lifter, LiftOptions, pcode_lift::vn_sort_key}`.
  Where a `LiftOptions` is constructed, nest the CFG knobs under `cfg:
  strider_cfg::CfgOptions { fn_max_size, allow_code_before_start_addr,
  known_targets }`. Where the rebuild loop mutates `known_targets` /
  `fn_max_size`, retarget to `lift_opts.cfg.known_targets` etc.

- [ ] **Step 3: Build.** `cargo build -p strider-orchestrator`
  (defers tests/benches/examples to Task 6). Expected: lib compiles.

- [ ] **Step 4: Commit.** `refactor(strider-orchestrator): source CFG
  types from strider-cfg`.

---

### Task 5: strider-py — split cfg/lift imports; build_cfg uses CfgOptions

**Files:**
- Modify: `crates/strider-py/Cargo.toml`
- Modify: `crates/strider-py/src/cfg.rs`, `function.rs`, `sleigh.rs`

- [ ] **Step 1: Cargo.toml.** Add `strider-cfg = { workspace = true }`.

- [ ] **Step 2: Paths.** `strider_lift::cfg::Cfg` → `strider_cfg::Cfg`
  (`PyCfg.inner`); `strider_lift::cfg::Builder` → `strider_cfg::Builder`.
  In `build_cfg`, replace the `strider_lift::LiftOptions { fn_max_size,
  allow_code_before_start_addr, ..default }` + `Builder::for_arch(...,
  &opts)` with a `strider_cfg::CfgOptions { fn_max_size,
  allow_code_before_start_addr, ..Default::default() }` passed directly.
  Fix `function.rs` / `sleigh.rs` cfg paths.

- [ ] **Step 3: Build.** `cargo build -p strider-py`. Expected: compiles.

- [ ] **Step 4: Commit.** `refactor(strider-py): source Cfg/Builder from
  strider-cfg`.

---

### Task 6: Sweep tests/benches/examples + full gate

**Files:**
- Modify: strider-orchestrator `tests/**` (~15 files),
  `benches/scaling.rs`, `examples/{memory_demo,orchestrator_demo}.rs`.
- Modify: strider-reader `tests/elf_smoke.rs` (doc comment).

- [ ] **Step 1: Sweep.** Across the listed test/bench/example files:
  `strider_lift::cfg::` → `strider_cfg::`; any constructed `LiftOptions`
  with CFG knobs → nest under `cfg: strider_cfg::CfgOptions { … }`; any
  direct `Builder::for_arch` callers build a `CfgOptions`. Update the
  reader doc comment to `strider_cfg::`.

- [ ] **Step 2: Rebuild the .so** (dev-workflow quirk): `cargo build -p
  strider-py && cp target/debug/libstrider_py.so
  crates/strider-py/strider/strider.abi3.so`.

- [ ] **Step 3: Full gate.**
  `cargo test --workspace` (0 failures),
  `cargo clippy --workspace --all-targets` (clean),
  `cd crates/strider-py && uv run pytest -q` (unchanged count, 832).

- [ ] **Step 4: Commit.** `refactor: complete strider-cfg/strider-lift
  split across downstream crates`.

- [ ] **Step 5: Update CLAUDE.md** crate inventory + dependency-flow
  section to add strider-cfg and the new edges. Commit.

---

## Self-review checklist
- Spec coverage: extraction (T1), options reshape (T1/T2), every
  downstream crate (T3–T6) — covered.
- No behavioural change: gate is "existing tests pass", no test
  weakening.
- Type consistency: `CfgOptions` fields named identically across crates;
  `LiftOptions.cfg` is the single nesting point.
