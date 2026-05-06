# Custom-Pipeline `per_address_ccs` Bug Fix + `analyze_cfg` API Simplification — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix `strider.run(pipeline=<custom>, per_address_ccs=...)` silently dropping the override, and collapse the three-method `Strider::analyze_cfg*` matrix into one + `AnalyzeOptions`.

**Architecture:** The override is consumed at lift time inside `IrStrider`, so a post-lift custom pipeline cannot recover it. Plumb `per_address_ccs` through the strider-py custom-pipeline path, and replace `analyze_cfg`/`analyze_cfg_with_vns`/`analyze_cfg_with_vns_and_overrides` with `analyze_cfg(cfg)` + `analyze_cfg_with(cfg, AnalyzeOptions)`. Drop `IrStrider::set_per_address_ccs` and the `Option<&'a HashMap<…>>` field; the field always borrows a (possibly empty) map.

**Tech Stack:** Rust 2024 edition (`std::sync::LazyLock` available), PyO3, pytest via `uv run pytest`.

**Spec:** `docs/superpowers/specs/2026-05-06-custom-pipeline-per-address-cc-design.md`.

**Required tooling:**
- `cargo test --workspace` — Rust tests.
- `cd crates/strider-py && uv run maturin develop` — rebuild Python extension after Rust changes.
- `cd crates/strider-py && uv run pytest tests/python/<file>.py -v` — Python tests.
- `cargo clippy --workspace --all-targets` — lint.

---

## File Structure

**Files modified:**
- `crates/strider/src/strider/pipeline.rs` — add `AnalyzeOptions`, `analyze_cfg_with`; delete `analyze_cfg_with_vns` + `analyze_cfg_with_vns_and_overrides`. Module-level `LazyLock` for the empty-map default.
- `crates/strider/src/strider/mod.rs` — `IrStrider::per_address_ccs` becomes a non-`Option` borrow; `IrStrider::new` takes the borrow as a parameter; `set_per_address_ccs` deleted.
- `crates/strider/src/strider/insn/control.rs` — two call sites simplify from `.and_then(|m| m.get(&k))` to `.get(&k)`.
- `crates/strider/src/orchestrator.rs` — single call site at line 827 migrates to `analyze_cfg_with`.
- `crates/strider-py/src/run.rs` — dispatcher forwards `per_address_ccs` to both branches; `run_with_custom_pipeline` builds the `BuiltCallingConvention` map and calls `analyze_cfg_with`.

**Files created:**
- `crates/strider/tests/analyze_cfg_with_overrides.rs` — Rust integration test pinning the new API's per-call override behaviour.

**Files extended:**
- `crates/strider-py/tests/python/test_per_address_cc.py` — adds the 2x2 matrix test that fails on master in the bottom-right cell.

---

## Task 1: Rust failing test for `analyze_cfg_with` (TDD red)

**Files:**
- Create: `crates/strider/tests/analyze_cfg_with_overrides.rs`

- [ ] **Step 1: Write the failing test**

```rust
//! Per-call test: `Strider::analyze_cfg_with` applies the
//! per-address-cc override at lift time without going through
//! `strider::run`.  Mirrors `tests/per_address_cc.rs` but exercises the
//! new options-bag API directly so a strider-py custom pipeline
//! (which calls `analyze_cfg_with` instead of running the orchestrator)
//! gets the same override behaviour.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use ir::node::NodeKind;
use rsleigh::mem_readers::BufMemReader;
use strider::{AnalyzeOptions, CallingConvention, SleighArch, Strider};
use target::CallingConvention as TargetCC;

/// Same fixture as `tests/per_address_cc.rs::x86_64_call_then_ret`:
/// `call 0x2000; ret` at 0x1000.
fn x86_64_call_then_ret() -> (Vec<u8>, u64, u64) {
    let bytes = vec![0xe8, 0xfb, 0x0f, 0x00, 0x00, 0xc3];
    (bytes, 0x1000, 0x2000)
}

fn make_strider() -> Strider {
    let arch = SleighArch::x86_64();
    let regs = arch.probe_regs().unwrap();
    Strider::new(arch, regs, CallingConvention::x86_64_systemv_abi()).unwrap()
}

#[test]
fn analyze_cfg_with_applies_per_address_override() {
    let (bytes, entry, call_target) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap();

    // Build the override map against the same Sleigh register table the
    // function-default CC was built against.
    let regs = arch.probe_regs().unwrap();
    let mut built: HashMap<u64, target::BuiltCallingConvention> = HashMap::new();
    built.insert(call_target, TargetCC::x86_64_all_preserving().build(&regs).unwrap());

    let outcome = strider
        .analyze_cfg_with(
            &cfg,
            AnalyzeOptions {
                per_address_ccs: &built,
                ..AnalyzeOptions::default()
            },
        )
        .unwrap();
    let bfg = outcome.graph;

    let call_id = bfg
        .all_node_ids()
        .find(|n| matches!(bfg.node_kind(*n), NodeKind::Call))
        .expect("function lifts to one Call");
    let override_list = bfg
        .call_clobbered_override(call_id)
        .expect("override CC must populate the side-table");
    let outs = bfg.node_outputs(call_id);
    assert_eq!(
        outs.len(),
        2 + override_list.len(),
        "Call's outputs = Control + Memory + override_list.len()"
    );
}

#[test]
fn analyze_cfg_with_default_options_matches_analyze_cfg() {
    let (bytes, entry, _) = x86_64_call_then_ret();
    let strider = make_strider();
    let arch = SleighArch::x86_64();
    let reader = BufMemReader::new(bytes, entry);
    let sleigh = rsleigh::Sleigh::new(arch.sla_spec, arch.pspec, reader).unwrap();
    let cfg = cfg::Builder::new(sleigh, entry, cfg::OptionsBuilder::new().build())
        .build()
        .unwrap();

    let outcome_default = strider.analyze_cfg(&cfg).unwrap();
    let outcome_with = strider
        .analyze_cfg_with(&cfg, AnalyzeOptions::default())
        .unwrap();

    let n_default = outcome_default.graph.all_node_ids().count();
    let n_with = outcome_with.graph.all_node_ids().count();
    assert_eq!(
        n_default, n_with,
        "analyze_cfg_with(default) must produce the same graph shape as analyze_cfg"
    );
}
```

