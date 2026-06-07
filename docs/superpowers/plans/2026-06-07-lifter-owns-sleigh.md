# Lifter owns the Sleigh — Plan

> REQUIRED SUB-SKILL: superpowers:subagent-driven-development for the wide
> Python/test sweep; core Rust driven directly.

**Sequencing:** workspace won't fully build until the Python task. Gate
per crate with `cargo build -p <crate>`; full gate at the end.

### Task 1 — strider-lift: `Lifter<R>` owns the Sleigh, cc per-call
- `Lifter<R> { arch, sleigh, sleigh_regs }`; `new(arch, sleigh) -> Result`
  (computes `sleigh_regs`). Drop `calling_convention` field +
  `new(arch,regs,cc)` + `from_built_cc`. Add `sleigh()` accessor.
- Lift API: `build_cfg(&mut self, entry, &CfgOptions) -> Cfg`;
  `analyze_cfg(&self, &Cfg, cc)`; `analyze_cfg_with(&self, &Cfg, cc,
  &LiftOptions)`; `lift(&mut self, entry, cc, &LiftOptions)`. None take a
  Sleigh arg.
- `PerRegionDriver`: add borrowed `cc` (param to `new`); `lifter.calling_
  convention` → `self.cc`; Sleigh from `&lifter.sleigh`.
- Remove `lift_function`.
- `lift/value/tests.rs`: build `Lifter::new(arch, sleigh)`, drive
  `PerRegionDriver` with the test `cc`.
- Gate: `cargo test -p strider-lift`.

### Task 2 — strider-orchestrator: `Strider<R> { lifter, rom }` + `LiftDriver<R>`
- `Strider<R> { lifter: Lifter<R>, rom }`; `new(arch, sleigh, rom)`;
  `analyze(entry, cc, &LiftOptions, &OptOptions)`. LoopState borrows
  `&mut self.lifter` + `&self.rom`; build_lift uses `lifter.build_cfg` +
  `lifter.analyze_cfg_with(&cfg, cc, opts)`.
- `LiftDriver<R> { lifter: Lifter<R>, alias_mode }`; `new(arch, sleigh)`;
  `analyze_cfg(&Cfg, cc)`/`analyze_cfg_with`; `build_cfg`;
  `build_optimizer_pipeline` = `default_pipeline()`.
- Gate: `cargo build -p strider-orchestrator` (+ its tests in Task 4).

### Task 3 — strider-py: `Lifter` owns the Sleigh
- `PyLifter { inner: LiftDriver<AnyMemReader>, cc }`. `Lifter(arch, mem,
  cc)` builds+owns the Sleigh. `analyze_cfg(cfg)` → `inner.analyze_cfg(&cfg,
  &self.cc)`. Add `build_cfg(addr, …)` method.
- `build_cfg` free fn → removed/relocated onto `PyLifter`.
- `PyCfg` drops `Py<PySleigh>`; holds `Py<PyLifter>`; `to_html`/`to_dot`/
  `html_str` render via the Lifter's Sleigh.
- `run.rs` / `strider_cls.rs`: adapt construction (Sleigh built into the
  Lifter/Strider).
- Gate: `cargo build -p strider-py`.

### Task 4 — sweep callers + full gate (subagent)
- orchestrator tests/benches/examples: `analyze_cfg(cfg, sleigh)` →
  `analyze_cfg(cfg, cc)`; build via the owning Lifter.
- Python tests (~14 files): `Sleigh(arch,mem)` + `Lifter(arch, sleigh,
  cc)` + `build_cfg(sleigh, addr)` → `Lifter(arch, mem, cc)` +
  `lifter.build_cfg(addr)`. Preserve every assertion.
- Rebuild `.so`; full gate (`cargo test --workspace`, clippy, pytest).
- Update CLAUDE.md / `__init__.pyi` stub for the new Lifter API.

### Task 5 — final review (subagent) + merge
