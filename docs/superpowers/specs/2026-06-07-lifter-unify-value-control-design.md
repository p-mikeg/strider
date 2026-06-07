# Unify the value + control lifter into one struct — Design

**Date:** 2026-06-07
**Status:** Approved (verification spike complete)

## Goal

Collapse strider-lift's three lifting structs into the natural two, so all
per-instruction lifting — value-producing *and* control-flow — happens as
`&mut self` methods on a single per-CFG lifter. Eliminate the separate
`ValueLifter` struct and the per-call reconstruction it forces.

## Current shape (and why it's wrong now)

Three structs:
- `Lifter` (owned, `Clone`) — config only: `calling_convention` + `arch` +
  `sleigh_regs`. The orchestrator builds one per function and reuses it
  across every CFG-rebuild iteration (avoids re-paying `Sleigh::regs()`).
  Carries the public `analyze_cfg` / `analyze_cfg_with` / `lift_function`
  entry points.
- `PerRegionDriver<'a, R>` (pub(crate), per-CFG) — owns the
  `FunctionBuilder`, borrows `&Lifter` + `&Cfg` + `&Sleigh`, holds
  `unresolved_branches` + `per_address_ccs`. **All control-flow lifting**
  (`process_insn`, `handle_branch`, `handle_call`, `handle_store`,
  `handle_return`, `handle_call_other`) lives here.
- `ValueLifter<'a, R>` (pub) — borrows `&mut FunctionBuilder` + `&Sleigh`.
  **All value-producing lifting** (`value/*.rs`) + `read_vn` / `write_vn`
  live here.

`ValueLifter`'s own doc says it is separate so the value logic can "be
reused as the inner-loop dispatch in strider-orchestrator." That goal is
**stale**: after the orchestrator-lift-boundary refactor the per-region
driver is in strider-lift, the same crate. There is no cross-crate reuse.

The split now only costs: because `ValueLifter` borrows `&mut builder`
while `PerRegionDriver` *owns* `builder`, a long-lived `ValueLifter` would
be a self-reference. So `lift/vn_io.rs::value_lifter()` mints a fresh
`ValueLifter` on **every** `read_vn`/`write_vn` and value-opcode dispatch
— ~87 reconstructions, all re-passing the same `(&mut builder, &sleigh)`.

## Design

Keep `Lifter` (config + entry points) and merge `ValueLifter` **into**
`PerRegionDriver`. As `&mut self` methods there is no self-reference, so:

- `read_vn` / `write_vn` become `&mut self` methods on `PerRegionDriver`
  (reading `self.builder` + `self.sleigh`).
- Every value handler in `value/*.rs` becomes an `impl PerRegionDriver`
  method (`&mut self`), organised across the same files via multiple
  `impl` blocks. Value dispatch (`value::lift`) becomes
  `self.lift_value(insn) -> Result<bool>`.
- `process_insn_inner` calls `self.lift_value(insn)?` then falls through
  to the control `match` — both arms now methods on the same `self`.

Delete: the `ValueLifter` struct, `lift/vn_io.rs` (the wrapper), and the
`pcode_lift/vn_io.rs` `impl ValueLifter` (its bodies move onto
`PerRegionDriver`). Keep the free utilities (`vn_sort_key`,
`first_input_or_err`, `nth_input_or_err`, `require_output_vn`,
`decode_space_id`) as functions in a small module.

### Config: borrowed (unchanged)

`PerRegionDriver` keeps borrowing `&Lifter` for config; the orchestrator
keeps building one `Lifter` per function and reusing it across rebuilds.
No new clones. (This is the "borrow, not re-clone per rebuild" choice.)

### `pcode_lift` vs `lift`

The `pcode_lift` value handlers move onto the merged lifter in `lift/`.
What remains of `pcode_lift` is the free-function utility set; keep it as
a module (e.g. `lift::vn_util` or retain `pcode_lift` for just the
helpers). `vn_sort_key` stays `pub` — the orchestrator imports it
(`strider_lift::pcode_lift::vn_sort_key`); if the module is renamed,
update that one downstream path.

## Tests

`tests/value_lifter.rs` (~40 cases) hand-builds `rsleigh::Insn` structs
with chosen REGISTER/CONST varnodes — they cannot be produced by decoding
bytes, so "drive a real CFG" is impossible. They currently construct
`ValueLifter::new(&mut builder, &sleigh)` directly.

After the merge the lifting entry is `pub(crate)`, so these tests move
**in-crate** as `#[cfg(test)]` units. A `with_test_lifter(|lifter| { … })`
helper owns the scaffold — a sleigh, a throwaway 1-region CFG (e.g. a
single `ret`), a default CC, the `Lifter` config, and the merged
`PerRegionDriver` — and hands `&mut lifter` to the closure (this owns the
locals so the per-CFG lifter, which borrows them, need not be returned).
Each test then calls `lifter.lift_value(&insn)` on its hand-built insn and
asserts on `lifter`'s builder/function. The throwaway CFG is never read by
value lifting (value methods touch only builder + sleigh). No assertion is
weakened; only the construction ritual changes.

## Scope / risk

Internal to strider-lift. One downstream touch point: if the `pcode_lift`
module is renamed, the orchestrator's `vn_sort_key` import path updates.
Behaviour-preserving: same IR for the same input. Gate: existing tests
pass (`cargo test --workspace` 0 failures, clippy clean, pytest 832).

The one judgement call (test mechanism) was delegated; chosen for: zero
loss of the hand-built-`Insn` coverage, no public test-only constructor
(honours the single-SSoT-constructor rule), and the merged lifter staying
`pub(crate)`.