- [ ] **Step 2: Verify test does not compile yet (red)**

Run: `cargo test --package strider --test analyze_cfg_with_overrides 2>&1 | head -30`
Expected: compile error — `unresolved import strider::AnalyzeOptions` and/or `no method named analyze_cfg_with`.

- [ ] **Step 3: Do NOT commit yet** — failing tests don't go in commits. Move to Task 2.

---

## Task 2: Python failing test extension (TDD red)

**Files:**
- Modify: `crates/strider-py/tests/python/test_per_address_cc.py`

- [ ] **Step 1: Append the new test case**

Append to `crates/strider-py/tests/python/test_per_address_cc.py`:

```python
import pytest

import strider
from strider import CallingConvention, MemoryMap, OptimizerPipeline, SleighArch, opt
from strider.pattern import Capture, call, function_arg


def _x86_64_arg_thru_hook_to_sink_bytes():
    """Layout at 0x1000:
        0x1000  e8 fb 0f 00 00     call 0x2000   ; "hook" (clobbers rdi by default)
        0x1005  e8 f6 1f 00 00     call 0x3000   ; "sink" — we match its arg0
        0x100a  c3                 ret
    """
    return bytes(
        [
            0xE8, 0xFB, 0x0F, 0x00, 0x00,  # call 0x2000
            0xE8, 0xF6, 0x1F, 0x00, 0x00,  # call 0x3000
            0xC3,                           # ret
        ]
    )


def _build_default_equivalent_pipeline(sleigh, sl, cc, mem):
    """Mirrors `Strider::build_optimizer_pipeline` from the Rust side
    (the passes `strider.run(pipeline=None)` runs internally).  Used to
    pin the bug: this pipeline must produce the same matches as the
    None default once the per_address_ccs plumbing is fixed."""
    pipe = OptimizerPipeline.empty()
    pipe.add(opt.ConstantFold())
    pipe.add(opt.KnownBits())
    pipe.add(opt.RedundantPhis())
    pipe.add(opt.DeadBranchElim())
    pipe.add(opt.LoadReadOnly(mem))
    pipe.add(opt.StackStoreDetect(sl, cc))
    pipe.add(opt.StackLoadForward(sl, cc, sleigh))
    pipe.add_post(opt.FunctionArgDetect(sl, cc))
    pipe.add_post(opt.CallStackArgCollect(sl, cc))
    return pipe


@pytest.mark.parametrize(
    "use_custom_pipeline,with_override,expected_hits",
    [
        (False, False, 0),  # default pipeline, no override → hook clobbers rdi
        (False, True, 1),   # default pipeline, override     → rdi flows through
        (True, False, 0),   # custom pipeline,  no override  → same as default
        (True, True, 1),    # custom pipeline,  override     → BUG: today this is 0
    ],
)
def test_per_address_ccs_honoured_in_both_pipeline_paths(
    use_custom_pipeline, with_override, expected_hits
):
    arch = SleighArch.x86_64()
    cc = CallingConvention.x86_64_systemv_abi()
    mem = MemoryMap()
    mem.add_region(0x1000, _x86_64_arg_thru_hook_to_sink_bytes())
    sl = strider.Sleigh(arch, mem)

    overrides = (
        {0x2000: CallingConvention.x86_64_all_preserving()} if with_override else {}
    )
    pipeline = (
        _build_default_equivalent_pipeline(arch, sl, cc, mem)
        if use_custom_pipeline
        else None
    )

    res = strider.run(
        arch=arch,
        cc=cc,
        mem=mem,
        rom=mem,
        entry=0x1000,
        per_address_ccs=overrides,
        pipeline=pipeline,
    )
    pat = call().at(0x3000).arg(0, function_arg(0))
    hits = res.graph.find_all(pat)
    assert len(hits) == expected_hits, (
        f"use_custom_pipeline={use_custom_pipeline} "
        f"with_override={with_override}: got {len(hits)} hits, expected {expected_hits}"
    )
```

- [ ] **Step 2: Run the test against master to confirm the bug (red)**

Run: `cd crates/strider-py && uv run pytest tests/python/test_per_address_cc.py::test_per_address_ccs_honoured_in_both_pipeline_paths -v 2>&1 | tail -30`

