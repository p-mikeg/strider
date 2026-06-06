# Component A — Move CFG→IR lift into `strider-lift` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Relocate all CFG→IR lifting from `strider-orchestrator` into `strider-lift`, behind an explicit `strider_lift::lift` API, leaving `strider-orchestrator` with only the resolve/optimize concern.

**Architecture:** This is a **behavior-preserving move**, not a feature. The CFG→IR machinery (`PerRegionDriver`, its `insn/` + `vn_io` submodules, the `analyze_cfg` free functions, `AnalyzeOutcome`/`AnalyzeOptions`) is already `strider_opt`-free; it moves verbatim into a new `strider_lift::lift` module exposing a `Lifter` struct (arch + cc + cached `SleighRegs`). The orchestrator's `LiftDriver` is reduced to its opt half (`alias_mode`, `build_optimizer_pipeline`) wrapping a `strider_lift::lift::Lifter` for the lift it delegates. Because the move is mutually-referential, the relocation lands as one atomic move-and-rewire, then verified green; there is no partial intermediate that compiles.

**Tech Stack:** Rust workspace; `cargo build/test --workspace`, `cargo clippy --workspace --all-targets`, `uv run pytest` (PyO3 bindings). `git mv` for relocations to preserve history.

**Success gate (every task):** `cargo build --workspace` clean, and at the final task `cargo test --workspace` + `cargo clippy --workspace --all-targets` + `uv run pytest` all green with no NEW failures vs the branch baseline.

---

## File Structure (what moves where)

**New in `strider-lift`** (`crates/strider-lift/src/lift/`):
- `lift/mod.rs` — the `Lifter` struct (moved lift-half of `LiftDriver`: `arch`, `calling_convention`, `sleigh_regs`; `new`/`from_built_cc`/`calling_convention`/`find_all_unique_vns`/`analyze_cfg`/`analyze_cfg_with`), plus `LiftOptions` (was `AnalyzeOptions`) and `LiftOutcome` (was `AnalyzeOutcome`), and the `analyze_cfg_with` free-function stages (`init_region_map`, `translate_regions`, `link_region_edges`, `finalise_outcome`).
- `lift/region_driver.rs` — `PerRegionDriver` (moved from `strider-orchestrator/src/strider/mod.rs`).
- `lift/insn/{mod.rs,control.rs}` — moved from `strider-orchestrator/src/strider/insn/`.
- `lift/vn_io.rs` — moved from `strider-orchestrator/src/strider/vn_io.rs`.

**Reduced in `strider-orchestrator`** (`crates/strider-orchestrator/src/strider/`):
- `pipeline.rs` — `LiftDriver` keeps ONLY the opt concern (`alias_mode`, `with_alias_mode`, `alias_mode()`, `build_optimizer_pipeline`) and holds a `strider_lift::lift::Lifter` it forwards lift calls to. `AnalyzeOutcome`/`AnalyzeOptions` and the `analyze_cfg` stages are deleted (now re-exported from `strider-lift`).
- `mod.rs`, `insn/`, `vn_io.rs` — deleted (moved).

**Naming:** `AnalyzeOutcome` → `strider_lift::lift::LiftOutcome`; `AnalyzeOptions` → `strider_lift::lift::LiftOptions`. The orchestrator re-exports them (`pub use strider_lift::lift::{LiftOutcome, LiftOptions};`) so its own call sites and strider-py keep compiling with a one-line `use` change.

---

## Task 1: Stand up the empty `strider_lift::lift` module

**Files:**
- Modify: `crates/strider-lift/src/lib.rs` (add `pub mod lift;`)
- Create: `crates/strider-lift/src/lift/mod.rs` (empty placeholder doc only)

- [ ] **Step 1: Create the module file**

`crates/strider-lift/src/lift/mod.rs`:
```rust
//! Binary CFG → IR lifting.  Owns the region-by-region translation of a
//! `crate::cfg::Cfg` into a `strider_ir::Function`, given a resolved set
//! of indirect-branch targets.  No optimization — that is the
//! orchestrator's concern.
```

- [ ] **Step 2: Declare the module**

