# Lifter owns the Sleigh — Design

**Date:** 2026-06-07
**Status:** Approved (full/unified scope confirmed by user)

## Goal

Make `strider_lift::Lifter<R>` the single lift engine that **owns** the
`rsleigh::Sleigh<R>`, with the calling convention passed **per lift call**
(not stored). This removes the `arch`/`sleigh_regs` duplication between
`Strider` and the per-call-cloned `Lifter`, gives the Sleigh its natural
owner, and lets the Lifter be constructed once and reused.

`strider_orchestrator::Strider<R>` collapses to `{ lifter, rom }`.

**Out of scope (deferred):** eliminating the per-CFG transient
`PerRegionDriver`. It stays; it's only minimally touched (gains the
per-call `cc`, sources the Sleigh from the Lifter). A later change may
rename it / reconsider it.

## Target shapes

### `strider_lift` — the engine

```rust
pub struct Lifter<R: rsleigh::MemReader> {
    arch: strider_target::SleighArch,
    sleigh: rsleigh::Sleigh<R>,
    sleigh_regs: rsleigh::SleighRegs,
}

impl<R: rsleigh::MemReader> Lifter<R> {
    pub fn new(arch, sleigh) -> Result<Self>;   // sleigh_regs = sleigh.regs()?
    pub fn arch(&self) -> &SleighArch;
    pub fn sleigh(&self) -> &rsleigh::Sleigh<R>;          // rendering / pcode
    pub fn sleigh_regs(&self) -> &rsleigh::SleighRegs;

    // CC is a per-call argument; no Sleigh argument (owned).
    pub fn build_cfg(&mut self, entry, &CfgOptions) -> Result<Cfg>;
    pub fn analyze_cfg(&self, &Cfg, cc) -> Result<LiftOutcome>;
    pub fn analyze_cfg_with(&self, &Cfg, cc, &LiftOptions) -> Result<LiftOutcome>;
    pub fn lift(&mut self, entry, cc, &LiftOptions) -> Result<LiftOutcome>; // build+lift
}
```

- The `calling_convention` field, `new(arch, regs, cc)`, and
  `from_built_cc(...)` are removed. `find_all_unique_vns` stays.
- `lift_function` free fn is removed (superseded by `Lifter::lift`).
- `PerRegionDriver<'a, R>` (transient, unchanged in spirit): borrows
  `&'a Lifter<R>` (now carrying the Sleigh) and gains a borrowed
  `cc: &'a BuiltCallingConvention`. The one `self.lifter.calling_convention`
  read becomes `self.cc`; the Sleigh comes from the Lifter.

### `strider_orchestrator`

```rust
pub struct Strider<R: rsleigh::MemReader> {
    lifter: Lifter<R>,
    rom: Option<Box<dyn ReadOnlyMemory>>,
}
impl Strider<R> {
    pub fn new(arch, sleigh, rom) -> Result<Self>;  // Lifter::new(arch, sleigh)
    pub fn analyze(&mut self, entry, cc, &LiftOptions, &OptOptions) -> Result<Function>;
}
```

- The fixed-point loop borrows `&mut self.lifter` (build+lift each
  rebuild) and `&self.rom` (opt). Disjoint fields, so the current
  borrow-split simplifies.
- `LiftDriver` becomes `LiftDriver<R> { lifter: Lifter<R>, alias_mode }`,
  wrapping the owning Lifter. `analyze_cfg(cfg, cc)` drops the Sleigh arg;
  `build_optimizer_pipeline()` already just returns `default_pipeline()`.
  `new(arch, sleigh)` (no cc).

### `strider-py` — the public surface change (the blast radius)

The Sleigh now lives inside the Lifter, so:

- `strider.Lifter(arch, mem, cc)` — builds + owns the Sleigh from `mem`
  (was `Lifter(arch, sleigh, cc)` taking a pre-built `Sleigh`). `cc` is
  stored on the Python handle and threaded into each lift call (keeping
  the Python ergonomic; the Rust engine is cc-per-call).
- `build_cfg(sleigh, addr)` free function → `lifter.build_cfg(addr, …)`
  method (uses the Lifter's owned Sleigh).
- `PyCfg` drops its `Py<PySleigh>` field; it holds a `Py<PyLifter>`
  back-reference instead, so `cfg.to_html` / `to_dot` / `html_str` render
  through the Lifter's Sleigh.
- `Sleigh(arch, mem)` pyclass stays (standalone pcode / advanced use).
- The high-level `strider.Strider` (`PyStriderRun`) already owns its
  Sleigh; only its internal construction changes.

~14 Python test files / 18 `build_cfg` call sites adapt:
`build_cfg(sleigh, addr)` → `lifter.build_cfg(addr)`, dropping the now
Lifter-owned standalone `Sleigh` where it was only used for that.

## Why `PerRegionDriver` stays (recorded; not revisited here)

The per-lift transient owns the half-built `FunctionBuilder` (consumed by
`build()`), borrows the engine + cfg, and lets every opcode handler be a
clean `&mut self` method instead of threading `(builder, sleigh, cc)`
through ~30 signatures. Merging it into the reused Lifter would conflate
stable engine state with mid-lift scratch and hit the iterate-cfg-while-
`&mut self` borrow wall. It is `pub(crate)` — invisible to users.

## Behaviour

Pure restructure — same IR for the same input. Gate: `cargo test
--workspace` 0 failures, `cargo clippy --workspace --all-targets` clean,
`uv run pytest` (count may shift if any test merges the
Sleigh/Lifter construction; no assertion weakened).

## Risk

Wide (lift surface + public Python API). Core Rust borrow mechanics
(Lifter owns Sleigh; build `&mut` then lift `&`) need care; the Python
sweep is mechanical. Executed with the core driven directly and the
Python/test sweep + reviews delegated to subagents.
