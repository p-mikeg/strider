# CFG Simplifications: unify unconditional edges + decouple Sleigh

> **For agentic workers:** Use TDD. The cross-arch IR snapshot suite is the
> oracle — both parts are *structural* refactors that MUST NOT change lifted
> IR. Keep `cargo test --workspace` + `uv run pytest` green after every task.

**Goal:** Two independent CFG simplifications requested as "no branch/fall-through
distinction" and "the CFG shouldn't require Sleigh."

**Tech stack:** Rust workspace; CFG in `crates/strider-lift/src/cfg/`; CFG→IR
driver in `crates/strider-analyze/src/strider/`; `petgraph::StableDiGraph`.

---

## Findings that reshape the original two claims

1. **Branch vs fall-through (Part A) — feasible.** `RegionEdgeKind` has four
   variants (`Fallthrough`, `Branch`, `IfCaseTrue`, `IfCaseFalse`). The
   `Fallthrough`/`Branch` pair is redundant: both are *unconditional* successor
   links, and `build_branch(dest)` and `link_regions(src,tgt)` both bottom out in
   `FunctionBuilder::link_region(child, ctrl, mem, parent)`. The conditional pair
   `IfCaseTrue`/`IfCaseFalse` is **not** redundant (the `CondBranch` terminator
   alone doesn't say which successor is which), so it stays.

2. **CFG owns Sleigh (Part B) — NOT removable as literally stated.** The CFG holds
   the full `rsleigh::Sleigh<R>` because the **CFG→IR lifting stage** needs it
   (`ValueLifter` register aliasing, `default_code_space` for tail calls,
   `decode_user_op` for CallOther) — not only the dot renderer. "Store only
   `SleighRegs`" is impossible (lifting needs the full stateful decoder). The
   feasible version: make `Cfg` a **pure data structure** and *thread* `Sleigh`
   explicitly to the lifter and the renderer (the renderer needs only `regs()`).
   This is a moderate cross-cutting refactor; see Part B's cost note.

---

## Part A — Unify `Fallthrough` + `Branch` into `Unconditional`

**Files:**
- Modify: `crates/strider-lift/src/cfg/types.rs` (`RegionEdgeKind`)
- Modify: `crates/strider-lift/src/cfg/builder/region_builder.rs` (edge emission;
  drop the "branch-to-next-insn → Fallthrough" reclassification)
- Modify: `crates/strider-lift/src/cfg/builder/split.rs` (split edge)
- Modify: `crates/strider-lift/src/cfg/query.rs` (`region_branch` +
  `region_fallthrough` → one `region_successor`; keep `region_if`)
- Modify: `crates/strider-lift/src/cfg/dot.rs` (edge styling)
- Modify: `crates/strider-analyze/src/strider/insn/control.rs` (`handle_branch`)
- Modify: `crates/strider-analyze/src/strider/pipeline.rs` (`link_region_edges`)
- Tests: existing cross-arch IR snapshot suite + cfg unit tests in
  `crates/strider-lift/tests/` and `crates/strider-lift/src/cfg/`

- [ ] **A1. Pin current behavior (characterization).** Run the full suite and
  the cross-arch snapshot; confirm green. This is the byte-for-byte oracle —
  the refactor must not change lifted IR for any arch.
- [ ] **A2. Collapse the edge enum.** Replace `RegionEdgeKind::{Fallthrough,
  Branch}` with a single `Unconditional` (keep `IfCaseTrue`/`IfCaseFalse`).
  Update the doc comment.
- [ ] **A3. Builder emits `Unconditional`.** In `region_builder.rs` and
  `split.rs`, emit `Unconditional` for every former `Fallthrough`/`Branch`
  edge. Remove the now-pointless "pcode `Branch` targeting the next machine
  instruction → reclassify as `Fallthrough`" logic — both map to
  `Unconditional`. (Leave `RegionTerminator` untouched for now; A7 revisits it.)
- [ ] **A4. Query API.** Replace `region_branch` + `region_fallthrough` with one
  `region_successor(region_id) -> Result<Option<NodeIndex>>` = `unique_outgoing(
  Unconditional)`. Keep `region_if`.
- [ ] **A5. Unify the consumer.** In `link_region_edges` (pipeline.rs), link
  EVERY `Unconditional` edge via `link_regions(src, tgt)` (drop the
  `insns.is_empty()` special-case). In `handle_branch` (control.rs), an
  unconditional-terminator region now does nothing — its successor is wired by
  the post-loop linker — so `handle_branch` collapses to "no-op for the
  unconditional case" (or is removed and its caller skips). `build_branch` stays
  (still used by the switch-ladder lowering).
- [ ] **A6. Snapshot check (the critical verification).** Re-run the cross-arch
  snapshot. If IR is byte-identical → the unification is behavior-preserving,
  done. If it differs, the only legitimate cause is `build_branch`'s
  `terminate_cur_region()` doing something `link_regions` doesn't for the
  branch case; fall back to keeping a single `build_branch` call in
  `handle_branch` for ALL unconditional terminators (read `Region.terminator`,
  not the edge) and leave the linker handling only fall-through — still a
  net simplification (edge enum 4→3). Re-snapshot until green.
- [ ] **A7. (Optional) Terminator redundancy.** Check whether
  `RegionTerminator::{Fallthrough, Branch}` still has any consumer after A5. If
  the only readers were the edge-kind queries, collapse those two terminator
  variants too. If anything still needs them, leave them and note why.
- [ ] **A8. dot styling + tests.** Update `dot.rs` (one style for
  `Unconditional`). Fix/extend cfg unit tests. `cargo test --workspace` +
  `pytest` green.

---

## Part B — Decouple `Cfg` from `Sleigh` (thread it explicitly)

> **Cost note / decision point:** This removes `Sleigh` ownership from the `Cfg`
> data structure and threads it through the lifter + renderer instead. It is a
> moderate cross-cutting change (orchestrator Sleigh-reuse loop, driver
> construction, cfg-builder return type, dot renderer signature, examples,
> strider-py). It does NOT reduce what Sleigh is needed for — only *where it
> lives*. Confirm this is worth the churn before executing; Part A is the
> higher-value, lower-risk win and can ship independently.

**Files:**
- Modify: `crates/strider-lift/src/cfg/mod.rs` (drop `sleigh` field + `sleigh()`
  + `into_sleigh()`; `Builder::build` returns the `Cfg` while the caller retains
  the `Sleigh` it passed in)
- Modify: `crates/strider-lift/src/cfg/dot.rs` (render fn takes `&SleighRegs`)
- Modify: `crates/strider-analyze/src/strider/vn_io.rs`,
  `insn/control.rs` (`default_code_space`), `insn/mod.rs` (`decode_user_op`):
  `PerRegionDriver` receives `&Sleigh` (or the bits it needs) as a field/param
  rather than `cfg.sleigh()`
- Modify: `crates/strider-analyze/src/orchestrator/mod.rs`: orchestrator OWNS
  the `Sleigh` across rebuild iterations (replaces the `cfg.into_sleigh()`
  harvest) and passes `&mut Sleigh` to each CFG build + `&Sleigh` to the driver
- Modify: `crates/strider-analyze/examples/*.rs`, `crates/strider-py/src/function.rs`
  (pass `Sleigh`/regs explicitly to the dot dumper)
- Modify/remove: `crates/strider-lift/tests/cfg_build_end_to_end.rs` `into_sleigh`
  round-trip test

- [ ] **B1. Decide construction ownership.** `Builder<R>` keeps `Sleigh` during
  construction (decode needs it). Change `Builder::build` so the `Sleigh` is NOT
  moved into the `Cfg`; the caller (orchestrator / `build_cfg`) keeps owning it.
  Update the cfg-builder return contract accordingly.
- [ ] **B2. Driver takes Sleigh explicitly.** Give `PerRegionDriver` a
  `sleigh: &Sleigh<R>` (or `&mut`, per `lift_one` needs — confirm the driver only
  reads it) and route `vn_io.rs` / `control.rs` / `insn/mod.rs` through it.
- [ ] **B3. Renderer takes regs.** `cfg::dot` render fn takes `&SleighRegs`
  (resolved by the caller via `sleigh.regs()`), not `cfg.sleigh()`.
- [ ] **B4. Orchestrator owns Sleigh.** Replace the `into_sleigh()` harvest loop
  with the orchestrator holding `Sleigh` and threading it into each rebuild +
  the driver. Remove `Cfg::into_sleigh` / `Cfg::sleigh` / the `sleigh` field.
- [ ] **B5. Fix examples + strider-py + the `into_sleigh` test.** Pass Sleigh
  explicitly everywhere.
- [ ] **B6. Snapshot + full green.** Cross-arch snapshot byte-identical;
  `cargo test --workspace` + `uv run pytest` + `clippy --workspace` +
  `rustdoc` all green.

---

## Sequencing

Ship **Part A first** (self-contained, high-value, low-risk, IR-preserving).
Then decide on **Part B** (bigger refactor, pure architectural cleanliness).
Each part is its own branch + PR to `rewrite/strider`.