In `crates/strider-lift/src/lib.rs`, add `pub mod lift;` next to the existing `pub mod cfg;` / `pub mod pcode_lift;` declarations.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p strider-lift`
Expected: clean (empty module).

- [ ] **Step 4: Commit**

```bash
git add crates/strider-lift/src/lib.rs crates/strider-lift/src/lift/mod.rs
git commit -m "refactor(strider-lift): add empty lift module for CFG->IR move"
```

---

## Task 2: Move the region driver + its submodules into `strider-lift`

**Files:**
- `git mv crates/strider-orchestrator/src/strider/mod.rs crates/strider-lift/src/lift/region_driver.rs`
- `git mv crates/strider-orchestrator/src/strider/insn crates/strider-lift/src/lift/insn`
- `git mv crates/strider-orchestrator/src/strider/vn_io.rs crates/strider-lift/src/lift/vn_io.rs`
- Modify: `crates/strider-lift/src/lift/mod.rs` (declare the moved submodules)

> NOTE: This task does **not** compile on its own — `PerRegionDriver` still references `LiftDriver` and `AnalyzeOutcome`, which move in Task 3. Tasks 2 + 3 form one atomic move; commit only after Task 3's build is green. Do the `git mv`s here, then proceed directly to Task 3 before building.

- [ ] **Step 1: Relocate the files** (the four `git mv`s above).

- [ ] **Step 2: Declare the moved submodules** in `crates/strider-lift/src/lift/mod.rs`:
```rust
mod insn;
mod region_driver;
mod vn_io;

pub(crate) use region_driver::PerRegionDriver;
```

- [ ] **Step 3: Rewrite crate-internal paths in the moved files.**
In `region_driver.rs`, `insn/mod.rs`, `insn/control.rs`, `vn_io.rs`, replace references that pointed at sibling orchestrator modules:
  - `crate::strider::vn_io` → `crate::lift::vn_io` (and analogous `crate::strider::*` → `crate::lift::*`).
  - References to `super::pipeline::{PerRegionDriver-collaborators}` resolve within `lift` after Task 3.
  - `strider_lift::cfg::…` (these files are now *in* `strider-lift`) → `crate::cfg::…`; `strider_lift::pcode_lift::…` → `crate::pcode_lift::…`.
  - `strider_ir::…`, `strider_target::…`, `rsleigh::…` stay as-is.
  Do not build yet — proceed to Task 3.

---

## Task 3: Move the `Lifter` (lift-half of `LiftDriver`) + outcome/options types

**Files:**
- Modify: `crates/strider-lift/src/lift/mod.rs` (add the `Lifter` struct, `LiftOptions`, `LiftOutcome`, and the `analyze_cfg_with` stage functions — all moved from `strider-orchestrator/src/strider/pipeline.rs`)
- Modify: `crates/strider-orchestrator/src/strider/pipeline.rs` (delete the moved items; reduce `LiftDriver` to its opt half wrapping a `Lifter`)

- [ ] **Step 1: Define the moved types in `strider-lift`.**
Into `crates/strider-lift/src/lift/mod.rs`, move from `pipeline.rs`: the `AnalyzeOutcome` struct (renamed `LiftOutcome`), the `AnalyzeOptions` struct (renamed `LiftOptions`), the `init_region_map` / `translate_regions` / `link_region_edges` / `finalise_outcome` free functions, and a new `Lifter` struct carrying the lift fields. The `Lifter` is the lift-half of `LiftDriver` verbatim:

```rust
use rustc_hash::FxHashMap;
use anyhow::{anyhow, Result};

/// Result of [`Lifter::analyze_cfg`].
pub struct LiftOutcome {
    pub function: strider_ir::Function,
    pub unresolved_branches: Vec<(crate::cfg::PcodeInsnAddr, strider_ir::node::NodeId)>,
    pub region_handles: RegionLiftHandles, // (moved as-is from AnalyzeOutcome)
}

/// Per-call lift knobs.
#[derive(Default)]
pub struct LiftOptions<'a> {
    pub all_vns: Option<Vec<rsleigh::Vn>>,
    pub per_address_ccs: Option<&'a FxHashMap<u64, strider_target::BuiltCallingConvention>>,
}

/// Architecture-level CFG→IR lifter: arch + resolved CC + cached SleighRegs.
#[derive(Clone)]
pub struct Lifter {
    pub(crate) calling_convention: strider_target::BuiltCallingConvention,
    pub(crate) arch: strider_target::SleighArch,
    pub(crate) sleigh_regs: rsleigh::SleighRegs,
}