Expected: three cases pass, one case (`use_custom_pipeline=True, with_override=True`) fails with `assert 0 == 1` — the bug.

If the parametrise output indicates a different failure pattern (e.g. zero hits where the spec predicts one in the no-bug cell, or a Python-side import error), STOP and re-investigate — the fixture or pattern is wrong, not the bug story.

- [ ] **Step 3: Do NOT commit yet** — failing tests don't go in commits.

---

## Task 3: Add `AnalyzeOptions` + `analyze_cfg_with`

**Files:**
- Modify: `crates/strider/src/strider/pipeline.rs`
- Modify: `crates/strider/src/lib.rs` (export the new type if it's not already)

- [ ] **Step 1: Add a module-level static empty map**

In `crates/strider/src/strider/pipeline.rs`, add near the top after the existing `use anyhow::…` line:

```rust
use std::sync::LazyLock;

/// Process-wide empty `per_address_ccs` map.  Borrowed by
/// `AnalyzeOptions::default()` so the default options bag has a real
/// `&'static` reference (not `Option`) and the per-call lookup site
/// stays a single `HashMap::get` with no `Option`-dance.
static EMPTY_PER_ADDRESS_CCS: LazyLock<
    std::collections::HashMap<u64, target::BuiltCallingConvention>,
> = LazyLock::new(std::collections::HashMap::new);
```

- [ ] **Step 2: Add the `AnalyzeOptions` struct**

In the same file, just below the `AnalyzeOutcome` impl:

```rust
/// Per-call lift options for [`Strider::analyze_cfg_with`].  Empty
/// defaults match the legacy `analyze_cfg(cfg)` behaviour: the
/// orchestrator uses this with both fields set; strider-py's
/// custom-pipeline path uses it with `per_address_ccs` set.
pub struct AnalyzeOptions<'a> {
    /// Pre-computed varnode set.  When `None`, `Strider` calls
    /// [`Strider::find_all_unique_vns`] itself.  When `Some`, must be
    /// sorted by `pcode_lift::vn_sort_key` and must include every
    /// varnode any instruction in `cfg` references.  Under-tracking
    /// drops pcode reads; over-tracking is safe but allocates one
    /// extra `InitialVar` per superfluous vn.  The orchestrator passes
    /// `Some(cached_vns)` so it shares one vn table across rebuild
    /// iterations.
    pub all_vns: Option<Vec<rsleigh::Vn>>,

    /// Per-target-address CC override map.  Keys are direct-call
    /// target addresses; values are CCs already resolved against the
    /// same Sleigh register table the function-default CC was built
    /// against.  Empty by default — every direct `Call` uses the
    /// function-default CC.
    pub per_address_ccs: &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
}

impl Default for AnalyzeOptions<'_> {
    fn default() -> Self {
        Self {
            all_vns: None,
            per_address_ccs: &EMPTY_PER_ADDRESS_CCS,
        }
    }
}
```

- [ ] **Step 3: Add the new `analyze_cfg_with` method**

Locate `analyze_cfg_with_vns_and_overrides` (around line 276) and add this method just above it (do NOT remove the old methods yet — that happens in Task 7):

```rust
/// Translates a complete CFG into an [`AnalyzeOutcome`] with
/// caller-supplied [`AnalyzeOptions`].
///
/// Equivalent to [`Strider::analyze_cfg`] when given
/// `AnalyzeOptions::default()`.
///
/// # Errors
///
/// Same as [`Self::analyze_cfg`].
pub fn analyze_cfg_with<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
    opts: AnalyzeOptions<'_>,
) -> Result<AnalyzeOutcome> {
    let all_vns = opts
        .all_vns
        .unwrap_or_else(|| self.find_all_unique_vns(cfg));
    self.analyze_cfg_with_vns_and_overrides(cfg, all_vns, opts.per_address_ccs)
}
```

- [ ] **Step 4: Re-export `AnalyzeOptions`**

Two re-exports to update:

`crates/strider/src/strider/mod.rs` — change:
```rust
pub use pipeline::{AnalyzeOutcome, RegionLiftHandles, Strider};
```
to:
```rust
pub use pipeline::{AnalyzeOptions, AnalyzeOutcome, RegionLiftHandles, Strider};
```

`crates/strider/src/lib.rs` — change:
```rust
pub use strider::{AnalyzeOutcome, RegionLiftHandles, Strider};
```
to:
```rust
pub use strider::{AnalyzeOptions, AnalyzeOutcome, RegionLiftHandles, Strider};
```

- [ ] **Step 5: Build and run the new Rust test**

Run: `cargo test --package strider --test analyze_cfg_with_overrides 2>&1 | tail -20`
Expected: both tests PASS.

If the test fails on the `analyze_cfg_with` call (the lift returns a graph that
doesn't carry the override side-table even though we passed `per_address_ccs`),
the bug lies in the new `analyze_cfg_with` body — confirm it threads
`opts.per_address_ccs` into the underlying lift call, not just
`opts.all_vns`. The reference shape is the existing
`analyze_cfg_with_vns_and_overrides` body.

- [ ] **Step 6: Run the existing strider tests to confirm no regression**

Run: `cargo test --package strider 2>&1 | tail -20`
Expected: all pre-existing tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/strider/src/strider/pipeline.rs crates/strider/src/strider/mod.rs crates/strider/tests/analyze_cfg_with_overrides.rs
git commit -m "$(cat <<'EOF'
strider: add AnalyzeOptions + analyze_cfg_with delegating to legacy methods

Introduces the options-bag-style `analyze_cfg_with(cfg, opts)` that
collapses what used to be three method names (`analyze_cfg`,
`analyze_cfg_with_vns`, `analyze_cfg_with_vns_and_overrides`) into one.
For now the new method delegates to the legacy variants; the legacy
methods are removed in a follow-up.  An `EMPTY_PER_ADDRESS_CCS` static
gives `AnalyzeOptions::default()` a real &'static borrow so the per-call
lookup site can drop its `.and_then(...)` once the IrStrider field is
also de-Optioned.

Adds a regression test pinning that `analyze_cfg_with` applies the
per-address-cc override at lift time and that
`analyze_cfg_with(default)` matches `analyze_cfg`'s output shape.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: De-`Option` `IrStrider::per_address_ccs`

**Files:**
- Modify: `crates/strider/src/strider/mod.rs`

- [ ] **Step 1: Update the field type and constructor**

Replace `crates/strider/src/strider/mod.rs` contents with:

```rust
use anyhow::Result;

