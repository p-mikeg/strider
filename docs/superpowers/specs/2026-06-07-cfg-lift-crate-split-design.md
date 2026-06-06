# strider-cfg / strider-lift crate split — Design

**Date:** 2026-06-07
**Status:** Approved (verification spike complete)

## Goal

Split today's `strider-lift` crate into two single-responsibility crates:

- **`strider-cfg`** — *bytes → CFG*. Decodes machine instructions via
  Sleigh and builds the control-flow graph (`Cfg`). IR-free.
- **`strider-lift`** — *CFG → IR*. Lifts a `Cfg` into a
  `strider_ir::Function` (the pcode→value lifter + the region driver).

And reshape the options so each crate owns its own knobs:

- `CfgOptions` becomes **public** and is the SSoT for CFG-shaping knobs.
- `LiftOptions` (in strider-lift) **embeds** a `CfgOptions` rather than
  duplicating its fields and snapshotting them.

## Why this is clean (verification spike findings)

The boundary already exists logically inside strider-lift; this makes it
physical.

- `cfg/` imports **zero** `strider_ir`, `pcode_lift`, or `lift`. Its only
  outward reference is `crate::LiftOptions`.
- `lift/` references `crate::cfg::*` pervasively (one-way). So
  `strider-lift → strider-cfg` is a clean DAG edge.
- The historical tangle is gone: there is **no** `IndirectResolverFn` /
  `with_indirect_resolver` / `dyn Fn` IR-callback left in `cfg/`. The
  indirect-branch redesign replaced the cfg-time IR callback with the
  `known_targets` feedback map, so the split fights no back-edge.

### The options inversion is a strict SSoT win

Today `CfgOptions` already exists, but as a **private copy** of a
`LiftOptions` subset, rebuilt by `CfgOptions::from_lift_options(...)` on
every `Builder::for_arch`. And `LiftOptions` sits at the strider-lift
crate root **specifically** to dodge a cfg→lift edge (its own doc says
so). Once cfg is its own crate that workaround inverts and disappears:

- `CfgOptions` (in strider-cfg) is the public SSoT:
  `{ fn_max_size, allow_code_before_start_addr, known_targets }`.
- `LiftOptions` (in strider-lift) becomes
  `{ cfg: CfgOptions, all_vns, per_address_ccs }`.
- `from_lift_options` is deleted — no field copy, no drift risk.
- `Builder::for_arch` takes `&CfgOptions` (it can no longer see
  `LiftOptions`, which now lives in the crate that depends on it).
  `lift_function` passes `&lift_opts.cfg`.

## What moves where

**→ strider-cfg** (the whole `cfg/` subtree, promoted to crate root):
`cfg/{mod,types,query,dot,options}.rs`, `cfg/builder/**`
(`mod`, `region_builder`, `split`, `indirect_resolver`). Public types:
`Cfg`, `Builder`, `RegionId`, `Region`, `RegionInstruction`,
`RegionTerminator`, `RegionGraph`, `MachineInsnAddr`, `PcodeInsnAddr`,
`ResolvedTargets`, `is_addr_tail_call`, `CfgOptions`, the `Result`/error
types, the `graphwalk::GraphRef for Cfg` impl. Internal `crate::cfg::X`
paths become `crate::X`.

**stays in strider-lift:** `pcode_lift/**`, `lift/**`, `lift_options.rs`
(reshaped). Both `lift/vn_io.rs` (the `PerRegionDriver` → `ValueLifter`
wrapper) and `pcode_lift/vn_io.rs` (register-aliasing) are distinct and
stay.

## Dependency graph after the split

```
strider-cfg          → strider-target, rsleigh, petgraph, dot, graphwalk,
                       rustc-hash, anyhow   (NO strider-ir)
strider-lift         → strider-cfg, strider-ir, strider-target, rsleigh,
                       anyhow   (drop graphwalk; drop petgraph if unused)
strider-opt          → strider-cfg (was strider-lift), strider-ir,
                       strider-pattern, strider-target, entity-utils
strider-orchestrator → strider-lift, strider-cfg (new), strider-opt, …
strider-py           → strider-cfg (new), strider-lift, strider-orchestrator, …
strider-reader       → (unchanged — only a doc-comment mentions cfg)
```

No cycles. `strider-opt → strider-cfg` is the one new edge the user
approved ("an okay edge"). strider-opt drops its `strider-lift` dep
entirely (it only ever used `ResolvedTargets`).

## Downstream rename surface (~26 files)

`strider_lift::cfg::X` → `strider_cfg::X` across:
- strider-opt: `indirect_branch_resolve/{classify,mod,table}.rs`,
  `pipeline.rs` (only `ResolvedTargets` + a doc path).
- strider-orchestrator: `orchestrator/mod.rs`, `strider/pipeline.rs`,
  plus ~15 files under `tests/`, `benches/scaling.rs`, two `examples/`.
- strider-py: `cfg.rs` (`PyCfg.inner`, `build_cfg` now builds
  `CfgOptions`), `function.rs`, `sleigh.rs`.
- strider-reader: `tests/elf_smoke.rs` — stale doc comment only.

Orchestrator keeps `strider_lift::{lift::Lifter, LiftOptions,
pcode_lift::vn_sort_key}` and switches `cfg::{Cfg, MachineInsnAddr,
is_addr_tail_call}` to `strider_cfg::`.

## Behaviour

Pure mechanical move + options reshape. **No behavioural change.** Same
CFG, same IR, same tests. The gate is: every existing test still passes
(`cargo test --workspace` 0 failures, `cargo clippy --workspace
--all-targets` clean, `uv run pytest` unchanged count).

## Risks

- Wide but low-risk rename sweep; the only logic edit is the options
  reshape (`LiftOptions` embeds `CfgOptions`; `Builder::for_arch` arg
  type; delete `from_lift_options`).
- The workspace will not fully build mid-extraction; tasks proceed up the
  DAG (`cargo build -p <crate>` per task), full gate at the end.
- `petgraph`/`graphwalk` dep ownership: confirm `lift`/`pcode_lift` don't
  use them before dropping from strider-lift.