impl Lifter {
    pub fn new(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::CallingConvention,
    ) -> Result<Self> {
        let calling_convention = calling_convention.build(&sleigh_regs)?;
        Ok(Self { arch, calling_convention, sleigh_regs })
    }

    #[must_use]
    pub fn from_built_cc(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::BuiltCallingConvention,
    ) -> Self {
        Self { arch, calling_convention, sleigh_regs }
    }

    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        &self.calling_convention
    }

    pub(crate) fn find_all_unique_vns(&self, cfg: &crate::cfg::Cfg) -> Vec<rsleigh::Vn> {
        // body moved verbatim; strider_lift::pcode_lift::vn_sort_key → crate::pcode_lift::vn_sort_key
        let mut set: rustc_hash::FxHashSet<rsleigh::Vn> = rustc_hash::FxHashSet::default();
        for region in cfg.regions() {
            for wrapped in region.insns.iter() {
                for vn in wrapped.insn.all_vns() { set.insert(vn); }
            }
        }
        let mut vns: Vec<rsleigh::Vn> = set.into_iter().collect();
        vns.sort_unstable_by_key(crate::pcode_lift::vn_sort_key);
        vns
    }

    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self, cfg: &crate::cfg::Cfg, sleigh: &rsleigh::Sleigh<R>,
    ) -> Result<LiftOutcome> {
        self.analyze_cfg_with(cfg, sleigh, LiftOptions::default())
    }

    pub fn analyze_cfg_with<R: rsleigh::MemReader>(
        &self, cfg: &crate::cfg::Cfg, sleigh: &rsleigh::Sleigh<R>, opts: LiftOptions<'_>,
    ) -> Result<LiftOutcome> {
        // body moved verbatim from LiftDriver::analyze_cfg_with — the four
        // stage calls (init_region_map / translate_regions /
        // link_region_edges / finalise_outcome), with PerRegionDriver::new(self, …).
        let all_vns = opts.all_vns.unwrap_or_else(|| self.find_all_unique_vns(cfg));
        let mut driver = PerRegionDriver::new(self, cfg, sleigh, all_vns, opts.per_address_ccs)?;
        let (cfg_region_ids, region_map) = init_region_map(&mut driver, cfg)?;
        let ir_region_of = |rid: crate::cfg::RegionId| -> Result<strider_ir::RegionId> {
            region_map.get(rid.index()).copied().flatten()
                .ok_or_else(|| anyhow!("no region {rid:?} in cfg"))
        };
        translate_regions(&mut driver, cfg, &cfg_region_ids, &ir_region_of)?;
        link_region_edges(&mut driver, cfg, &ir_region_of)?;
        finalise_outcome(driver, cfg, &cfg_region_ids, &ir_region_of)
    }
}
```
Move `RegionLiftHandles`, `init_region_map`, `translate_regions`, `link_region_edges`, `finalise_outcome` verbatim into `lift/mod.rs`, rewriting their `self: &LiftDriver` parameters to `&Lifter` and `strider_lift::cfg`→`crate::cfg`. Update `PerRegionDriver::new`'s first param type from `&LiftDriver` to `&Lifter` in `region_driver.rs`.

- [ ] **Step 2: Reduce `LiftDriver` in `pipeline.rs`** to the opt concern wrapping a `Lifter`:
```rust
#[derive(Clone)]
pub struct LiftDriver {
    pub(crate) lifter: strider_lift::lift::Lifter,
    pub(crate) alias_mode: strider_opt::AliasMode,
}

impl LiftDriver {
    pub fn new(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::CallingConvention,
    ) -> Result<Self> {
        Ok(Self {
            lifter: strider_lift::lift::Lifter::new(arch, sleigh_regs, calling_convention)?,
            alias_mode: strider_opt::AliasMode::default(),
        })
    }

    #[must_use]
    pub fn from_built_cc(
        arch: strider_target::SleighArch,
        sleigh_regs: rsleigh::SleighRegs,
        calling_convention: strider_target::BuiltCallingConvention,
    ) -> Self {
        Self {
            lifter: strider_lift::lift::Lifter::from_built_cc(arch, sleigh_regs, calling_convention),
            alias_mode: strider_opt::AliasMode::default(),
        }
    }