mod insn;
mod pipeline;
mod vn_io;

pub use pipeline::{AnalyzeOptions, AnalyzeOutcome, RegionLiftHandles, Strider};

/// Per-function translation context that converts a [`cfg::Cfg`] into an IR
/// graph region by region.
///
/// Holds a reference to the shared [`Strider`] (register / calling-convention
/// information) and a fresh [`ir::FunctionBuilder`].
pub struct IrStrider<'a, R: rsleigh::MemReader> {
    pub(crate) strider: &'a Strider,
    pub(crate) builder: ir::FunctionBuilder,
    pub(crate) cfg: &'a cfg::Cfg<R>,
    /// Anchors for the tier-2 resolver.  Each entry maps a
    /// `BranchIndirect`'s pcode address to the IR `NodeOutputId` whose
    /// producer represents `target_vn`'s value at that BranchIndirect
    /// site.  Populated by `handle_unresolved_indirect_branch` at lift
    /// time, drained by `analyze_cfg` into the [`AnalyzeOutcome`].
    pub(crate) unresolved_branches: Vec<(cfg::PcodeInsnAddr, ir::Value)>,
    /// Per-target-address CC override map.  Defaults to a process-wide
    /// empty map (`pipeline::EMPTY_PER_ADDRESS_CCS`); set to a real map
    /// at constructor time when the caller has overrides.  Lookup is a
    /// single `HashMap::get` regardless.
    pub(crate) per_address_ccs:
        &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
}

impl<'a, R: rsleigh::MemReader> IrStrider<'a, R> {
    /// Creates a new `IrStrider` for the given CFG.
    ///
    /// Constructs the IR [`FunctionBuilder`] with the supplied
    /// `all_vns` (the set of every varnode any instruction in `cfg`
    /// references, sorted by `pcode_lift::vn_sort_key` for stable
    /// `VarId` numbering).  `per_address_ccs` is the lift-time CC
    /// override map; pass `&EMPTY_PER_ADDRESS_CCS` (or an empty
    /// `HashMap`) when the caller has no overrides.
    pub(crate) fn new(
        strider: &'a Strider,
        cfg: &'a cfg::Cfg<R>,
        all_vns: Vec<rsleigh::Vn>,
        per_address_ccs: &'a std::collections::HashMap<u64, target::BuiltCallingConvention>,
    ) -> Result<Self> {
        let builder = ir::FunctionBuilder::new(all_vns, &strider.calling_convention)?;
        Ok(Self {
            strider,
            builder,
            cfg,
            unresolved_branches: Vec::new(),
            per_address_ccs,
        })
    }
}
```

(Removes `set_per_address_ccs` entirely; turns the field into a non-`Option` borrow; adds the new constructor parameter.)

- [ ] **Step 2: Update existing `IrStrider::new` callers**

`pipeline.rs::analyze_cfg_with_vns` (line ~265) and `analyze_cfg_with_vns_and_overrides` (line ~285) both call `IrStrider::new`. Update both:

In `analyze_cfg_with_vns`:
```rust
pub fn analyze_cfg_with_vns<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
    all_vns: Vec<rsleigh::Vn>,
) -> Result<AnalyzeOutcome> {
    self.analyze_cfg_with_vns_and_overrides(cfg, all_vns, &EMPTY_PER_ADDRESS_CCS)
}
```

In `analyze_cfg_with_vns_and_overrides`:
```rust
pub fn analyze_cfg_with_vns_and_overrides<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
    all_vns: Vec<rsleigh::Vn>,
    per_address_built_ccs: &std::collections::HashMap<
        u64,
        target::BuiltCallingConvention,
    >,
) -> Result<AnalyzeOutcome> {
    let mut ir_strider = IrStrider::new(self, cfg, all_vns, per_address_built_ccs)?;
    // (deleted: the `if !per_address_built_ccs.is_empty() { set_… }` block)
    ir_strider.builder.build_entry()?;
    // … rest unchanged …
```

`EMPTY_PER_ADDRESS_CCS` is `pub(crate)` to the `strider` module — change its declaration from `static` to `pub(crate) static` so `mod.rs` can borrow it. Or keep it private and have `analyze_cfg_with_vns` simply pass an empty `HashMap::new()` literal; that's simpler. Recommended:

```rust
pub fn analyze_cfg_with_vns<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
    all_vns: Vec<rsleigh::Vn>,
) -> Result<AnalyzeOutcome> {
    let empty = std::collections::HashMap::new();
    self.analyze_cfg_with_vns_and_overrides(cfg, all_vns, &empty)
}
```

(One-call-site allocation cost is irrelevant; the static is for the public `Default` only.)

- [ ] **Step 3: Verify the strider crate still compiles**

Run: `cargo build --package strider 2>&1 | tail -20`
Expected: clean build.

If the compiler flags `self.per_address_ccs.and_then(...)` in `insn/control.rs`, that's Task 5 — proceed.

- [ ] **Step 4: Update `insn/control.rs` lookup sites**

Edit `crates/strider/src/strider/insn/control.rs`:

Lines 245-247 (inside `handle_call`):
```rust
// Before:
let override_cc = self
    .per_address_ccs
    .and_then(|m| m.get(&target_addr));
// After:
let override_cc = self.per_address_ccs.get(&target_addr);
```

Lines 280-282 (inside `handle_tail_call`):
```rust
// Before:
let override_cc = self
    .per_address_ccs
    .and_then(|m| m.get(&target));
// After:
let override_cc = self.per_address_ccs.get(&target);
```

- [ ] **Step 5: Build the strider crate**

Run: `cargo build --package strider 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 6: Run the strider crate test suite**

Run: `cargo test --package strider 2>&1 | tail -20`
Expected: all tests pass (orchestrator path was already going through `analyze_cfg_with_vns_and_overrides`, so this refactor is mechanically equivalent).

- [ ] **Step 7: Commit**

```bash
git add crates/strider/src/strider/mod.rs crates/strider/src/strider/pipeline.rs crates/strider/src/strider/insn/control.rs
git commit -m "$(cat <<'EOF'
strider: drop IrStrider::set_per_address_ccs and Option<&HashMap>

IrStrider::per_address_ccs is now an unconditional &'a HashMap; callers
hand a real (possibly empty) map at constructor time.  The two lookup
sites in insn/control.rs collapse from `.and_then(|m| m.get(&k))` to
`.get(&k)`.  Behaviour is identical — the previous `if !empty { set }`
guard was cosmetic, since `HashMap::get` on an empty map returns None
the same way.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Migrate orchestrator to `analyze_cfg_with`

**Files:**
- Modify: `crates/strider/src/orchestrator.rs:824-831`

- [ ] **Step 1: Read the current call site**

```bash
sed -n '820,832p' /home/mike/Desktop/strider/crates/strider/src/orchestrator.rs
```

Confirms the snippet matches:
```rust
*vn_cache_region_count = regions_now.len();
let mut all_vns: Vec<rsleigh::Vn> = vn_cache.iter().copied().collect();
all_vns.sort_unstable_by_key(pcode_lift::vn_sort_key);

let outcome = opts.strider.analyze_cfg_with_vns_and_overrides(
    &cfg,
    all_vns,
    &opts.per_address_built_ccs,
)?;
```

- [ ] **Step 2: Replace with `analyze_cfg_with`**

Edit `crates/strider/src/orchestrator.rs`. Replace the block:

```rust
let outcome = opts.strider.analyze_cfg_with_vns_and_overrides(
    &cfg,
    all_vns,
    &opts.per_address_built_ccs,
)?;
```

with:

```rust
let outcome = opts.strider.analyze_cfg_with(
    &cfg,
    crate::AnalyzeOptions {
        all_vns: Some(all_vns),
        per_address_ccs: &opts.per_address_built_ccs,
    },
)?;
```

(`crate::AnalyzeOptions` matches the re-export added in Task 3 Step 4. If the re-export goes through `crate::strider::AnalyzeOptions` instead, use that path — match the existing `crate::Strider` import style at the top of orchestrator.rs.)

- [ ] **Step 3: Build**

Run: `cargo build --package strider 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 4: Run the orchestrator test suite**

Run: `cargo test --package strider --test orchestrator_indirect_branch --test per_address_cc --test per_address_cc_indirect 2>&1 | tail -20`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add crates/strider/src/orchestrator.rs
git commit -m "$(cat <<'EOF'
strider: orchestrator migrates to analyze_cfg_with

Replaces the only external caller of analyze_cfg_with_vns_and_overrides
(in the indirect-branch fixed-point loop) with the new options-bag form.
Mechanical change — cached vns and per-address CCs threaded through
AnalyzeOptions instead of positional arguments.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Delete legacy `analyze_cfg_with_vns*` methods

**Files:**
- Modify: `crates/strider/src/strider/pipeline.rs`

- [ ] **Step 1: Confirm no external callers remain**

Run:
```bash
grep -rn "analyze_cfg_with_vns" /home/mike/Desktop/strider/ --include="*.rs" | grep -v ".git/"
```