    #[must_use]
    pub fn calling_convention(&self) -> &strider_target::BuiltCallingConvention {
        self.lifter.calling_convention()
    }
    #[must_use]
    pub const fn with_alias_mode(mut self, mode: strider_opt::AliasMode) -> Self {
        self.alias_mode = mode; self
    }
    #[must_use]
    pub const fn alias_mode(&self) -> strider_opt::AliasMode { self.alias_mode }

    #[must_use]
    pub fn build_optimizer_pipeline(&self) -> strider_opt::OptimizerPipeline {
        let mut p = strider_opt::default_pipeline();
        p.add(strider_opt::StackOffsetDetect::new());
        p.add(strider_opt::LoadForward::new());
        p.add_post_pass(strider_opt::CallStackArgCollect::new());
        p.add_post_pass(strider_opt::FunctionArgDetect::new());
        p
    }

    pub fn analyze_cfg<R: rsleigh::MemReader>(
        &self, cfg: &strider_lift::cfg::Cfg, sleigh: &rsleigh::Sleigh<R>,
    ) -> Result<strider_lift::lift::LiftOutcome> {
        self.lifter.analyze_cfg(cfg, sleigh)
    }
    pub fn analyze_cfg_with<R: rsleigh::MemReader>(
        &self, cfg: &strider_lift::cfg::Cfg, sleigh: &rsleigh::Sleigh<R>,
        opts: strider_lift::lift::LiftOptions<'_>,
    ) -> Result<strider_lift::lift::LiftOutcome> {
        self.lifter.analyze_cfg_with(cfg, sleigh, opts)
    }
}
```
Delete from `pipeline.rs`: the old `AnalyzeOutcome`, `AnalyzeOptions`, `RegionLiftHandles`, `find_all_unique_vns`, and the four stage functions (now in `strider-lift`). Add `pub use strider_lift::lift::{LiftOutcome, LiftOptions};` to `pipeline.rs` (or `strider/mod.rs` if that's where the orchestrator re-exports lift types) so existing `crate::AnalyzeOutcome` references switch to `crate::LiftOutcome` with a rename.

- [ ] **Step 3: Update orchestrator re-exports + call sites.**
In `crates/strider-orchestrator/src/strider/mod.rs` (now just the `pipeline` re-export module after `region_driver`/`insn`/`vn_io` moved out) and `lib.rs`, change `AnalyzeOutcome`/`AnalyzeOptions` to `LiftOutcome`/`LiftOptions`. In `orchestrator/mod.rs`, the `build_lift` destructure `AnalyzeOutcome { function, unresolved_branches, region_handles: _ }` → `LiftOutcome { … }`, and `AnalyzeOptions { … }` → `LiftOptions { … }`.

- [ ] **Step 4: Build until clean.**

Run: `cargo build --workspace`
Expected: clean. Fix residual path errors (the compiler enumerates each `crate::strider::*` / `strider_lift::cfg::*` that needs rewriting).

- [ ] **Step 5: Run lift + orchestrator tests.**

Run: `cargo test -p strider-lift -p strider-orchestrator`
Expected: 0 failures (behavior preserved).

- [ ] **Step 6: Commit (Tasks 2+3 together).**

```bash
git add -A -- crates/
git commit -m "refactor: move CFG->IR lift (PerRegionDriver + analyze_cfg) into strider-lift