Expected: only definition sites in `pipeline.rs`. If anything outside `pipeline.rs` still references either name, STOP — those callers must be migrated first.

- [ ] **Step 2: Delete the legacy methods**

In `crates/strider/src/strider/pipeline.rs`, delete the two methods `analyze_cfg_with_vns` and `analyze_cfg_with_vns_and_overrides` along with their doc-comments. The doc comment on `analyze_cfg` may reference them — update to point at `analyze_cfg_with` instead. Keep `analyze_cfg`:

```rust
/// Translates a complete control-flow graph into an [`AnalyzeOutcome`].
///
/// Equivalent to [`Self::analyze_cfg_with`] with default
/// [`AnalyzeOptions`] — empty override map, scans `cfg` for varnodes.
/// Callers that need either knob (the orchestrator's cached vn table,
/// or strider-py's per-address CC override map) use `analyze_cfg_with`.
///
/// # Errors
///
/// Returns an `anyhow::Error` when the CFG is malformed (missing
/// region, unknown terminator), instruction translation fails (an
/// unsupported opcode or varnode), or IR validation fails.
pub fn analyze_cfg<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
) -> Result<AnalyzeOutcome> {
    self.analyze_cfg_with(cfg, AnalyzeOptions::default())
}
```

Move the lift-body code (the entire `let mut ir_strider = IrStrider::new(...)` through `Ok(AnalyzeOutcome { ... })` block) **into** `analyze_cfg_with`. The new structure is:

```rust
pub fn analyze_cfg_with<R: rsleigh::MemReader>(
    &self,
    cfg: &cfg::Cfg<R>,
    opts: AnalyzeOptions<'_>,
) -> Result<AnalyzeOutcome> {
    let all_vns = opts
        .all_vns
        .unwrap_or_else(|| self.find_all_unique_vns(cfg));
    let mut ir_strider = IrStrider::new(self, cfg, all_vns, opts.per_address_ccs)?;
    ir_strider.builder.build_entry()?;

    // … rest of the body that used to live in analyze_cfg_with_vns_and_overrides …
}
```

The body to move starts at the line `ir_strider.builder.build_entry()?;` and continues through the `Ok(AnalyzeOutcome { graph, unresolved_branches, region_handles })` at the end. Cut, paste, delete the now-empty old methods.

- [ ] **Step 3: Build**

Run: `cargo build --package strider 2>&1 | tail -10`
Expected: clean build.

- [ ] **Step 4: Run the workspace test suite**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: every test passes (Rust-side bug-fix tests too — confirms the new API plumbs through end-to-end).

- [ ] **Step 5: Commit**

```bash
git add crates/strider/src/strider/pipeline.rs
git commit -m "$(cat <<'EOF'
strider: delete legacy analyze_cfg_with_vns* methods

The lift body has migrated into analyze_cfg_with; analyze_cfg now
delegates to analyze_cfg_with(default).  External Rust callers (the
orchestrator) and the workspace tests already use the new shape.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: Plumb `per_address_ccs` into strider-py custom-pipeline path (the actual bug fix)

**Files:**
- Modify: `crates/strider-py/src/run.rs`

- [ ] **Step 1: Update the dispatcher signature pass-through**

Edit `crates/strider-py/src/run.rs`. In the `run` function (line 70-95), replace:

```rust
match pipeline {
    Some(p) => run_with_custom_pipeline(
        py,
        arch,
        cc,
        mem,
        entry,
        rom,
        p,
        allow_code_before_start_addr,
        function_max_size,
        compact,
    ),
    None => run_via_orchestrator(
        py,
        arch,
        cc,
        mem,
        entry,
        rom,
        allow_code_before_start_addr,
        function_max_size,
        compact,
        per_address_ccs.unwrap_or_default(),
    ),
}
```

with:

```rust
let per_address_ccs = per_address_ccs.unwrap_or_default();
match pipeline {
    Some(p) => run_with_custom_pipeline(
        py,
        arch,
        cc,
        mem,
        entry,
        rom,
        p,
        allow_code_before_start_addr,
        function_max_size,
        compact,
        per_address_ccs,
    ),
    None => run_via_orchestrator(
        py,
        arch,
        cc,
        mem,
        entry,
        rom,
        allow_code_before_start_addr,
        function_max_size,
        compact,
        per_address_ccs,
    ),
}
```

- [ ] **Step 2: Update `run_with_custom_pipeline`**

Replace the entire `run_with_custom_pipeline` function with:

```rust
/// Custom-pipeline path — preserves the v1 contract: lift once via
/// `analyze_cfg_with`, then apply the user's pipeline.  Indirect
/// branches are not resolved on this path.  `per_address_ccs` is
/// honoured at lift time the same way as on the orchestrator path.
#[allow(clippy::too_many_arguments)]
fn run_with_custom_pipeline(
    py: Python<'_>,
    arch: PySleighArch,
    cc: PyCallingConvention,
    mem: ReaderInput,
    entry: u64,
    rom: Option<RomInput>,
    pipeline: &crate::opt::PyOptimizerPipeline,
    allow_code_before_start_addr: bool,
    function_max_size: Option<u64>,
    compact: bool,
    per_address_ccs_py: std::collections::HashMap<u64, PyCallingConvention>,
) -> PyResult<PyRunResult> {
    let _ = rom; // custom pipeline owns its own pass list
    let reader: AnyMemReader = mem.into_any().map_err(into_lift_err)?;
    let sleigh = Py::new(py, PySleigh::new_internal(arch.clone(), reader)?)?;

    let s = PyStrider::new_internal(py, arch.clone(), &sleigh, cc.clone())?;
    let strider_obj = Py::new(py, s)?;

    let cfg_obj = Py::new(
        py,
        crate::cfg::build_cfg(
            py,
            sleigh.clone_ref(py),
            entry,
            allow_code_before_start_addr,
            function_max_size,
        )?,
    )?;

    // Resolve per-address CCs against the same Sleigh register table
    // the function-default CC was built against — mirrors the
    // orchestrator's `LoopState::new` behaviour so both pipeline paths
    // honour `per_address_ccs` identically.
    let per_address_built_ccs: std::collections::HashMap<u64, target::BuiltCallingConvention> =
        if per_address_ccs_py.is_empty() {
            std::collections::HashMap::new()
        } else {
            let regs = sleigh.borrow(py).regs.clone();
            per_address_ccs_py
                .into_iter()
                .map(|(addr, py_cc)| {
                    py_cc
                        .inner
                        .build(&regs)
                        .map(|built| (addr, built))
                        .map_err(|e| {
                            into_lift_err(anyhow::anyhow!(
                                "per-address CC at {addr:#x} unresolved: {e:?}"
                            ))
                        })
                })
                .collect::<Result<_, _>>()?
        };

    let strider_borrow = strider_obj.borrow(py);
    let outcome = strider_borrow
        .inner
        .analyze_cfg_with(
            &cfg_obj.borrow(py).inner,
            strider::AnalyzeOptions {
                per_address_ccs: &per_address_built_ccs,
                ..strider::AnalyzeOptions::default()
            },
        )
        .map_err(into_lift_err)?;
    let graph = outcome.graph;
    drop(strider_borrow);
    let py_graph = Py::new(py, PyGraph::new(graph, cfg_obj.clone_ref(py)))?;

    let actual_pipeline = pipeline.drain_into_pipeline()?;
    {
        let py_graph_borrow = py_graph.borrow(py);
        let mut graph = py_graph_borrow.write_inner().map_err(into_strider_err)?;
        actual_pipeline
            .run_on_built(&mut graph)
            .map_err(|e| into_strider_err(anyhow::anyhow!("optimize failed: {e:?}")))?;
        if compact {
            graph.compact();
        }
    }

    Ok(PyRunResult {
        cfg: cfg_obj,
        graph: py_graph,
        sleigh,
    })
}
```

(Imports: `target` and `strider` are already top-level workspace crates the binding uses; if the `use` block at the top of run.rs doesn't yet pull in `strider::AnalyzeOptions`, it's fine — qualify with `strider::AnalyzeOptions` inline as shown.)

- [ ] **Step 3: Update the docstring on `run`**

In the same file, update the `pub fn run` doc-comment block (above line 56) — find the section that documents `per_address_ccs` and replace it with text that explicitly states applicability on both paths. If no per-parameter docstring exists, leave the function-level doc alone; the docstring on the Python side (`crates/strider-py/python/strider/__init__.pyi` if present, else inline pyo3 attrs) is what users see. Search:

```bash
grep -n "per_address_ccs" /home/mike/Desktop/strider/crates/strider-py/src/run.rs /home/mike/Desktop/strider/crates/strider-py/python/strider/*.pyi 2>/dev/null
```

If a pyi file exists with a `per_address_ccs` doc paragraph, edit that file to read:

```
per_address_ccs:
    Optional map of {target_address: CallingConvention} overrides.
    When a Call's target matches a key, that CC fully replaces the
    function-default for that one Call.  Applied at lift time and
    therefore honoured on both pipeline paths (the default
    None-orchestrator path and the custom `pipeline=` path).  Empty
    by default.
```

If no pyi file exists, no docstring change is needed in this task — the parameter is already documented in `RunConfig`'s Rust-side comment.

- [ ] **Step 4: Rebuild the Python extension**

Run: `cd /home/mike/Desktop/strider/crates/strider-py && uv run maturin develop 2>&1 | tail -10`
Expected: build succeeds.

- [ ] **Step 5: Run the new Python test**

Run: `cd /home/mike/Desktop/strider/crates/strider-py && uv run pytest tests/python/test_per_address_cc.py -v 2>&1 | tail -30`
Expected: all four matrix cells pass — the previously-red bottom-right cell now reports 1 hit.

- [ ] **Step 6: Run the broader Python test suite**

Run: `cd /home/mike/Desktop/strider/crates/strider-py && uv run pytest 2>&1 | tail -30`
Expected: no regressions in any other test.

- [ ] **Step 7: Commit**

```bash
git add crates/strider-py/src/run.rs crates/strider-py/python/strider/*.pyi crates/strider-py/tests/python/test_per_address_cc.py
git commit -m "$(cat <<'EOF'
strider-py: honour per_address_ccs on the custom-pipeline path

Bug: strider.run(pipeline=<custom>, per_address_ccs=...) silently dropped
the override because run_with_custom_pipeline never accepted the kwarg
and called analyze_cfg (no overrides).  Calls were already built with
the function-default CC by the time the user's pipeline ran, so no
post-lift pass could recover the override.

Fix: dispatcher forwards per_address_ccs to both branches;
run_with_custom_pipeline resolves CCs against the Sleigh register
table (mirroring LoopState::new) and lifts via analyze_cfg_with.

Adds a 2x2 matrix test pinning the fix on a synthetic
arg-thru-hook-to-sink x86_64 fixture.  Pre-fix: bottom-right cell
fails (0 hits where 1 expected); post-fix: all four cells pass.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: Lint pass

- [ ] **Step 1: Run clippy**

Run: `cargo clippy --workspace --all-targets -- -D warnings 2>&1 | tail -30`
Expected: no warnings.

If clippy flags `let empty = HashMap::new();` in `analyze_cfg_with_vns` (Task 4 Step 2 — but that method may have been deleted in Task 6, so this point is moot), STOP and verify Task 6 actually removed both legacy methods.

- [ ] **Step 2: Run the full workspace tests one more time**

Run: `cargo test --workspace 2>&1 | tail -15 && cd crates/strider-py && uv run pytest 2>&1 | tail -10`
Expected: all green.

- [ ] **Step 3: Commit any clippy-driven fixes**

If Step 1 surfaced any warnings that needed fixing:

```bash
git add -A
git commit -m "$(cat <<'EOF'
strider: clippy fixes from analyze_cfg API refactor

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

If no fixes were needed, no commit.

---

## Task 9: Code review

- [ ] **Step 1: Dispatch the feature-dev:code-reviewer agent**

Use the `Agent` tool with `subagent_type: "feature-dev:code-reviewer"`. Prompt:

> Review the staged commits on branch `feature/ai` against `master` for the `analyze_cfg` API refactor + `per_address_ccs` custom-pipeline bug fix. Spec at `docs/superpowers/specs/2026-05-06-custom-pipeline-per-address-cc-design.md`.
>
> Focus areas:
> 1. **Bug fix correctness**: does `run_with_custom_pipeline` now pass `per_address_ccs` through to lift time, and does the new test actually exercise the fix end-to-end?
> 2. **API surface**: is the `AnalyzeOptions` struct's lifetime story sane? Does `Default` work as advertised? Any rough edges with the `LazyLock<HashMap>` static?
> 3. **Migration completeness**: any leftover callers of the deleted `analyze_cfg_with_vns*` methods inside or outside the repo? Any leftover doc references?
> 4. **Test quality**: is the parametrised Python test isolating the bug (the failing cell on master) without false-positive cells that would also fail before the API refactor for unrelated reasons?
> 5. **Adherence to project conventions**: does the new code match the workspace's anyhow-only error story, the asm-fingerprint contract (still-attribution-aware), and the testing style of `tests/per_address_cc.rs`?
>
> Run `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cd crates/strider-py && uv run pytest` to verify the branch is green. Report only high-confidence issues that would block merge.

- [ ] **Step 2: Address review feedback**

Apply any high-confidence findings from the agent. Each fix in its own commit. If the agent reports issues that look wrong, push back rather than blindly accept (per superpowers:receiving-code-review).

- [ ] **Step 3: Final commit summary**

Run: `git log --oneline master..HEAD`
Confirm the commit chain reads sensibly: spec → AnalyzeOptions+test → de-Option IrStrider → orchestrator migration → delete legacy methods → strider-py bug fix → optional clippy/review fixes.

---

## Self-Review

**Spec coverage:**
- ✅ Bug 1 root cause (lift-time consumption) — covered in plan intro.
- ✅ Plumb `per_address_ccs` into `run_with_custom_pipeline` — Task 7.
- ✅ Collapse three methods into one + options — Tasks 3, 6.
- ✅ Drop `set_per_address_ccs` and `Option<&'a …>` — Task 4.
- ✅ Update strider.run docstring — Task 7 Step 3.
- ✅ Tests: Rust integration test — Task 1; Python 2x2 matrix test — Task 2.
- ✅ Implementation order: TDD red → refactor → bug fix → tests green → lint → review (matches spec section "Implementation order").

**Placeholder scan:** No "TBD" / "TODO" / vague directives. Each step has either exact code or an exact command.

**Type consistency:** `AnalyzeOptions<'a>` consistent across Tasks 3, 5, 6, 7. `EMPTY_PER_ADDRESS_CCS` referenced consistently. `IrStrider::new` signature defined in Task 4 Step 1 matches usage in Task 4 Step 2 and (implicitly) Task 6 Step 2.

---

## Execution Notes

- Frequent commits — every task ends in a commit (or none if no changes).
- TDD discipline — Tasks 1-2 produce uncommitted failing tests; Tasks 3-7 turn them green commit by commit.
- Rebuild Python extension between Rust changes that affect the strider-py FFI boundary. The full incantation is `cd crates/strider-py && uv run maturin develop`. Forgetting this means pytest tests run against stale `.so` and either pass for the wrong reason or fail with confusing import errors.
- The `LazyLock` static is `'static`-borrowed. Ensure the workspace's effective Rust version actually has it — `rustc --version` should report 1.80+; the workspace Cargo.toml specifies edition 2024 which mandates 1.85+.