The region driver, its insn/vn_io submodules, the analyze_cfg stages, and
the lift-half of LiftDriver (now strider_lift::lift::Lifter) move into a
new strider_lift::lift module; AnalyzeOutcome/Options become
LiftOutcome/LiftOptions.  LiftDriver keeps only the opt concern
(alias_mode, build_optimizer_pipeline) and forwards lift calls to its
Lifter.  Behavior-preserving move; same tests green."
```

---

## Task 4: Update strider-py to the moved types

**Files:**
- Modify: any `crates/strider-py/src/*.rs` that named `AnalyzeOutcome`/`AnalyzeOptions` or reached the lift via `LiftDriver`'s lift methods.

- [ ] **Step 1: Find the references**

Run: `grep -rn "AnalyzeOutcome\|AnalyzeOptions\|analyze_cfg" crates/strider-py/src/`
Expected: a small set (the `PyStrider::analyze_cfg` wrapper and outcome handling).

- [ ] **Step 2: Rename at those sites** to `strider_orchestrator::LiftOutcome` / `LiftOptions` (re-exported), or `strider_lift::lift::{LiftOutcome, LiftOptions}` directly. The `LiftDriver::analyze_cfg` forwarder keeps the same call shape, so `PyStrider` needs only the type-name rename.

- [ ] **Step 3: Build the wheel**

Run: `cd crates/strider-py && uv run maturin develop`
Expected: builds + installs.

- [ ] **Step 4: Commit**

```bash
git add -A -- crates/strider-py
git commit -m "refactor(strider-py): use strider-lift LiftOutcome/LiftOptions"
```

---

## Task 5: Move the lift tests with the lift code

**Files:**
- Any tests that exercised `PerRegionDriver` / `analyze_cfg` internals as unit tests inside the moved files travel with them (already moved in Task 2). Integration tests in `crates/strider-orchestrator/tests/` that test *lifting in isolation* (not the resolve loop) — assess each: if it only drives `LiftDriver::analyze_cfg` on a synthetic CFG, it can stay (the orchestrator re-exports the forwarder) OR move to `crates/strider-lift/tests/`.

- [ ] **Step 1: Inventory**

Run: `grep -rln "analyze_cfg\|PerRegionDriver" crates/strider-orchestrator/tests/`
Expected: a list of lift-touching test files.

- [ ] **Step 2: Decide per file.** Leave a test in `strider-orchestrator/tests/` if it exercises the resolve loop or the orchestrator surface; move it to `strider-lift/tests/` only if it is purely a CFG→IR lift test with no `strider_opt`/orchestrator dependency. When in doubt, leave it (the re-exported forwarder keeps it compiling) and note the decision in the commit message.

- [ ] **Step 3: Run the full Rust test suite**

Run: `cargo test --workspace`
Expected: 0 NEW failures vs the branch baseline.

- [ ] **Step 4: Commit** (only if any test moved)

```bash
git add -A -- crates/
git commit -m "test: relocate pure CFG->IR lift tests to strider-lift"
```

---

## Task 6: Final gate

- [ ] **Step 1:** `cargo test --workspace` → 0 NEW failures.
- [ ] **Step 2:** `cargo clippy --workspace --all-targets` → 0 warnings.
- [ ] **Step 3:** `cd crates/strider-py && uv run maturin develop && uv run pytest` → all pass.
- [ ] **Step 4:** Confirm the boundary: `grep -rn "use strider_opt\|strider_opt::" crates/strider-lift/src/lift/` is **empty** (the moved lift is opt-free, proving the boundary holds).
- [ ] **Step 5: Push** `git push origin feature/orchestrator-lift-boundary`.

---

## Self-Review notes

- **Spec coverage (Component A section):** "move `PerRegionDriver`/`insn`/`vn_io`" → Task 2; "`analyze_cfg`→`lift_function`/`Lifter`, `AnalyzeOutcome`→`LiftOutcome`, `AnalyzeOptions`→`LiftOptions`" → Task 3; "lift fields of `LiftDriver` move, opt half stays" → Task 3; "`VnCache` stays in orchestrator, feeds `all_vns`" → preserved (orchestrator still owns `VnCache`; `LiftOptions::all_vns` unchanged); "strider-lift stays opt-free" → Task 6 Step 4 verifies. The design's free-function `lift_function(sleigh, arch, entry, cc, options, known_targets)` is realized here as `Lifter::analyze_cfg_with` (a method carrying arch+cc+regs) operating on a `Cfg` the orchestrator already builds via `cfg::Builder` — Component B will thread `known_targets` and entry through the `Strider` loop, so A keeps the existing `Cfg`-in shape rather than collapsing CFG-build into the same call (deferred to B, noted to avoid scope creep here).
- **Placeholder scan:** none — every step names exact files/commands; moved-code steps specify the `git mv` + the concrete path rewrites the compiler will confirm.
- **Type consistency:** `Lifter` / `LiftOutcome` / `LiftOptions` used consistently across Tasks 3–5; `LiftDriver` retains `calling_convention()` / `alias_mode()` / `build_optimizer_pipeline` / `analyze_cfg(_with)` so orchestrator + strider-py call sites need only type renames, not call rewrites.
